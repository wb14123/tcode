use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{Local, TimeZone};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tcode_runtime::fts::{self, IndexProgress, SearchResult};
use tcode_runtime::session::{SessionMeta, SessionMode};

use crate::session::{self, Session};

const TICK_RATE: Duration = Duration::from_millis(50);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

struct SessionEntry {
    id: String,
    status: String,
    mode: SessionMode,
    description: Option<String>,
    last_active_at: u64,
}

struct IndexState {
    current: usize,
    total: usize,
    finished: bool,
    error: Option<String>,
}

struct SearchState {
    active: bool,
    query: String,
    generation: u64,
    pending: bool,
    last_edit: Option<Instant>,
    in_progress: bool,
    cancel: Option<Arc<AtomicBool>>,
    results_query: Option<String>,
    results: Vec<SearchResult>,
    error: Option<String>,
}

struct SearchMessage {
    generation: u64,
    query: String,
    result: std::result::Result<Vec<SearchResult>, String>,
}

struct DisplayItem<'a> {
    entry: &'a SessionEntry,
    snippet: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct SnippetCell {
    ch: char,
    highlighted: bool,
}

/// Show an interactive session picker and return the selected session ID.
/// Returns `None` if the user cancels (Esc/q) or there are no sessions.
pub fn pick_session() -> Result<Option<String>> {
    let base_path = session::base_path()?;
    let sessions = session::list_sessions_at(&base_path)?;
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
            let mode = meta.as_ref().map(|m| m.mode).unwrap_or_default();
            let description = meta.and_then(|m| m.description);
            Some((
                SessionEntry {
                    id,
                    status: status.to_string(),
                    mode,
                    description,
                    last_active_at: last_active,
                },
                last_active,
            ))
        })
        .collect();

    // Sort by last_active_at descending (most recent first)
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    let entries: Vec<SessionEntry> = entries.into_iter().map(|(e, _)| e).collect();

    run_picker(&entries, base_path)
}

fn run_picker(entries: &[SessionEntry], base_path: PathBuf) -> Result<Option<String>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_picker_loop(&mut terminal, entries, base_path);

    let cleanup_result = (|| -> Result<()> {
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    })();

    match (result, cleanup_result) {
        (Err(e), _) => Err(e),
        (Ok(_), Err(e)) => Err(e),
        (Ok(result), Ok(())) => Ok(result),
    }
}

