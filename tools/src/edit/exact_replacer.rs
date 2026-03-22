use anyhow::Result;

use super::replace_helpers::counted_replace;

/// Exact string replacement. Requires the old_string to appear exactly as-is in the content.
pub struct ExactReplacer;

impl ExactReplacer {
    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        counted_replace(
            content,
            old,
            new,
            replace_all,
            "old_string was not found in the file. Make sure it matches exactly, \
             including whitespace and indentation.",
            |count| format!(
                "old_string appears {} times in the file. Provide more surrounding context \
                 to make it unique, or set replace_all to true.",
                count
            ),
        )
    }
}
