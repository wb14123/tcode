#[cfg(test)]
mod write_tests;

use std::path::Path;

use anyhow::{Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

/// Writes (overwrites) a file. For existing files, you MUST Read first — tool will fail otherwise.
/// ALWAYS prefer editing over writing. NEVER create new files unless explicitly required.
/// NEVER proactively create docs (*.md) or READMEs. Only use emojis if requested.
#[tool]
pub fn write(
    ctx: ToolContext,
    /// The absolute path to the file to write
    file_path: String,
    /// The content to write to the file. Always provide the FULL content — no truncation or omissions, including unchanged parts.
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
        let file_extension = path.extension().and_then(|e| e.to_str()).unwrap_or("txt");
        if let Err(e) = crate::file_permission::check_file_write_permission(
            &ctx.permission, path, &content, file_extension,
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
