use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rand::Rng;

/// Returns the base path for all sessions: ~/.tcode/sessions/
pub fn base_path() -> PathBuf {
    dirs::home_dir()
        .expect("Could not find home directory")
        .join(".tcode")
        .join("sessions")
}

/// Generate a unique 8-character session ID (lowercase alphanumeric)
pub fn generate_session_id() -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..8)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// List all session directories under ~/.tcode/sessions/
pub fn list_sessions() -> io::Result<Vec<String>> {
    let base = base_path();
    if !base.exists() {
        return Ok(vec![]);
    }
    let mut sessions = vec![];
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                sessions.push(name.to_string());
            }
        }
    }
    Ok(sessions)
}

/// Manages session-specific directories and files under ~/.tcode/sessions/<session_id>/
/// Files are created with restricted permissions (0600) in a directory with 0700 permissions.
pub struct Session {
    session_dir: PathBuf,
}

impl Session {
    /// Create a new session with the given ID.
    /// Creates the session directory with restricted permissions.
    pub fn new(session_id: String) -> Result<Self> {
        let base_dir = base_path();
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

    /// Path for a per-tool-call JSONL file (written by server, read by tool-call display)
    pub fn tool_call_file(&self, tool_call_id: &str) -> PathBuf {
        self.session_dir.join(format!("tool-call-{}.jsonl", tool_call_id))
    }

    /// Path for a per-tool-call status file (written by server, read by tool-call display)
    pub fn tool_call_status_file(&self, tool_call_id: &str) -> PathBuf {
        self.session_dir.join(format!("tool-call-{}-status.txt", tool_call_id))
    }

    /// Path for stdout log (captures injected stdout from tools like proxychains)
    pub fn stdout_log(&self) -> PathBuf {
        self.session_dir.join("stdout.log")
    }

    /// Path for stderr log (captures injected stderr from tools like proxychains)
    pub fn stderr_log(&self) -> PathBuf {
        self.session_dir.join("stderr.log")
    }
}

