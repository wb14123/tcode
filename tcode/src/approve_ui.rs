use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use llm_rs::permission::{PermissionDecision, PermissionKey, PermissionScope};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::protocol::{ClientMessage, ServerMessage};

/// Arguments for the approve popup.
pub struct ApproveArgs {
    pub socket_path: PathBuf,
    pub tool: String,
    pub key: String,
    pub value: Option<String>,
    pub manage: bool,
    /// Add-permission mode (interactive value input).
    pub add: bool,
    pub prompt: String,
    pub request_id: Option<String>,
    pub preview_file_path: Option<PathBuf>,
    /// When true, only "Allow once" and "Deny" are shown (no session/project).
    pub once_only: bool,
}

/// Result of the approval UI interaction.
pub enum ApproveResult {
    /// User made a decision (approve/deny) — normal exit.
    Done,
    /// User cancelled without making a decision (q/Esc).
    Cancelled,
    /// User wants to view the preview file in a maximized popup.
    ViewPopup,
}

fn make_key(args: &ApproveArgs) -> PermissionKey {
    PermissionKey {
        tool: args.tool.clone(),
        key: args.key.clone(),
        value: args.value.clone().unwrap_or_default(),
    }
}

/// Send a ClientMessage over the socket synchronously, return the ServerMessage.
fn send_msg_sync(socket_path: &PathBuf, msg: &ClientMessage) -> Result<ServerMessage> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    // LengthDelimitedCodec: 4-byte big-endian length prefix, then payload
    let json = serde_json::to_vec(msg)?;
    let len = (json.len() as u32).to_be_bytes();
    stream.write_all(&len)?;
    stream.write_all(&json)?;
    stream.flush()?;

    // Read response
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;
    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf)?;

    let server_msg: ServerMessage = serde_json::from_slice(&resp_buf)?;
    Ok(server_msg)
}

fn send_and_expect_ack(socket_path: &PathBuf, msg: &ClientMessage) -> Result<()> {
    match send_msg_sync(socket_path, msg)? {
        ServerMessage::Ack => Ok(()),
        ServerMessage::Error { message } => anyhow::bail!("Server error: {}", message),
        _ => anyhow::bail!("Unexpected server response"),
    }
}

fn send_resolve(
    socket_path: &PathBuf,
    key: PermissionKey,
    decision: PermissionDecision,
    request_id: Option<String>,
) -> Result<()> {
    let msg = ClientMessage::ResolvePermission {
        key,
        decision,
        request_id,
    };
    send_and_expect_ack(socket_path, &msg)
}

fn send_revoke(socket_path: &PathBuf, key: PermissionKey) -> Result<()> {
    let msg = ClientMessage::RevokePermission { key };
    send_and_expect_ack(socket_path, &msg)
}

pub fn run_approve(args: ApproveArgs) -> Result<ApproveResult> {
    if args.add {
        return run_add_permission_loop(&args);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = if args.manage {
        run_manage_loop(&mut terminal, &args)
    } else {
        run_approve_loop(&mut terminal, &args)
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

fn render_title<'a>(text: &'a str, color: Color) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![Span::styled(
        text,
        Style::default().fg(color),
    )]))
    .alignment(Alignment::Center)
    .block(Block::default().borders(Borders::BOTTOM))
}

fn render_details<'a>(args: &'a ApproveArgs) -> Paragraph<'a> {
    let value_str = args.value.as_deref().unwrap_or("");
    Paragraph::new(vec![
        Line::from(vec![
            Span::raw("  Tool: "),
            Span::styled(&args.tool, Style::default().fg(Color::Cyan)),
            Span::raw("  Key: "),
            Span::styled(&args.key, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Value: "),
            Span::styled(value_str, Style::default().fg(Color::White)),
        ]),
    ])
}

