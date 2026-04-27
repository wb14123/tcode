use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use crate::config::RemoteModePolicy;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use subtle::ConstantTimeEq;
use tcode_runtime::{
    bootstrap::{RuntimeProbeStatus, RuntimeSettings, probe_runtime_status, send_socket_message},
    protocol::{ClientKind, ClientLeaseInfo, ClientMessage, ServerMessage, SessionRuntimeInfo},
    session::{SessionMode, base_path, read_session_mode, validate_session_id},
};
use zeroize::{Zeroize, ZeroizeOnDrop};

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
/// Chosen as 7 days for a single-user PoC: long enough to avoid
/// nuisance re-logins for an actively-used remote, short enough to
/// bound the blast radius of a leaked cookie.
pub(crate) const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A shared secret. Custom `Debug` impl redacts the value.
///
/// Intentionally does NOT derive `Clone` or implement `Display`.
/// No `as_str` / getter accessor is exposed — password comparison goes through
/// the constant-time [`Secret::verify`] method rather than leaking the inner
/// string.
///
/// The inner `String` is zeroized on drop via `zeroize::ZeroizeOnDrop`, so the
/// plaintext does not linger in freed heap for the lifetime of the process
/// (modulo pre-existing copies — see the startup `TCODE_REMOTE_PASSWORD`
/// env-var path which is outside this type's control).
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn new(s: String) -> Self {
        Self(s)
    }

    /// Constant-time byte-wise compare of the stored secret against `candidate`
    /// **for candidates of the same length**.
    ///
    /// `subtle::ConstantTimeEq::ct_eq` on `[u8]` short-circuits to `Choice(0)`
    /// when the input lengths differ, so a length mismatch is detectable via
    /// timing. For this PoC the stored secret is a single pre-configured
    /// password; candidate-length is not a secret worth protecting. What we
    /// DO protect against is correct-prefix-vs-wrong-prefix distinction among
    /// same-length candidates — that is the practical timing oracle the
    /// attacker tries to exploit, and `ct_eq` defeats it.
    pub(crate) fn verify(&self, candidate: &[u8]) -> bool {
        self.0.as_bytes().ct_eq(candidate).into()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(<redacted>)")
    }
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
/// `Debug` is intentionally not derived so the password cannot be printed
/// by accident via a `#[derive(Debug)]` on an enclosing type.
pub(crate) struct AppState {
    /// Configured shared secret. Compared against incoming login payloads via
    /// [`Secret::verify`]; never exposed directly.
    pub(crate) password: Secret,
    /// Live session tokens, mapped to their absolute expiry instant.
    ///
    /// PoC note: we store the raw token strings. A memory-dump attacker with
    /// access to this map already has access to the single configured password
    /// too, so hashing the stored tokens does not change the threat model.
    /// Hash-at-rest with a constant-time compare is a later hardening step.
    ///
    /// Eviction policy: lazy. `verify_session` evicts an expired entry on
    /// the rejecting call; `revoke_session` removes by key. There is no
    /// background sweeper, so a token minted and never re-checked stays
    /// in the map until something looks it up. For a single-user PoC the
    /// resulting bounded growth (one entry per login over a 7-day window)
    /// is acceptable; a future hardening pass may add an opportunistic
    /// sweep on `mint_session` or a periodic background task.
    sessions: parking_lot::RwLock<HashMap<String, Instant>>,
    runtime: Option<RuntimeState>,
    remote_mode_policy: RemoteModePolicy,
    secure_session_cookie: bool,
}

impl AppState {
    /// Test-only convenience: construct from a raw `String`. Production
    /// code goes through [`AppState::from_secret`] so the `Secret` wrapper
    /// travels the whole pipeline from `RemoteConfig`.
    #[cfg(test)]
    pub(crate) fn new(password: String) -> Self {
        Self::from_secret(Secret::new(password))
    }

