use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::Result;
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;
use tcode_runtime::fts::SearchResult;
use tcode_runtime::session::SessionMode;

use super::{
    FilterMode, IndexState, SearchState, SessionEntry, build_display_items,
    entries_from_index_markers, entry_index_by_id, load_all_entries, pick_session_at,
    session_summary_line, show_empty_folder_hint, status_text,
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
fn status_text_shows_no_status_when_a_name_match_is_visible() {
    // A query can match a session name while the content search finds nothing.
    let search = search_state_with_results(Vec::new());
    let text = status_text_of(FilterMode::CurrentFolder, 1, &search);
    assert!(
        !text.contains("No matches"),
        "a visible name match must not report no matches: {text}"
    );
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

// ---------------------------------------------------------------------------
// session_summary_line (name and description labels)
// ---------------------------------------------------------------------------

/// Build an entry with a fixed status and timestamp, so only the id, the name,
/// and the description vary between cases. The timestamp renders as
/// `unknown time`.
fn session_entry(id: &str, name: Option<&str>, description: Option<&str>) -> SessionEntry {
    SessionEntry {
        id: id.to_string(),
        status: "active".to_string(),
        mode: SessionMode::Normal,
        name: name.map(str::to_string),
        description: description.map(str::to_string),
        last_active_at: 0,
    }
}

fn summary_text(name: Option<&str>, description: Option<&str>) -> String {
    line_text(&session_summary_line(
        &session_entry("aaaa1111", name, description),
        true,
    ))
}

#[test]
fn summary_line_shows_name_only() {
    let text = summary_text(Some("my session"), None);
    assert_eq!(text, "aaaa1111 (active, normal) unknown time my session");
}

#[test]
fn summary_line_shows_description_only() {
    let text = summary_text(None, Some("the description"));
    assert_eq!(
        text,
        "aaaa1111 (active, normal) unknown time the description"
    );
}

#[test]
fn summary_line_shows_name_then_description() {
    let text = summary_text(Some("my session"), Some("the description"));
    assert_eq!(
        text,
        "aaaa1111 (active, normal) unknown time my session the description"
    );
    let name_at = text.find("my session").expect("name is rendered");
    let description_at = text
        .find("the description")
        .expect("description is rendered");
    assert!(
        name_at < description_at,
        "the name must come before the description: {text}"
    );
}

#[test]
fn summary_line_shows_no_label_when_both_are_missing() {
    let text = summary_text(None, None);
    assert_eq!(text, "aaaa1111 (active, normal) unknown time");
}

#[test]
fn summary_line_styles_name_bold_and_description_cyan() {
    let line = session_summary_line(
        &session_entry("aaaa1111", Some("my session"), Some("the description")),
        true,
    );

    let name_span = line
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "my session")
        .expect("the name has its own span");
    assert!(
        name_span.style.add_modifier.contains(Modifier::BOLD),
        "the name must be bold"
    );
    assert_eq!(
        name_span.style.fg,
        Some(Color::Yellow),
        "the name is yellow"
    );

    let description_span = line
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "the description")
        .expect("the description has its own span");
    assert_eq!(
        description_span.style.fg,
        Some(Color::Cyan),
        "the description stays cyan"
    );
}

// ---------------------------------------------------------------------------
// build_display_items (name matches rank above content matches)
// ---------------------------------------------------------------------------

fn search_state_for(query: &str, results: Vec<SearchResult>) -> SearchState {
    search_state(
        true,
        query.to_string(),
        false,
        Some(query.to_string()),
        results,
        None,
    )
}

fn result_for(session_id: &str, snippet: &str) -> SearchResult {
    SearchResult {
        session_id: session_id.to_string(),
        snippet: snippet.to_string(),
    }
}

fn display_ids(entries: &[SessionEntry], search: &SearchState) -> Vec<String> {
    let entry_by_id = entry_index_by_id(entries);
    build_display_items(entries, &entry_by_id, search)
        .iter()
        .map(|item| item.entry.id.clone())
        .collect()
}

fn display_snippets(entries: &[SessionEntry], search: &SearchState) -> Vec<Option<String>> {
    let entry_by_id = entry_index_by_id(entries);
    build_display_items(entries, &entry_by_id, search)
        .iter()
        .map(|item| item.snippet.map(str::to_string))
        .collect()
}

#[test]
fn search_puts_a_name_match_before_content_matches() {
    let entries = vec![
        session_entry("aaaaaaaa", Some("Release notes"), None),
        session_entry("bbbbbbbb", None, None),
    ];
    let search = search_state_for("release", vec![result_for("bbbbbbbb", "...release...")]);

    assert_eq!(display_ids(&entries, &search), vec!["aaaaaaaa", "bbbbbbbb"]);
    assert_eq!(
        display_snippets(&entries, &search),
        vec![None, Some("...release...".to_string())],
        "a name-only match shows no snippet"
    );
}

#[test]
fn search_matches_a_name_by_case_insensitive_terms_in_any_order() {
    let entries = vec![session_entry(
        "aaaaaaaa",
        Some("Weekly Release Notes"),
        None,
    )];
    let search = search_state_for("notes weekly", Vec::new());

    assert_eq!(display_ids(&entries, &search), vec!["aaaaaaaa"]);
}

#[test]
fn search_matches_a_name_only_when_all_terms_are_present() {
    let entries = vec![session_entry("aaaaaaaa", Some("Release notes"), None)];
    let search = search_state_for("release budget", Vec::new());

    assert!(display_ids(&entries, &search).is_empty());
}

#[test]
fn search_shows_a_session_once_when_its_name_and_content_match() {
    let entries = vec![session_entry("aaaaaaaa", Some("Release notes"), None)];
    let search = search_state_for("release", vec![result_for("aaaaaaaa", "...release...")]);

    assert_eq!(display_ids(&entries, &search), vec!["aaaaaaaa"]);
    assert_eq!(
        display_snippets(&entries, &search),
        vec![Some("...release...".to_string())],
        "a name match keeps the content snippet when the content matched too"
    );
}

#[test]
fn search_does_not_match_a_session_without_a_name() {
    let entries = vec![session_entry("aaaaaaaa", None, None)];
    let search = search_state_for("release", Vec::new());

    assert!(display_ids(&entries, &search).is_empty());
}

#[test]
fn search_matches_a_non_ascii_name_through_extra_spaces() {
    let entries = vec![session_entry("aaaaaaaa", Some("发布 记录"), None)];
    let search = search_state_for("  记录   发布  ", Vec::new());

    assert_eq!(display_ids(&entries, &search), vec!["aaaaaaaa"]);
}

#[test]
fn search_keeps_the_entry_order_within_the_name_group() {
    let entries = vec![
        session_entry("cccccccc", Some("Release two"), None),
        session_entry("aaaaaaaa", Some("Release one"), None),
    ];
    let search = search_state_for("release", Vec::new());

    assert_eq!(display_ids(&entries, &search), vec!["cccccccc", "aaaaaaaa"]);
}

#[test]
fn search_drops_results_outside_the_filtered_entries() {
    let entries = vec![session_entry("aaaaaaaa", None, None)];
    let search = search_state_for("release", vec![result_for("bbbbbbbb", "...release...")]);

    assert!(display_ids(&entries, &search).is_empty());
}
