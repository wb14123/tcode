use std::fs::{self, Permissions};
use std::io;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use llm_rs::conversation::ConversationSummary;
use rand::RngExt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Normal,
    WebOnly,
}

impl SessionMode {
    pub fn label(self) -> &'static str {
        match self {
            SessionMode::Normal => "normal",
            SessionMode::WebOnly => "web-only",
        }
    }

    pub fn is_web_only(self) -> bool {
        matches!(self, SessionMode::WebOnly)
    }
}

/// Lightweight metadata written alongside conversation state for quick access
/// (e.g. session listing) without loading the full state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub description: Option<String>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub last_active_at: Option<u64>,
    #[serde(default)]
    pub mode: SessionMode,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_millis() as u64
}

pub fn read_session_meta(session_dir: &Path) -> Result<Option<SessionMeta>> {
    let path = session_dir.join("session-meta.json");
    match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str::<SessionMeta>(&json)
            .map(Some)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub fn read_session_mode(session_dir: &Path) -> Result<SessionMode> {
    Ok(read_session_meta(session_dir)?
        .map(|meta| meta.mode)
        .unwrap_or_default())
}

fn write_initial_session_meta(session_dir: &Path, mode: SessionMode) -> Result<()> {
    if read_session_meta(session_dir)?.is_some() {
        return Ok(());
    }

    std::fs::create_dir_all(session_dir)
        .with_context(|| format!("failed to create {}", session_dir.display()))?;

    let now = now_millis();
    let meta = SessionMeta {
        description: None,
        created_at: Some(now),
        last_active_at: Some(now),
        mode,
    };
    let meta_json = serde_json::to_string_pretty(&meta)?;
    let meta_path = session_dir.join("session-meta.json");

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&meta_path)
    {
        Ok(mut file) => file
            .write_all(meta_json.as_bytes())
            .with_context(|| format!("failed to write {}", meta_path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_session_meta(session_dir)?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("failed to create {}", meta_path.display())),
    }
}

pub fn ensure_session_mode_initialized(session_dir: &Path, mode: SessionMode) -> Result<()> {
    write_initial_session_meta(session_dir, mode)
}

pub fn session_meta_from_summary(
    session_dir: &Path,
    summary: &ConversationSummary,
    default_mode: SessionMode,
) -> Result<SessionMeta> {
    let existing = read_session_meta(session_dir)?;
    let now = now_millis();
    let mode = existing
        .as_ref()
        .map(|meta| meta.mode)
        .unwrap_or(default_mode);
    Ok(SessionMeta {
        description: summary
            .description
            .clone()
            .or_else(|| existing.as_ref().and_then(|meta| meta.description.clone())),
        created_at: summary
            .created_at
            .or_else(|| existing.as_ref().and_then(|meta| meta.created_at))
            .or(Some(now)),
        last_active_at: summary.last_active_at.or(Some(now)),
        mode,
    })
}

pub fn update_session_meta_from_summary(
    session_dir: &Path,
    summary: &ConversationSummary,
    default_mode: SessionMode,
) -> Result<SessionMeta> {
    std::fs::create_dir_all(session_dir)
        .with_context(|| format!("failed to create {}", session_dir.display()))?;
    let meta = session_meta_from_summary(session_dir, summary, default_mode)?;
    let meta_json = serde_json::to_string_pretty(&meta)?;
    let temp_nonce: u64 = rand::rng().random();
    let meta_tmp = session_dir.join(format!(
        "session-meta.json.{}.{}.tmp",
        std::process::id(),
        temp_nonce
    ));
    let meta_target = session_dir.join("session-meta.json");
    std::fs::write(&meta_tmp, meta_json)
        .with_context(|| format!("failed to write {}", meta_tmp.display()))?;
    if let Err(e) = std::fs::rename(&meta_tmp, &meta_target) {
        if let Err(cleanup_err) = std::fs::remove_file(&meta_tmp) {
            tracing::warn!(
                temp_file = %meta_tmp.display(),
                error = %cleanup_err,
                "failed to remove session metadata temp file after rename failure"
            );
        }
        return Err(e).with_context(|| format!("failed to rename {}", meta_target.display()));
    }
    Ok(meta)
}

/// Returns the base path for all sessions: ~/.tcode/sessions/
pub fn base_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not find home directory")?
        .join(".tcode")
        .join("sessions"))
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

/// Return true when `session_id` is one generated by tcode.
pub fn is_valid_session_id(session_id: &str) -> bool {
    session_id.len() == 8
        && session_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

pub fn validate_session_id(session_id: &str) -> Result<()> {
    if !is_valid_session_id(session_id) {
        bail!("invalid session id: expected 8 lowercase alphanumeric characters");
    }
    Ok(())
}

fn is_safe_subagent_component(component: &str) -> bool {
    let Some(id) = component.strip_prefix("subagent-") else {
        return false;
    };
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn validate_session_path(session_id: &str) -> Result<()> {
    let mut parts = session_id.split('/');
    let Some(root) = parts.next() else {
        bail!("invalid session id: expected a root session id");
    };
    validate_session_id(root)?;
    if parts.any(|part| !is_safe_subagent_component(part)) {
        bail!("invalid session id: subagent session paths must stay under a valid root session");
    }
    Ok(())
}

/// List all session directories under ~/.tcode/sessions/
pub fn list_sessions() -> io::Result<Vec<String>> {
    let base = base_path().map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    if !base.exists() {
        return Ok(vec![]);
    }
    let mut sessions = vec![];
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Some(name) = entry.file_name().to_str()
            && is_valid_session_id(name)
        {
            sessions.push(name.to_string());
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
        validate_session_path(&session_id)?;
        let base_dir = base_path()?;
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

    /// Path for the subscription usage file (written by server, read by nvim)
    pub fn usage_file(&self) -> PathBuf {
        self.session_dir.join("usage.txt")
    }

    /// Path for the token usage file (written by server, read by nvim)
    pub fn token_usage_file(&self) -> PathBuf {
        self.session_dir.join("token_usage.txt")
    }

    /// Path for the socket file
    pub fn socket_path(&self) -> PathBuf {
        self.session_dir.join("server.sock")
    }

    /// Path for a per-tool-call JSONL file (written by server, read by tool-call display)
    pub fn tool_call_file(&self, tool_call_id: &str) -> PathBuf {
        self.session_dir
            .join(format!("tool-call-{}.jsonl", tool_call_id))
    }

    /// Path for a per-tool-call status file (written by server, read by tool-call display)
    pub fn tool_call_status_file(&self, tool_call_id: &str) -> PathBuf {
        self.session_dir
            .join(format!("tool-call-{}-status.txt", tool_call_id))
    }

    /// Path for the conversation state file (persisted LLM conversation)
    pub fn conversation_state_file(&self) -> PathBuf {
        self.session_dir.join("conversation-state.json")
    }

    /// Path for the lightweight session metadata file
    pub fn session_meta_file(&self) -> PathBuf {
        self.session_dir.join("session-meta.json")
    }

    pub fn read_mode(&self) -> Result<SessionMode> {
        read_session_mode(&self.session_dir)
    }

    pub fn ensure_mode_initialized(&self, mode: SessionMode) -> Result<()> {
        ensure_session_mode_initialized(&self.session_dir, mode)
    }

    pub fn update_meta_from_summary(
        &self,
        summary: &ConversationSummary,
        default_mode: SessionMode,
    ) -> Result<SessionMeta> {
        update_session_meta_from_summary(&self.session_dir, summary, default_mode)
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
