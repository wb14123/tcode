use tcode_runtime::session::SessionMode;

use super::{SessionListEntry, escape_tmux_format, session_list_line};

/// Build an entry with an inactive status and a fixed ID.
fn entry(name: Option<&str>, description: Option<&str>) -> SessionListEntry {
    SessionListEntry {
        id: "abc123xy".to_string(),
        status: "inactive".to_string(),
        mode: SessionMode::Normal,
        name: name.map(str::to_string),
        description: description.map(str::to_string),
        last_active_at: 1,
    }
}

#[test]
fn line_with_name_only() {
    assert_eq!(
        session_list_line(&entry(Some("Refactor branch"), None)),
        "  abc123xy (inactive, normal) Refactor branch"
    );
}

#[test]
fn line_with_description_only() {
    assert_eq!(
        session_list_line(&entry(None, Some("Project refactor"))),
        "  abc123xy (inactive, normal) Project refactor"
    );
}

#[test]
fn line_with_name_and_description() {
    assert_eq!(
        session_list_line(&entry(Some("Refactor branch"), Some("Project refactor"))),
        "  abc123xy (inactive, normal) Refactor branch - Project refactor"
    );
}

#[test]
fn line_without_name_or_description() {
    assert_eq!(
        session_list_line(&entry(None, None)),
        "  abc123xy (inactive, normal)"
    );
}

#[test]
fn tmux_format_escapes_the_expanded_sequences() {
    assert_eq!(escape_tmux_format("#{session_name}"), "##{session_name}");
    assert_eq!(escape_tmux_format("#(id -u)"), "##(id -u)");
}

#[test]
fn tmux_format_leaves_other_text_alone() {
    assert_eq!(escape_tmux_format("plain name"), "plain name");
    assert_eq!(escape_tmux_format("doc #[1] #tag"), "doc #[1] #tag");
}
