use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use llm_rs::conversation::{Message, MessageEndStatus};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::session::Session;
use crate::tree_nav::TreeNav;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum NodeStatus {
    Generating,
    Running,
    Idle,
    Succeeded,
    Failed,
    Cancelled,
    Permission,
    Denied,
}

impl NodeStatus {
    fn label(&self) -> &'static str {
        match self {
            NodeStatus::Generating => "generating",
            NodeStatus::Running => "running",
            NodeStatus::Idle => "idle",
            NodeStatus::Succeeded => "done",
            NodeStatus::Failed => "failed",
            NodeStatus::Cancelled => "cancelled",
            NodeStatus::Permission => "permission",
            NodeStatus::Denied => "denied",
        }
    }

    fn color(&self) -> Color {
        match self {
            NodeStatus::Generating => Color::Yellow,
            NodeStatus::Running => Color::Yellow,
            NodeStatus::Idle => Color::DarkGray,
            NodeStatus::Succeeded => Color::Green,
            NodeStatus::Failed | NodeStatus::Cancelled | NodeStatus::Denied => Color::Red,
            NodeStatus::Permission => Color::LightYellow,
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            NodeStatus::Succeeded | NodeStatus::Failed | NodeStatus::Cancelled
        )
    }

    fn from_end_status(s: &MessageEndStatus) -> Self {
        match s {
            MessageEndStatus::Succeeded => NodeStatus::Succeeded,
            MessageEndStatus::Failed => NodeStatus::Failed,
            MessageEndStatus::Cancelled => NodeStatus::Cancelled,
            MessageEndStatus::Timeout => NodeStatus::Failed,
            MessageEndStatus::UserDenied => NodeStatus::Denied,
        }
    }
}

#[derive(Debug, Clone)]
enum NodeType {
    Root {
        session_id: String,
    },
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        tool_args: String,
        status: NodeStatus,
        input_tokens: i32,
        output_tokens: i32,
    },
    SubAgent {
        conversation_id: String,
        description: String,
        status: NodeStatus,
        input_tokens: i32,
        output_tokens: i32,
    },
}

#[derive(Debug, Clone)]
struct TreeNode {
    kind: NodeType,
    depth: usize,
    children: Vec<usize>,
    collapsed: bool,
}

struct FileTracker {
    offset: u64,
    line_buffer: String,
    /// Index of the Root or SubAgent node that owns this conversation dir.
    owner_node: usize,
}

struct TreeState {
    arena: Vec<TreeNode>,
    /// Visible node indices (rebuilt by `rebuild_visible`).
    visible: Vec<usize>,
    selected: usize,
    filter_active_only: bool,
    /// display.jsonl path → tracker.
    file_trackers: HashMap<PathBuf, FileTracker>,
    /// dir path → root/subagent node index.
    dir_to_node: HashMap<PathBuf, usize>,
    /// tool_call_id → node index.
    tool_call_idx: HashMap<String, usize>,
    /// conversation_id → node index.
    conversation_idx: HashMap<String, usize>,
    /// Maps tool_call_id → node arena index for subagent nodes created by SubAgentInputStart.
    subagent_tc_idx: HashMap<String, usize>,
    /// Session root directory.
    session_dir: PathBuf,
    /// Session ID (for display).
    session_id: String,
    /// Transient status/error message shown in the title bar.
    status_message: Option<String>,
}

impl TreeState {
    fn new(session_dir: PathBuf, session_id: String) -> Self {
        let mut state = TreeState {
            arena: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            filter_active_only: false,
            file_trackers: HashMap::new(),
            dir_to_node: HashMap::new(),
            tool_call_idx: HashMap::new(),
            conversation_idx: HashMap::new(),
            subagent_tc_idx: HashMap::new(),
            session_dir: session_dir.clone(),
            session_id: session_id.clone(),
            status_message: None,
        };
        // Create root node
        let root_idx = state.arena.len();
        state.arena.push(TreeNode {
            kind: NodeType::Root { session_id },
            depth: 0,
            children: Vec::new(),
            collapsed: false,
        });
        state.dir_to_node.insert(session_dir.clone(), root_idx);

        // Register the main display.jsonl
        let display_file = session_dir.join("display.jsonl");
        state.file_trackers.insert(
            display_file,
            FileTracker {
                offset: 0,
                line_buffer: String::new(),
                owner_node: root_idx,
            },
        );
        state
    }