fn run_picker_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    entries: &[SessionEntry],
    base_path: PathBuf,
) -> Result<Option<String>> {
    let mut list_state = ListState::default();
    list_state.select(Some(0));

    let entry_by_id: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id.clone(), index))
        .collect();

    let (index_tx, index_rx) = mpsc::channel();
    let index_base_path = base_path.clone();
    thread::spawn(move || {
        fts::ensure_indexed_with_progress(&index_base_path, index_tx);
    });
    let mut index_state = IndexState {
        current: 0,
        total: entries.len(),
        finished: false,
        error: None,
    };

    let (search_tx, search_rx) = mpsc::channel();
    let mut search_state = SearchState {
        active: false,
        query: String::new(),
        generation: 0,
        pending: false,
        last_edit: None,
        in_progress: false,
        cancel: None,
        results_query: None,
        results: Vec::new(),
        error: None,
    };

    loop {
        if poll_index_progress(&index_rx, &mut index_state) {
            refresh_search_after_indexing(&mut search_state);
        }
        poll_search_results(&search_rx, &mut search_state);
        maybe_start_search(&base_path, &search_tx, &mut search_state);

        let visible_len = display_len(entries, &entry_by_id, &search_state);
        clamp_selection(&mut list_state, visible_len);

        terminal.draw(|f| {
            let status = status_text(&index_state, &search_state);
            let mut constraints = Vec::new();
            if search_state.active {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Min(3));
            if status.is_some() {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Length(1));

            let chunks = Layout::vertical(constraints).split(f.area());
            let mut chunk_index = 0;

            if search_state.active {
                let search_area = chunks[chunk_index];
                chunk_index += 1;
                let search_bar = Paragraph::new(Line::from(vec![
                    Span::styled("/ ", Style::default().fg(Color::Yellow)),
                    Span::raw(search_state.query.clone()),
                ]));
                f.render_widget(search_bar, search_area);

                let query_width = search_state.query.chars().count() as u16;
                let cursor_x = search_area
                    .x
                    .saturating_add(2)
                    .saturating_add(query_width)
                    .min(
                        search_area
                            .x
                            .saturating_add(search_area.width.saturating_sub(1)),
                    );
                f.set_cursor_position((cursor_x, search_area.y));
            }

            let list_area = chunks[chunk_index];
            chunk_index += 1;

            let display_items = build_display_items(entries, &entry_by_id, &search_state);
            let item_width = usize::from(list_area.width.saturating_sub(4));
            let items: Vec<ListItem> = display_items
                .iter()
                .map(|item| list_item_for_display(item, item_width))
                .collect();

            let title = if search_state.active {
                " Search sessions "
            } else {
                " Select a session "
            };
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::REVERSED),
                )
                .highlight_symbol("> ");

            f.render_stateful_widget(list, list_area, &mut list_state);

            if let Some(status) = status {
                let status_area = chunks[chunk_index];
                chunk_index += 1;
                f.render_widget(Paragraph::new(status), status_area);
            }

            let help = if search_state.active {
                search_help_bar()
            } else {
                default_help_bar()
            };
            f.render_widget(Paragraph::new(help), chunks[chunk_index]);
        })?;

        if !event::poll(TICK_RATE)? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            cancel_running_search(&mut search_state);
            break Ok(None);
        }

        match key.code {
            KeyCode::Esc if search_state.active => {
                clear_search(&mut search_state);
                list_state.select(Some(0));
            }
            KeyCode::Char('/') if !search_state.active => {
                activate_search(&mut search_state);
                list_state.select(Some(0));
            }
            KeyCode::Char('q') | KeyCode::Esc if !search_state.active => {
                cancel_running_search(&mut search_state);
                break Ok(None);
            }
            KeyCode::Enter => {
                let selected_id = {
                    let display_items = build_display_items(entries, &entry_by_id, &search_state);
                    list_state
                        .selected()
                        .and_then(|i| display_items.get(i))
                        .map(|item| item.entry.id.clone())
                };
                if let Some(selected_id) = selected_id {
                    cancel_running_search(&mut search_state);
                    break Ok(Some(selected_id));
                }
            }
            KeyCode::Up => {
                move_selection_up(
                    &mut list_state,
                    display_len(entries, &entry_by_id, &search_state),
                );
            }
            KeyCode::Char('k') if !search_state.active => {
                move_selection_up(
                    &mut list_state,
                    display_len(entries, &entry_by_id, &search_state),
                );
            }
            KeyCode::Down => {
                move_selection_down(
                    &mut list_state,
                    display_len(entries, &entry_by_id, &search_state),
                );
            }
            KeyCode::Char('j') if !search_state.active => {
                move_selection_down(
                    &mut list_state,
                    display_len(entries, &entry_by_id, &search_state),
                );
            }
            KeyCode::Backspace if search_state.active => {
                search_state.query.pop();
                search_query_changed(&mut search_state);
                list_state.select(Some(0));
            }
            KeyCode::Char(c) if search_state.active && is_text_modifier(key.modifiers) => {
                search_state.query.push(c);
                search_query_changed(&mut search_state);
                list_state.select(Some(0));
            }
            _ => {}
        }
    }
}

fn poll_index_progress(
    index_rx: &mpsc::Receiver<IndexProgress>,
    index_state: &mut IndexState,
) -> bool {
    let mut finished_now = false;
    loop {
        match index_rx.try_recv() {
            Ok(IndexProgress::Progress { current, total }) => {
                index_state.current = current;
                index_state.total = total;
            }
            Ok(IndexProgress::Done(result)) => {
                if !index_state.finished {
                    finished_now = true;
                }
                index_state.finished = true;
                index_state.error = result.err().map(|e| e.to_string());
                break;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                if !index_state.finished {
                    finished_now = true;
                    index_state.finished = true;
                    index_state.error = Some("indexing stopped unexpectedly".to_string());
                }
                break;
            }
        }
    }
    finished_now
}