    #[cfg(test)]
    pub(crate) fn new_with_policy(password: String, remote_mode_policy: RemoteModePolicy) -> Self {
        let mut state = Self::from_secret(Secret::new(password));
        state.remote_mode_policy = remote_mode_policy;
        state
    }

    #[cfg(test)]
    pub(crate) fn new_with_insecure_http(password: String) -> Self {
        let mut state = Self::from_secret(Secret::new(password));
        state.secure_session_cookie = false;
        state
    }

    #[cfg(test)]
    pub(crate) fn from_secret(password: Secret) -> Self {
        Self {
            password,
            sessions: parking_lot::RwLock::new(HashMap::new()),
            runtime: None,
            remote_mode_policy: RemoteModePolicy::default(),
            secure_session_cookie: true,
        }
    }

    pub(crate) fn from_secret_and_runtime(
        password: Secret,
        runtime_settings: RuntimeSettings,
        remote_mode_policy: RemoteModePolicy,
        secure_session_cookie: bool,
    ) -> Self {
        Self {
            password,
            sessions: parking_lot::RwLock::new(HashMap::new()),
            runtime: Some(RuntimeState {
                settings: runtime_settings,
                runtimes: parking_lot::Mutex::new(HashMap::new()),
                start_lock: tokio::sync::Mutex::new(()),
            }),
            remote_mode_policy,
            secure_session_cookie,
        }
    }

    /// Mint a fresh random session token, store it with an absolute expiry
    /// of `Instant::now() + SESSION_TTL`, and return the base64url
    /// (unpadded, `SESSION_TOKEN_B64_LEN`-char) string for the cookie value.
    ///
    /// Returns an error if the OS CSPRNG is unavailable; the caller maps that
    /// to HTTP 500. Do NOT log the returned token.
    pub(crate) fn mint_session(&self) -> Result<String, getrandom::Error> {
        let mut buf = [0u8; SESSION_TOKEN_BYTES];
        getrandom::fill(&mut buf)?;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        let expires_at = Instant::now() + SESSION_TTL;
        self.sessions.write().insert(token.clone(), expires_at);
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
        self.sessions.write().insert(token.clone(), expires_at);
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
    /// On expiry the entry is evicted in place (lazy cleanup), so a
    /// subsequent call cannot resurrect it without a fresh `mint_session`.
    /// PoC note: `HashMap::get` does a non-constant-time equality check,
    /// but every stored token carries 256 bits of CSPRNG entropy — a
    /// remote attacker cannot feasibly guess even the first byte, so the
    /// timing side-channel has nothing to act on. A later hardening pass
    /// may switch to hash-stored tokens with constant-time compare.
    /// Do NOT log `candidate`.
    pub(crate) fn verify_session(&self, candidate: &str) -> bool {
        // Fast reject on obviously wrong lengths — protects the hash path
        // from pathological inputs and keeps the happy path tight.
        if candidate.len() != SESSION_TOKEN_B64_LEN {
            return false;
        }
        // Read-lock-only fast path for the overwhelmingly common case
        // (live, non-expired token). We only escalate to a write lock
        // when we actually need to evict.
        let now = Instant::now();
        if let Some(&expires_at) = self.sessions.read().get(candidate) {
            if now < expires_at {
                return true;
            }
        } else {
            return false;
        }
        // Token was present but expired. Re-check under the write lock
        // (a concurrent `revoke_session` or `insert_session_with_expiry`
        // may have changed state) and evict if still expired.
        let mut guard = self.sessions.write();
        match guard.get(candidate) {
            Some(&expires_at) if now >= expires_at => {
                guard.remove(candidate);
                false
            }
            Some(_) => true, // Refreshed under write lock — accept.
            None => false,
        }
    }

    /// Remove `candidate` from the live session map, if present. Idempotent.
    /// Do NOT log `candidate`.
    pub(crate) fn revoke_session(&self, candidate: &str) {
        self.sessions.write().remove(candidate);
    }

    pub(crate) fn remote_mode_policy(&self) -> RemoteModePolicy {
        self.remote_mode_policy
    }

    pub(crate) fn secure_session_cookie(&self) -> bool {
        self.secure_session_cookie
    }

    pub(crate) fn new_session_mode(&self) -> SessionMode {
        self.remote_mode_policy.new_session_mode()
    }

    pub(crate) fn allows_session_mode(&self, mode: SessionMode) -> bool {
        self.remote_mode_policy.allows_session_mode(mode)
    }

    pub(crate) async fn ensure_runtime(&self, session_id: &str) -> Result<()> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow!("session runtime support is not configured"))?;

