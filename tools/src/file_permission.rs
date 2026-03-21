use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use llm_rs::permission::ScopedPermissionManager;

/// Shared permission scope used by both the `read` and `glob` tools.
const FILE_READ_SCOPE: &str = "file_read";

/// Shared permission scope for file write operations.
const FILE_WRITE_SCOPE: &str = "file_write";

/// Determine the directory to use for permission checks.
///
/// For directories, uses the path itself. For files, uses the parent directory.
fn permission_dir_for(canonical_path: &Path, is_dir: bool) -> PathBuf {
    if is_dir {
        canonical_path.to_path_buf()
    } else {
        canonical_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| canonical_path.to_path_buf())
    }
}

/// Walk ancestors from `permission_dir` up to root, checking for a stored permission.
fn has_ancestor_permission(
    permission: &ScopedPermissionManager,
    scope: &str,
    permission_dir: &Path,
) -> bool {
    let mut ancestor: Option<&Path> = Some(permission_dir);
    while let Some(dir) = ancestor {
        let dir_str = dir.to_string_lossy();
        if permission.has_permission_for(scope, "path", &dir_str) {
            return true;
        }
        ancestor = dir.parent();
    }
    false
}

/// Canonicalize a path, handling non-existent files by canonicalizing the parent
/// directory and appending the filename. Returns `(canonical_path, exists)`.
async fn canonicalize_path(path: &Path) -> Result<(PathBuf, bool)> {
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let canonical = tokio::fs::canonicalize(path)
            .await
            .map_err(|e| anyhow!("Failed to resolve path {}: {}", path.display(), e))?;
        Ok((canonical, true))
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("No parent directory for {}", path.display()))?;
        let filename = path
            .file_name()
            .ok_or_else(|| anyhow!("No filename in path {}", path.display()))?;
        let canonical_parent = tokio::fs::canonicalize(parent).await.map_err(|e| {
            anyhow!(
                "Failed to resolve parent directory {}: {}",
                parent.display(),
                e
            )
        })?;
        Ok((canonical_parent.join(filename), false))
    }
}

/// Check whether the caller has permission to read `path`.
///
/// Paths inside cwd are auto-allowed. For paths outside cwd, checks for a
/// stored `file_read` permission or prompts the user.
pub async fn check_file_read_permission(
    permission: &ScopedPermissionManager,
    path: &Path,
    is_dir: bool,
) -> Result<()> {
    let (canonical_path, exists) = canonicalize_path(path).await?;
    if !exists {
        return Err(anyhow!("Path does not exist: {}", path.display()));
    }

    // Inside cwd — no permission required
    let cwd =
        std::env::current_dir().map_err(|e| anyhow!("Failed to get current directory: {}", e))?;
    let canonical_cwd = tokio::fs::canonicalize(&cwd)
        .await
        .map_err(|e| anyhow!("Failed to resolve cwd {}: {}", cwd.display(), e))?;
    if canonical_path.starts_with(&canonical_cwd) {
        return Ok(());
    }

    let permission_dir = permission_dir_for(&canonical_path, is_dir);
    if has_ancestor_permission(permission, FILE_READ_SCOPE, &permission_dir) {
        return Ok(());
    }

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
        Err(anyhow!(
            "User denied read permission for {}",
            path.display()
        ))
    }
}

/// Check whether the caller has permission to write `path`.
///
/// Writes always require explicit permission — even for paths inside cwd.
/// Prompts with a content preview so the user can inspect the change.
pub async fn check_file_write_permission(
    permission: &ScopedPermissionManager,
    path: &Path,
    content: &str,
) -> Result<()> {
    let (canonical_path, exists) = canonicalize_path(path).await?;

    let permission_dir = permission_dir_for(&canonical_path, false);
    if has_ancestor_permission(permission, FILE_WRITE_SCOPE, &permission_dir) {
        return Ok(());
    }

    let permission_dir_str = permission_dir.to_string_lossy().to_string();
    let prompt = if exists {
        format!("Allow write to existing file {}?", path.display())
    } else {
        format!("Allow creating new file {}?", path.display())
    };
    let file_extension = path.extension().and_then(|e| e.to_str()).unwrap_or("txt");

    if permission
        .ask_permission_with_preview(
            FILE_WRITE_SCOPE,
            &prompt,
            "path",
            &permission_dir_str,
            content,
            file_extension,
        )
        .await
    {
        Ok(())
    } else {
        Err(anyhow!(
            "User denied write permission for {}",
            path.display()
        ))
    }
}
