use std::path::Path;

use anyhow::{Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

/// Writes a file to the local filesystem.
///
/// Usage:
/// - This tool will overwrite the existing file if there is one at the provided path.
/// - If this is an existing file, you MUST use the Read tool first to read the file's contents.
///   This tool will fail if you did not read the file first.
/// - ALWAYS prefer editing existing files in the codebase. NEVER write new files unless
///   explicitly required.
/// - NEVER proactively create documentation files (*.md) or README files. Only create
///   documentation files if explicitly requested by the User.
/// - Only use emojis if the user explicitly requests it.
#[tool]
pub fn write(
    ctx: ToolContext,
    /// The absolute path to the file to write
    file_path: String,
    /// The content to write to the file. Always provide the full intended content
    /// of the file without any truncation or omissions. You MUST include ALL parts
    /// of the file, even those that haven't changed.
    content: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let path = Path::new(&file_path);

        // Validate absolute path
        if !path.is_absolute() {
            yield Err(anyhow!("file_path must be an absolute path, got: {}", file_path));
            return;
        }

        // Check parent directory exists
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            yield Err(anyhow!(
                "Parent directory does not exist: {}. Create it first.",
                parent.display()
            ));
            return;
        }

        // Record mtime before permission check (which may block on user approval)
        let pre_mtime = crate::file_write_util::record_mtime(path);

        // Permission check (may block on user approval with preview)
        if let Err(e) = crate::file_permission::check_file_write_permission(
            &ctx.permission, path, &content,
        ).await {
            yield Err(e);
            return;
        }

        // Write with lock and mtime verification
        match crate::file_write_util::locked_write(path, content.as_bytes(), pre_mtime) {
            Ok(existed) => {
                let line_count = content.lines().count();
                let byte_count = content.len();
                let status = if existed { "overwrote existing" } else { "created new" };
                yield Ok(format!(
                    "Successfully {} file {} ({} bytes, {} lines)",
                    status, file_path, byte_count, line_count
                ));
            }
            Err(e) => {
                yield Err(e);
            }
        }
    }
}
