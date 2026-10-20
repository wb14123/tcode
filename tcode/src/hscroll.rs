//! A horizontally pannable row, used by the permission window.

use ratatui::buffer::CellWidth;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

pub(crate) struct PreparedRow {
    line: Line<'static>,
}

impl PreparedRow {
    pub(crate) fn new(line: Line<'static>) -> Self {
        Self {
            line: single_line(line),
        }
    }

    /// Renders the window of `width` columns starting at `offset`.
    pub(crate) fn render(&self, width: usize, offset: usize) -> Line<'static> {
        slice_line(&self.line, offset, width)
    }

    /// Widest offset that still shows content at `width`.
    pub(crate) fn max_offset(&self, width: usize) -> usize {
        line_width(&self.line).saturating_sub(width)
    }
}

/// Width in terminal cells, matching how ratatui draws the text.
fn line_width(line: &Line<'_>) -> usize {
    line.styled_graphemes(Style::default())
        .map(|grapheme| usize::from(grapheme.symbol.cell_width()))
        .sum()
}

/// Slices `line` to at most `max_width` columns starting at display column
/// `offset`.
///
/// Grapheme clusters are never split: a cluster cut by the left edge leaves its
/// visible cells blank, and one cut by the right edge is dropped. Zero-width
/// graphemes are kept only when they continue a run of kept graphemes.
pub(crate) fn slice_line(line: &Line<'_>, offset: usize, max_width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut start = 0;
    let mut used = 0;
    let mut in_run = false;

    for grapheme in line.styled_graphemes(Style::default()) {
        let width = usize::from(grapheme.symbol.cell_width());
        if width == 0 {
            if in_run {
                push(&mut spans, grapheme.symbol, grapheme.style);
            }
            continue;
        }

        let grapheme_start = start;
        start += width;
        if grapheme_start < offset {
            let blank = (grapheme_start + width).saturating_sub(offset);
            if blank > 0 {
                if used + blank > max_width {
                    break;
                }
                push(&mut spans, &" ".repeat(blank), grapheme.style);
                used += blank;
            }
            in_run = false;
            continue;
        }
        if used + width > max_width {
            break;
        }
        push(&mut spans, grapheme.symbol, grapheme.style);
        used += width;
        in_run = true;
    }

    Line::from(spans)
}

fn push(spans: &mut Vec<Span<'static>>, symbol: &str, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(symbol),
        _ => spans.push(Span::styled(symbol.to_string(), style)),
    }
}

/// Control characters draw as nothing, so text holding `\n` or `\t` would join
/// the surrounding words; fold them onto a space instead.
fn single_line(mut line: Line<'static>) -> Line<'static> {
    for span in &mut line.spans {
        if span.content.contains(char::is_control) {
            span.content = span
                .content
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
                .into();
        }
    }
    line
}
