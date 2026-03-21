use anyhow::{Result, anyhow};

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

        // Find all contiguous blocks where trimmed lines match.
        let mut matches: Vec<(usize, usize)> = Vec::new(); // (start_line_idx, end_line_idx exclusive)
        let mut i = 0;
        while i + old_lines.len() <= content_lines.len() {
            let all_match = old_lines
                .iter()
                .enumerate()
                .all(|(j, old_line)| content_lines[i + j].trim() == old_line.trim());
            if all_match {
                matches.push((i, i + old_lines.len()));
                i += old_lines.len(); // skip past this match
            } else {
                i += 1;
            }
        }

        if matches.is_empty() {
            return Err(anyhow!(
                "old_string was not found in the file (even after trimming whitespace). \
                 Make sure the content matches."
            ));
        }
        if !replace_all && matches.len() > 1 {
            return Err(anyhow!(
                "old_string appears {} times in the file (after trimming). Provide more \
                 surrounding context to make it unique, or set replace_all to true.",
                matches.len()
            ));
        }

        // Build the result by replacing matched blocks.
        // Process matches in reverse order so indices stay valid.
        let mut result_lines: Vec<&str> = content_lines.clone();
        let new_lines: Vec<&str> = new.lines().collect();

        let matches_to_apply = if replace_all {
            &matches[..]
        } else {
            &matches[..1]
        };

        for &(start, end) in matches_to_apply.iter().rev() {
            result_lines.splice(start..end, new_lines.iter().copied());
        }

        // Preserve trailing newline if original content had one.
        let mut result = result_lines.join("\n");
        if content.ends_with('\n') {
            result.push('\n');
        }

        Ok(result)
    }
}
