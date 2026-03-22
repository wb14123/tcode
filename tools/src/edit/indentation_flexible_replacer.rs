use anyhow::{Result, anyhow};

use super::replace_helpers::{apply_line_matches, scan_line_blocks};

/// Strips the minimum common indentation from both old_string and content blocks,
/// then matches. This handles cases where the LLM provides correct relative indentation
/// but wrong absolute indentation.
pub struct IndentationFlexibleReplacer;

impl IndentationFlexibleReplacer {
    /// Calculate the minimum indentation (number of leading spaces/tabs) across all non-empty lines.
    fn min_indent(lines: &[&str]) -> usize {
        lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0)
    }

    /// Strip `n` leading characters from each line.
    fn strip_indent(lines: &[&str], n: usize) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                if l.len() >= n {
                    l[n..].to_string()
                } else {
                    l.trim_start().to_string()
                }
            })
            .collect()
    }

    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        let old_lines: Vec<&str> = old.lines().collect();
        if old_lines.is_empty() {
            return Err(anyhow!("old_string is empty"));
        }

        let content_lines: Vec<&str> = content.lines().collect();
        let old_min = Self::min_indent(&old_lines);
        let old_stripped = Self::strip_indent(&old_lines, old_min);
        let block_len = old_lines.len();

        let matches = scan_line_blocks(&content_lines, block_len, |i| {
            let block = &content_lines[i..i + block_len];
            let block_min = Self::min_indent(block);
            let block_stripped = Self::strip_indent(block, block_min);
            block_stripped == old_stripped
        });

        apply_line_matches(
            content,
            content_lines,
            &matches,
            new,
            replace_all,
            "IndentationFlexibleReplacer: no match found after stripping common indentation",
            |count| format!(
                "IndentationFlexibleReplacer: found {} matches. Provide more context or use replace_all.",
                count
            ),
        )
    }
}
