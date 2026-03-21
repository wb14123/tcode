#[cfg(test)]
mod edit_tests;
mod exact_replacer;
mod line_trimmed_replacer;

use std::path::Path;

use anyhow::{Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;
use uuid::Uuid;

use crate::file_permission::FILE_WRITE_SCOPE;
use exact_replacer::ExactReplacer;
use line_trimmed_replacer::LineTrimmedReplacer;

/// Performs exact string replacements in files.
///
/// Usage:
/// - You must use your `Read` tool at least once in the conversation before editing.
///   This tool will error if you attempt an edit without reading the file.
/// - When editing text from Read tool output, ensure you preserve the exact indentation
///   (tabs/spaces) as it appears in the line content (inside the `<line>` tags).
///   Never include XML tags or line number attributes in the old_string or new_string.
/// - ALWAYS prefer editing existing files in the codebase. NEVER write new files unless
///   explicitly required.
/// - Only use emojis if the user explicitly requests it.
/// - The edit will FAIL if `old_string` is not found in the file.
/// - The edit will FAIL if `old_string` is not unique in the file. Either provide
///   a larger string with more surrounding context to make it unique or use `replace_all`
///   to change every instance of `old_string`.
/// - Use `replace_all` for replacing and renaming strings across the file.
#[tool]
pub fn edit(
    ctx: ToolContext,
    /// The absolute path to the file to edit
    file_path: String,
    /// The exact string to find in the file
    old_string: String,
    /// The replacement string
    new_string: String,
    /// Replace all occurrences of old_string (default: false)
    #[serde(default)]
    replace_all: bool,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let path = Path::new(&file_path);

        // Validate absolute path
        if !path.is_absolute() {
            yield Err(anyhow!("file_path must be an absolute path, got: {}", file_path));
            return;
        }

        // Validate file exists
        if !path.exists() {
            yield Err(anyhow!("File does not exist: {}", file_path));
            return;
        }

        // Read current content
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => {
                yield Err(anyhow!("Failed to read {}: {}", file_path, e));
                return;
            }
        };

        // Record mtime before permission check
        let pre_mtime = crate::file_write_util::record_mtime(path);

        // Validate old_string != new_string
        if old_string == new_string {
            yield Err(anyhow!("old_string and new_string are identical. No changes needed."));
            return;
        }

        // Try ExactReplacer first, fall back to LineTrimmedReplacer
        let new_content = match ExactReplacer::replace(&content, &old_string, &new_string, replace_all) {
            Ok(result) => result,
            Err(_simple_err) => {
                match LineTrimmedReplacer::replace(&content, &old_string, &new_string, replace_all) {
                    Ok(result) => result,
                    Err(_trimmed_err) => {
                        // Report the original (simple) error since it's more intuitive
                        yield Err(_simple_err);
                        return;
                    }
                }
            }
        };

        // Permission check (inline, using FILE_WRITE_SCOPE)
        let canonical_path = match tokio::fs::canonicalize(path).await {
            Ok(p) => p,
            Err(e) => {
                yield Err(anyhow!("Failed to resolve path {}: {}", file_path, e));
                return;
            }
        };
        let permission_dir = canonical_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| canonical_path.clone());

        let permission_dir_str = permission_dir.to_string_lossy().to_string();

        // Check ancestor permissions
        let already_permitted = {
            let mut ancestor: Option<&Path> = Some(&permission_dir);
            let mut found = false;
            while let Some(dir) = ancestor {
                let dir_str = dir.to_string_lossy();
                if ctx.permission.has_permission_for(FILE_WRITE_SCOPE, "path", &dir_str) {
                    found = true;
                    break;
                }
                ancestor = dir.parent();
            }
            found
        };

        if already_permitted {
            // Skip permission prompt
        } else {
            // Write new_content to a temp file for diff preview
            let session_dir = ctx.permission.session_dir();
            let preview_dir_and_tmp = session_dir.map(|sd| {
                let preview_dir = sd.join("tool-file-preview");
                if let Err(e) = std::fs::create_dir_all(&preview_dir) {
                    tracing::warn!("Failed to create preview dir: {}", e);
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("txt");
                let tmp_filename = format!("tcode-edit-{}.{}", Uuid::new_v4(), ext);
                let tmp_path = preview_dir.join(tmp_filename);
                if let Err(e) = std::fs::write(&tmp_path, &new_content) {
                    tracing::warn!("Failed to write temp file: {}", e);
                }
                (preview_dir, tmp_path)
            });

            let tcodediff_content = preview_dir_and_tmp.as_ref().map(|(preview_dir, tmp_path)| {
                let diff_content = format!("{}\n{}", file_path, tmp_path.display());
                let diff_filename = format!("tcode-edit-{}.tcodediff", Uuid::new_v4());
                let diff_path = preview_dir.join(diff_filename);
                if let Err(e) = std::fs::write(&diff_path, &diff_content) {
                    tracing::warn!("Failed to write tcodediff file: {}", e);
                }
                (diff_path, diff_content)
            });

            let prompt = format!("Allow editing file {}?", file_path);

            let result = if let Some((ref diff_path, _)) = tcodediff_content {
                // Use ask_permission_with_preview with the tcodediff file
                let diff_content_str = std::fs::read_to_string(diff_path).unwrap_or_default();
                ctx.permission.ask_permission_with_preview(
                    FILE_WRITE_SCOPE,
                    &prompt,
                    "path",
                    &permission_dir_str,
                    &diff_content_str,
                    "tcodediff",
                ).await
            } else {
                ctx.permission.ask_permission_for(
                    FILE_WRITE_SCOPE,
                    &prompt,
                    "path",
                    &permission_dir_str,
                ).await
            };

            // Clean up the new_content temp file (tcodediff file is cleaned by permission manager)
            if let Some((_, ref tmp_path)) = preview_dir_and_tmp
                && let Err(e) = std::fs::remove_file(tmp_path)
            {
                tracing::warn!("Failed to clean up edit temp file: {}", e);
            }

            if let Err(e) = result {
                yield Err(e);
                return;
            }
        }

        // Write the new content
        match crate::file_write_util::locked_write(path, new_content.as_bytes(), pre_mtime) {
            Ok(_) => {
                let replacements = if replace_all {
                    let count = content.matches(&old_string).count();
                    format!("{} replacement(s)", count)
                } else {
                    "1 replacement".to_string()
                };
                yield Ok(format!(
                    "Successfully edited {} ({})",
                    file_path, replacements
                ));
            }
            Err(e) => {
                yield Err(e);
            }
        }
    }
}
