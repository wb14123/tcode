use anyhow::{Result, anyhow};

use super::levenshtein::similarity;
use super::replace_helpers::{apply_line_matches, scan_line_blocks};

/// Matches blocks by anchoring on the first and last lines (exact trimmed match),
/// then verifying middle lines have >= 80% Levenshtein similarity.
pub struct BlockAnchorReplacer;

const MIDDLE_LINE_SIMILARITY_THRESHOLD: f64 = 0.8;

impl BlockAnchorReplacer {
    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        let old_lines: Vec<&str> = old.lines().collect();
        if old_lines.len() < 2 {
            return Err(anyhow!(
                "BlockAnchorReplacer requires at least 2 lines in old_string"
            ));
        }

        let content_lines: Vec<&str> = content.lines().collect();
        let first_trimmed = old_lines[0].trim();
        let last_trimmed = old_lines[old_lines.len() - 1].trim();

        if first_trimmed.is_empty() || last_trimmed.is_empty() {
            return Err(anyhow!(
                "BlockAnchorReplacer requires non-empty first and last anchor lines"
            ));
        }

        let block_len = old_lines.len();

        let matches = scan_line_blocks(&content_lines, block_len, |i| {
            // Check first and last line anchors (exact trimmed match)
            if content_lines[i].trim() != first_trimmed
                || content_lines[i + block_len - 1].trim() != last_trimmed
            {
                return false;
            }
            // Check middle lines with Levenshtein similarity
            if block_len <= 2 {
                return true; // no middle lines
            }
            old_lines[1..block_len - 1]
                .iter()
                .enumerate()
                .all(|(j, old_line)| {
                    let content_line = content_lines[i + 1 + j];
                    similarity(old_line.trim(), content_line.trim())
                        >= MIDDLE_LINE_SIMILARITY_THRESHOLD
                })
        });

        apply_line_matches(
            content,
            content_lines,
            &matches,
            new,
            replace_all,
            "BlockAnchorReplacer: no matching block found with first/last line anchors",
            |count| format!(
                "BlockAnchorReplacer: found {} matching blocks. Provide more context or use replace_all.",
                count
            ),
        )
    }
}
