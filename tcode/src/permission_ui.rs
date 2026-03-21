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
use crate::tree_nav::TreeNav;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Escape a string for safe inclusion in single-quoted shell arguments.
fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

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
    Tool {
        name: String,
    },
    Key {
        key: String,
    },
    Value {
        value: String,
        status: PermStatus,
        prompt: Option<String>,
        request_id: Option<String>,
        preview_file_path: Option<String>,
    },
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
    /// Frame counter for flash animation.
    frame_count: u64,
    /// Whether we've already sent a terminal bell for the current pending batch.
    bell_sent: bool,
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
            frame_count: 0,
            bell_sent: false,
        }
    }

    /// Query the server and rebuild the tree.
    fn refresh_from_server(&mut self, socket_path: &PathBuf) {
        if let Some(perm_state) = query_permission_state_sync(socket_path) {
            self.rebuild_from_state(&perm_state);
        }
    }

    /// Check if any pending permissions exist in the tree.
    fn has_pending(&self) -> bool {
        self.arena.iter().any(|n| {
            matches!(
                &n.kind,
                NodeKind::Value {
                    status: PermStatus::Pending,
                    ..
                }
            )
        })
    }

    /// Rebuild the tree from a PermissionState snapshot.
    fn rebuild_from_state(&mut self, state: &PermissionState) {
        self.arena.clear();

        // Group all entries by tool -> key -> Vec<(value, status, prompt, request_id, preview_file_path)>
        type EntryTuple = (
            String,
            PermStatus,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let mut groups: HashMap<String, HashMap<String, Vec<EntryTuple>>> = HashMap::new();

        for p in &state.pending {
            groups
                .entry(p.tool.clone())
                .or_default()
                .entry(p.key.clone())
                .or_default()
                .push((
                    p.value.clone(),
                    PermStatus::Pending,
                    Some(p.prompt.clone()),
                    Some(p.request_id.clone()),
                    p.preview_file_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string()),
                ));
        }
        for k in &state.session {
            groups
                .entry(k.tool.clone())
                .or_default()
                .entry(k.key.clone())
                .or_default()
                .push((k.value.clone(), PermStatus::Session, None, None, None));
        }
        for k in &state.project {
            groups
                .entry(k.tool.clone())
                .or_default()
                .entry(k.key.clone())
                .or_default()
                .push((k.value.clone(), PermStatus::Project, None, None, None));
        }

        // Sort tools alphabetically, but put tools with pending items first
        let mut tool_names: Vec<String> = groups.keys().cloned().collect();
        tool_names.sort_by(|a, b| {
            let a_pending = groups[a]
                .values()
                .any(|vals| vals.iter().any(|e| e.1 == PermStatus::Pending));
            let b_pending = groups[b]
                .values()
                .any(|vals| vals.iter().any(|e| e.1 == PermStatus::Pending));
            b_pending.cmp(&a_pending).then(a.cmp(b))
        });

        for tool_name in &tool_names {
            let tool_idx = self.arena.len();
            self.arena.push(TreeNode {
                kind: NodeKind::Tool {
                    name: tool_name.clone(),
                },
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
                    kind: NodeKind::Key {
                        key: key_name.clone(),
                    },
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

                for (value, status, prompt, request_id, preview_file_path) in values {
                    if self.filter_pending_only && status != PermStatus::Pending {
                        continue;
                    }
                    let val_idx = self.arena.len();
                    self.arena.push(TreeNode {
                        kind: NodeKind::Value {
                            value,
                            status,
                            prompt,
                            request_id,
                            preview_file_path,
                        },
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

    fn collect_visible(&mut self, idx: usize) {
        self.visible.push(idx);
        if !self.arena[idx].collapsed {
            let children = self.arena[idx].children.clone();
            for child in children {
                self.collect_visible(child);
            }
        }
    }

    fn toggle_filter(&mut self) {
        self.filter_pending_only = !self.filter_pending_only;
    }

    /// Open the approval or management popup for the selected leaf node.
    /// Returns `true` if the user made a decision (approve/deny) on a pending item,
    /// `false` if cancelled, managing an existing permission, or toggling collapse.
    fn open_popup(&mut self) -> bool {
        let Some(&idx) = self.visible.get(self.selected) else {
            return false;
        };
        let node = &self.arena[idx];

        match &node.kind {
            NodeKind::Value {
                value,
                status,
                prompt,
                request_id,
                preview_file_path,
                ..
            } => {
                let (tool_name, key_name) = self.find_tool_key_for(idx);

                match status {
                    PermStatus::Pending => launch_approval_popup(
                        &self.session_id,
                        &tool_name,
                        &key_name,
                        value,
                        prompt.as_deref(),
                        request_id.as_deref(),
                        preview_file_path.as_deref(),
                    ),
                    PermStatus::Session | PermStatus::Project => {
                        // Manage (revoke) existing permission — use the old
                        // inline popup path since launch_approval_popup is
                        // for pending items only.
                        let exe = match std::env::current_exe() {
                            Ok(p) => p.to_string_lossy().to_string(),
                            Err(_) => return false,
                        };
                        let cmd = format!(
                            "{} --session={} approve --manage --tool {} --key {} --value {}",
                            exe, self.session_id, tool_name, key_name, value
                        );
                        let popup_cmd = format!(
                            "tmux display-popup -E -w 60 -h 20 \"{}\"",
                            cmd.replace('"', "\\\"")
                        );
                        match Command::new("sh").args(["-c", &popup_cmd]).output() {
                            Ok(out) => out.status.code() == Some(0),
                            Err(e) => {
                                tracing::warn!("failed to launch manage popup: {}", e);
                                false
                            }
                        }
                    }
                }
            }
            NodeKind::Tool { .. } | NodeKind::Key { .. } => {
                // Toggle collapse for non-leaf
                self.toggle_collapse();
                false
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
                if let NodeKind::Key { key } = &node.kind
                    && node.children.contains(&value_idx)
                {
                    key_name = key.clone();
                    // Find tool parent of this key node
                    let key_idx = self
                        .arena
                        .iter()
                        .position(|n| std::ptr::eq(n, node))
                        .unwrap_or(0);
                    for tool_node in &self.arena {
                        if let NodeKind::Tool { name } = &tool_node.kind
                            && tool_node.children.contains(&key_idx)
                        {
                            tool_name = name.clone();
                            break;
                        }
                    }
                    break;
                }
            }
        }

        (tool_name, key_name)
    }
}

impl TreeNav for PermissionTreeState {
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
        let top_level: Vec<usize> = self
            .arena
            .iter()
            .enumerate()
            .filter(|(_, n)| n.depth == 0)
            .map(|(i, _)| i)
            .collect();
        for idx in top_level {
            self.collect_visible(idx);
        }
        self.clamp_selection();
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
        let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);

        // Flash: toggle every 3 frames (~600ms cycle at 200ms poll)
        let flash_on = (state.frame_count / 3).is_multiple_of(2);

        let any_pending = state.has_pending();

        // Build list items
        let items: Vec<ListItem> = state
            .visible
            .iter()
            .map(|&idx| {
                let node = &state.arena[idx];
                let indent = "  ".repeat(node.depth);
                match &node.kind {
                    NodeKind::Tool { name } => {
                        let prefix = if node.collapsed { "+" } else { "-" };
                        let has_pending = node.children.iter().any(|&ki| {
                            state.arena[ki].children.iter().any(|&vi| {
                                matches!(
                                    &state.arena[vi].kind,
                                    NodeKind::Value {
                                        status: PermStatus::Pending,
                                        ..
                                    }
                                )
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
                        ListItem::new(Line::from(Span::raw(format!(
                            "{}{} {}",
                            indent, prefix, key
                        ))))
                    }
                    NodeKind::Value { value, status, .. } => {
                        let (icon_style, text_style, label_style) =
                            if matches!(status, PermStatus::Pending) {
                                let s = if flash_on {
                                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(Color::DarkGray)
                                };
                                (s, s, s)
                            } else {
                                (
                                    Style::default().fg(status.color()),
                                    Style::default(),
                                    Style::default().fg(Color::DarkGray),
                                )
                            };
                        let line = Line::from(vec![
                            Span::styled(format!("{}  ", indent), text_style),
                            Span::styled(format!("[{}]", status.icon()), icon_style),
                            Span::styled(format!(" {}", value), text_style),
                            Span::styled(format!(" ({})", status.label()), label_style),
                        ]);
                        ListItem::new(line)
                    }
                }
            })
            .collect();

        let filter_indicator = if state.filter_pending_only {
            " [pending only]"
        } else {
            ""
        };
        let title_text = format!(" Permissions{} ", filter_indicator);

        let (border_style, title_style) = if any_pending && flash_on {
            (
                Style::default().fg(Color::Red),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )
        } else {
            (Style::default(), Style::default())
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(Span::styled(title_text, title_style)),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_stateful_widget(list, chunks[0], list_state);

        // Status bar
        let status_text = state
            .status_message
            .as_deref()
            .unwrap_or("j/k:nav  o:open  f:filter  q:quit");
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

pub fn query_permission_state_sync(socket_path: &PathBuf) -> Option<PermissionState> {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream as StdUnixStream;

    let mut stream = StdUnixStream::connect(socket_path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok()?;

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
// Standalone approval helpers (used by both permission_ui and approve-next CLI)
// ---------------------------------------------------------------------------

/// Launch a tmux popup for a single pending approval.
/// Returns `true` if the user made a decision (approve/deny), `false` if cancelled.
pub fn launch_approval_popup(
    session_id: &str,
    tool: &str,
    key: &str,
    value: &str,
    prompt: Option<&str>,
    request_id: Option<&str>,
    preview_file_path: Option<&str>,
) -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return false,
    };

    let mut cmd = format!(
        "{} --session={} approve --tool {} --key {} --value {}",
        exe, session_id, tool, key, value
    );
    if let Some(p) = prompt
        && !p.is_empty()
    {
        let escaped = shell_escape(p);
        cmd.push_str(&format!(" --prompt '{}'", escaped));
    }
    if let Some(rid) = request_id {
        cmd.push_str(&format!(" --request-id {}", rid));
    }
    if let Some(pfp) = preview_file_path {
        let escaped = shell_escape(pfp);
        cmd.push_str(&format!(" --preview-file-path '{}'", escaped));
    }

    // Query tmux window size for dynamic popup sizing
    let (max_w, max_h) = Command::new("tmux")
        .args(["display-message", "-p", "#{window_width} #{window_height}"])
        .output()
        .ok()
        .and_then(|out| {
            let s = String::from_utf8_lossy(&out.stdout);
            let mut parts = s.split_whitespace();
            let w: usize = parts.next()?.parse().ok()?;
            let h: usize = parts.next()?.parse().ok()?;
            Some((w * 80 / 100, h * 80 / 100))
        })
        .unwrap_or((60, 18));

    let prompt_len = prompt.map(|p| p.len()).unwrap_or(0);
    let has_preview = preview_file_path.is_some();
    let preview_extra: usize = if has_preview { 1 } else { 0 };
    let (popup_width, popup_height) = if prompt_len > 0 {
        let chars_per_line = 25.0_f64;
        let w = (prompt_len as f64 * chars_per_line).sqrt().ceil() as usize + 6;
        let w = w.clamp(60, max_w);
        let usable = w.saturating_sub(6);
        let prompt_lines = if usable > 0 {
            prompt_len.div_ceil(usable)
        } else {
            1
        };
        let h = (18 + preview_extra + prompt_lines).clamp(20, max_h);
        (w, h)
    } else {
        (60, (20 + preview_extra).min(max_h))
    };

    let mut decided = false;
    loop {
        let popup_cmd = format!(
            "tmux display-popup -E -w {} -h {} \"{}\"",
            popup_width,
            popup_height,
            cmd.replace('"', "\\\"")
        );
        let exit_code = match Command::new("sh").args(["-c", &popup_cmd]).output() {
            Ok(out) => out.status.code().unwrap_or(0),
            Err(e) => {
                tracing::warn!("failed to launch approval popup: {}", e);
                break;
            }
        };
        match exit_code {
            10 => {
                if let Some(pfp) = preview_file_path {
                    let escaped = shell_escape(pfp);
                    let nvim_popup = format!(
                        "tmux display-popup -E -w '80%' -h '80%' \"nvim -R '{}'\"",
                        escaped,
                    );
                    if let Err(e) = Command::new("sh").args(["-c", &nvim_popup]).output() {
                        tracing::warn!("failed to launch nvim popup: {}", e);
                    }
                }
            }
            0 => {
                decided = true;
                break;
            }
            _ => break,
        }
    }
    decided
}

/// Loop through all pending approvals, showing a popup for each.
/// Returns `None` if the user cancelled a popup, `Some(n)` for `n` approvals made
/// (0 means nothing was pending).
pub fn approve_all_pending(session_id: &str, socket_path: &PathBuf) -> Option<usize> {
    let mut count = 0;
    loop {
        let Some(state) = query_permission_state_sync(socket_path) else {
            break;
        };
        let Some(pending) = state.pending.first() else {
            break;
        };
        let decided = launch_approval_popup(
            session_id,
            &pending.tool,
            &pending.key,
            &pending.value,
            Some(&pending.prompt),
            Some(&pending.request_id),
            pending
                .preview_file_path
                .as_ref()
                .map(|p| p.to_string_lossy())
                .as_deref(),
        );
        if !decided {
            return None;
        }
        count += 1;
    }
    Some(count)
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
        if let Ok(event) = res
            && fs_tx.send(event).is_err()
        {
            tracing::debug!("permission ui watcher channel closed");
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
    state.refresh_from_server(&socket_path);
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
            state.refresh_from_server(&socket_path);
        }

        state.sync_list_state(&mut list_state);

        // Send terminal bell when new pending permissions appear (triggers tmux window alert)
        let any_pending = state.has_pending();
        if any_pending && !state.bell_sent {
            // BEL character: tmux monitor-bell (on by default) will highlight the window
            print!("\x07");
            if let Err(e) = io::Write::flush(&mut io::stdout()) {
                tracing::warn!("failed to flush bell character: {}", e);
            }
            state.bell_sent = true;
        } else if !any_pending {
            state.bell_sent = false;
        }

        // Render
        state.frame_count = state.frame_count.wrapping_add(1);
        render_tree(&mut terminal, &mut state, &mut list_state)?;

        // Handle keyboard input
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
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
                    let decided = state.open_popup();
                    state.refresh_from_server(&socket_path);
                    if decided {
                        approve_all_pending(&state.session_id, &socket_path);
                        state.refresh_from_server(&socket_path);
                    }
                }
                KeyCode::Char('f') => {
                    state.toggle_filter();
                    state.refresh_from_server(&socket_path);
                }
                KeyCode::Char('R') => {
                    state.refresh_from_server(&socket_path);
                }
                _ => {}
            }
        }
    }

    // Teardown
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
