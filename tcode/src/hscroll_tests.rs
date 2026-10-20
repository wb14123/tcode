use ratatui::buffer::CellWidth;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::hscroll::{PreparedRow, slice_line};

/// Concatenated text of a rendered line.
fn text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Width in terminal cells, as ratatui draws the line.
fn drawn_width(line: &Line<'_>) -> usize {
    line.styled_graphemes(Style::default())
        .map(|grapheme| usize::from(grapheme.symbol.cell_width()))
        .sum()
}

#[test]
fn slice_line_at_offset_zero_keeps_everything() {
    let line = Line::from(vec![Span::raw("abc"), Span::raw("def")]);
    let out = slice_line(&line, 0, 6);
    assert_eq!(text(&out), "abcdef");
    assert_eq!(out.width(), 6);

    let out = slice_line(&line, 0, 100);
    assert_eq!(text(&out), "abcdef");
}

#[test]
fn slice_line_from_mid_offset_returns_tail() {
    let line = Line::from(vec![Span::raw("abc"), Span::raw("def")]);
    let out = slice_line(&line, 2, 3);
    assert_eq!(text(&out), "cde");
}

#[test]
fn slice_line_offset_at_or_beyond_width_is_empty() {
    let line = Line::raw("abc");
    assert!(slice_line(&line, 3, 5).spans.is_empty());
    assert!(slice_line(&line, 99, 5).spans.is_empty());
    assert!(slice_line(&Line::default(), 0, 5).spans.is_empty());
}

#[test]
fn slice_line_zero_max_width_is_empty() {
    assert!(slice_line(&Line::raw("abc"), 0, 0).spans.is_empty());
}

#[test]
fn slice_line_blanks_wide_grapheme_at_left_edge() {
    let line = Line::raw("\u{4e16}\u{754c}");
    // Offsetting by one column lands inside the first (2-column) grapheme: its
    // remaining column stays blank so the next grapheme keeps its column.
    let out = slice_line(&line, 1, 4);
    assert_eq!(text(&out), " \u{754c}");
    assert_eq!(out.width(), 3);

    assert_eq!(text(&slice_line(&line, 1, 1)), " ");
}

#[test]
fn slice_line_drops_wide_grapheme_at_right_edge() {
    let line = Line::raw("a\u{4e16}b");
    let out = slice_line(&line, 0, 2);
    assert_eq!(text(&out), "a");
    assert!(out.width() <= 2);
}

#[test]
fn slice_line_preserves_styles_across_grapheme_boundaries() {
    let red = Style::new().fg(Color::Red);
    let blue = Style::new().fg(Color::Blue);
    let line = Line::from(vec![
        Span::styled("abcde", red),
        Span::styled("fghij", blue),
    ]);

    let out = slice_line(&line, 3, 4);
    assert_eq!(text(&out), "defg");
    assert_eq!(out.spans.len(), 2);
    assert_eq!(out.spans[0].style.fg, Some(Color::Red));
    assert_eq!(out.spans[1].style.fg, Some(Color::Blue));
}

#[test]
fn slice_line_keeps_combining_mark_at_boundary() {
    let line = Line::raw("abe\u{301}c");
    let out = slice_line(&line, 2, 2);
    assert_eq!(text(&out), "e\u{301}c");
    assert_eq!(out.width(), 2);
}

#[test]
fn slice_line_never_exceeds_max_width() {
    let samples = [
        "abcdefghij",
        "\u{4e16}\u{754c}\u{4e16}\u{754c}",
        "a\u{4e16}b\u{1f600}c",
        "e\u{301}e\u{301}e\u{301}",
    ];
    for sample in samples {
        let line = Line::raw(sample);
        for offset in 0..8 {
            for max_width in 0..8 {
                let out = slice_line(&line, offset, max_width);
                assert!(
                    out.width() <= max_width,
                    "{sample:?} offset {offset} max_width {max_width}: {:?}",
                    text(&out)
                );
            }
        }
    }
}

#[test]
fn prepared_row_renders_the_whole_row_at_offset_zero() {
    let row = PreparedRow::new(Line::raw("name"));

    let out = row.render(10, 0);
    assert_eq!(text(&out), "name");
    assert_eq!(out.width(), 4);
    assert_eq!(row.max_offset(10), 0);
}