    /// Full refresh: reset offsets to 0 and re-read everything.
    fn full_refresh(&mut self) {
        // Reset all trackers to start
        for tracker in self.file_trackers.values_mut() {
            tracker.offset = 0;
            tracker.line_buffer.clear();
        }
        // Clear children of root but keep root itself
        self.arena[0].children.clear();
        self.arena.truncate(1);
        self.tool_call_idx.clear();
        self.conversation_idx.clear();
        self.subagent_tc_idx.clear();
        // Keep only the root dir mapping
        let root_dir = self.session_dir.clone();
        self.dir_to_node.clear();
        self.dir_to_node.insert(root_dir, 0);
        // Keep only root display.jsonl tracker
        let root_display = self.session_dir.join("display.jsonl");
        let old_trackers: Vec<PathBuf> = self.file_trackers.keys().cloned().collect();
        for path in old_trackers {
            if path != root_display {
                self.file_trackers.remove(&path);
            } else if let Some(t) = self.file_trackers.get_mut(&path) {
                t.offset = 0;
                t.line_buffer.clear();
                t.owner_node = 0;
            }
        }
        // Now discover subagent dirs and register them
        self.discover_subagent_dirs(&self.session_dir.clone(), 0, 1);
        // Read all files
        self.read_all_files();
        self.rebuild_visible();
    }

    /// Discover subagent-* directories recursively from `dir` and register trackers.
    fn discover_subagent_dirs(&mut self, dir: &Path, parent_node: usize, depth: usize) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let Some(conversation_id) = name.strip_prefix("subagent-") else {
                continue;
            };
            let conversation_id = conversation_id.to_string();

            // Only create/register node if directory not already tracked
            if !self.dir_to_node.contains_key(&path) {
                // If a node already exists (from SubAgentStart message), reuse it;
                // otherwise create a new one.
                let node_idx =
                    if let Some(&existing_idx) = self.conversation_idx.get(&conversation_id) {
                        existing_idx
                    } else {
                        let idx = self.arena.len();
                        self.arena.push(TreeNode {
                            kind: NodeType::SubAgent {
                                conversation_id: conversation_id.clone(),
                                description: String::new(),
                                status: NodeStatus::Running,
                                input_tokens: 0,
                                output_tokens: 0,
                            },
                            depth,
                            children: Vec::new(),
                            collapsed: false,
                        });
                        self.arena[parent_node].children.push(idx);
                        self.conversation_idx.insert(conversation_id.clone(), idx);
                        idx
                    };
                self.dir_to_node.insert(path.clone(), node_idx);

                let display_file = path.join("display.jsonl");
                self.file_trackers
                    .entry(display_file)
                    .or_insert_with(|| FileTracker {
                        offset: 0,
                        line_buffer: String::new(),
                        owner_node: node_idx,
                    });
            }

