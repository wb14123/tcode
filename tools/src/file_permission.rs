use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use llm_rs::permission::{SCOPE_FILE_READ, SCOPE_FILE_WRITE, ScopedPermissionManager};

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

/// If `dir` is inside the project directory (cwd), return cwd so that a single
/// permission grant covers the whole project. Otherwise return `dir` unchanged.
fn widen_to_project_dir(dir: &Path) -> PathBuf {
    let cwd = match std::env::current_dir().and_then(|p| p.canonicalize()) {
        Ok(c) => c,
        Err(_) => return dir.to_path_buf(),
    };
    if dir.starts_with(&cwd) {
        cwd
    } else {
        dir.to_path_buf()
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

/// Check whether a read permission already exists for `path` without prompting.
///
/// Returns `true` if the path is inside cwd (auto-allowed) or if a stored
/// `file_read` permission covers it. Returns `false` if the path doesn't exist
/// or no permission is found (but does NOT prompt the user).
pub async fn has_file_read_permission(
    permission: &ScopedPermissionManager,
    path: &Path,
    is_dir: bool,
) -> bool {
    let Ok((canonical_path, exists)) = canonicalize_path(path).await else {
        return false;
    };
    if !exists {
        return false;
    }

    // Inside cwd — auto-allowed
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let Ok(canonical_cwd) = tokio::fs::canonicalize(&cwd).await else {
        return false;
    };
    if canonical_path.starts_with(&canonical_cwd) {
        return true;
    }

    let permission_dir = permission_dir_for(&canonical_path, is_dir);
    has_ancestor_permission(permission, SCOPE_FILE_READ, &permission_dir)
}

/// Check whether a write permission already exists for `path` without prompting.
///
/// Returns `true` if a stored `file_write` permission covers it.
/// Returns `false` if no permission is found (but does NOT prompt the user).
/// Unlike reads, writes inside cwd still require explicit permission.
pub async fn has_file_write_permission(permission: &ScopedPermissionManager, path: &Path) -> bool {
    let Ok((canonical_path, _exists)) = canonicalize_path(path).await else {
        return false;
    };

    let parent_dir = permission_dir_for(&canonical_path, false);
    let permission_dir = widen_to_project_dir(&parent_dir);
    has_ancestor_permission(permission, SCOPE_FILE_WRITE, &permission_dir)
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
    if has_ancestor_permission(permission, SCOPE_FILE_READ, &permission_dir) {
        return Ok(());
    }

    let permission_dir_str = permission_dir.to_string_lossy().to_string();
    permission
        .ask_permission_for(
            SCOPE_FILE_READ,
            &format!("Allow read access to {}?", path.display()),
            "path",
            &permission_dir_str,
        )
        .await
}

/// Check whether the caller has permission to write `path`.
///
/// Writes always require explicit permission — even for paths inside cwd.
/// Prompts with a preview so the user can inspect the change. The caller
/// controls what is shown via `preview_content` and `preview_type` (e.g. a
/// file extension like `"rs"` for full-content previews, or `"tcodediff"`
/// for diff previews).
///
/// When the file is inside the project directory (cwd), the permission prompt
/// covers the entire project folder instead of just the parent directory,
/// so a single "allow for session/project" grants write access project-wide.
pub async fn check_file_write_permission(
    permission: &ScopedPermissionManager,
    path: &Path,
    preview_content: &str,
    preview_type: &str,
) -> Result<()> {
    let (canonical_path, exists) = canonicalize_path(path).await?;

    let parent_dir = permission_dir_for(&canonical_path, false);
    let permission_dir = widen_to_project_dir(&parent_dir);
    if has_ancestor_permission(permission, SCOPE_FILE_WRITE, &permission_dir) {
        return Ok(());
    }

    let permission_dir_str = permission_dir.to_string_lossy().to_string();
    let prompt = if exists {
        format!("Allow write to existing file {}?", path.display())
    } else {
        format!("Allow creating new file {}?", path.display())
    };

    permission
        .ask_permission_with_preview(
            SCOPE_FILE_WRITE,
            &prompt,
            "path",
            &permission_dir_str,
            preview_content,
            preview_type,
        )
        .await
}
