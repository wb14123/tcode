use anyhow::{Result, anyhow};

/// String replacement against file content.
pub struct Replacer;

impl Replacer {
    /// Exact string replacement. Requires the old_string to appear exactly as-is in the content.
    pub fn replace_exact(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
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

    /// Replace `old` with `new`, tolerating a line-ending mismatch between the search
    /// text and the content.
    ///
    /// The search text is looked for as given first, so a search text that matches
    /// exactly, including one that already carries Windows line endings, is replaced
    /// as supplied. When the search text is not present as given, the search is
    /// retried with Windows line endings, so a search text written with `\n` finds
    /// its text in a file that uses `\r\n`; the replacement text is converted the
    /// same way, so the lines an edit inserts carry the endings the file uses. Text
    /// outside the replaced region is copied byte for byte.
    ///
    /// A search text that matches more than once without `replace_all` is reported
    /// as ambiguous on either attempt, never retried, so an ambiguous search can
    /// never silently edit the wrong occurrence.
    ///
    /// Returns the resulting content together with the number of occurrences the
    /// successful search text matched.
    pub fn replace(
        content: &str,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<(String, usize)> {
        let exact_count = content.matches(old).count();
        if exact_count > 0 {
            let updated = Self::replace_exact(content, old, new, replace_all)?;
            return Ok((updated, exact_count));
        }

        let windows_old = to_windows_line_endings(old);
        let windows_new = to_windows_line_endings(new);
        let windows_count = content.matches(windows_old.as_str()).count();
        let updated = Self::replace_exact(content, &windows_old, &windows_new, replace_all)?;
        Ok((updated, windows_count))
    }
}

/// Turn every `\n` that is not already preceded by `\r` into `\r\n`.
///
/// Idempotent: a text that already uses `\r\n` is returned unchanged, so a text is
/// never rewritten into `\r\r\n`. A `\r` that is not part of a line ending is left
/// as it is.
fn to_windows_line_endings(text: &str) -> String {
    let mut converted = String::with_capacity(text.len());
    let mut previous = None;
    for ch in text.chars() {
        if ch == '\n' && previous != Some('\r') {
            converted.push('\r');
        }
        converted.push(ch);
        previous = Some(ch);
    }
    converted
}
