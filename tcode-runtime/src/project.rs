use std::fmt::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::Digest;

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
