use anyhow::{Result, anyhow};

/// Exact string replacement. Requires the old_string to appear exactly as-is in the content.
pub struct ExactReplacer;

impl ExactReplacer {
    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        let count = content.matches(old).count();
        if count == 0 {
            return Err(anyhow!(
                "old_string was not found in the file. Make sure it matches exactly, \
                 including whitespace and indentation."
            ));
        }
        if !replace_all && count > 1 {
            return Err(anyhow!(
                "old_string appears {} times in the file. Provide more surrounding context \
                 to make it unique, or set replace_all to true.",
                count
            ));
        }
        if replace_all {
            Ok(content.replace(old, new))
        } else {
            Ok(content.replacen(old, new, 1))
        }
    }
}