/// Break text into lines of at most `width` chars, with 2-char padding on each side.
/// Returns the broken lines as styled `Line`s.
fn break_prompt_into_lines<'a>(text: &'a str, width: u16) -> Vec<Line<'a>> {
    let usable = (width as usize).saturating_sub(4); // 2 left indent + 2 right padding
    if usable == 0 || text.is_empty() {
        return vec![Line::from(vec![
            Span::raw("  "),
            Span::styled(text, Style::default().fg(Color::White)),
        ])];
    }

    let mut lines = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let end = remaining
            .char_indices()
            .nth(usable)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());
        let (chunk, rest) = remaining.split_at(end);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(chunk, Style::default().fg(Color::White)),
        ]));
        remaining = rest;
    }
    lines
}

/// Which phase of the approve popup we're in.
enum ApprovePhase {
    Menu,
    DenyReason {
        input: String,
        error: Option<String>,
    },
}

fn run_approve_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    args: &ApproveArgs,
) -> Result<ApproveResult> {
    let has_prompt = !args.prompt.is_empty();
    let has_preview = args.preview_file_path.is_some();
    let once_only = args.once_only;
    let mut scroll_offset: u16 = 0;
    let mut phase = ApprovePhase::Menu;

    loop {
        // Compute broken lines and clamp scroll_offset before draw (only used in Menu phase)
        let term_size = terminal.size()?;
        let broken_lines = if has_prompt {
            break_prompt_into_lines(&args.prompt, term_size.width)
        } else {
            Vec::new()
        };

        let phase_ref = &phase;
        terminal.draw(|frame| {
            let area = frame.area();

            match phase_ref {
                ApprovePhase::Menu => {
                    if has_prompt {
                        let total_lines = broken_lines.len() as u16;
                        let preview_row: u16 = if has_preview { 1 } else { 0 };
                        let preview_extra: u16 = preview_row;
                        // once_only: title(3)+blank(1)+allow(2)+preview(0|1)+blank(1)+deny(2) = 9+preview_extra
                        // !once_only: adds sep(2)+blank(1)+sess(3)+blank(1) = +7 → 16+preview_extra
                        let fixed_rows: u16 = if once_only { 9 } else { 16 } + preview_extra;
                        let prompt_space_no_hints = area.height.saturating_sub(fixed_rows);
                        let needs_scroll = total_lines > prompt_space_no_hints;
                        // Only reserve hint rows when scrolling is needed
                        let (hint_up, hint_down) = if needs_scroll { (1u16, 1u16) } else { (0, 0) };

                        // Build constraint list dynamically: common prefix, optional session/project
                        // group, then deny/cancel as the final entry.
                        let mut constraints = vec![
                            Constraint::Length(3),           // [0] Title
                            Constraint::Length(hint_up), // [1] Scroll-up hint (0 when not needed)
                            Constraint::Min(1),          // [2] Prompt content
                            Constraint::Length(hint_down), // [3] Scroll-down hint (0 when not needed)
                            Constraint::Length(1),         // [4] Blank
                            Constraint::Length(2),         // [5] Allow once
                            Constraint::Length(preview_row), // [6] View in nvim (0 when no preview)
                            Constraint::Length(1),         // [7] Blank
                        ];
                        if !once_only {
                            constraints.push(Constraint::Length(2)); // [8] Separator + key:value
                            constraints.push(Constraint::Length(1)); // [9] Blank
                            constraints.push(Constraint::Length(3)); // [10] Session/Project options
                            constraints.push(Constraint::Length(1)); // [11] Blank
                        }
                        constraints.push(Constraint::Length(2)); // [last] Deny/Cancel
                        let chunks = Layout::vertical(constraints).split(area);

                        frame.render_widget(
                            render_title("Permission Request", Color::Yellow),
                            chunks[0],
                        );

                        let prompt_content_height = chunks[2].height;
                        let scrollable = total_lines.saturating_sub(prompt_content_height);
                        // Clamp scroll offset
                        if scroll_offset > scrollable {
                            scroll_offset = scrollable;
                        }

                        // Render scroll-up hint (area is 0-height when not needed)
                        if scroll_offset > 0 {
                            let hint = Paragraph::new(Line::from(Span::styled(
                                "  \u{25b2} scroll up (k/\u{2191})",
                                Style::default().fg(Color::DarkGray),
                            )));
                            frame.render_widget(hint, chunks[1]);
                        }

                        // Render prompt content with scroll
                        let prompt_text =
                            Paragraph::new(broken_lines.clone()).scroll((scroll_offset, 0));
                        frame.render_widget(prompt_text, chunks[2]);

                        // Render scroll-down hint (area is 0-height when not needed)
                        if scroll_offset < scrollable {
                            let hint = Paragraph::new(Line::from(Span::styled(
                                "  \u{25bc} scroll down (j/\u{2193})",
                                Style::default().fg(Color::DarkGray),
                            )));
                            frame.render_widget(hint, chunks[3]);
                        }

                        let allow_once = Paragraph::new(vec![Line::from(Span::styled(
                            "  [1] Allow once",
                            Style::default().fg(Color::Green),
                        ))]);
                        frame.render_widget(allow_once, chunks[5]);

                        // Render [v] View in nvim (area is 0-height when no preview)
                        if has_preview {
                            let view_nvim = Paragraph::new(vec![Line::from(Span::styled(
                                "  [v] View in nvim",
                                Style::default().fg(Color::Magenta),
                            ))]);
                            frame.render_widget(view_nvim, chunks[6]);
                        }

                        // Render session/project group only when not once_only
                        if !once_only {
                            let separator = Paragraph::new(vec![
                                Line::from(Span::styled(
                                    "  -- Or allow all matching requests --",
                                    Style::default().fg(Color::DarkGray),
                                )),
                                Line::from(vec![
                                    Span::raw("  "),
                                    Span::styled(&args.key, Style::default().fg(Color::DarkGray)),
                                    Span::styled(": ", Style::default().fg(Color::DarkGray)),
                                    Span::styled(
                                        args.value.as_deref().unwrap_or(""),
                                        Style::default().fg(Color::Cyan),
                                    ),
                                ]),
                            ]);
                            frame.render_widget(separator, chunks[8]);

                            let session_project = Paragraph::new(vec![
                                Line::from(Span::styled(
                                    "  [2] Allow for session",
                                    Style::default().fg(Color::Cyan),
                                )),
                                Line::from(Span::styled(
                                    "  [3] Allow for project",
                                    Style::default().fg(Color::Blue),
                                )),
                            ]);
                            frame.render_widget(session_project, chunks[10]);
                        }

                        let deny_cancel = Paragraph::new(vec![
                            Line::from(Span::styled(
                                "  [4] Deny (with optional reason)",
                                Style::default().fg(Color::Red),
                            )),
                            Line::from(Span::styled(
                                "  [q] Cancel",
                                Style::default().fg(Color::DarkGray),
                            )),
                        ]);
                        let last = chunks.len() - 1;
                        frame.render_widget(deny_cancel, chunks[last]);
                    } else {
                        // No prompt: build option lines dynamically based on once_only
                        let mut option_lines = vec![Line::from(Span::styled(
                            "  [1] Allow once",
                            Style::default().fg(Color::Green),
                        ))];
                        if !once_only {
                            option_lines.push(Line::from(Span::styled(
                                "  [2] Allow for session",
                                Style::default().fg(Color::Cyan),
                            )));
                            option_lines.push(Line::from(Span::styled(
                                "  [3] Allow for project",
                                Style::default().fg(Color::Blue),
                            )));
                        }
                        option_lines.push(Line::from(Span::styled(
                            "  [4] Deny (with optional reason)",
                            Style::default().fg(Color::Red),
                        )));
                        option_lines.push(Line::from(Span::styled(
                            "  [q] Cancel",
                            Style::default().fg(Color::DarkGray),
                        )));
                        let options_height = option_lines.len() as u16;

                        let chunks = Layout::vertical([
                            Constraint::Length(3),              // Title
                            Constraint::Length(2),              // Details
                            Constraint::Length(1),              // Blank
                            Constraint::Length(options_height), // Options
                            Constraint::Min(0),                 // Spacer
                        ])
                        .split(area);

                        frame.render_widget(
                            render_title("Permission Request", Color::Yellow),
                            chunks[0],
                        );
                        frame.render_widget(render_details(args), chunks[1]);

                        let options = Paragraph::new(option_lines);
                        frame.render_widget(options, chunks[3]);
                    }
                }
                ApprovePhase::DenyReason { input, error } => {
                    // Text-input modal for the optional deny reason. Mirrors the
                    // AddPhase::Input layout: title, tool/key info, input line
                    // with block cursor, error slot, instructions.
                    let chunks = Layout::vertical([
                        Constraint::Length(3), // Title
                        Constraint::Length(2), // Tool/Key info
                        Constraint::Length(1), // Blank
                        Constraint::Length(1), // Prompt/help line
                        Constraint::Length(1), // Input
                        Constraint::Length(1), // Inline error (blank when None)
                        Constraint::Length(1), // Blank
                        Constraint::Length(1), // Instructions
                        Constraint::Min(0),    // Spacer
                    ])
                    .split(area);

                    frame.render_widget(render_title("Deny Reason", Color::Red), chunks[0]);

                    let details = Paragraph::new(vec![Line::from(vec![
                        Span::raw("  Tool: "),
                        Span::styled(&args.tool, Style::default().fg(Color::Cyan)),
                        Span::raw("  Key: "),
                        Span::styled(&args.key, Style::default().fg(Color::Cyan)),
                    ])]);
                    frame.render_widget(details, chunks[1]);

                    let prompt_line = Paragraph::new(Line::from(Span::styled(
                        "  Reason (optional):",
                        Style::default().fg(Color::DarkGray),
                    )));
                    frame.render_widget(prompt_line, chunks[3]);

                    let input_widget = Paragraph::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(input.as_str(), Style::default().fg(Color::White)),
                        Span::styled("\u{2588}", Style::default().fg(Color::White)),
                    ]));
                    frame.render_widget(input_widget, chunks[4]);

                    if let Some(err) = error {
                        let err_line = Paragraph::new(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("\u{26a0} {err}"),
                                Style::default().fg(Color::Red),
                            ),
                        ]));
                        frame.render_widget(err_line, chunks[5]);
                    }

                    let instructions = Paragraph::new(vec![Line::from(Span::styled(
                        "  [Enter] Deny  [Esc] Back to menu  [Ctrl-C] Cancel",
                        Style::default().fg(Color::DarkGray),
                    ))]);
                    frame.render_widget(instructions, chunks[7]);
                }
            }
        })?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Compute the next phase to transition into (if any). We can't
            // reassign `phase` while it's borrowed by the match, so we return
            // an Option<ApprovePhase> and apply it after the match ends.
            let next_phase: Option<ApprovePhase> = match &mut phase {
                ApprovePhase::Menu => {
                    if has_prompt {
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                scroll_offset = scroll_offset.saturating_sub(1);
                                continue;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                scroll_offset = scroll_offset.saturating_add(1);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    match key.code {
                        KeyCode::Char('v') if has_preview => {
                            return Ok(ApproveResult::ViewPopup);
                        }
                        KeyCode::Char('4') => Some(ApprovePhase::DenyReason {
                            input: String::new(),
                            error: None,
                        }),
                        KeyCode::Char('q') | KeyCode::Esc => {
                            return Ok(ApproveResult::Cancelled);
                        }
                        code => {
                            let decision = match code {
                                KeyCode::Char('1') => Some(PermissionDecision::AllowOnce),
                                KeyCode::Char('2') if !once_only => {
                                    Some(PermissionDecision::AllowSession)
                                }
                                KeyCode::Char('3') if !once_only => {
                                    Some(PermissionDecision::AllowProject)
                                }
                                _ => None,
                            };
                            if let Some(decision) = decision {
                                let pk = make_key(args);
                                // AllowOnce targets specific invocation; others apply to all
                                let rid = if matches!(decision, PermissionDecision::AllowOnce) {
                                    args.request_id.clone()
                                } else {
                                    None
                                };
                                send_resolve(&args.socket_path, pk, decision, rid)?;
                                return Ok(ApproveResult::Done);
                            }
                            None
                        }
                    }
                }
                ApprovePhase::DenyReason { input, error } => match key.code {
                    // Ctrl-C cancels the whole popup (request stays pending).
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(ApproveResult::Cancelled);
                    }
                    // Esc goes back to the Menu phase (does NOT cancel).
                    KeyCode::Esc => Some(ApprovePhase::Menu),
                    // Enter resolves with Deny { reason }. Empty / whitespace-only
                    // input → reason: None. Leading / trailing whitespace is trimmed.
                    KeyCode::Enter => {
                        let trimmed = input.trim();
                        let reason = if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        };
                        let decision = PermissionDecision::Deny { reason };
                        let pk = make_key(args);
                        // Deny applies to all waiters (same as the previous [4] behavior).
                        send_resolve(&args.socket_path, pk, decision, None)?;
                        return Ok(ApproveResult::Done);
                    }
                    KeyCode::Backspace => {
                        if input.is_empty() {
                            // Empty + backspace → go back to Menu (mirrors AddPhase::Input).
                            Some(ApprovePhase::Menu)
                        } else {
                            input.pop();
                            *error = None;
                            None
                        }
                    }
                    KeyCode::Char(c) => {
                        if input.chars().count() >= 500 {
                            *error = Some("Reason too long (500 char max)".to_string());
                        } else {
                            input.push(c);
                            *error = None;
                        }
                        None
                    }
                    _ => None,
                },
            };

            if let Some(new_phase) = next_phase {
                phase = new_phase;
            }
        }
    }
}

