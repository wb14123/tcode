use anyhow::{Result, anyhow};

/// Replaces ALL exact occurrences of old_string in the content.
/// This is specifically designed for the `replace_all` flag — it only
/// activates when replace_all is true, and always replaces every occurrence.
pub struct MultiOccurrenceReplacer;

impl MultiOccurrenceReplacer {
    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        if !replace_all {
            return Err(anyhow!(
                "MultiOccurrenceReplacer: only applicable when replace_all is true"
            ));
        }

        let count = content.matches(old).count();
        if count == 0 {
            return Err(anyhow!(
                "MultiOccurrenceReplacer: old_string was not found in the file"
            ));
        }

        Ok(content.replace(old, new))
    }
}