#[test]
fn prepared_row_pans_the_whole_row() {
    let row = PreparedRow::new(Line::raw("abcdefghijklmnopqrstuvwxyz"));
    assert_eq!(row.max_offset(10), 26 - 10);

    let out = row.render(10, 0);
    assert_eq!(text(&out), "abcdefghij");
    assert_eq!(out.width(), 10);

    let out = row.render(10, 5);
    assert_eq!(text(&out), "fghijklmno");
    assert_eq!(out.width(), 10);
}

#[test]
fn prepared_row_at_max_offset_reaches_last_column() {
    let row = PreparedRow::new(Line::raw("abcdefghijklmnopqrstuvwxyz"));

    let out = row.render(10, row.max_offset(10));
    assert_eq!(text(&out), "qrstuvwxyz");
    assert_eq!(out.width(), 10);
}

#[test]
fn prepared_row_degenerate_widths_do_not_panic() {
    let row = PreparedRow::new(Line::raw("abcdef"));
    assert!(row.render(0, 0).spans.is_empty());
    assert!(row.render(0, 3).spans.is_empty());
    assert_eq!(row.max_offset(0), 6);

    let out = row.render(1, 0);
    assert_eq!(text(&out), "a");
    assert_eq!(out.width(), 1);

    // The last content column stays reachable at a single-column width.
    let out = row.render(1, row.max_offset(1));
    assert_eq!(text(&out), "f");
    assert_eq!(out.width(), 1);
}

#[test]
fn prepared_row_shows_the_window_at_narrow_widths() {
    let row = PreparedRow::new(Line::raw("abc"));
    assert_eq!(row.max_offset(2), 1);

    assert_eq!(text(&row.render(2, 0)), "ab");
    assert_eq!(text(&row.render(2, 1)), "bc");
}

#[test]
fn prepared_row_wide_text_never_exceeds_the_window() {
    let row = PreparedRow::new(Line::raw("\u{4e16}\u{754c}".repeat(6)));

    for width in [9usize, 10, 11] {
        for offset in 0..row.max_offset(width) {
            let drawn = drawn_width(&row.render(width, offset));
            assert!(drawn <= width, "width {width} offset {offset}");
            // A cluster cut at the right edge leaves at most one cell blank.
            assert!(drawn + 1 >= width, "width {width} offset {offset}");
        }
    }
}

#[test]
fn prepared_row_measures_ligature_text_like_the_drawer() {
    // Arabic lam-alef: `Line::width` folds the pair into one column as a
    // ligature, while the drawer gives each half a cell.
    let row = PreparedRow::new(Line::raw("\u{644}\u{627}".repeat(20)));
    assert_eq!(row.max_offset(30), 40 - 30);

    assert_eq!(drawn_width(&row.render(30, 0)), 30);
    assert_eq!(drawn_width(&row.render(30, row.max_offset(30))), 30);
}

#[test]
fn prepared_row_empty_content_renders_nothing() {
    let row = PreparedRow::new(Line::default());
    assert_eq!(row.max_offset(0), 0);
    assert_eq!(row.max_offset(5), 0);
    assert!(row.render(5, 0).spans.is_empty());
    assert!(row.render(5, 3).spans.is_empty());
}

#[test]
fn prepared_row_wide_content_never_exceeds_width() {
    let row = PreparedRow::new(Line::raw("a\u{4e16}\u{754c}b"));

    for width in 0..8 {
        for offset in 0..8 {
            let out = row.render(width, offset);
            assert!(
                out.width() <= width,
                "width {width} offset {offset}: {:?}",
                text(&out)
            );
        }
    }
}

#[test]
fn prepared_row_folds_control_characters_into_spaces() {
    // `Span` keeps newlines; `Line::raw` would split on them before folding.
    let row = PreparedRow::new(Line::from(Span::raw("a\nb\tc\rd")));

    let out = row.render(40, 0);
    let rendered = text(&out);
    assert_eq!(rendered, "a b c d");
    assert_eq!(out.width(), 7);
    assert!(!rendered.chars().any(char::is_control));
}
