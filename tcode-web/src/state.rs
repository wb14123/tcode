use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::WebUser;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use tcode_runtime::{
    bootstrap::{RuntimeProbeStatus, RuntimeSettings, probe_runtime_status, send_socket_message},
    protocol::{ClientKind, ClientLeaseInfo, ClientMessage, ServerMessage, SessionRuntimeInfo},
    session::{read_session_mode, validate_session_id},
};

/// Raw token byte length (32 = 256 bits of entropy).
const SESSION_TOKEN_BYTES: usize = 32;
/// Byte length of every minted token. Used as a cheap prefilter in
/// `verify_session` to reject obviously wrong-length cookie values
/// before the `HashMap` lookup.
pub(crate) const SESSION_TOKEN_B64_LEN: usize = 43;
// Compile-time sanity check tying the two constants together.
// base64url-NO-PAD length = ceil(bytes * 4 / 3).
const _: () = assert!(SESSION_TOKEN_B64_LEN == (SESSION_TOKEN_BYTES * 4).div_ceil(3));

/// Absolute lifetime of a minted session token. Once `SESSION_TTL` has
/// elapsed since `mint_session`, the token is rejected by `verify_session`
/// (and lazily evicted on the rejecting call). The login cookie's
/// `Max-Age` is set to the same value so the browser drops the cookie
/// in lockstep — the server is the authority, the browser-side
/// expiration is defense-in-depth.
///
/// Chosen as 7 days: long enough to avoid nuisance re-logins for an
/// actively-used remote, short enough to bound the blast radius of a
/// leaked cookie.
pub(crate) const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

struct UserSession {
    username: String,
    expires_at: Instant,
}

struct RuntimeHandle {
    task: tokio::task::JoinHandle<()>,
}

struct RuntimeState {
    settings: RuntimeSettings,
    runtimes: parking_lot::Mutex<HashMap<String, RuntimeHandle>>,
    start_lock: tokio::sync::Mutex<()>,
}

pub(crate) enum SessionRuntimeStatus {
    Active,
    Inactive,
    Unresponsive,
}

impl SessionRuntimeStatus {
    pub(crate) fn unavailable_message() -> &'static str {
        "session runtime is unavailable; runtime may still be starting"
    }
}

/// Shared application state handed to every axum handler via `with_state`.
///
/// `Debug` is intentionally not derived so sensitive fields cannot be printed
/// by accident via a `#[derive(Debug)]` on an enclosing type.
pub(crate) struct AppState {
    /// Per-user config loaded from `web-users.toml`.
    pub(crate) users: HashMap<String, WebUser>,
    /// Live session tokens, mapped to user session info with absolute expiry.
    ///
    /// Eviction policy: lazy. `verify_session` evicts an expired entry on
    /// the rejecting call; `revoke_session` removes by key. There is no
    /// background sweeper, so a token minted and never re-checked stays
    /// in the map until something looks it up.
    sessions: parking_lot::RwLock<HashMap<String, UserSession>>,
    runtime: Option<RuntimeState>,
    secure_session_cookie: bool,
}

