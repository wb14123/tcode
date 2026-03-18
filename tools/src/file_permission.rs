use std::path::Path;

use anyhow::{anyhow, Result};
use llm_rs::permission::ScopedPermissionManager;

/// Shared permission scope used by both the `read` and `glob` tools.
const FILE_READ_SCOPE: &str = "file_read";

/// Check whether the caller has permission to read `path`.
///
/// 1. Canonicalizes both `path` and `cwd` to neutralize `..`/symlink traversal.
/// 2. If the canonical path is inside the canonical cwd, no permission is needed.
/// 3. Otherwise, walks ancestors from the permission directory up to `/`,
///    checking for a previously-granted `file_read` permission.
/// 4. If no ancestor is approved, prompts the user for the permission directory.
pub async fn check_file_read_permission(
    permission: &ScopedPermissionManager,
    path: &Path,
    is_dir: bool,
) -> Result<()> {
    let canonical_path = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| anyhow!("Failed to resolve path {}: {}", path.display(), e))?;

    let cwd = std::env::current_dir()
        .map_err(|e| anyhow!("Failed to get current directory: {}", e))?;

    let canonical_cwd = tokio::fs::canonicalize(&cwd)
        .await
        .map_err(|e| anyhow!("Failed to resolve cwd {}: {}", cwd.display(), e))?;

    // Inside cwd — no permission required
    if canonical_path.starts_with(&canonical_cwd) {
        return Ok(());
    }

    // Determine the directory to check permission for
    let permission_dir = if is_dir {
        canonical_path.clone()
    } else {
        canonical_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| canonical_path.clone())
    };

    // Hierarchical walk: check ancestors from permission_dir up to root
    let mut ancestor = Some(permission_dir.as_path());
    while let Some(dir) = ancestor {
        let dir_str = dir.to_string_lossy();
        if permission.has_permission_for(FILE_READ_SCOPE, "path", &dir_str) {
            return Ok(());
        }
        ancestor = dir.parent();
    }

    // No ancestor approved — ask for the permission directory
    let permission_dir_str = permission_dir.to_string_lossy().to_string();
    if permission
        .ask_permission_for(
            FILE_READ_SCOPE,
            &format!("Allow read access to {}?", path.display()),
            "path",
            &permission_dir_str,
        )
        .await
    {
        Ok(())
    } else {
        Err(anyhow!("User denied read permission for {}", path.display()))
    }
}
