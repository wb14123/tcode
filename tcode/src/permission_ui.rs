use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use llm_rs::permission::PermissionState;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::protocol::{ClientMessage, ServerMessage};
use crate::session::Session;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum PermStatus {
    Pending,
    Session,
    Project,
}

impl PermStatus {
    fn label(&self) -> &'static str {
        match self {
            PermStatus::Pending => "pending",
            PermStatus::Session => "session",
            PermStatus::Project => "project",
        }
    }

    fn color(&self) -> Color {
        match self {
            PermStatus::Pending => Color::Yellow,
            PermStatus::Session => Color::Cyan,
            PermStatus::Project => Color::Green,
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            PermStatus::Pending => "?",
            PermStatus::Session => "S",
            PermStatus::Project => "P",
        }
    }
}

/// Flat node in the permission tree (tool, key, or value leaf).
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum NodeKind {
    Tool { name: String },
    Key { key: String },
    Value { value: String, status: PermStatus, prompt: Option<String> },
}

#[derive(Debug, Clone)]
struct TreeNode {
    kind: NodeKind,
    depth: usize,
    children: Vec<usize>,
    collapsed: bool,
}

struct PermissionTreeState {
    arena: Vec<TreeNode>,
    visible: Vec<usize>,
    selected: usize,
    filter_pending_only: bool,
    session_id: String,
    status_message: Option<String>,
}

impl PermissionTreeState {
    fn new(session_id: String) -> Self {
        PermissionTreeState {
            arena: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            filter_pending_only: false,
            session_id,
            status_message: None,
        }
    }

    /// Rebuild the tree from a PermissionState snapshot.
    fn rebuild_from_state(&mut self, state: &PermissionState) {
        self.arena.clear();

        // Group all entries by tool -> key -> Vec<(value, status, prompt)>
        let mut groups: HashMap<String, HashMap<String, Vec<(String, PermStatus, Option<String>)>>> =
            HashMap::new();

        for p in &state.pending {
            groups
                .entry(p.tool.clone())
                .or_default()
                .entry(p.key.clone())
                .or_default()
                .push((p.value.clone(), PermStatus::Pending, Some(p.prompt.clone())));
        }
        for k in &state.session {
            groups
                .entry(k.tool.clone())
                .or_default()
                .entry(k.key.clone())
                .or_default()
                .push((k.value.clone(), PermStatus::Session, None));
        }
        for k in &state.project {
            groups
                .entry(k.tool.clone())
                .or_default()
                .entry(k.key.clone())
                .or_default()
                .push((k.value.clone(), PermStatus::Project, None));
        }

        // Sort tools alphabetically, but put tools with pending items first
        let mut tool_names: Vec<String> = groups.keys().cloned().collect();
        tool_names.sort_by(|a, b| {
            let a_pending = groups[a].values().any(|vals| vals.iter().any(|(_, s, _)| *s == PermStatus::Pending));
            let b_pending = groups[b].values().any(|vals| vals.iter().any(|(_, s, _)| *s == PermStatus::Pending));
            b_pending.cmp(&a_pending).then(a.cmp(b))
        });

        for tool_name in &tool_names {
            let tool_idx = self.arena.len();
            self.arena.push(TreeNode {
                kind: NodeKind::Tool { name: tool_name.clone() },
                depth: 0,
                children: Vec::new(),
                collapsed: false,
            });

            let key_map = &groups[tool_name];
            let mut key_names: Vec<String> = key_map.keys().cloned().collect();
            key_names.sort();

            for key_name in &key_names {
                let key_idx = self.arena.len();
                self.arena.push(TreeNode {
                    kind: NodeKind::Key { key: key_name.clone() },
                    depth: 1,
                    children: Vec::new(),
                    collapsed: false,
                });
                self.arena[tool_idx].children.push(key_idx);

                let mut values = key_map[key_name].clone();
                // Sort: pending first, then alphabetical
                values.sort_by(|a, b| {
                    let a_pending = a.1 == PermStatus::Pending;
                    let b_pending = b.1 == PermStatus::Pending;
                    b_pending.cmp(&a_pending).then(a.0.cmp(&b.0))
                });

                for (value, status, prompt) in values {
                    if self.filter_pending_only && status != PermStatus::Pending {
                        continue;
                    }
                    let val_idx = self.arena.len();
                    self.arena.push(TreeNode {
                        kind: NodeKind::Value { value, status, prompt },
                        depth: 2,
                        children: Vec::new(),
                        collapsed: false,
                    });
                    self.arena[key_idx].children.push(val_idx);
                }
            }
        }

        self.rebuild_visible();
    }

    fn rebuild_visible(&mut self) {
        self.visible.clear();
        // Top-level nodes are tools (depth 0)
        let top_level: Vec<usize> = self.arena.iter().enumerate()
            .filter(|(_, n)| n.depth == 0)
            .map(|(i, _)| i)
            .collect();
        for idx in top_level {
            self.collect_visible(idx);
        }
        // Clamp selection
        if !self.visible.is_empty() && self.selected >= self.visible.len() {
            self.selected = self.visible.len() - 1;
        }
    }

