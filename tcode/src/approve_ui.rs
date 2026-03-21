use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use llm_rs::permission::{PermissionDecision, PermissionKey};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
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
    pub value: String,
    pub manage: bool,
    pub prompt: String,
    pub request_id: Option<String>,
    pub preview_file_path: Option<PathBuf>,
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
        value: args.value.clone(),
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
    Paragraph::new(vec![
        Line::from(vec![
            Span::raw("  Tool: "),
            Span::styled(&args.tool, Style::default().fg(Color::Cyan)),
            Span::raw("  Key: "),
            Span::styled(&args.key, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Value: "),
            Span::styled(&args.value, Style::default().fg(Color::White)),
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

fn run_approve_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    args: &ApproveArgs,
) -> Result<ApproveResult> {
    let has_prompt = !args.prompt.is_empty();
    let has_preview = args.preview_file_path.is_some();
    let mut scroll_offset: u16 = 0;

    loop {
        // Compute broken lines and clamp scroll_offset before draw
        let term_size = terminal.size()?;
        let broken_lines = if has_prompt {
            break_prompt_into_lines(&args.prompt, term_size.width)
        } else {
            Vec::new()
        };

        terminal.draw(|frame| {
            let area = frame.area();

            if has_prompt {
                let total_lines = broken_lines.len() as u16;
                let preview_extra: u16 = if has_preview { 1 } else { 0 };
                // Fixed rows: title(3) + blank(1) + allow(2) + preview(0|1) + blank(1) + sep(2) + blank(1) + sess(3) + blank(1) + deny(2) = 16 or 17
                let fixed_rows: u16 = 16 + preview_extra;
                let prompt_space_no_hints = area.height.saturating_sub(fixed_rows);
                let needs_scroll = total_lines > prompt_space_no_hints;

                // Only reserve hint rows when scrolling is needed
                let (hint_up, hint_down) = if needs_scroll { (1u16, 1u16) } else { (0, 0) };

                let preview_row: u16 = if has_preview { 1 } else { 0 };
                let chunks = Layout::vertical([
                    Constraint::Length(3),           // [0] Title
                    Constraint::Length(hint_up),     // [1] Scroll-up hint (0 when not needed)
                    Constraint::Min(1),              // [2] Prompt content
                    Constraint::Length(hint_down),   // [3] Scroll-down hint (0 when not needed)
                    Constraint::Length(1),           // [4] Blank
                    Constraint::Length(2),           // [5] Allow once
                    Constraint::Length(preview_row), // [6] View in nvim (0 when no preview)
                    Constraint::Length(1),           // [7] Blank
                    Constraint::Length(2),           // [8] Separator + key:value
                    Constraint::Length(1),           // [9] Blank
                    Constraint::Length(3),           // [10] Session/Project options
                    Constraint::Length(1),           // [11] Blank
                    Constraint::Length(2),           // [12] Deny/Cancel
                ])
                .split(area);

                frame.render_widget(render_title("Permission Request", Color::Yellow), chunks[0]);

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
                let prompt_text = Paragraph::new(broken_lines.clone()).scroll((scroll_offset, 0));
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

                let separator = Paragraph::new(vec![
                    Line::from(Span::styled(
                        "  -- Or allow all matching requests --",
                        Style::default().fg(Color::DarkGray),
                    )),
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(&args.key, Style::default().fg(Color::DarkGray)),
                        Span::styled(": ", Style::default().fg(Color::DarkGray)),
                        Span::styled(&args.value, Style::default().fg(Color::Cyan)),
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

                let deny_cancel = Paragraph::new(vec![
                    Line::from(Span::styled("  [4] Deny", Style::default().fg(Color::Red))),
                    Line::from(Span::styled(
                        "  [q] Cancel",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]);
                frame.render_widget(deny_cancel, chunks[12]);
            } else {
                // Fallback layout without prompt (same as before but with key:value)
                let chunks = Layout::vertical([
                    Constraint::Length(3), // Title
                    Constraint::Length(2), // Details
                    Constraint::Length(1), // Blank
                    Constraint::Length(5), // Options
                    Constraint::Min(0),    // Spacer
                ])
                .split(area);

                frame.render_widget(render_title("Permission Request", Color::Yellow), chunks[0]);
                frame.render_widget(render_details(args), chunks[1]);

                let options = Paragraph::new(vec![
                    Line::from(Span::styled(
                        "  [1] Allow once",
                        Style::default().fg(Color::Green),
                    )),
                    Line::from(Span::styled(
                        "  [2] Allow for session",
                        Style::default().fg(Color::Cyan),
                    )),
                    Line::from(Span::styled(
                        "  [3] Allow for project",
                        Style::default().fg(Color::Blue),
                    )),
                    Line::from(Span::styled("  [4] Deny", Style::default().fg(Color::Red))),
                    Line::from(Span::styled(
                        "  [q] Cancel",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]);
                frame.render_widget(options, chunks[3]);
            }
        })?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
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
            let decision = match key.code {
                KeyCode::Char('v') if has_preview => return Ok(ApproveResult::ViewPopup),
                KeyCode::Char('1') => Some(PermissionDecision::AllowOnce),
                KeyCode::Char('2') => Some(PermissionDecision::AllowSession),
                KeyCode::Char('3') => Some(PermissionDecision::AllowProject),
                KeyCode::Char('4') => Some(PermissionDecision::Deny),
                KeyCode::Char('q') | KeyCode::Esc => return Ok(ApproveResult::Cancelled),
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
