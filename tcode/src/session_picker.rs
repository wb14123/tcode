use std::io;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use llm_rs::conversation::SessionMeta;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::session::{self, Session};

struct SessionEntry {
    id: String,
    status: String,
    description: Option<String>,
}

/// Show an interactive session picker and return the selected session ID.
/// Returns `None` if the user cancels (Esc/q) or there are no sessions.
pub fn pick_session() -> Result<Option<String>> {
    let sessions = session::list_sessions()?;
    if sessions.is_empty() {
        println!("No sessions found in ~/.tcode/sessions/");
        return Ok(None);
    }

    // Collect session info with metadata for sorting
    let mut entries: Vec<(SessionEntry, u64)> = sessions
        .into_iter()
        .filter_map(|id| {
            let session = Session::new(id.clone()).ok()?;
            let status = if std::os::unix::net::UnixStream::connect(session.socket_path()).is_ok() {
                "active"
            } else {
                "inactive"
            };
            let meta = std::fs::read_to_string(session.session_meta_file())
                .ok()
                .and_then(|json| serde_json::from_str::<SessionMeta>(&json).ok());
            let last_active = meta.as_ref().and_then(|m| m.last_active_at).unwrap_or(0);
            let description = meta.and_then(|m| m.description);
            Some((
                SessionEntry {
                    id,
                    status: status.to_string(),
                    description,
                },
                last_active,
            ))
        })
        .collect();

    // Sort by last_active_at descending (most recent first)
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    let entries: Vec<SessionEntry> = entries.into_iter().map(|(e, _)| e).collect();

    run_picker(&entries)
}

fn run_picker(entries: &[SessionEntry]) -> Result<Option<String>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut list_state = ListState::default();
    list_state.select(Some(0));

    let result = loop {
        terminal.draw(|f| {
            let chunks =
                Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());

            let items: Vec<ListItem> = entries
                .iter()
                .map(|e| {
                    let status_style = if e.status == "active" {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    let mut spans = vec![
                        Span::raw(&e.id),
                        Span::raw(" "),
                        Span::styled(format!("({})", e.status), status_style),
                    ];
                    if let Some(ref desc) = e.description {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            desc.as_str(),
                            Style::default().fg(Color::Cyan),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Select a session "),
                )
                .highlight_style(
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::REVERSED),
                )
                .highlight_symbol("> ");

            f.render_stateful_widget(list, chunks[0], &mut list_state);

            let help = Paragraph::new(Line::from(vec![
                Span::styled(" ↑/k", Style::default().fg(Color::Yellow)),
                Span::raw(" up  "),
                Span::styled("↓/j", Style::default().fg(Color::Yellow)),
                Span::raw(" down  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" select  "),
                Span::styled("Esc/q", Style::default().fg(Color::Yellow)),
                Span::raw(" cancel"),
            ]));
            f.render_widget(help, chunks[1]);
        })?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break None,
                KeyCode::Enter => {
                    if let Some(i) = list_state.selected() {
                        break Some(entries[i].id.clone());
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = list_state.selected().unwrap_or(0);
                    let next = if i == 0 { entries.len() - 1 } else { i - 1 };
                    list_state.select(Some(next));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = list_state.selected().unwrap_or(0);
                    let next = if i >= entries.len() - 1 { 0 } else { i + 1 };
                    list_state.select(Some(next));
                }
                _ => {}
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    Ok(result)
}