fn poll_search_results(search_rx: &mpsc::Receiver<SearchMessage>, search_state: &mut SearchState) {
    while let Ok(message) = search_rx.try_recv() {
        if message.generation != search_state.generation {
            continue;
        }

        search_state.in_progress = false;
        search_state.cancel = None;

        if !search_state.active || search_state.pending || message.query != search_state.query {
            continue;
        }

        match message.result {
            Ok(results) => {
                search_state.results_query = Some(message.query);
                search_state.results = results;
                search_state.error = None;
            }
            Err(error) => {
                search_state.results_query = None;
                search_state.results.clear();
                search_state.error = Some(error);
            }
        }
    }
}

fn maybe_start_search(
    base_path: &Path,
    search_tx: &mpsc::Sender<SearchMessage>,
    search_state: &mut SearchState,
) {
    if !search_state.active || !search_state.pending || search_state.query.trim().is_empty() {
        return;
    }

    let Some(last_edit) = search_state.last_edit else {
        return;
    };
    if last_edit.elapsed() < SEARCH_DEBOUNCE {
        return;
    }

    cancel_running_search(search_state);

    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = Arc::clone(&cancel);
    let thread_tx = search_tx.clone();
    let thread_base_path = base_path.to_path_buf();
    let query = search_state.query.clone();
    let generation = search_state.generation;

    search_state.in_progress = true;
    search_state.pending = false;
    search_state.cancel = Some(cancel);
    search_state.error = None;

    thread::spawn(move || {
        let result = fts::search_with_cancel(&thread_base_path, &query, thread_cancel.as_ref())
            .map_err(|e| e.to_string());
        let message = SearchMessage {
            generation,
            query,
            result,
        };
        if let Err(e) = thread_tx.send(message) {
            tracing::debug!(error = %e, "session picker closed before search result was delivered");
        }
    });
}

fn activate_search(search_state: &mut SearchState) {
    cancel_running_search(search_state);
    search_state.active = true;
    search_state.query.clear();
    search_state.generation = search_state.generation.wrapping_add(1);
    search_state.pending = false;
    search_state.last_edit = None;
    search_state.results_query = None;
    search_state.results.clear();
    search_state.error = None;
}

fn clear_search(search_state: &mut SearchState) {
    cancel_running_search(search_state);
    search_state.active = false;
    search_state.query.clear();
    search_state.generation = search_state.generation.wrapping_add(1);
    search_state.pending = false;
    search_state.last_edit = None;
    search_state.results_query = None;
    search_state.results.clear();
    search_state.error = None;
}

fn search_query_changed(search_state: &mut SearchState) {
    cancel_running_search(search_state);
    search_state.generation = search_state.generation.wrapping_add(1);
    search_state.last_edit = Some(Instant::now());
    search_state.error = None;

    if search_state.query.trim().is_empty() {
        search_state.pending = false;
        search_state.results_query = None;
        search_state.results.clear();
    } else {
        search_state.pending = true;
    }
}

fn refresh_search_after_indexing(search_state: &mut SearchState) {
    if !search_state.active || search_state.query.trim().is_empty() {
        return;
    }

    search_state.generation = search_state.generation.wrapping_add(1);
    search_state.pending = true;
    search_state.last_edit = Some(Instant::now());
    search_state.error = None;
}

fn cancel_running_search(search_state: &mut SearchState) {
    if let Some(cancel) = search_state.cancel.take() {
        cancel.store(true, Ordering::Relaxed);
    }
    search_state.in_progress = false;
}

fn is_text_modifier(modifiers: KeyModifiers) -> bool {
    !modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT)
}

fn build_display_items<'a>(
    entries: &'a [SessionEntry],
    entry_by_id: &HashMap<String, usize>,
    search_state: &'a SearchState,
) -> Vec<DisplayItem<'a>> {
    if has_current_search_results(search_state) {
        return search_state
            .results
            .iter()
            .filter_map(|result| {
                entry_by_id
                    .get(&result.session_id)
                    .and_then(|index| entries.get(*index))
                    .map(|entry| DisplayItem {
                        entry,
                        snippet: Some(result.snippet.as_str()),
                    })
            })
            .collect();
    }

    entries
        .iter()
        .map(|entry| DisplayItem {
            entry,
            snippet: None,
        })
        .collect()
}

fn has_current_search_results(search_state: &SearchState) -> bool {
    search_state.active
        && !search_state.query.trim().is_empty()
        && search_state.results_query.as_deref() == Some(search_state.query.as_str())
}

