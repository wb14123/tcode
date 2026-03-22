use anyhow::{Result, anyhow};

/// Shared helper for string-based replacers (ExactReplacer, EscapeNormalizedReplacer, TrimmedBoundaryReplacer).
/// Counts occurrences of `needle` in `content`, validates uniqueness, and performs the replacement.
///
/// `not_found_msg` is used as-is when count is 0.
/// `multiple_msg_fn` is called with the count when `!replace_all && count > 1`.
pub fn counted_replace(
    content: &str,
    needle: &str,
    new: &str,
    replace_all: bool,
    not_found_msg: &str,
    multiple_msg_fn: impl FnOnce(usize) -> String,
) -> Result<String> {
    let count = content.matches(needle).count();
    if count == 0 {
        return Err(anyhow!("{}", not_found_msg));
    }
    if !replace_all && count > 1 {
        return Err(anyhow!("{}", multiple_msg_fn(count)));
    }
    if replace_all {
        Ok(content.replace(needle, new))
    } else {
        Ok(content.replacen(needle, new, 1))
    }
}

/// Shared helper for line-based replacers that find block matches by (start, end) line indices,
/// then splice in replacement lines.
/// Validates match count and applies replacements in reverse order to preserve indices.
///
/// `not_found_msg` is used as-is when no matches.
/// `multiple_msg_fn` is called with the match count when `!replace_all && count > 1`.
pub fn apply_line_matches(
    content: &str,
    content_lines: Vec<&str>,
    matches: &[(usize, usize)],
    new: &str,
    replace_all: bool,
    not_found_msg: &str,
    multiple_msg_fn: impl FnOnce(usize) -> String,
) -> Result<String> {
    if matches.is_empty() {
        return Err(anyhow!("{}", not_found_msg));
    }
    if !replace_all && matches.len() > 1 {
        return Err(anyhow!("{}", multiple_msg_fn(matches.len())));
    }

    let new_lines: Vec<&str> = new.lines().collect();
    let mut result_lines = content_lines;

    let matches_to_apply = if replace_all {
        &matches[..]
    } else {
        &matches[..1]
    };

    for &(start, end) in matches_to_apply.iter().rev() {
        result_lines.splice(start..end, new_lines.iter().copied());
    }

    let mut result = result_lines.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }

    Ok(result)
}

/// Shared sliding-window block scanner. Scans `content_lines` for contiguous blocks
/// of `block_len` lines where `is_match` returns true for the block starting at that index.
/// Returns a list of (start, end_exclusive) line index pairs.
pub fn scan_line_blocks(
    content_lines: &[&str],
    block_len: usize,
    mut is_match: impl FnMut(usize) -> bool,
) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let mut i = 0;
    while i + block_len <= content_lines.len() {
        if is_match(i) {
            matches.push((i, i + block_len));
            i += block_len;
        } else {
            i += 1;
        }
    }
    matches
}
