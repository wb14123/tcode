use anyhow::{Result, anyhow};

use super::levenshtein::similarity;
use super::replace_helpers::{apply_line_matches, scan_line_blocks};

/// The most relaxed fuzzy matcher. Finds a contiguous block in content
/// where at least 50% of lines have >= 80% Levenshtein similarity
/// to corresponding lines in old_string.
pub struct ContextAwareReplacer;

const LINE_SIMILARITY_THRESHOLD: f64 = 0.8;
const MIN_LINE_MATCH_RATIO: f64 = 0.5;

impl ContextAwareReplacer {
    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        let old_lines: Vec<&str> = old.lines().collect();
        if old_lines.is_empty() {
            return Err(anyhow!("ContextAwareReplacer: old_string is empty"));
        }

        let content_lines: Vec<&str> = content.lines().collect();
        let block_len = old_lines.len();
        let min_matching = ((block_len as f64) * MIN_LINE_MATCH_RATIO).ceil() as usize;

        let matches = scan_line_blocks(&content_lines, block_len, |i| {
            let mut matching_lines = 0;
            for j in 0..block_len {
                let sim = similarity(old_lines[j].trim(), content_lines[i + j].trim());
                if sim >= LINE_SIMILARITY_THRESHOLD {
                    matching_lines += 1;
                }
            }
            matching_lines >= min_matching
        });

        apply_line_matches(
            content,
            content_lines,
            &matches,
            new,
            replace_all,
            "ContextAwareReplacer: no block found with >= 50% line similarity",
            |count| format!(
                "ContextAwareReplacer: found {} matching blocks. Provide more context or use replace_all.",
                count
            ),
        )
    }
}