            // Recurse into this subagent dir
            let node_idx = self.dir_to_node[&path];
            let next_depth = depth + 1;
            self.discover_subagent_dirs(&path, node_idx, next_depth);
        }
    }

    /// Read new bytes from all tracked files and process events.
    fn read_all_files(&mut self) {
        let paths: Vec<PathBuf> = self.file_trackers.keys().cloned().collect();
        for path in paths {
            self.read_file(&path);
        }
    }

    /// Read new bytes from a single tracked file and process events.
    fn read_file(&mut self, path: &PathBuf) {
        let (offset, owner_node, mut line_buffer) = match self.file_trackers.get(path) {
            Some(t) => (t.offset, t.owner_node, t.line_buffer.clone()),
            None => return,
        };

        let file_size = match fs::metadata(path) {
            Ok(m) => m.len(),
            Err(_) => return,
        };
        if file_size <= offset {
            return;
        }

        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return,
        };
        if let Err(e) = file.seek(SeekFrom::Start(offset)) {
            tracing::warn!(path = %path.display(), error = %e, "Failed to seek in JSONL file");
            return;
        }

        let mut new_bytes = Vec::new();
        if let Err(e) = file.read_to_end(&mut new_bytes) {
            tracing::warn!(path = %path.display(), error = %e, "Failed to read JSONL file");
            return;
        }
        let new_text = String::from_utf8_lossy(&new_bytes);

        // Prepend buffered partial line
        let combined = if line_buffer.is_empty() {
            new_text.to_string()
        } else {
            let mut s = std::mem::take(&mut line_buffer);
            s.push_str(&new_text);
            s
        };

        let mut lines: Vec<&str> = combined.split('\n').collect();
        // If no trailing newline, last element is incomplete
        let last = lines.pop().unwrap_or("");
        let new_line_buffer = if combined.ends_with('\n') {
            // last is "" after split, nothing buffered
            String::new()
        } else {
            last.to_string()
        };

        // Determine the directory that owns this file
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();

        for line in &lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(line) {
                Ok(msg) => self.process_event(&dir, owner_node, &msg),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        line = %line.chars().take(80).collect::<String>(),
                        "Failed to parse JSONL line"
                    );
                }
            }
        }

        // Update tracker
        if let Some(tracker) = self.file_trackers.get_mut(path) {
            tracker.offset = offset + new_bytes.len() as u64;
            tracker.line_buffer = new_line_buffer;
        }
    }

    /// Incremental update: discover new subagent dirs, read new bytes from all files.
    fn incremental_update(&mut self) {
        // Discover any new subagent dirs
        let root = self.session_dir.clone();
        self.discover_subagent_dirs(&root, 0, 1);
        // Read new data from all tracked files
        self.read_all_files();
        self.rebuild_visible();
    }

    /// Process a single Message event, updating tree nodes.
    fn process_event(&mut self, dir: &Path, owner_node: usize, msg: &Message) {
        match msg {
            Message::AssistantToolCallStart {
                tool_call_id,
                tool_name,
                ..
            } => {
                if self.tool_call_idx.contains_key(tool_call_id) {
                    return; // Already tracked
                }
                let depth = self.arena[owner_node].depth + 1;
                let node_idx = self.arena.len();
                self.arena.push(TreeNode {
                    kind: NodeType::ToolCall {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        tool_args: String::new(),
                        status: NodeStatus::Generating,
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                    depth,
                    children: Vec::new(),
                    collapsed: false,
                });
                self.arena[owner_node].children.push(node_idx);
                self.tool_call_idx.insert(tool_call_id.clone(), node_idx);
            }
            Message::ToolMessageStart {
                tool_call_id,
                tool_name,
                tool_args,
                ..
            } => {
                if let Some(&idx) = self.tool_call_idx.get(tool_call_id) {
                    // Node already exists (created by AssistantToolCallStart),
                    // update status from Generating to Running and fill in tool_args.
                    if let NodeType::ToolCall {
                        status,
                        tool_args: existing_args,
                        ..
                    } = &mut self.arena[idx].kind
                    {
                        *status = NodeStatus::Running;
                        if existing_args.is_empty() && !tool_args.is_empty() {
                            *existing_args = tool_args.clone();
                        }
                    }
                    return;
                }
                let depth = self.arena[owner_node].depth + 1;
                let node_idx = self.arena.len();
                self.arena.push(TreeNode {
                    kind: NodeType::ToolCall {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        tool_args: tool_args.clone(),
                        status: NodeStatus::Running,
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                    depth,
                    children: Vec::new(),
                    collapsed: false,
                });
                self.arena[owner_node].children.push(node_idx);
                self.tool_call_idx.insert(tool_call_id.clone(), node_idx);
            }
            Message::ToolMessageEnd {
                tool_call_id,
                end_status,
                input_tokens,
                output_tokens,
                ..
            } => {
                if let Some(&idx) = self.tool_call_idx.get(tool_call_id)
                    && let NodeType::ToolCall {
                        status,
                        input_tokens: it,
                        output_tokens: ot,
                        ..
                    } = &mut self.arena[idx].kind
                {
                    *status = NodeStatus::from_end_status(end_status);
                    *it = *input_tokens;
                    *ot = *output_tokens;
                }
            }
            Message::SubAgentStart {
                tool_call_id,
                conversation_id,
                description,
                ..
            } => {
                // First check if we have a node pre-created by SubAgentInputStart
                if let Some(idx) = self.subagent_tc_idx.remove(tool_call_id) {
                    // If discover_subagent_dirs already created a duplicate node for this
                    // conversation_id, clean it up: remove from parent's children and
                    // repoint its mappings to the node we're keeping.
                    if let Some(&dup_idx) = self.conversation_idx.get(conversation_id)
                        && dup_idx != idx
                    {
                        // Remove duplicate from its parent's children
                        for node in self.arena.iter_mut() {
                            node.children.retain(|&c| c != dup_idx);
                        }
                        // Repoint file_trackers that referenced the duplicate
                        for tracker in self.file_trackers.values_mut() {
                            if tracker.owner_node == dup_idx {
                                tracker.owner_node = idx;
                            }
                        }
                        // Repoint dir_to_node entries
                        for v in self.dir_to_node.values_mut() {
                            if *v == dup_idx {
                                *v = idx;
                            }
                        }
                        // Transfer children from duplicate to this node
                        let dup_children: Vec<usize> =
                            self.arena[dup_idx].children.drain(..).collect();
                        self.arena[idx].children.extend(dup_children);
                    }

                    // Update the existing node with the now-known conversation_id and description
                    if let NodeType::SubAgent {
                        description: desc,
                        conversation_id: cid,
                        status,
                        ..
                    } = &mut self.arena[idx].kind
                    {
                        *desc = description.clone();
                        *cid = conversation_id.clone();
                        *status = NodeStatus::Running;
                    }
                    self.conversation_idx.insert(conversation_id.clone(), idx);

                    // Register the subagent directory and display file if they exist
                    let sa_dir = dir.join(format!("subagent-{}", conversation_id));
                    if sa_dir.is_dir() {
                        self.dir_to_node.insert(sa_dir.clone(), idx);
                        let sa_display = sa_dir.join("display.jsonl");
                        // Use and_modify to ensure we overwrite any tracker
                        // that discover_subagent_dirs created pointing to the duplicate node
                        self.file_trackers
                            .entry(sa_display)
                            .and_modify(|t| t.owner_node = idx)
                            .or_insert_with(|| FileTracker {
                                offset: 0,
                                line_buffer: String::new(),
                                owner_node: idx,
                            });
                    }
                } else if let Some(&idx) = self.conversation_idx.get(conversation_id) {
                    // Update description and status
                    if let NodeType::SubAgent {
                        description: desc,
                        status,
                        ..
                    } = &mut self.arena[idx].kind
                    {
                        *desc = description.clone();
                        *status = NodeStatus::Running;
                    }
                } else {
                    // Create new subagent node (dir may not exist yet)
                    let depth = self.arena[owner_node].depth + 1;
                    let node_idx = self.arena.len();
                    self.arena.push(TreeNode {
                        kind: NodeType::SubAgent {
                            conversation_id: conversation_id.clone(),
                            description: description.clone(),
                            status: NodeStatus::Running,
                            input_tokens: 0,
                            output_tokens: 0,
                        },
                        depth,
                        children: Vec::new(),
                        collapsed: false,
                    });
                    self.arena[owner_node].children.push(node_idx);
                    self.conversation_idx
                        .insert(conversation_id.clone(), node_idx);

                    // Register the subagent directory and display file if they exist
                    let sa_dir = dir.join(format!("subagent-{}", conversation_id));
                    if sa_dir.is_dir() {
                        self.dir_to_node.insert(sa_dir.clone(), node_idx);
                        let sa_display = sa_dir.join("display.jsonl");
                        self.file_trackers
                            .entry(sa_display)
                            .or_insert_with(|| FileTracker {
                                offset: 0,
                                line_buffer: String::new(),
                                owner_node: node_idx,
                            });
                    }
                }
            }
            Message::SubAgentEnd {
                conversation_id,
                end_status,
                input_tokens,
                output_tokens,
                ..
            } => {
                if let Some(&idx) = self.conversation_idx.get(conversation_id)
                    && let NodeType::SubAgent {
                        status,
                        input_tokens: it,
                        output_tokens: ot,
                        ..
                    } = &mut self.arena[idx].kind
                {
                    *status = NodeStatus::from_end_status(end_status);
                    *it = *input_tokens;
                    *ot = *output_tokens;
                }
            }
            Message::SubAgentTurnEnd {
                conversation_id,
                end_status,
                input_tokens,
                output_tokens,
                ..
            } => {
                if let Some(&idx) = self.conversation_idx.get(conversation_id)
                    && let NodeType::SubAgent {
                        status,
                        input_tokens: it,
                        output_tokens: ot,
                        ..
                    } = &mut self.arena[idx].kind
                {
                    *status = match end_status {
                        MessageEndStatus::Cancelled => NodeStatus::Cancelled,
                        _ => NodeStatus::Idle,
                    };
                    *it = *input_tokens;
                    *ot = *output_tokens;
                }
            }
            Message::SubAgentContinue {
                conversation_id, ..
            } => {
                if let Some(&idx) = self.conversation_idx.get(conversation_id)
                    && let NodeType::SubAgent { status, .. } = &mut self.arena[idx].kind
                {
                    *status = NodeStatus::Running;
                }
            }
            Message::ToolRequestPermission { tool_call_id, .. } => {
                if let Some(&idx) = self.tool_call_idx.get(tool_call_id)
                    && let NodeType::ToolCall { status, .. } = &mut self.arena[idx].kind
                {
                    *status = NodeStatus::Permission;
                }
            }
            Message::ToolPermissionApproved { tool_call_id, .. } => {
                if let Some(&idx) = self.tool_call_idx.get(tool_call_id)
                    && let NodeType::ToolCall { status, .. } = &mut self.arena[idx].kind
                {
                    *status = NodeStatus::Running;
                }
            }
            Message::SubAgentWaitingPermission {
                conversation_id, ..
            } => {
                if let Some(&idx) = self.conversation_idx.get(conversation_id)
                    && let NodeType::SubAgent { status, .. } = &mut self.arena[idx].kind
                    && !status.is_terminal()
                {
                    *status = NodeStatus::Permission;
                }
            }
            Message::SubAgentPermissionApproved {
                conversation_id, ..
            } => {
                if let Some(&idx) = self.conversation_idx.get(conversation_id)
                    && let NodeType::SubAgent { status, .. } = &mut self.arena[idx].kind
                    && matches!(status, NodeStatus::Permission)
                {
                    *status = NodeStatus::Running;
                }
            }
            Message::SubAgentPermissionDenied {
                conversation_id, ..
            } => {
                if let Some(&idx) = self.conversation_idx.get(conversation_id)
                    && let NodeType::SubAgent { status, .. } = &mut self.arena[idx].kind
                    && !status.is_terminal()
                {
                    *status = NodeStatus::Denied;
                }
            }
            Message::SubAgentInputStart {
                tool_call_id,
                tool_name,
                ..
            } => {
                if tool_name == "subagent" {
                    // Create a SubAgent node with Generating status as a placeholder.
                    // conversation_id is not yet known; it will be filled in by SubAgentStart.
                    let depth = self.arena[owner_node].depth + 1;
                    let node_idx = self.arena.len();
                    self.arena.push(TreeNode {
                        kind: NodeType::SubAgent {
                            conversation_id: String::new(),
                            description: String::new(),
                            status: NodeStatus::Generating,
                            input_tokens: 0,
                            output_tokens: 0,
                        },
                        depth,
                        children: Vec::new(),
                        collapsed: false,
                    });
                    self.arena[owner_node].children.push(node_idx);
                    self.subagent_tc_idx.insert(tool_call_id.clone(), node_idx);
                }
                // For continue_subagent: do nothing — the existing node stays at [idle]
            }
            Message::SubAgentInputChunk { .. } => {
                // Subagent input chunks don't affect the tree
            }
            // All other message types are ignored
            _ => {}
        }
    }

    /// Check if a node is currently running (or waiting for permission).
    fn is_running(&self, idx: usize) -> bool {
        match &self.arena[idx].kind {
            NodeType::Root { .. } => false,
            NodeType::ToolCall { status, .. } | NodeType::SubAgent { status, .. } => {
                matches!(
                    status,
                    NodeStatus::Generating | NodeStatus::Running | NodeStatus::Permission
                )
            }
        }
    }

    /// Sort children: running first, then by arena index (creation time).
    fn sorted_children(&self, children: &[usize]) -> Vec<usize> {
        let mut sorted = children.to_vec();
        sorted.sort_by_key(|&child_idx| (!self.is_running(child_idx), child_idx));
        sorted
    }

    fn dfs_collect(&mut self, idx: usize) {
        // If filtering active only, skip finished nodes without running children
        if self.filter_active_only && !self.has_active_descendant(idx) {
            return;
        }
        self.visible.push(idx);
        if !self.arena[idx].collapsed {
            let children = self.sorted_children(&self.arena[idx].children.clone());
            for &child_idx in &children {
                self.dfs_collect(child_idx);
            }
        }
    }

    fn has_active_descendant(&self, idx: usize) -> bool {
        let node = &self.arena[idx];
        let is_active = match &node.kind {
            NodeType::Root { .. } => true,
            NodeType::ToolCall { status, .. } | NodeType::SubAgent { status, .. } => {
                matches!(status, NodeStatus::Generating | NodeStatus::Running)
            }
        };
        if is_active {
            return true;
        }
        for &child in &node.children {
            if self.has_active_descendant(child) {
                return true;
            }
        }
        false
    }

    fn toggle_filter(&mut self) {
        self.filter_active_only = !self.filter_active_only;
        self.rebuild_visible();
    }

    /// Cancel the selected subagent conversation or tool call via CLI subcommands.
    fn cancel_selected(&mut self) {
        let idx = match self.visible.get(self.selected) {
            Some(&i) => i,
            None => return,
        };
        let node = &self.arena[idx];
        let exe = match self.resolve_exe() {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("{}", e);
                self.status_message = Some(e);
                return;
            }
        };
        let args: Vec<String> = match &node.kind {
            NodeType::SubAgent {
                conversation_id,
                status,
                ..
            } => {
                if matches!(status, NodeStatus::Running | NodeStatus::Idle) {
                    vec![
                        format!("--session={}", self.session_id),
                        "cancel-conversation".to_string(),
                        conversation_id.clone(),
                    ]
                } else {
                    return; // Already finished
                }
            }
            NodeType::ToolCall {
                tool_call_id,
                status,
                ..
            } => {
                if matches!(status, NodeStatus::Running) {
                    vec![
                        format!("--session={}", self.session_id),
                        "cancel-tool".to_string(),
                        tool_call_id.clone(),
                    ]
                } else {
                    return; // Not running
                }
            }
            _ => return,
        };
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        match std::process::Command::new(&exe).args(&args_ref).output() {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                tracing::error!("Cancel command failed: {}", stderr);
                self.status_message = Some(format!("Cancel failed: {}", stderr));
            }
            Err(e) => {
                tracing::error!("Failed to run cancel command: {}", e);
                self.status_message = Some(format!("Cancel failed: {}", e));
            }
            _ => {
                self.status_message = None;
            }
        }
    }

    /// Resolve the current executable path, stripping the " (deleted)" suffix
    /// that Linux appends to `/proc/self/exe` when the binary was replaced.
    fn resolve_exe(&self) -> Result<String, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to determine current executable: {}", e))?;
        let exe_str = exe.to_string_lossy().to_string();

        // On Linux, /proc/self/exe gets " (deleted)" appended after a rebuild
        if exe_str.ends_with(" (deleted)") {
            let stripped = &exe_str[..exe_str.len() - " (deleted)".len()];
            if Path::new(stripped).exists() {
                return Ok(stripped.to_string());
            }
            return Err(format!(
                "Executable was replaced and original not found: {}",
                stripped
            ));
        }

        if !exe.exists() {
            return Err(format!("Executable not found: {}", exe_str));
        }

        Ok(exe_str)
    }

    /// Look up the session string for a node that has a `dir_to_node` entry.
    fn dir_node_to_session(&self, node_idx: usize) -> Option<String> {
        self.dir_to_node
            .iter()
            .find_map(|(dir, &nidx)| {
                if nidx == node_idx {
                    Some(dir.clone())
                } else {
                    None
                }
            })
            .and_then(|dir| {
                dir.strip_prefix(&self.session_dir).ok().map(|rel| {
                    if rel.as_os_str().is_empty() {
                        self.session_id.clone()
                    } else {
                        format!("{}/{}", self.session_id, rel.display())
                    }
                })
            })
    }

    /// Get the session path for the parent conversation that owns a node.
    fn parent_session(&self, idx: usize) -> String {
        let parent_idx = self
            .arena
            .iter()
            .position(|n| n.children.contains(&idx))
            .unwrap_or(0);
        match self.dir_node_to_session(parent_idx) {
            Some(session) => session,
            None => {
                if parent_idx != 0 {
                    tracing::warn!(
                        parent_idx,
                        node_idx = idx,
                        "dir_node_to_session returned None for non-root parent, falling back to root session"
                    );
                }
                self.session_id.clone()
            }
        }
    }

    /// Open detail view for the selected node via CLI subcommands.
    fn open_detail(&mut self) {
        let idx = match self.visible.get(self.selected) {
            Some(&i) => i,
            None => return,
        };
        let node = &self.arena[idx];
        let exe = match self.resolve_exe() {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("{}", e);
                self.status_message = Some(e);
                return;
            }
        };
        let parent_session = self.parent_session(idx);
        let args: Vec<String> = match &node.kind {
            NodeType::ToolCall { tool_call_id, .. } => {
                vec![
                    format!("--session={}", parent_session),
                    "open-tool-call".to_string(),
                    tool_call_id.clone(),
                ]
            }
            NodeType::SubAgent {
                conversation_id, ..
            } => {
                vec![
                    format!("--session={}", parent_session),
                    "open-subagent".to_string(),
                    conversation_id.clone(),
                ]
            }
            NodeType::Root { .. } => return,
        };
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        match std::process::Command::new(&exe).args(&args_ref).output() {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                tracing::error!("Failed to open detail view: {}", stderr);
                self.status_message = Some(format!("Open failed: {}", stderr));
            }
            Err(e) => {
                tracing::error!("Failed to run open detail command: {}", e);
                self.status_message = Some(format!("Open failed: {}", e));
            }
            _ => {
                self.status_message = None;
            }
        }
    }
}