fn display_len(
    entries: &[SessionEntry],
    entry_by_id: &HashMap<String, usize>,
    search_state: &SearchState,
) -> usize {
    build_display_items(entries, entry_by_id, search_state).len()
}

fn clamp_selection(list_state: &mut ListState, len: usize) {
    if len == 0 {
        list_state.select(None);
        return;
    }

    let selected = list_state.selected().unwrap_or(0).min(len - 1);
    list_state.select(Some(selected));
}

fn move_selection_up(list_state: &mut ListState, len: usize) {
    if len == 0 {
        list_state.select(None);
        return;
    }

    let i = list_state.selected().unwrap_or(0);
    let next = if i == 0 { len - 1 } else { i - 1 };
    list_state.select(Some(next));
}

fn move_selection_down(list_state: &mut ListState, len: usize) {
    if len == 0 {
        list_state.select(None);
        return;
    }

    let i = list_state.selected().unwrap_or(0);
    let next = if i >= len - 1 { 0 } else { i + 1 };
    list_state.select(Some(next));
}

fn list_item_for_display(item: &DisplayItem<'_>, width: usize) -> ListItem<'static> {
    if let Some(snippet) = item.snippet {
        ListItem::new(vec![
            session_summary_line(item.entry, true),
            highlighted_snippet_line(snippet, width),
        ])
    } else {
        ListItem::new(session_summary_line(item.entry, true))
    }
}

fn session_summary_line(entry: &SessionEntry, include_description: bool) -> Line<'static> {
    let mut spans = base_session_spans(entry);
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format_last_active(entry.last_active_at),
        Style::default().fg(Color::DarkGray),
    ));
    if include_description && let Some(ref desc) = entry.description {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.clone(), Style::default().fg(Color::Cyan)));
    }
    Line::from(spans)
}

fn base_session_spans(entry: &SessionEntry) -> Vec<Span<'static>> {
    let status_style = if entry.status == "active" {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    vec![
        Span::raw(entry.id.clone()),
        Span::raw(" "),
        Span::styled(
            format!("({}, {})", entry.status, entry.mode.label()),
            status_style,
        ),
    ]
}

fn format_last_active(last_active_at: u64) -> String {
    if last_active_at == 0 {
        return "unknown time".to_string();
    }

    let Ok(timestamp_ms) = i64::try_from(last_active_at) else {
        return "unknown time".to_string();
    };

    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown time".to_string())
}

fn highlighted_snippet_line(snippet: &str, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    let available_width = width.saturating_sub(2);
    if available_width == 0 {
        return Line::from(spans);
    }

    let cells = snippet_cells(snippet);
    let (visible_cells, has_left_overflow, has_right_overflow) =
        snippet_window(&cells, available_width);

    if has_left_overflow {
        spans.push(Span::styled("...", Style::default().fg(Color::Gray)));
    }
    push_snippet_cells(&mut spans, &visible_cells);
    if has_right_overflow {
        spans.push(Span::styled("...", Style::default().fg(Color::Gray)));
    }

    Line::from(spans)
}

fn snippet_cells(snippet: &str) -> Vec<SnippetCell> {
    let mut cells = Vec::new();
    let mut highlighted = false;

    for ch in snippet.chars() {
        match ch {
            '\x01' => highlighted = true,
            '\x02' => highlighted = false,
            _ => cells.push(SnippetCell { ch, highlighted }),
        }
    }

    remove_segmented_cjk_spaces(cells)
}

fn remove_segmented_cjk_spaces(cells: Vec<SnippetCell>) -> Vec<SnippetCell> {
    cells
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            if cell.ch.is_whitespace() && is_between_cjk_chars(&cells, index) {
                None
            } else {
                Some(*cell)
            }
        })
        .collect()
}

fn is_between_cjk_chars(cells: &[SnippetCell], index: usize) -> bool {
    let prev = cells[..index]
        .iter()
        .rev()
        .find(|cell| !cell.ch.is_whitespace())
        .map(|cell| cell.ch);
    let next = cells[index + 1..]
        .iter()
        .find(|cell| !cell.ch.is_whitespace())
        .map(|cell| cell.ch);

    prev.is_some_and(is_cjk_char) && next.is_some_and(is_cjk_char)
}