    fn collect_visible(&mut self, idx: usize) {
        self.visible.push(idx);
        if !self.arena[idx].collapsed {
            let children = self.arena[idx].children.clone();
            for child in children {
                self.collect_visible(child);
            }
        }
    }

    fn move_down(&mut self) {
        if !self.visible.is_empty() && self.selected < self.visible.len() - 1 {
            self.selected += 1;
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn toggle_collapse(&mut self) {
        if let Some(&idx) = self.visible.get(self.selected) {
            if !self.arena[idx].children.is_empty() {
                self.arena[idx].collapsed = !self.arena[idx].collapsed;
                self.rebuild_visible();
            }
        }
    }

    fn toggle_filter(&mut self) {
        self.filter_pending_only = !self.filter_pending_only;
    }

    /// Open the approval or management popup for the selected leaf node.
    fn open_popup(&mut self) {
        let Some(&idx) = self.visible.get(self.selected) else { return };
        let node = &self.arena[idx];

        match &node.kind {
            NodeKind::Value { value, status, .. } => {
                // Walk up to find tool and key
                let (tool_name, key_name) = self.find_tool_key_for(idx);
                let exe = match std::env::current_exe() {
                    Ok(p) => p.to_string_lossy().to_string(),
                    Err(_) => return,
                };

                let cmd = match status {
                    PermStatus::Pending => {
                        format!(
                            "{} --session={} approve --tool {} --key {} --value {}",
                            exe, self.session_id, tool_name, key_name, value
                        )
                    }
                    PermStatus::Session | PermStatus::Project => {
                        format!(
                            "{} --session={} approve --manage --tool {} --key {} --value {}",
                            exe, self.session_id, tool_name, key_name, value
                        )
                    }
                };

                let popup_cmd = format!(
                    "tmux display-popup -E -w 60 -h 12 \"{}\"",
                    cmd.replace('"', "\\\"")
                );
                let _ = Command::new("sh").args(["-c", &popup_cmd]).output();
            }
            NodeKind::Tool { .. } | NodeKind::Key { .. } => {
                // Toggle collapse for non-leaf
                self.toggle_collapse();
            }
        }
    }

    /// Find the tool name and key name for a value node by walking up the tree.
    fn find_tool_key_for(&self, value_idx: usize) -> (String, String) {
        let mut tool_name = String::new();
        let mut key_name = String::new();

        for node in &self.arena {
            for &child_idx in &node.children {
                if child_idx == value_idx {
                    // This node is the key parent
                    if let NodeKind::Key { key } = &node.kind {
                        key_name = key.clone();
                    }
                }
                if self.arena[child_idx].children.contains(&value_idx) || child_idx == value_idx {
                    // Check if this is the tool parent of the key
                    if let NodeKind::Tool { name } = &node.kind {
                        tool_name = name.clone();
                    }
                }
            }
        }

        // Fallback: try direct parent search
        if key_name.is_empty() || tool_name.is_empty() {
            for node in &self.arena {
                if let NodeKind::Key { key } = &node.kind {
                    if node.children.contains(&value_idx) {
                        key_name = key.clone();
                        // Find tool parent of this key node
                        let key_idx = self.arena.iter().position(|n| std::ptr::eq(n, node)).unwrap_or(0);
                        for tool_node in &self.arena {
                            if let NodeKind::Tool { name } = &tool_node.kind {
                                if tool_node.children.contains(&key_idx) {
                                    tool_name = name.clone();
                                    break;
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }

        (tool_name, key_name)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_tree(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut PermissionTreeState,
    list_state: &mut ListState,
) -> Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        let chunks = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
        ]).split(area);

        // Build list items
        let items: Vec<ListItem> = state.visible.iter().map(|&idx| {
            let node = &state.arena[idx];
            let indent = "  ".repeat(node.depth);
            match &node.kind {
                NodeKind::Tool { name } => {
                    let prefix = if node.collapsed { "+" } else { "-" };
                    let has_pending = node.children.iter().any(|&ki| {
                        state.arena[ki].children.iter().any(|&vi| {
                            matches!(&state.arena[vi].kind, NodeKind::Value { status: PermStatus::Pending, .. })
                        })
                    });
                    let status_icon = if has_pending { " ?" } else { "" };
                    let line = Line::from(vec![
                        Span::raw(format!("{}{} {}", indent, prefix, name)),
                        Span::styled(status_icon, Style::default().fg(Color::Yellow)),
                    ]);
                    ListItem::new(line)
                }
                NodeKind::Key { key } => {
                    let prefix = if node.collapsed { "+" } else { "-" };
                    ListItem::new(Line::from(Span::raw(format!("{}{} {}", indent, prefix, key))))
                }
                NodeKind::Value { value, status, .. } => {
                    let line = Line::from(vec![
                        Span::raw(format!("{}  ", indent)),
                        Span::styled(
                            format!("[{}]", status.icon()),
                            Style::default().fg(status.color()),
                        ),
                        Span::raw(format!(" {}", value)),
                        Span::styled(
                            format!(" ({})", status.label()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]);
                    ListItem::new(line)
                }
            }
        }).collect();

        let filter_indicator = if state.filter_pending_only { " [pending only]" } else { "" };
        let title = format!(" Permissions{} ", filter_indicator);

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_stateful_widget(list, chunks[0], list_state);

        // Status bar
        let status_text = state.status_message.as_deref().unwrap_or("j/k:nav  o:open  f:filter  q:quit");
        let status = Paragraph::new(Line::from(Span::styled(
            status_text,
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(status, chunks[1]);
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Server query
// ---------------------------------------------------------------------------

fn query_permission_state_sync(socket_path: &PathBuf) -> Option<PermissionState> {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream as StdUnixStream;

    let mut stream = StdUnixStream::connect(socket_path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok()?;

    // LengthDelimitedCodec: 4-byte big-endian length prefix, then payload
    let msg = ClientMessage::GetPermissionState;
    let json = serde_json::to_vec(&msg).ok()?;
    let len = (json.len() as u32).to_be_bytes();
    stream.write_all(&len).ok()?;
    stream.write_all(&json).ok()?;
    stream.flush().ok()?;

    // Read response: 4-byte length prefix then payload
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;
    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).ok()?;

    let server_msg: ServerMessage = serde_json::from_slice(&resp_buf).ok()?;
    match server_msg {
        ServerMessage::PermissionState(state) => Some(state),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Display file watcher (for PermissionUpdated signal)
// ---------------------------------------------------------------------------

/// Check if any new PermissionUpdated lines have appeared in the display file.
fn check_for_permission_updates(display_path: &PathBuf, offset: &mut u64) -> bool {
    let mut file = match File::open(display_path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    if file.seek(SeekFrom::Start(*offset)).is_err() {
        return false;
    }
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return false;
    }
    let new_offset = *offset + buf.len() as u64;
    let has_update = buf.lines().any(|line| line.contains("PermissionUpdated"));
    *offset = new_offset;
    has_update
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub fn run_permission_ui(session: Session) -> Result<()> {
    let session_dir = session.session_dir().clone();
    let session_id = session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let socket_path = session.socket_path();
    let display_path = session.display_file();

    // Set up filesystem watcher
    let (fs_tx, fs_rx) = mpsc::channel();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = fs_tx.send(event);
        }
    })?;
    if session_dir.exists() {
        watcher.watch(&session_dir, RecursiveMode::NonRecursive)?;
    }

    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = PermissionTreeState::new(session_id);

    // Initial query
    let mut file_offset: u64 = 0;
    if let Some(perm_state) = query_permission_state_sync(&socket_path) {
        state.rebuild_from_state(&perm_state);
    }
    // Advance offset past existing content
    if let Ok(metadata) = std::fs::metadata(&display_path) {
        file_offset = metadata.len();
    }

    let mut list_state = ListState::default();
    if !state.visible.is_empty() {
        list_state.select(Some(0));
    }

    loop {
        // Drain filesystem events and check for PermissionUpdated
        let mut need_refresh = false;
        while fs_rx.try_recv().is_ok() {
            // Any change to the display file might contain PermissionUpdated
            if check_for_permission_updates(&display_path, &mut file_offset) {
                need_refresh = true;
            }
        }

        if need_refresh {
            if let Some(perm_state) = query_permission_state_sync(&socket_path) {
                state.rebuild_from_state(&perm_state);
            }
        }

        // Sync list state
        if state.visible.is_empty() {
            list_state.select(None);
        } else {
            list_state.select(Some(state.selected));
        }

        // Render
        render_tree(&mut terminal, &mut state, &mut list_state)?;

        // Handle keyboard input
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                state.status_message = None;
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                    KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                    KeyCode::Char(' ') => state.toggle_collapse(),
                    KeyCode::Enter | KeyCode::Char('o') => {
                        state.open_popup();
                        // Refresh after popup closes (user may have approved)
                        if let Some(perm_state) = query_permission_state_sync(&socket_path) {
                            state.rebuild_from_state(&perm_state);
                        }
                    }
                    KeyCode::Char('f') => {
                        state.toggle_filter();
                        // Re-query and rebuild with new filter
                        if let Some(perm_state) = query_permission_state_sync(&socket_path) {
                            state.rebuild_from_state(&perm_state);
                        }
                    }
                    KeyCode::Char('R') => {
                        if let Some(perm_state) = query_permission_state_sync(&socket_path) {
                            state.rebuild_from_state(&perm_state);
                        }
                    }
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
