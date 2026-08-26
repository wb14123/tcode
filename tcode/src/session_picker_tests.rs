use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::Result;
use ratatui::text::Line;
use tcode_runtime::fts::SearchResult;

use super::{
    FilterMode, IndexState, SearchState, entries_from_index_markers, load_all_entries,
    pick_session_at, show_empty_folder_hint, status_text,
};
use crate::test_support::TestDir;

/// Render a status line to plain text for test assertions.
fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Exact-match check between a session's recorded cwd and the current folder.
/// `None` (no recorded cwd) never matches; an entry cwd that cannot be
/// canonicalized (folder deleted) never matches; otherwise the two canonical
/// paths must be exactly equal.
fn session_cwd_matches_current(entry_cwd: Option<&Path>, current_dir: &Path) -> bool {
    let Some(entry_cwd) = entry_cwd else {
        return false;
    };
    let Ok(entry_cwd) = std::fs::canonicalize(entry_cwd) else {
        return false;
    };
    super::canonical_paths_match(&entry_cwd, current_dir)
}

#[test]
fn exact_match_is_true() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let current = std::fs::canonicalize(dir.path())?;
    assert!(session_cwd_matches_current(Some(dir.path()), &current));
    Ok(())
}

#[test]
fn subdirectory_of_current_is_false() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub)?;
    let current = std::fs::canonicalize(dir.path())?;
    assert!(!session_cwd_matches_current(Some(&sub), &current));
    Ok(())
}

#[test]
fn parent_of_current_is_false() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let parent = dir.path().parent().expect("test dir has a parent");
    let current = std::fs::canonicalize(dir.path())?;
    assert!(!session_cwd_matches_current(Some(parent), &current));
    Ok(())
}

#[test]
fn recorded_cwd_that_no_longer_exists_is_false() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let gone = dir.path().join("gone");
    std::fs::create_dir_all(&gone)?;
    std::fs::remove_dir_all(&gone)?;
    let current = std::fs::canonicalize(dir.path())?;
    assert!(!session_cwd_matches_current(Some(&gone), &current));
    Ok(())
}

#[test]
fn symlinked_equivalent_paths_are_true() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real)?;
    let link = dir.path().join("link");
    symlink(&real, &link)?;
    let current = std::fs::canonicalize(&real)?;
    assert!(session_cwd_matches_current(Some(&link), &current));
    Ok(())
}

#[test]
fn none_never_matches() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let current = std::fs::canonicalize(dir.path())?;
    assert!(!session_cwd_matches_current(None, &current));
    Ok(())
}

/// Create a session directory under `base` with a readable meta recording the
/// given last_active_at timestamp.
fn write_session(base: &Path, id: &str, last_active_at: u64) -> Result<()> {
    let dir = base.join(id);
    fs::create_dir_all(&dir)?;
    let meta = serde_json::json!({
        "description": null,
        "last_active_at": last_active_at,
        "mode": "normal",
    });
    fs::write(dir.join("session-meta.json"), serde_json::to_vec(&meta)?)?;
    Ok(())
}

/// Create a zero-byte index marker for `id` under the given index directory.
fn write_marker(index: &Path, id: &str) -> Result<()> {
    fs::create_dir_all(index)?;
    fs::write(index.join(id), "")?;
    Ok(())
}

#[test]
fn folder_entries_come_from_index_markers() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let base = dir.path().join("sessions");
    let index = dir.path().join("index");
    fs::create_dir_all(&base)?;

    // Two sessions are indexed under the current project; a third session
    // exists but is not indexed and must not appear (no fallback scan).
    write_session(&base, "aaaa1111", 200)?;
    write_session(&base, "bbbb2222", 100)?;
    write_session(&base, "cccc3333", 300)?;
    write_marker(&index, "aaaa1111")?;
    write_marker(&index, "bbbb2222")?;

    let entries = entries_from_index_markers(&base, &index);
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    assert_eq!(ids, vec!["aaaa1111".to_string(), "bbbb2222".to_string()]);
    // Entries are sorted by last_active_at descending.
    assert_eq!(entries[0].last_active_at, 200);
    assert_eq!(entries[1].last_active_at, 100);
    Ok(())
}