impl TreeNav for TreeState {
    fn node_children(&self, idx: usize) -> &[usize] {
        &self.arena[idx].children
    }
    fn node_collapsed(&self, idx: usize) -> bool {
        self.arena[idx].collapsed
    }
    fn set_node_collapsed(&mut self, idx: usize, collapsed: bool) {
        self.arena[idx].collapsed = collapsed;
    }
    fn visible(&self) -> &[usize] {
        &self.visible
    }
    fn selected(&self) -> usize {
        self.selected
    }
    fn set_selected(&mut self, idx: usize) {
        self.selected = idx;
    }

    fn rebuild_visible(&mut self) {
        self.visible.clear();
        if self.arena.is_empty() {
            return;
        }
        let root_children = self.sorted_children(&self.arena[0].children.clone());
        for &child_idx in &root_children {
            self.dfs_collect(child_idx);
        }
        self.clamp_selection();
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_tree(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut TreeState,
    list_state: &mut ListState,
) -> Result<()> {
    terminal.draw(|f| {
        let chunks = Layout::vertical([
            Constraint::Length(1), // Title
            Constraint::Min(3),    // Tree
            Constraint::Length(2), // Help
        ])
        .split(f.area());

        // Title
        let title = if let Some(ref msg) = state.status_message {
            Line::from(vec![
                Span::styled(
                    format!(" Tree: {} ", state.session_id),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("| {} ", msg),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(
                    format!(" Tree: {} ", state.session_id),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "({} nodes) ",
                        state.arena.len().saturating_sub(1) // exclude root
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
                if state.filter_active_only {
                    Span::styled("[showing: running] ", Style::default().fg(Color::Yellow))
                } else {
                    Span::styled("[showing: all] ", Style::default().fg(Color::DarkGray))
                },
            ])
        };
        f.render_widget(Paragraph::new(title), chunks[0]);

        // Tree list
        let items: Vec<ListItem> = state
            .visible
            .iter()
            .enumerate()
            .map(|(vi, &node_idx)| render_node_line(state, node_idx, vi, f.area().width as usize))
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::REVERSED),
            );
        f.render_stateful_widget(list, chunks[1], list_state);

        // Help bar
        let help = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" j/↓", Style::default().fg(Color::Yellow)),
                Span::raw(" down  "),
                Span::styled("k/↑", Style::default().fg(Color::Yellow)),
                Span::raw(" up  "),
                Span::styled("Space", Style::default().fg(Color::Yellow)),
                Span::raw(" toggle  "),
                Span::styled("Enter/o", Style::default().fg(Color::Yellow)),
                Span::raw(" open  "),
            ]),
            Line::from(vec![
                Span::styled(" C-k", Style::default().fg(Color::Yellow)),
                Span::raw(" cancel  "),
                Span::styled("f", Style::default().fg(Color::Yellow)),
                Span::raw(" running/all  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ]),
        ]);
        f.render_widget(help, chunks[2]);
    })?;
    Ok(())
}

