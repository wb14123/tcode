use llm_rs::permission::{PendingPermissionInfo, PermissionState, WILDCARD_VALUE};
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

use crate::hscroll::PreparedRow;
use crate::permission_ui::{PermissionTreeState, key_row, tool_row, value_row};
use crate::tree_nav::TreeNav;

/// Concatenated text of a rendered line.
fn text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Offsets exercised for every overflowing row: start, middle, furthest right.
fn probe_offsets(row: &PreparedRow, width: usize) -> [usize; 3] {
    let max = row.max_offset(width);
    [0, max / 2, max]
}

const LONG_COMMAND: &str = "cargo test --package tcode --all-targets --no-fail-fast ";

fn long_value_row() -> PreparedRow {
    value_row(
        "    ",
        &LONG_COMMAND.repeat(4),
        "S",
        "session",
        Color::Cyan,
        false,
        false,
    )
}

#[test]
fn long_value_scrolls_without_losing_content() {
    let row = long_value_row();
    for width in [24usize, 40, 80] {
        let max = row.max_offset(width);
        assert!(max > 0, "row must overflow at width {width}");

        for offset in probe_offsets(&row, width) {
            let line = row.render(width, offset);
            assert!(
                line.width() <= width,
                "row exceeds viewport at width {width} offset {offset}: {:?}",
                text(&line)
            );
        }

        let head = text(&row.render(width, 0));
        assert!(
            head.starts_with("      [S] cargo test"),
            "indent, icon or value missing at width {width}: {head:?}"
        );

        let tail = text(&row.render(width, max));
        assert!(
            tail.ends_with("(session)"),
            "status suffix unreachable at width {width}: {tail:?}"
        );
    }

    // Degenerate widths must not panic and must stay within the viewport.
    for width in 0..=2usize {
        for offset in [0usize, 1, 12] {
            let line = row.render(width, offset);
            assert!(
                line.width() <= width,
                "width {width} offset {offset}: {:?}",
                text(&line)
            );
        }
    }
}

#[test]
fn wildcard_value_keeps_allow_all_styling() {
    let row = value_row(
        "",
        WILDCARD_VALUE,
        "P",
        "project",
        Color::Green,
        false,
        false,
    );
    let line = row.render(200, 0);
    assert_eq!(text(&line), "  [P] * (allow all) (project)");

    let star = line
        .spans
        .iter()
        .find(|span| span.content.as_ref() == " *")
        .map(|span| span.style)
        .expect("wildcard star span must exist");
    assert_eq!(star.fg, Some(Color::Yellow));
    assert!(star.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn pending_value_flashes_then_dims() {
    let row = value_row(
        "  ",
        "rm -rf build",
        "?",
        "pending",
        Color::Yellow,
        true,
        true,
    );
    for span in row.render(200, 0).spans {
        assert_eq!(span.style.fg, Some(Color::Red), "{:?}", span.content);
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    let dim_row = value_row(
        "  ",
        "rm -rf build",
        "?",
        "pending",
        Color::Yellow,
        true,
        false,
    );
    for span in dim_row.render(200, 0).spans {
        assert_eq!(span.style.fg, Some(Color::DarkGray), "{:?}", span.content);
    }
}

#[test]
fn tool_row_shows_container_suffix_and_pending_icon() {
    let in_container = tool_row("", false, "bash (in container dev)", true);
    let line = in_container.render(200, 0);
    assert_eq!(text(&line), "- bash (in container dev) ?");

    let icon = line
        .spans
        .iter()
        .find(|span| span.content.as_ref() == " ?")
        .map(|span| span.style)
        .expect("pending icon span must exist");
    assert_eq!(icon.fg, Some(Color::Yellow));

    let outside = tool_row("  ", true, "file_read (outside container)", false);
    assert_eq!(
        text(&outside.render(200, 0)),
        "  + file_read (outside container)"
    );

    let plain = tool_row("  ", false, "grep", false);
    assert_eq!(text(&plain.render(200, 0)), "  - grep");
}

#[test]
fn key_row_marks_collapse_state() {
    assert_eq!(
        text(&key_row("  ", false, "path").render(200, 0)),
        "  - path"
    );
    assert_eq!(
        text(&key_row("  ", true, "path").render(200, 0)),
        "  + path"
    );
}

#[test]
fn cjk_value_keeps_width_and_boundaries() {
    let value = "世界和平参数".repeat(6);
    let row = value_row("  ", &value, "S", "session", Color::Cyan, false, false);
    let allowed: Vec<char> = value
        .chars()
        .chain("[S]".chars())
        .chain(" (session)".chars())
        .collect();

    for width in [16usize, 40, 80] {
        for offset in probe_offsets(&row, width) {
            let line = row.render(width, offset);
            let rendered = text(&line);
            let rendered_width = line.width();
            assert!(
                rendered_width <= width,
                "width {width} offset {offset} rendered {rendered_width}: {rendered:?}"
            );
            assert!(
                rendered_width + 2 >= width,
                "row underfills at width {width} offset {offset}: {rendered:?}"
            );
            for ch in rendered.chars() {
                assert!(
                    allowed.contains(&ch),
                    "unexpected char {ch:?} at width {width} offset {offset}: {rendered:?}"
                );
            }
        }
    }
}

#[test]
fn value_folds_control_characters_into_spaces() {
    let row = value_row(
        "",
        "echo a\nb\tc",
        "S",
        "session",
        Color::Cyan,
        false,
        false,
    );

    let line = row.render(40, 0);
    let rendered = text(&line);
    assert!(rendered.starts_with("  [S] echo a b c"), "{rendered:?}");
    assert_eq!(line.width(), rendered.len());
    assert!(!rendered.chars().any(char::is_control));
}

/// Snapshot with one pending `bash/command` entry per value, in the given order.
fn pending_snapshot(values: &[&str]) -> PermissionState {
    PermissionState {
        pending: values
            .iter()
            .map(|value| PendingPermissionInfo {
                tool: "bash".to_string(),
                prompt: "run it?".to_string(),
                key: "command".to_string(),
                value: (*value).to_string(),
                request_id: format!("req-{value}"),
                preview_file_path: None,
                once_only: false,
            })
            .collect(),
        session: Vec::new(),
        project: Vec::new(),
    }
}

/// Selects the last value row of the `bash/command` group and scrolls right.
fn scrolled_state(values: &[&str]) -> PermissionTreeState {
    let mut state = PermissionTreeState::new("test-session".to_string(), None);
    state.rebuild_from_state(&pending_snapshot(values));
    // Visible rows: the tool, the key, then one row per value.
    state.set_selected(values.len() + 1);
    state.hscroll_offset = 7;
    state
}

#[test]
fn navigation_resets_offset_but_a_same_index_does_not() {
    let mut state = scrolled_state(&["one", "two"]);

    state.set_selected(state.selected());
    assert_eq!(state.hscroll_offset, 7);

    state.move_up();
    assert_eq!(state.hscroll_offset, 0);
}

#[test]
fn refresh_keeps_offset_while_the_selected_value_survives() {
    let mut state = scrolled_state(&["one", "two"]);
    assert_eq!(state.selected(), 3);

    // `two` is still pending, but the list shrank below the old index.
    state.rebuild_from_state(&pending_snapshot(&["two"]));
    assert_eq!(state.selected(), 2);
    assert_eq!(state.hscroll_offset, 7);
}

#[test]
fn refresh_resets_offset_when_the_selected_value_is_gone() {
    let mut state = scrolled_state(&["one", "two"]);

    state.rebuild_from_state(&pending_snapshot(&["one"]));
    assert_eq!(state.hscroll_offset, 0);
}
