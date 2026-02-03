use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Manages session-specific directories and files under /tmp/tcode/sessions/<session_id>/
/// Files are created with restricted permissions (0600) in a directory with 0700 permissions.
pub struct Session {
    session_dir: PathBuf,
}

impl Session {
    /// Create a new session with the given ID.
    /// Creates the session directory with restricted permissions.
    pub fn new(session_id: String) -> Result<Self> {
        let base_dir = PathBuf::from("/tmp/tcode/sessions");
        let session_dir = base_dir.join(&session_id);

        // Create base directory if needed
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create session base directory {:?}", base_dir))?;
        fs::set_permissions(&base_dir, Permissions::from_mode(0o700))?;

        // Create session directory with restricted permissions
        fs::create_dir_all(&session_dir)
            .with_context(|| format!("Failed to create session directory {:?}", session_dir))?;
        fs::set_permissions(&session_dir, Permissions::from_mode(0o700))?;

        Ok(Self { session_dir })
    }

    pub fn session_dir(&self) -> &PathBuf {
        &self.session_dir
    }

    /// Path for the edit message file (written by nvim, read by edit client)
    pub fn msg_file(&self) -> PathBuf {
        self.session_dir.join("edit-msg.txt")
    }

    /// Path for the display content file (written by server, read by nvim)
    pub fn display_file(&self) -> PathBuf {
        self.session_dir.join("display.jsonl")
    }

    /// Path for the status file (written by server, read by nvim)
    pub fn status_file(&self) -> PathBuf {
        self.session_dir.join("status.txt")
    }

    /// Path for the socket file
    pub fn socket_path(&self) -> PathBuf {
        self.session_dir.join("server.sock")
    }

    /// Clean up session files and directory
    pub fn cleanup(&self) {
        let _ = fs::remove_file(self.msg_file());
        let _ = fs::remove_file(self.display_file());
        let _ = fs::remove_file(self.status_file());
        let _ = fs::remove_file(self.socket_path());
        let _ = fs::remove_dir(&self.session_dir);
    }
}

