use std::fmt::Write;
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::Digest;

use crate::session::validate_session_id;

/// Compute the project config directory for a given working directory.
///
/// Returns `~/.tcode/projects/<sha256(cwd)>/` where the hash is computed
/// from the raw OS bytes of the canonical path. On Linux, `current_dir()`
/// resolves symlinks via `getcwd`; on other platforms, callers should pass a
/// canonicalized path if they need symlink-agnostic hashing. The directory is
/// NOT created — callers create it as needed.
pub fn project_config_dir(cwd: &Path) -> Result<PathBuf> {
    let mut hasher = sha2::Sha256::new();
    hasher.update(cwd.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        // write! to a String is infallible (fmt::Write for String never errors)
        write!(&mut hash, "{:02x}", byte).expect("writes to String are infallible");
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".tcode").join("projects").join(&hash))
}

/// Compute the per-project session index directory for a given working
/// directory.
///
/// Returns `~/.tcode/projects/<sha256(canonicalized cwd)>/sessions/` where
/// the hash is computed from the **canonicalized** `cwd` via
/// [`project_config_dir`], so symlinked spellings of the same folder share
/// one entry. The directory is NOT created — callers create it as needed.
pub fn project_sessions_dir(cwd: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(cwd)
        .with_context(|| format!("failed to canonicalize {}", cwd.display()))?;
    Ok(project_config_dir(&canonical)?.join("sessions"))
}

/// Ensure a session id marker exists under the per-project session index for
/// `cwd`.
///
/// The marker is a zero-byte file named `session_id` inside
/// [`project_sessions_dir`]`(cwd)`, created with `create_new` so concurrent
/// callers race safely and `AlreadyExists` is an idempotent no-op. The
/// `sessions/` directory is created with 0700 permissions (matching the
/// session-dir convention). A `cwd` that cannot be canonicalized is logged and
/// skipped (never fails the caller); an invalid `session_id` is logged and
/// skipped the same way. Other failures are returned to the caller, which is
/// expected to log them warn-only.
pub fn ensure_session_indexed(session_id: &str, cwd: &Path) -> Result<()> {
    if let Err(e) = validate_session_id(session_id) {
        tracing::warn!(
            session_id = %session_id,
            error = %e,
            "skipping session index write: invalid session id"
        );
        return Ok(());
    }
    let canonical = match std::fs::canonicalize(cwd) {
        Ok(canonical) => canonical,
        Err(e) => {
            tracing::warn!(
                path = ?cwd,
                error = %e,
                "skipping session index write: failed to canonicalize cwd"
            );
            return Ok(());
        }
    };
    let sessions_dir = project_sessions_dir(&canonical)?;
    std::fs::create_dir_all(&sessions_dir)
        .with_context(|| format!("failed to create {}", sessions_dir.display()))?;
    std::fs::set_permissions(&sessions_dir, Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", sessions_dir.display()))?;
    let marker = sessions_dir.join(session_id);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e)
            .with_context(|| format!("failed to create session index marker {}", marker.display())),
    }
}