#[test]
fn missing_index_yields_empty_folder_view() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let base = dir.path().join("sessions");
    fs::create_dir_all(&base)?;
    write_session(&base, "aaaa1111", 100)?;

    let missing = dir.path().join("no-index");
    let entries = entries_from_index_markers(&base, &missing);
    assert!(
        entries.is_empty(),
        "a missing index must show an empty view, not fall back to a scan"
    );
    Ok(())
}

#[test]
fn empty_index_yields_empty_folder_view() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let base = dir.path().join("sessions");
    let index = dir.path().join("index");
    fs::create_dir_all(&base)?;
    fs::create_dir_all(&index)?;
    write_session(&base, "aaaa1111", 100)?;

    let entries = entries_from_index_markers(&base, &index);
    assert!(
        entries.is_empty(),
        "an empty index must show an empty view, not fall back to a scan"
    );
    Ok(())
}

#[test]
fn stale_index_markers_are_skipped() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let base = dir.path().join("sessions");
    let index = dir.path().join("index");
    fs::create_dir_all(&base)?;
    write_session(&base, "aaaa1111", 100)?;
    write_marker(&index, "aaaa1111")?;
    // Marker for a session whose directory no longer exists.
    write_marker(&index, "bbbb2222")?;

    let entries = entries_from_index_markers(&base, &index);
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    assert_eq!(ids, vec!["aaaa1111".to_string()]);
    Ok(())
}

#[test]
fn unreadable_meta_is_skipped_in_folder_view() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let base = dir.path().join("sessions");
    let index = dir.path().join("index");
    fs::create_dir_all(&base)?;
    let session_dir = base.join("aaaa1111");
    fs::create_dir_all(&session_dir)?;
    fs::write(session_dir.join("session-meta.json"), "{not json")?;
    write_marker(&index, "aaaa1111")?;

    let entries = entries_from_index_markers(&base, &index);
    assert!(entries.is_empty(), "an unreadable meta must be skipped");
    Ok(())
}

#[test]
fn all_mode_lists_everything() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let base = dir.path().join("sessions");
    fs::create_dir_all(&base)?;
    write_session(&base, "aaaa1111", 200)?;
    // No meta file at all — kept with defaults in the all-sessions view.
    fs::create_dir_all(base.join("bbbb2222"))?;

    let entries = load_all_entries(&base);
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    assert_eq!(ids, vec!["aaaa1111".to_string(), "bbbb2222".to_string()]);
    assert_eq!(entries[0].last_active_at, 200);
    assert_eq!(entries[1].last_active_at, 0);
    Ok(())
}

#[test]
fn zero_total_sessions_exits_early() -> Result<()> {
    let dir = TestDir::new("session_picker");
    let base = dir.path().join("sessions");
    fs::create_dir_all(&base)?;
    assert!(
        pick_session_at(&base)?.is_none(),
        "no sessions at all must exit before any UI work"
    );
    Ok(())
}

#[test]
fn empty_folder_hint_shows_when_folder_view_is_empty_but_sessions_exist() {
    assert!(show_empty_folder_hint(
        FilterMode::CurrentFolder,
        0,
        5,
        Some(Path::new("/some/dir")),
        false,
    ));
}

#[test]
fn empty_folder_hint_hidden_when_nothing_exists_total() {
    assert!(!show_empty_folder_hint(
        FilterMode::CurrentFolder,
        0,
        0,
        Some(Path::new("/some/dir")),
        false,
    ));
}

#[test]
fn empty_folder_hint_hidden_when_folder_view_has_items() {
    assert!(!show_empty_folder_hint(
        FilterMode::CurrentFolder,
        1,
        5,
        Some(Path::new("/some/dir")),
        false,
    ));
}

#[test]
fn empty_folder_hint_hidden_in_all_mode() {
    assert!(!show_empty_folder_hint(
        FilterMode::All,
        0,
        5,
        Some(Path::new("/some/dir")),
        false,
    ));
}

#[test]
fn empty_folder_hint_hidden_without_current_dir() {
    assert!(!show_empty_folder_hint(
        FilterMode::CurrentFolder,
        0,
        5,
        None,
        false,
    ));
}