impl AppState {
    #[cfg(test)]
    pub(crate) fn new_with_test_user() -> Self {
        let mut users = HashMap::new();
        let session_dir = std::path::PathBuf::from("/tmp/test-user-sessions");
        std::fs::create_dir_all(&session_dir).ok();
        let trash_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp/test-user-trash");
        std::fs::create_dir_all(&trash_dir).ok();
        users.insert(
            "test-user".to_string(),
            WebUser {
                password_hash: "$argon2id$v=19$m=65536,t=3,p=4$dGVzdHNhbHQxMjM0NTY3OA$Qb/h6/Mzserubz9fRZL7WhsOHwa0mU/KPavVjxWLsdY".to_string(),
                session_dir: std::path::PathBuf::from("/tmp/test-user-sessions"),
                trash_dir,
            },
        );
        Self {
            users,
            sessions: parking_lot::RwLock::new(HashMap::new()),
            runtime: None,
            secure_session_cookie: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_insecure_http() -> Self {
        let mut state = Self::new_with_test_user();
        state.secure_session_cookie = false;
        state
    }

    #[cfg(test)]
    pub(crate) fn new_with_custom_user_dir(session_dir: std::path::PathBuf) -> Self {
        let mut users = HashMap::new();
        let trash_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp/test-user-trash");
        std::fs::create_dir_all(&trash_dir).ok();
        users.insert(
            "test-user".to_string(),
            WebUser {
                password_hash:
                    "$argon2id$v=19$m=65536,t=3,p=4$dGVzdHNhbHQxMjM0NTY3OA$Qb/h6/Mzserubz9fRZL7WhsOHwa0mU/KPavVjxWLsdY".to_string(),
                session_dir,
                trash_dir,
            },
        );
        Self {
            users,
            sessions: parking_lot::RwLock::new(HashMap::new()),
            runtime: None,
            secure_session_cookie: true,
        }
    }

    pub(crate) fn from_users_and_runtime(
        users: HashMap<String, WebUser>,
        runtime_settings: RuntimeSettings,
        secure_session_cookie: bool,
    ) -> Self {
        Self {
            users,
            sessions: parking_lot::RwLock::new(HashMap::new()),
            runtime: Some(RuntimeState {
                settings: runtime_settings,
                runtimes: parking_lot::Mutex::new(HashMap::new()),
                start_lock: tokio::sync::Mutex::new(()),
            }),
            secure_session_cookie,
        }
    }

    /// Mint a fresh random session token, store it with an absolute expiry
    /// of `Instant::now() + SESSION_TTL`, and return the base64url
    /// (unpadded, `SESSION_TOKEN_B64_LEN`-char) string for the cookie value.
    ///
    /// Returns an error if the OS CSPRNG is unavailable; the caller maps that
    /// to HTTP 500. Do NOT log the returned token.
    pub(crate) fn mint_session(&self, username: String) -> Result<String, getrandom::Error> {
        let mut buf = [0u8; SESSION_TOKEN_BYTES];
        getrandom::fill(&mut buf)?;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        let expires_at = Instant::now() + SESSION_TTL;
        self.sessions.write().insert(
            token.clone(),
            UserSession {
                username,
                expires_at,
            },
        );
        Ok(token)
    }

    /// Test-only: insert a synthetic token with an arbitrary expiry, so
    /// expiry-related tests can observe past/future deadlines without
    /// blocking on real time. Returns the inserted token.
    #[cfg(test)]
    pub(crate) fn insert_session_with_expiry(
        &self,
        expires_at: Instant,
    ) -> Result<String, getrandom::Error> {
        let mut buf = [0u8; SESSION_TOKEN_BYTES];
        getrandom::fill(&mut buf)?;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        self.sessions.write().insert(
            token.clone(),
            UserSession {
                username: "test-user".to_string(),
                expires_at,
            },
        );
        Ok(token)
    }

    /// Test-only: number of entries currently in the session map. Used
    /// by tests to confirm lazy eviction actually removed an expired
    /// entry rather than just rejecting it.
    #[cfg(test)]
    pub(crate) fn sessions_len_for_test(&self) -> usize {
        self.sessions.read().len()
    }

    /// Verify that `candidate` names a live, non-expired session.
    ///
    /// Returns `Some(username)` on success, `None` on failure.
    /// On expiry the entry is evicted in place (lazy cleanup).
    /// Do NOT log `candidate`.
    pub(crate) fn verify_session(&self, candidate: &str) -> Option<String> {
        // Fast reject on obviously wrong lengths — protects the hash path
        // from pathological inputs and keeps the happy path tight.
        if candidate.len() != SESSION_TOKEN_B64_LEN {
            return None;
        }
        // Read-lock-only fast path for the overwhelmingly common case
        // (live, non-expired token). We only escalate to a write lock
        // when we actually need to evict.
        let now = Instant::now();
        if let Some(session) = self.sessions.read().get(candidate) {
            if now < session.expires_at {
                return Some(session.username.clone());
            }
        } else {
            return None;
        }
        // Token was present but expired. Re-check under the write lock
        // (a concurrent `revoke_session` or `insert_session_with_expiry`
        // may have changed state) and evict if still expired.
        let now = Instant::now();
        let mut guard = self.sessions.write();
        match guard.get(candidate) {
            Some(session) if now >= session.expires_at => {
                guard.remove(candidate);
                None
            }
            Some(session) => Some(session.username.clone()), // Refreshed under write lock.
            None => None,
        }
    }

    /// Remove `candidate` from the live session map, if present. Idempotent.
    /// Do NOT log `candidate`.
    pub(crate) fn revoke_session(&self, candidate: &str) {
        self.sessions.write().remove(candidate);
    }

    pub(crate) fn secure_session_cookie(&self) -> bool {
        self.secure_session_cookie
    }

    pub(crate) async fn ensure_runtime(&self, session_id: &str, session_dir: &Path) -> Result<()> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow!("session runtime support is not configured"))?;

        if Self::runtime_is_live(&runtime.runtimes, session_id) {
            self.runtime_status(session_id, session_dir).await?;
            return Ok(());
        }

        let _guard = runtime.start_lock.lock().await;
        if Self::runtime_is_live(&runtime.runtimes, session_id) {
            self.runtime_status(session_id, session_dir).await?;
            return Ok(());
        }

        let handle = runtime
            .settings
            .start_runtime_at(session_id, session_dir)
            .await?;
        runtime
            .runtimes
            .lock()
            .insert(session_id.to_string(), RuntimeHandle { task: handle });
        Ok(())
    }

    pub(crate) async fn runtime_status(
        &self,
        session_id: &str,
        session_dir: &Path,
    ) -> Result<SessionRuntimeStatus> {
        let socket_path = Self::socket_path_for_session(session_dir, session_id)?;
        Ok(match probe_runtime_status(&socket_path).await? {
            RuntimeProbeStatus::Active(info) => {
                let expected_mode = read_session_mode(session_dir)?;
                if info.session_mode != expected_mode {
                    bail!(
                        "session runtime mode mismatch for {session_id}: persisted mode is {}, active runtime mode is {}",
                        expected_mode.label(),
                        info.session_mode.label()
                    );
                }
                SessionRuntimeStatus::Active
            }
            RuntimeProbeStatus::NoSocket | RuntimeProbeStatus::NoListener => {
                SessionRuntimeStatus::Inactive
            }
            RuntimeProbeStatus::Unresponsive => SessionRuntimeStatus::Unresponsive,
        })
    }

