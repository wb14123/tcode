use anyhow::{Result, anyhow};

use super::replace_helpers::counted_replace;

/// Trims leading and trailing whitespace/blank lines from the find string,
/// then looks for a match in the content. This handles cases where the LLM
/// includes extra blank lines or whitespace at the boundaries of old_string.
pub struct TrimmedBoundaryReplacer;

impl TrimmedBoundaryReplacer {
    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        let trimmed_old = old.trim();
        if trimmed_old.is_empty() {
            return Err(anyhow!(
                "TrimmedBoundaryReplacer: old_string is empty after trimming"
            ));
        }

        // If trimming didn't change anything, this replacer has nothing new to try
        if trimmed_old == old {
            return Err(anyhow!(
                "TrimmedBoundaryReplacer: old_string has no leading/trailing whitespace to trim"
            ));
        }

        counted_replace(
            content,
            trimmed_old,
            new,
            replace_all,
            "TrimmedBoundaryReplacer: no match found after trimming boundaries",
            |count| format!(
                "TrimmedBoundaryReplacer: found {} matches after trimming. Provide more context or use replace_all.",
                count
            ),
        )
    }
}
