use anyhow::{Result, anyhow};

use super::replace_helpers::counted_replace;

/// Unescapes common escape sequences before matching.
/// Handles cases where the LLM sends literal escape sequences (e.g., `\\n`)
/// instead of actual newlines.
pub struct EscapeNormalizedReplacer;

impl EscapeNormalizedReplacer {
    /// Unescape common escape sequences in a string.
    fn unescape(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.peek() {
                    Some('n') => {
                        result.push('\n');
                        chars.next();
                    }
                    Some('t') => {
                        result.push('\t');
                        chars.next();
                    }
                    Some('r') => {
                        result.push('\r');
                        chars.next();
                    }
                    Some('\\') => {
                        result.push('\\');
                        chars.next();
                    }
                    Some('"') => {
                        result.push('"');
                        chars.next();
                    }
                    Some('\'') => {
                        result.push('\'');
                        chars.next();
                    }
                    _ => {
                        result.push(c);
                    }
                }
            } else {
                result.push(c);
            }
        }
        result
    }

    pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
        // Try unescaping old_string and see if it matches content
        let old_unescaped = Self::unescape(old);

        if old_unescaped == old {
            // No escape sequences found — nothing different to try
            return Err(anyhow!(
                "EscapeNormalizedReplacer: old_string has no escape sequences to normalize"
            ));
        }

        // Replace using the unescaped old_string, but keep new_string as-is
        // (the user's new_string should already be in the desired format)
        counted_replace(
            content,
            &old_unescaped,
            new,
            replace_all,
            "EscapeNormalizedReplacer: no match found after unescaping old_string",
            |count| format!(
                "EscapeNormalizedReplacer: found {} matches after unescaping. Provide more context or use replace_all.",
                count
            ),
        )
    }
}