    pub(crate) async fn register_web_client_lease(
        &self,
        session_id: &str,
        session_dir: &Path,
        client_label: Option<String>,
        resume_if_inactive: bool,
    ) -> Result<Option<ClientLeaseInfo>> {
        if resume_if_inactive {
            self.ensure_runtime(session_id, session_dir).await?;
            match self.runtime_status(session_id, session_dir).await? {
                SessionRuntimeStatus::Active => {}
                SessionRuntimeStatus::Inactive | SessionRuntimeStatus::Unresponsive => {
                    return Err(anyhow!(SessionRuntimeStatus::unavailable_message()));
                }
            }
        } else {
            match self.runtime_status(session_id, session_dir).await? {
                SessionRuntimeStatus::Active => {}
                SessionRuntimeStatus::Inactive => return Ok(None),
                SessionRuntimeStatus::Unresponsive => {
                    return Err(anyhow!(SessionRuntimeStatus::unavailable_message()));
                }
            }
        }

        let response = self
            .send_socket_message(
                session_id,
                ClientMessage::RegisterClientLease {
                    client_kind: ClientKind::Web,
                    client_label,
                },
                session_dir,
            )
            .await?;
        match response {
            ServerMessage::ClientLeaseRegistered(info) => Ok(Some(info)),
            ServerMessage::Error { message } => Err(anyhow!(message)),
            other => Err(anyhow!(
                "unexpected runtime response to client lease registration: {other:?}"
            )),
        }
    }

    pub(crate) async fn heartbeat_client_lease(
        &self,
        session_id: &str,
        session_dir: &Path,
        client_id: String,
    ) -> Result<SessionRuntimeInfo> {
        let response = match self
            .send_socket_message(
                session_id,
                ClientMessage::HeartbeatClientLease { client_id },
                session_dir,
            )
            .await
        {
            Ok(response) => response,
            Err(e) => {
                tracing::debug!(session_id, error = %e, "client lease heartbeat failed");
                return match self.runtime_status(session_id, session_dir).await? {
                    SessionRuntimeStatus::Active | SessionRuntimeStatus::Unresponsive => {
                        Err(anyhow!(SessionRuntimeStatus::unavailable_message()))
                    }
                    SessionRuntimeStatus::Inactive => Ok(SessionRuntimeInfo::inactive()),
                };
            }
        };

        match response {
            ServerMessage::SessionRuntimeInfo(info) => Ok(info),
            ServerMessage::Error { message } => Err(anyhow!(message)),
            other => Err(anyhow!(
                "unexpected runtime response to client lease heartbeat: {other:?}"
            )),
        }
    }

    pub(crate) async fn detach_client_lease(
        &self,
        session_id: &str,
        session_dir: &Path,
        client_id: String,
    ) {
        match self
            .send_socket_message(
                session_id,
                ClientMessage::DetachClientLease { client_id },
                session_dir,
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(session_id, error = %e, "client lease detach failed");
            }
        }
    }

    pub(crate) async fn send_runtime_message_if_active(
        &self,
        session_id: &str,
        session_dir: &Path,
        message: ClientMessage,
    ) -> Result<Option<ServerMessage>> {
        match self.runtime_status(session_id, session_dir).await? {
            SessionRuntimeStatus::Active => {}
            SessionRuntimeStatus::Inactive => return Ok(None),
            SessionRuntimeStatus::Unresponsive => {
                return Err(anyhow!(SessionRuntimeStatus::unavailable_message()));
            }
        }

        self.send_socket_message(session_id, message, session_dir)
            .await
            .map(Some)
            .with_context(|| format!("active runtime message failed for session {session_id}"))
    }

    pub(crate) async fn send_socket_message(
        &self,
        session_id: &str,
        message: ClientMessage,
        session_dir: &Path,
    ) -> Result<ServerMessage> {
        let response = send_socket_message(
            Self::socket_path_for_session(session_dir, session_id)?,
            &message,
        )
        .await?
        .ok_or_else(|| anyhow!("runtime closed socket without responding"))?;
        Ok(response)
    }

    fn socket_path_for_session(session_dir: &Path, session_id: &str) -> Result<std::path::PathBuf> {
        validate_session_id(session_id)?;
        Ok(session_dir.join("server.sock"))
    }

    fn runtime_is_live(
        runtimes: &parking_lot::Mutex<HashMap<String, RuntimeHandle>>,
        session_id: &str,
    ) -> bool {
        let mut guard = runtimes.lock();
        if let Some(handle) = guard.get(session_id)
            && !handle.task.is_finished()
        {
            return true;
        }
        guard.remove(session_id);
        false
    }
}
