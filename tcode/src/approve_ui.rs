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

fn send_resolve(socket_path: &PathBuf, key: PermissionKey, decision: PermissionDecision, request_id: Option<String>) -> Result<()> {
    let msg = ClientMessage::ResolvePermission { key, decision, request_id };
    match send_msg_sync(socket_path, &msg)? {
        ServerMessage::Ack => Ok(()),
        ServerMessage::Error { message } => anyhow::bail!("Server error: {}", message),
        _ => anyhow::bail!("Unexpected server response"),
    }
}

fn send_revoke(socket_path: &PathBuf, key: PermissionKey) -> Result<()> {
    let msg = ClientMessage::RevokePermission { key };
    match send_msg_sync(socket_path, &msg)? {
        ServerMessage::Ack => Ok(()),
        ServerMessage::Error { message } => anyhow::bail!("Server error: {}", message),
        _ => anyhow::bail!("Unexpected server response"),
    }
}

pub fn run_approve(args: ApproveArgs) -> Result<()> {
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

fn run_approve_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    args: &ApproveArgs,
) -> Result<()> {
    let has_prompt = !args.prompt.is_empty();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            if has_prompt {
                // Layout with prompt displayed prominently
                let chunks = Layout::vertical([
                    Constraint::Length(3),  // Title
                    Constraint::Length(2),  // Prompt
                    Constraint::Length(1),  // Blank
                    Constraint::Length(2),  // Allow once
                    Constraint::Length(1),  // Blank
                    Constraint::Length(2),  // Separator + key:value
                    Constraint::Length(1),  // Blank
                    Constraint::Length(3),  // Session/Project options
                    Constraint::Length(1),  // Blank
                    Constraint::Length(2),  // Deny/Cancel
                    Constraint::Min(0),    // Spacer
                ]).split(area);

                let title = Paragraph::new(Line::from(vec![
                    Span::styled("Permission Request", Style::default().fg(Color::Yellow)),
                ]))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::BOTTOM));
                frame.render_widget(title, chunks[0]);

                let prompt_text = Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(&args.prompt, Style::default().fg(Color::White)),
                    ]),
                ]);
                frame.render_widget(prompt_text, chunks[1]);

                let allow_once = Paragraph::new(vec![
                    Line::from(Span::styled("  [1] Allow once", Style::default().fg(Color::Green))),
                ]);
                frame.render_widget(allow_once, chunks[3]);

                let separator = Paragraph::new(vec![
                    Line::from(Span::styled("  -- Or allow all matching requests --", Style::default().fg(Color::DarkGray))),
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(&args.key, Style::default().fg(Color::DarkGray)),
                        Span::styled(": ", Style::default().fg(Color::DarkGray)),
                        Span::styled(&args.value, Style::default().fg(Color::Cyan)),
                    ]),
                ]);
                frame.render_widget(separator, chunks[5]);

                let session_project = Paragraph::new(vec![
                    Line::from(Span::styled("  [2] Allow for session", Style::default().fg(Color::Cyan))),
                    Line::from(Span::styled("  [3] Allow for project", Style::default().fg(Color::Blue))),
                ]);
                frame.render_widget(session_project, chunks[7]);

                let deny_cancel = Paragraph::new(vec![
                    Line::from(Span::styled("  [4] Deny", Style::default().fg(Color::Red))),
                    Line::from(Span::styled("  [q] Cancel", Style::default().fg(Color::DarkGray))),
                ]);
                frame.render_widget(deny_cancel, chunks[9]);
            } else {
                // Fallback layout without prompt (same as before but with key:value)
                let chunks = Layout::vertical([
                    Constraint::Length(3), // Title
                    Constraint::Length(2), // Details
                    Constraint::Length(1), // Blank
                    Constraint::Length(5), // Options
                    Constraint::Min(0),   // Spacer
                ]).split(area);

                let title = Paragraph::new(Line::from(vec![
                    Span::styled("Permission Request", Style::default().fg(Color::Yellow)),
                ]))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::BOTTOM));
                frame.render_widget(title, chunks[0]);

                let details = Paragraph::new(vec![
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
                ]);
                frame.render_widget(details, chunks[1]);

                let options = Paragraph::new(vec![
                    Line::from(Span::styled("  [1] Allow once", Style::default().fg(Color::Green))),
                    Line::from(Span::styled("  [2] Allow for session", Style::default().fg(Color::Cyan))),
                    Line::from(Span::styled("  [3] Allow for project", Style::default().fg(Color::Blue))),
                    Line::from(Span::styled("  [4] Deny", Style::default().fg(Color::Red))),
                    Line::from(Span::styled("  [q] Cancel", Style::default().fg(Color::DarkGray))),
                ]);
                frame.render_widget(options, chunks[3]);
            }
        })?;

        if event::poll(std::time::Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let decision = match key.code {
                    KeyCode::Char('1') => Some(PermissionDecision::AllowOnce),
                    KeyCode::Char('2') => Some(PermissionDecision::AllowSession),
                    KeyCode::Char('3') => Some(PermissionDecision::AllowProject),
                    KeyCode::Char('4') => Some(PermissionDecision::Deny),
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
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
                    return Ok(());
                }
            }
        }
    }
}

fn run_manage_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    args: &ApproveArgs,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::vertical([
                Constraint::Length(3), // Title
                Constraint::Length(2), // Details
                Constraint::Length(1), // Blank
                Constraint::Length(2), // Options
                Constraint::Min(0),   // Spacer
            ]).split(area);

            let title = Paragraph::new(Line::from(vec![
                Span::styled("Manage Permission", Style::default().fg(Color::Cyan)),
            ]))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::BOTTOM));
            frame.render_widget(title, chunks[0]);

            let details = Paragraph::new(vec![
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
            ]);
            frame.render_widget(details, chunks[1]);

            let options = Paragraph::new(vec![
                Line::from(Span::styled("  [r] Revoke", Style::default().fg(Color::Red))),
                Line::from(Span::styled("  [q] Cancel", Style::default().fg(Color::DarkGray))),
            ]);
            frame.render_widget(options, chunks[3]);
        })?;

        if event::poll(std::time::Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('r') => {
                        let pk = make_key(args);
                        send_revoke(&args.socket_path, pk)?;
                        return Ok(());
                    }
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}
