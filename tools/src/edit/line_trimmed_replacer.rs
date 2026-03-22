use anyhow::{Result, anyhow};

use super::replace_helpers::{apply_line_matches, scan_line_blocks};

/// Fallback replacer that trims each line before comparing, to handle
/// indentation mismatches between the LLM's old_string and the actual file.
pub struct LineTrimmedReplacer;

impl LineTrimmedReplacer {
    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        let old_lines: Vec<&str> = old.lines().collect();
        if old_lines.is_empty() {
            return Err(anyhow!("old_string is empty"));
        }

        let content_lines: Vec<&str> = content.lines().collect();
        let block_len = old_lines.len();

        let matches = scan_line_blocks(&content_lines, block_len, |i| {
            old_lines
                .iter()
                .enumerate()
                .all(|(j, old_line)| content_lines[i + j].trim() == old_line.trim())
        });

        apply_line_matches(
            content,
            content_lines,
            &matches,
            new,
            replace_all,
            "old_string was not found in the file (even after trimming whitespace). \
             Make sure the content matches.",
            |count| format!(
                "old_string appears {} times in the file (after trimming). Provide more \
                 surrounding context to make it unique, or set replace_all to true.",
                count
            ),
        )
    }
}