#[test]
fn empty_folder_hint_hidden_during_active_search() {
    assert!(!show_empty_folder_hint(
        FilterMode::CurrentFolder,
        0,
        5,
        Some(Path::new("/some/dir")),
        true,
    ));
}

// ---------------------------------------------------------------------------
// status_text (folder-mode search with all results filtered out)
// ---------------------------------------------------------------------------

/// Construct an `IndexState` with only the fields the status line reads set;
/// the remaining fields take their initial values.
fn index_state(finished: bool, error: Option<String>) -> IndexState {
    IndexState {
        current: 0,
        total: 0,
        finished,
        error,
    }
}

/// Construct a `SearchState` with only the fields the status line reads set;
/// the remaining fields take their initial values.
fn search_state(
    active: bool,
    query: String,
    in_progress: bool,
    results_query: Option<String>,
    results: Vec<SearchResult>,
    error: Option<String>,
) -> SearchState {
    SearchState {
        active,
        query,
        generation: 0,
        pending: false,
        last_edit: None,
        in_progress,
        cancel: None,
        results_query,
        results,
        error,
    }
}

fn index_finished() -> IndexState {
    index_state(true, None)
}

fn idle_search() -> SearchState {
    search_state(false, String::new(), false, None, Vec::new(), None)
}

fn search_state_with_results(results: Vec<SearchResult>) -> SearchState {
    search_state(
        true,
        "query".to_string(),
        false,
        Some("query".to_string()),
        results,
        None,
    )
}

fn status_text_of(filter_mode: FilterMode, visible_len: usize, search: &SearchState) -> String {
    status_text(&index_finished(), search, filter_mode, visible_len)
        .map(|line| line_text(&line))
        .unwrap_or_default()
}

#[test]
fn status_text_is_none_when_idle() {
    let text = status_text(
        &index_finished(),
        &idle_search(),
        FilterMode::CurrentFolder,
        3,
    );
    assert!(text.is_none(), "idle picker must show no status line");
}

#[test]
fn status_text_shows_searching_while_in_progress() {
    let search = search_state(true, "query".to_string(), true, None, Vec::new(), None);
    let text = status_text_of(FilterMode::CurrentFolder, 0, &search);
    assert!(text.contains("Searching..."), "unexpected status: {text}");
}

#[test]
fn status_text_shows_no_matches_when_results_are_empty() {
    let search = search_state_with_results(Vec::new());
    let text = status_text_of(FilterMode::CurrentFolder, 0, &search);
    assert!(text.contains("No matches"), "unexpected status: {text}");
}

#[test]
fn status_text_shows_folder_no_matches_when_results_are_filtered_out() {
    let search = search_state_with_results(vec![SearchResult {
        session_id: "aaaa1111".to_string(),
        snippet: "snippet".to_string(),
    }]);
    let text = status_text_of(FilterMode::CurrentFolder, 0, &search);
    assert!(
        text.contains("No matches in this folder - press Tab to show all sessions"),
        "unexpected status: {text}"
    );
}

#[test]
fn status_text_folder_no_matches_not_shown_in_all_mode() {
    let search = search_state_with_results(vec![SearchResult {
        session_id: "aaaa1111".to_string(),
        snippet: "snippet".to_string(),
    }]);
    let text = status_text_of(FilterMode::All, 0, &search);
    assert!(
        !text.contains("No matches in this folder"),
        "all-sessions mode must not show the folder message: {text}"
    );
}

#[test]
fn status_text_folder_no_matches_not_shown_when_items_are_visible() {
    let search = search_state_with_results(vec![SearchResult {
        session_id: "aaaa1111".to_string(),
        snippet: "snippet".to_string(),
    }]);
    let text = status_text_of(FilterMode::CurrentFolder, 1, &search);
    assert!(
        !text.contains("No matches in this folder"),
        "a non-empty display must not show the folder message: {text}"
    );
}

#[test]
fn status_text_shows_search_failure() {
    let search = search_state(
        true,
        "query".to_string(),
        false,
        None,
        Vec::new(),
        Some("boom".to_string()),
    );
    let text = status_text_of(FilterMode::CurrentFolder, 0, &search);
    assert!(
        text.contains("Search failed: boom"),
        "unexpected status: {text}"
    );
}
