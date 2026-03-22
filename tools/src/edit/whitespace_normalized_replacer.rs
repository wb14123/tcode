use anyhow::{Result, anyhow};

use super::replace_helpers::{apply_line_matches, scan_line_blocks};

/// Collapses all whitespace (runs of spaces, tabs, newlines) into single spaces,
/// then matches. This handles cases where the LLM produces different whitespace
/// formatting than what's in the file.
pub struct WhitespaceNormalizedReplacer;

impl WhitespaceNormalizedReplacer {
    /// Collapse all whitespace runs into single spaces and trim.
    fn normalize(s: &str) -> String {
        s.split_whitespace().collect::<Vec<&str>>().join(" ")
    }

    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        let old_normalized = Self::normalize(old);
        if old_normalized.is_empty() {
            return Err(anyhow!(
                "old_string is empty after whitespace normalization"
            ));
        }

        let content_lines: Vec<&str> = content.lines().collect();
        let old_line_count = old.lines().count();
        if old_line_count == 0 {
            return Err(anyhow!("old_string has no lines"));
        }

        let matches = scan_line_blocks(&content_lines, old_line_count, |i| {
            let block = content_lines[i..i + old_line_count].join("\n");
            Self::normalize(&block) == old_normalized
        });

        apply_line_matches(
            content,
            content_lines,
            &matches,
            new,
            replace_all,
            "WhitespaceNormalizedReplacer: no match found after normalizing whitespace",
            |count| format!(
                "WhitespaceNormalizedReplacer: found {} matches. Provide more context or use replace_all.",
                count
            ),
        )
    }
}