        if Self::runtime_is_live(&runtime.runtimes, session_id) {
            self.runtime_status(session_id).await?;
            return Ok(());
        }

        let _guard = runtime.start_lock.lock().await;
        if Self::runtime_is_live(&runtime.runtimes, session_id) {
            self.runtime_status(session_id).await?;
            return Ok(());
        }

        let handle = runtime.settings.start_runtime(session_id).await?;
        runtime
            .runtimes
            .lock()
            .insert(session_id.to_string(), RuntimeHandle { task: handle });
        Ok(())
    }

    pub(crate) async fn runtime_status(&self, session_id: &str) -> Result<SessionRuntimeStatus> {
        let socket_path = Self::socket_path_for_session(session_id)?;
        Ok(match probe_runtime_status(&socket_path).await? {
            RuntimeProbeStatus::Active(info) => {
                let session_dir = base_path()?.join(session_id);
                let expected_mode = read_session_mode(&session_dir)?;
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
        client_label: Option<String>,
        resume_if_inactive: bool,
    ) -> Result<Option<ClientLeaseInfo>> {
        if resume_if_inactive {
            self.ensure_runtime(session_id).await?;
            match self.runtime_status(session_id).await? {
                SessionRuntimeStatus::Active => {}
                SessionRuntimeStatus::Inactive | SessionRuntimeStatus::Unresponsive => {
                    return Err(anyhow!(SessionRuntimeStatus::unavailable_message()));
                }
            }
        } else {
            match self.runtime_status(session_id).await? {
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
        client_id: String,
    ) -> Result<SessionRuntimeInfo> {
        let response = match self
            .send_socket_message(
                session_id,
                ClientMessage::HeartbeatClientLease { client_id },
            )
            .await
        {
            Ok(response) => response,
            Err(e) => {
                tracing::debug!(session_id, error = %e, "client lease heartbeat failed");
                return match self.runtime_status(session_id).await? {
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

    pub(crate) async fn detach_client_lease(&self, session_id: &str, client_id: String) {
        match self
            .send_socket_message(session_id, ClientMessage::DetachClientLease { client_id })
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
        message: ClientMessage,
    ) -> Result<Option<ServerMessage>> {
        match self.runtime_status(session_id).await? {
            SessionRuntimeStatus::Active => {}
            SessionRuntimeStatus::Inactive => return Ok(None),
            SessionRuntimeStatus::Unresponsive => {
                return Err(anyhow!(SessionRuntimeStatus::unavailable_message()));
            }
        }

        self.send_socket_message(session_id, message)
            .await
            .map(Some)
            .with_context(|| format!("active runtime message failed for session {session_id}"))
    }

    pub(crate) async fn send_socket_message(
        &self,
        session_id: &str,
        message: ClientMessage,
    ) -> Result<ServerMessage> {
        let response = send_socket_message(Self::socket_path_for_session(session_id)?, &message)
            .await?
            .ok_or_else(|| anyhow!("runtime closed socket without responding"))?;
        Ok(response)
    }

    fn socket_path_for_session(session_id: &str) -> Result<std::path::PathBuf> {
        validate_session_id(session_id)?;
        Ok(base_path()?.join(session_id).join("server.sock"))
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