/// Extract a compact summary from tool_args JSON.
/// e.g. for Bash: show the command; for Read: show the file_path; otherwise flatten key=val.
fn summarize_tool_args(args: &str) -> String {
    let obj: serde_json::Value = match serde_json::from_str(args) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let map = match obj.as_object() {
        Some(m) => m,
        None => return String::new(),
    };
    // Pick the most useful single field if present
    for key in &["command", "file_path", "pattern", "query", "url", "prompt"] {
        if let Some(val) = map.get(*key)
            && let Some(s) = val.as_str()
        {
            return s.to_string();
        }
    }
    // Fallback: show all fields compactly
    let parts: Vec<String> = map
        .iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{}={}", k, s)
        })
        .collect();
    parts.join(" ")
}

fn render_node_line(
    state: &TreeState,
    node_idx: usize,
    _vi: usize,
    width: usize,
) -> ListItem<'static> {
    let node = &state.arena[node_idx];
    let depth = node.depth;

    // Build tree connector prefix
    let indent = if depth > 1 {
        "│  ".repeat(depth - 1)
    } else {
        String::new()
    };
    let connector = if depth > 0 {
        // Check if this is the last child of its parent
        let is_last = is_last_visible_sibling(state, node_idx);
        if is_last { "└─ " } else { "├─ " }
    } else {
        ""
    };

    let (label, status_span, tokens_span, collapse_indicator) = match &node.kind {
        NodeType::Root { session_id } => (
            format!("Root: {}", session_id),
            Span::raw(""),
            Span::raw(""),
            String::new(),
        ),
        NodeType::ToolCall {
            tool_name,
            tool_args,
            status,
            input_tokens,
            output_tokens,
            ..
        } => {
            let tok = if *input_tokens > 0 || *output_tokens > 0 {
                Span::styled(
                    format!(" [{}/{}]", input_tokens, output_tokens),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                Span::raw("")
            };
            let args_summary = summarize_tool_args(tool_args);
            let label = if args_summary.is_empty() {
                format!("[tool] {}", tool_name)
            } else {
                format!("[tool] {} {}", tool_name, args_summary)
            };
            (
                label,
                Span::styled(
                    status.label().to_string(),
                    Style::default().fg(status.color()),
                ),
                tok,
                String::new(),
            )
        }
        NodeType::SubAgent {
            description,
            status,
            input_tokens,
            output_tokens,
            ..
        } => {
            let tok = if *input_tokens > 0 || *output_tokens > 0 {
                Span::styled(
                    format!(" [{}/{}]", input_tokens, output_tokens),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                Span::raw("")
            };
            let collapse = if !node.children.is_empty() {
                if node.collapsed {
                    " [+]".to_string()
                } else {
                    " [-]".to_string()
                }
            } else {
                String::new()
            };
            (
                format!("[agent] {}", description),
                Span::styled(
                    status.label().to_string(),
                    Style::default().fg(status.color()),
                ),
                tok,
                collapse,
            )
        }
    };

    // Build the line: prefix + label (truncated to fit) + padding + status + tokens
    let prefix = format!("{}{}", indent, connector);
    let status_text = format!("{}", status_span.content);
    let tokens_text = format!("{}", tokens_span.content);
    let right_len = status_text.len() + tokens_text.len();
    let collapse_len = collapse_indicator.len();
    // Reserve: 2 for borders, 1 for min padding between label and status
    let fixed_overhead = prefix.len() + collapse_len + right_len + 3;
    let max_label_width = width.saturating_sub(fixed_overhead);
    let label = if label.len() > max_label_width && max_label_width > 3 {
        let truncate_at = max_label_width - 3;
        // Find a valid char boundary at or before the target byte position
        let boundary = label.floor_char_boundary(truncate_at);
        format!("{}...", &label[..boundary])
    } else {
        label
    };

    let left_part = format!("{}{}{}", prefix, label, collapse_indicator);
    let total_left = left_part.len();
    let padding = if width > total_left + right_len + 2 {
        " ".repeat(width - total_left - right_len - 2)
    } else {
        " ".to_string()
    };

    let type_color = match &node.kind {
        NodeType::Root { .. } => Color::Cyan,
        NodeType::ToolCall { .. } => Color::LightBlue,
        NodeType::SubAgent { .. } => Color::LightCyan,
    };

    ListItem::new(Line::from(vec![
        Span::styled(prefix, Style::default().fg(Color::DarkGray)),
        Span::styled(label, Style::default().fg(type_color)),
        Span::styled(collapse_indicator, Style::default().fg(Color::DarkGray)),
        Span::raw(padding),
        status_span,
        tokens_span,
    ]))
}

/// Check if a node is the last visible sibling among its parent's children.
fn is_last_visible_sibling(state: &TreeState, node_idx: usize) -> bool {
    // Find this node's parent
    for node in &state.arena {
        if node.children.contains(&node_idx) {
            return node.children.last() == Some(&node_idx);
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the interactive tree view for a session.
pub fn run_tree(session: Session) -> Result<()> {
    let session_dir = session.session_dir().clone();
    let session_id = session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let socket_path = session.socket_path();

    // Set up notify watcher
    let (fs_tx, fs_rx) = mpsc::channel();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res
            && fs_tx.send(event).is_err()
        {
            tracing::debug!("fs watcher channel closed");
        }
    })?;

    // Watch session directory recursively (inotify auto-watches new subdirs)
    if session_dir.exists() {
        watcher.watch(&session_dir, RecursiveMode::Recursive)?;
    }

    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Build initial tree state
    let mut state = TreeState::new(session_dir.clone(), session_id);
    state.full_refresh();

    let mut list_state = ListState::default();
    if !state.visible.is_empty() {
        list_state.select(Some(0));
    }

    // Event loop
    loop {
        // 1. Drain filesystem events
        let mut fs_changed = false;
        while let Ok(_event) = fs_rx.try_recv() {
            fs_changed = true;
        }
        if fs_changed {
            state.incremental_update();
        }

        // 2. Sync list_state with tree state
        state.sync_list_state(&mut list_state);

        // 3. Render
        render_tree(&mut terminal, &mut state, &mut list_state)?;

        // 4. Handle keyboard input (poll with timeout)
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            // Clear status message on any key press
            state.status_message = None;
            // Handle Ctrl-k before plain k (cancel vs move up)
            if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
                state.cancel_selected();
            } else if key.code == KeyCode::Char('p')
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                crate::permission_ui::approve_all_pending(&state.session_id, &socket_path);
            } else {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                    KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                    KeyCode::Char(' ') => state.toggle_collapse(),
                    KeyCode::Enter | KeyCode::Char('o') => state.open_detail(),
                    KeyCode::Char('f') => state.toggle_filter(),

                    _ => {}
                }
            }
        }
    }

    // Teardown
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