fn run_manage_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    args: &ApproveArgs,
) -> Result<ApproveResult> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::vertical([
                Constraint::Length(3), // Title
                Constraint::Length(2), // Details
                Constraint::Length(1), // Blank
                Constraint::Length(2), // Options
                Constraint::Min(0),    // Spacer
            ])
            .split(area);

            frame.render_widget(render_title("Manage Permission", Color::Cyan), chunks[0]);
            frame.render_widget(render_details(args), chunks[1]);

            let options = Paragraph::new(vec![
                Line::from(Span::styled(
                    "  [r] Revoke",
                    Style::default().fg(Color::Red),
                )),
                Line::from(Span::styled(
                    "  [q] Cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ]);
            frame.render_widget(options, chunks[3]);
        })?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('r') => {
                    let pk = make_key(args);
                    send_revoke(&args.socket_path, pk)?;
                    return Ok(ApproveResult::Done);
                }
                KeyCode::Char('q') | KeyCode::Esc => return Ok(ApproveResult::Cancelled),
                _ => {}
            }
        }
    }
}

/// Two-phase TUI for adding a permission: Phase 1 = text input for value,
/// Phase 2 = scope selection (Session / Project).
fn run_add_permission_loop(args: &ApproveArgs) -> Result<ApproveResult> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_add_permission_inner(&mut terminal, args);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

