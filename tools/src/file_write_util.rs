use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Result, anyhow};
use fs2::FileExt;

/// Record the modification time of a file before an approval flow.
/// Returns `None` if the file doesn't exist yet (new file).
pub fn record_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Write `content` to `path` with exclusive advisory lock and mtime verification.
///
/// `pre_mtime` is the mtime recorded before the approval flow (from `record_mtime`).
/// If the file was modified between the recording and the write, returns an error.
///
/// For new files (`pre_mtime` is `None`), verifies the file still doesn't exist
/// before writing.
///
/// Returns `true` if the file existed before this write, `false` if it was newly created.
pub fn locked_write(path: &Path, content: &[u8], pre_mtime: Option<SystemTime>) -> Result<bool> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .open(path)
        .map_err(|e| anyhow!("Failed to open {} for writing: {}", path.display(), e))?;

    file.lock_exclusive()
        .map_err(|e| anyhow!("Failed to lock {}: {}", path.display(), e))?;

    // Check existence and mtime after acquiring the lock to avoid TOCTOU races.
    let metadata = file
        .metadata()
        .map_err(|e| anyhow!("Failed to read metadata for {}: {}", path.display(), e))?;
    let current_mtime = metadata.modified().ok();
    let existed = metadata.len() > 0 || pre_mtime.is_some();

    if pre_mtime.is_none() && current_mtime.is_some() && metadata.len() > 0 {
        return Err(anyhow!(
            "File {} was created while waiting for approval. Aborting write to prevent data loss.",
            path.display()
        ));
    }

    // Verify mtime hasn't changed since user saw the preview
    if pre_mtime.is_some() && pre_mtime != current_mtime {
        return Err(anyhow!(
            "File {} was modified while waiting for approval. Aborting write to prevent data loss.",
            path.display()
        ));
    }

    // Truncate and write
    file.set_len(0)
        .map_err(|e| anyhow!("Failed to truncate {}: {}", path.display(), e))?;
    (&file)
        .write_all(content)
        .map_err(|e| anyhow!("Failed to write {}: {}", path.display(), e))?;

    // Lock released on drop
    Ok(existed)
}
