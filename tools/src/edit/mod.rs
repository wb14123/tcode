#[cfg(test)]
mod edit_tests;
mod exact_replacer;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;
use uuid::Uuid;

use exact_replacer::ExactReplacer;

/// Build a tcodediff preview for the permission prompt.
///
/// Writes `new_content` to a temp file and creates a `.tcodediff` file that
/// references the original file and the temp file. Returns
/// `(tcodediff_content, tmp_path)` where `tmp_path` is the temp file that
/// should be cleaned up after the permission check.
fn build_tcodediff_preview(
    session_dir: &Path,
    file_path: &str,
    path: &Path,
    new_content: &str,
) -> Result<(String, PathBuf)> {
    let preview_dir = session_dir.join("tool-file-preview");
    std::fs::create_dir_all(&preview_dir).context("Failed to create preview dir")?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("txt");
    let tmp_filename = format!("tcode-edit-{}.{}", Uuid::new_v4(), ext);
    let tmp_path = preview_dir.join(tmp_filename);
    std::fs::write(&tmp_path, new_content).context("Failed to write edit temp file")?;

    let diff_content = format!("{}\n{}", file_path, tmp_path.display());
    let diff_filename = format!("tcode-edit-{}.tcodediff", Uuid::new_v4());
    let diff_path = preview_dir.join(diff_filename);
    std::fs::write(&diff_path, &diff_content).context("Failed to write tcodediff file")?;

    Ok((diff_content, tmp_path))
}

/// Performs exact string replacements in files.
///
/// Usage:
/// - You must use your `Read` tool at least once in the conversation before editing.
///   This tool will error if you attempt an edit without reading the file.
/// - You do NOT need to re-read a file before each edit if you already have the relevant
///   section in context. One read of the relevant section per file is sufficient.
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

        let new_content = match ExactReplacer::replace(&content, &old_string, &new_string, replace_all) {
            Ok(result) => result,
            Err(e) => {
                yield Err(e);
                return;
            }
        };

        // Build tcodediff preview and check write permission
        let preview_and_tmp = ctx.permission.session_dir()
            .map(|sd| build_tcodediff_preview(sd, &file_path, path, &new_content))
            .transpose();
        let preview_and_tmp = match preview_and_tmp {
            Ok(v) => v,
            Err(e) => {
                yield Err(e);
                return;
            }
        };

        let (preview_content, preview_type) = match preview_and_tmp {
            Some((ref content, _)) => (content.as_str(), "tcodediff"),
            None => (new_content.as_str(), "txt"),
        };

        let result = crate::file_permission::check_file_write_permission(
            &ctx.permission, path, preview_content, preview_type,
        ).await;

        // Clean up the temp file (best-effort; tcodediff file is cleaned by permission manager)
        if let Some((_, ref tmp_path)) = preview_and_tmp
            && let Err(e) = std::fs::remove_file(tmp_path)
        {
            tracing::warn!("Failed to clean up edit temp file: {}", e);
        }

        if let Err(e) = result {
            yield Err(e);
            return;
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