/// Which phase of the add-permission popup we're in.
enum AddPhase {
    /// Initial menu: choose between "enter specific value" and "allow all (*)".
    Menu,
    /// Text-input phase for a specific value.
    Input,
    /// Scope-selection phase (Session / Project).
    SelectScope,
}

fn run_add_permission_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    args: &ApproveArgs,
) -> Result<ApproveResult> {
    let mut input = String::new();
    let mut phase = AddPhase::Menu;
    // `came_via_wildcard` tracks whether SelectScope was reached via the
    // Menu's `[2] Allow all values (*)` path. Lifetime: one loop iteration
    // through the state machine — reset to false when entering Input via
    // `[1]`, set to true when entering SelectScope via `[2]`, and cleared
    // when SelectScope's Backspace returns to Menu. SelectScope confirm
    // (keys `2`/`3`) returns Done immediately and exits the loop, so stale
    // values can never leak across iterations.
    let mut came_via_wildcard = false;
    let mut input_error: Option<String> = None;

    loop {
        let phase_ref = &phase;
        let input_ref = &input;
        let input_error_ref = &input_error;

        terminal.draw(|frame| {
            let area = frame.area();

            match phase_ref {
                AddPhase::Menu => {
                    let chunks = Layout::vertical([
                        Constraint::Length(3), // Title
                        Constraint::Length(2), // Tool/Key info
                        Constraint::Length(1), // Blank
                        Constraint::Length(3), // Options (2 lines + 1 padding)
                        Constraint::Length(1), // Blank
                        Constraint::Length(1), // Instructions
                        Constraint::Min(0),    // Spacer
                    ])
                    .split(area);

                    frame.render_widget(render_title("Add Permission", Color::Green), chunks[0]);

                    let details = Paragraph::new(vec![Line::from(vec![
                        Span::raw("  Tool: "),
                        Span::styled(&args.tool, Style::default().fg(Color::Cyan)),
                        Span::raw("  Key: "),
                        Span::styled(&args.key, Style::default().fg(Color::Cyan)),
                    ])]);
                    frame.render_widget(details, chunks[1]);

                    let options = Paragraph::new(vec![
                        Line::from(Span::styled(
                            "  [1] Enter a specific value",
                            Style::default().fg(Color::Cyan),
                        )),
                        Line::from(Span::styled(
                            "  [2] Allow all values (*)",
                            Style::default().fg(Color::Yellow),
                        )),
                    ]);
                    frame.render_widget(options, chunks[3]);

                    let instructions = Paragraph::new(vec![Line::from(Span::styled(
                        "  [Esc] Cancel",
                        Style::default().fg(Color::DarkGray),
                    ))]);
                    frame.render_widget(instructions, chunks[5]);
                }
                AddPhase::Input => {
                    let chunks = Layout::vertical([
                        Constraint::Length(3), // Title
                        Constraint::Length(2), // Tool/Key info
                        Constraint::Length(1), // Blank
                        Constraint::Length(1), // Value input
                        Constraint::Length(1), // Inline error (blank when None)
                        Constraint::Length(1), // Blank
                        Constraint::Length(2), // Instructions
                        Constraint::Min(0),    // Spacer
                    ])
                    .split(area);

                    frame.render_widget(render_title("Add Permission", Color::Green), chunks[0]);

                    let details = Paragraph::new(vec![Line::from(vec![
                        Span::raw("  Tool: "),
                        Span::styled(&args.tool, Style::default().fg(Color::Cyan)),
                        Span::raw("  Key: "),
                        Span::styled(&args.key, Style::default().fg(Color::Cyan)),
                    ])]);
                    frame.render_widget(details, chunks[1]);

                    let cursor_line = format!("  Value: {input_ref}█");
                    let input_widget = Paragraph::new(Line::from(Span::styled(
                        cursor_line,
                        Style::default().fg(Color::White),
                    )));
                    frame.render_widget(input_widget, chunks[3]);

                    if let Some(err) = input_error_ref {
                        let err_line = Paragraph::new(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("\u{26a0} {err}"),
                                Style::default().fg(Color::Red),
                            ),
                        ]));
                        frame.render_widget(err_line, chunks[4]);
                    }

                    let instructions = Paragraph::new(vec![Line::from(Span::styled(
                        "  [Enter] Confirm  [Backspace] Back to menu  [Esc] Cancel",
                        Style::default().fg(Color::DarkGray),
                    ))]);
                    frame.render_widget(instructions, chunks[6]);
                }
                AddPhase::SelectScope => {
                    let chunks = Layout::vertical([
                        Constraint::Length(3), // Title
                        Constraint::Length(2), // Tool/Key info
                        Constraint::Length(1), // Value display
                        Constraint::Length(1), // Blank
                        Constraint::Length(4), // Options
                        Constraint::Min(0),    // Spacer
                    ])
                    .split(area);

                    frame.render_widget(render_title("Add Permission", Color::Green), chunks[0]);

                    let details = Paragraph::new(vec![Line::from(vec![
                        Span::raw("  Tool: "),
                        Span::styled(&args.tool, Style::default().fg(Color::Cyan)),
                        Span::raw("  Key: "),
                        Span::styled(&args.key, Style::default().fg(Color::Cyan)),
                    ])]);
                    frame.render_widget(details, chunks[1]);

                    let value_display = if input_ref == "*" {
                        Paragraph::new(Line::from(vec![
                            Span::raw("  Value: "),
                            Span::styled("*", Style::default().fg(Color::Yellow)),
                            Span::styled(" (allow all)", Style::default().fg(Color::DarkGray)),
                        ]))
                    } else {
                        Paragraph::new(Line::from(vec![
                            Span::raw("  Value: "),
                            Span::styled(input_ref.as_str(), Style::default().fg(Color::White)),
                        ]))
                    };
                    frame.render_widget(value_display, chunks[2]);

                    let options = Paragraph::new(vec![
                        Line::from(Span::styled(
                            "  [2] Allow for session",
                            Style::default().fg(Color::Cyan),
                        )),
                        Line::from(Span::styled(
                            "  [3] Allow for project",
                            Style::default().fg(Color::Blue),
                        )),
                        Line::from(Span::styled(
                            "  [Backspace] Edit value",
                            Style::default().fg(Color::Yellow),
                        )),
                        Line::from(Span::styled(
                            "  [q] Cancel",
                            Style::default().fg(Color::DarkGray),
                        )),
                    ]);
                    frame.render_widget(options, chunks[4]);
                }
            }
        })?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match phase {
            AddPhase::Menu => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(ApproveResult::Cancelled);
                }
                KeyCode::Esc => {
                    return Ok(ApproveResult::Cancelled);
                }
                KeyCode::Char('1') => {
                    phase = AddPhase::Input;
                    came_via_wildcard = false;
                }
                KeyCode::Char('2') => {
                    input = "*".to_string();
                    came_via_wildcard = true;
                    phase = AddPhase::SelectScope;
                }
                _ => {}
            },
            AddPhase::Input => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(ApproveResult::Cancelled);
                }
                KeyCode::Esc => {
                    return Ok(ApproveResult::Cancelled);
                }
                KeyCode::Enter => {
                    if input == "*" {
                        input_error = Some(
                            "* is reserved \u{2014} use [2] Allow all values from the menu instead"
                                .to_string(),
                        );
                    } else if !input.is_empty() {
                        input_error = None;
                        phase = AddPhase::SelectScope;
                    } else {
                        // Input is empty: clear any stale error so it doesn't
                        // linger. In practice this branch is unreachable because
                        // Backspace already clears the error when input becomes
                        // empty, but being explicit keeps the intent obvious.
                        input_error = None;
                    }
                }
                KeyCode::Backspace => {
                    if input.is_empty() {
                        input_error = None;
                        phase = AddPhase::Menu;
                    } else {
                        input.pop();
                        input_error = None;
                    }
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    input_error = None;
                }
                _ => {}
            },
            AddPhase::SelectScope => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(ApproveResult::Cancelled);
                }
                KeyCode::Char('2') => {
                    send_add_permission(args, &input, PermissionScope::Session)?;
                    return Ok(ApproveResult::Done);
                }
                KeyCode::Char('3') => {
                    send_add_permission(args, &input, PermissionScope::Project)?;
                    return Ok(ApproveResult::Done);
                }
                KeyCode::Backspace => {
                    if came_via_wildcard {
                        input.clear();
                        came_via_wildcard = false;
                        phase = AddPhase::Menu;
                    } else {
                        phase = AddPhase::Input;
                    }
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    return Ok(ApproveResult::Cancelled);
                }
                _ => {}
            },
        }
    }
}

fn send_add_permission(args: &ApproveArgs, value: &str, scope: PermissionScope) -> Result<()> {
    let key = PermissionKey {
        tool: args.tool.clone(),
        key: args.key.clone(),
        value: value.to_string(),
    };
    let msg = ClientMessage::AddPermission { key, scope };
    send_and_expect_ack(&args.socket_path, &msg)
}