fn is_cjk_char(ch: char) -> bool {
    ('\u{3400}'..='\u{4DBF}').contains(&ch)
        || ('\u{4E00}'..='\u{9FFF}').contains(&ch)
        || ('\u{F900}'..='\u{FAFF}').contains(&ch)
        || ('\u{20000}'..='\u{2A6DF}').contains(&ch)
        || ('\u{2A700}'..='\u{2B73F}').contains(&ch)
        || ('\u{2B740}'..='\u{2B81F}').contains(&ch)
        || ('\u{2B820}'..='\u{2CEAF}').contains(&ch)
}

fn snippet_window(cells: &[SnippetCell], max_width: usize) -> (Vec<SnippetCell>, bool, bool) {
    if cells.len() <= max_width {
        return (cells.to_vec(), false, false);
    }
    if max_width <= 6 {
        return (cells.iter().take(max_width).copied().collect(), false, true);
    }

    let anchor = cells
        .iter()
        .position(|cell| cell.highlighted)
        .unwrap_or_default();
    let mut start = anchor.saturating_sub(max_width / 3);
    start = start.min(cells.len().saturating_sub(max_width));
    let mut end = start.saturating_add(max_width).min(cells.len());
    let mut has_left_overflow = start > 0;
    let mut has_right_overflow = end < cells.len();

    let reserved_width =
        if has_left_overflow { 3 } else { 0 } + if has_right_overflow { 3 } else { 0 };
    if reserved_width < max_width {
        let content_width = max_width - reserved_width;
        start = anchor.saturating_sub(content_width / 3);
        start = start.min(cells.len().saturating_sub(content_width));
        end = start.saturating_add(content_width).min(cells.len());
        has_left_overflow = start > 0;
        has_right_overflow = end < cells.len();
    }

    (
        cells[start..end].to_vec(),
        has_left_overflow,
        has_right_overflow,
    )
}

fn push_snippet_cells(spans: &mut Vec<Span<'static>>, cells: &[SnippetCell]) {
    let mut current = String::new();
    let mut current_highlighted = cells.first().is_some_and(|cell| cell.highlighted);

    for cell in cells {
        if cell.highlighted != current_highlighted {
            push_snippet_span(spans, &mut current, current_highlighted);
            current_highlighted = cell.highlighted;
        }
        current.push(cell.ch);
    }
    push_snippet_span(spans, &mut current, current_highlighted);
}

fn push_snippet_span(spans: &mut Vec<Span<'static>>, current: &mut String, highlighted: bool) {
    if current.is_empty() {
        return;
    }

    let text = std::mem::take(current);
    let style = if highlighted {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    spans.push(Span::styled(text, style));
}

fn status_text(index_state: &IndexState, search_state: &SearchState) -> Option<Line<'static>> {
    let mut parts = Vec::new();

    if search_state.in_progress {
        parts.push("Searching...".to_string());
    } else if let Some(error) = &search_state.error {
        parts.push(format!("Search failed: {error}"));
    } else if has_current_search_results(search_state) && search_state.results.is_empty() {
        parts.push("No matches".to_string());
    }

    if let Some(error) = &index_state.error {
        parts.push(format!("Indexing failed: {error}"));
    } else if !index_state.finished {
        parts.push(format!(
            "Checking search index... ({}/{})",
            index_state.current, index_state.total
        ));
    }

    if parts.is_empty() {
        None
    } else {
        Some(Line::from(Span::styled(
            parts.join("  |  "),
            Style::default().fg(Color::Yellow),
        )))
    }
}

fn default_help_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled(" ↑/k", Style::default().fg(Color::Yellow)),
        Span::raw(" up  "),
        Span::styled("↓/j", Style::default().fg(Color::Yellow)),
        Span::raw(" down  "),
        Span::styled("/", Style::default().fg(Color::Yellow)),
        Span::raw(" search  "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" select  "),
        Span::styled("Esc/q", Style::default().fg(Color::Yellow)),
        Span::raw(" cancel"),
    ])
}

fn search_help_bar() -> Line<'static> {
    Line::from(vec![
        Span::raw("type to search  "),
        Span::styled("↑/↓", Style::default().fg(Color::Yellow)),
        Span::raw(" navigate  "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" select  "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" clear  "),
        Span::styled("Ctrl-C", Style::default().fg(Color::Yellow)),
        Span::raw(" exit"),
    ])
}
