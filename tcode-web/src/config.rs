use std::net::{IpAddr, Ipv4Addr};

use anyhow::bail;
use llm_rs::tool::ContainerConfig;
use tcode_runtime::session::SessionMode;

use crate::state::Secret;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteModePolicy {
    #[default]
    All,
    WebOnlyOnly,
}

impl RemoteModePolicy {
    pub fn new_session_mode(self) -> SessionMode {
        match self {
            Self::All => SessionMode::Normal,
            Self::WebOnlyOnly => SessionMode::WebOnly,
        }
    }

    pub fn allows_session_mode(self, mode: SessionMode) -> bool {
        match self {
            Self::All => true,
            Self::WebOnlyOnly => mode.is_web_only(),
        }
    }
}

/// Configuration for the `tcode remote` web backend.
///
/// Constructed via [`RemoteConfig::try_new`], which enforces password
/// validation rules and emits advisory tracing logs. Port-range validation
/// is enforced by clap's `value_parser!(u16).range(1..)` at the argv layer,
/// so `try_new` trusts `port >= 1`.
pub struct RemoteConfig {
    bind_addr: IpAddr,
    pub(crate) port: u16,
    /// Shared secret, stored exactly as received (no trimming). Wrapped in
    /// [`Secret`] so the plaintext is zeroized on drop — including when
    /// `RemoteConfig` is dropped on an early error path before reaching
    /// `AppState`. The login handler compares byte-for-byte, so whitespace
    /// in the operator's secret is significant.
    pub(crate) password: Secret,
    pub(crate) profile: Option<String>,
    pub(crate) container_config: Option<ContainerConfig>,
    pub(crate) remote_mode_policy: RemoteModePolicy,
}

impl RemoteConfig {
    /// Validate and construct a `RemoteConfig`.
    ///
    /// - Rejects empty / all-whitespace passwords with an error that names
    ///   `TCODE_REMOTE_PASSWORD` so operators can self-correct.
    /// - Emits a `warn!` when the trimmed password has fewer than 16 Unicode
    ///   characters (heuristic nudge, not a gate).
    /// - Emits a `warn!` when `password_on_argv` is true, since the secret
    ///   was recorded in `ps` / shell history.
    ///
    /// `password_on_argv` is detected by the binary via an argv scan before
    /// calling this constructor. See `tcode/src/main.rs::password_on_argv`.
    pub fn try_new(port: u16, password: String, password_on_argv: bool) -> anyhow::Result<Self> {
        // Hard rejections run first so their error messages are not preceded
        // by unrelated advisory output.
        if password.trim().is_empty() {
            bail!("password must be non-empty; pass --password or set TCODE_REMOTE_PASSWORD");
        }

        // Argv-leak advisory is emitted first: it describes a disclosure that
        // has already happened (the secret is already in `ps` / shell history).
        // The short-password warning is a future-risk heuristic, so it comes
        // second.
        if password_on_argv {
            tracing::warn!(
                "--password was supplied via the command line; the secret was recorded in `ps` and shell history. Prefer TCODE_REMOTE_PASSWORD next time."
            );
        }

        if password.trim().chars().count() < 16 {
            tracing::warn!(
                "tcode remote password is shorter than recommended; consider a longer secret"
            );
        }

        Ok(Self::with_loopback_defaults(port, password))
    }

    /// Test-only constructor that skips clap's port-range check and all
    /// validation / advisory logging. Gated behind `#[cfg(test)]` so
    /// production code cannot reach it.
    #[cfg(test)]
    pub(crate) fn for_test(port: u16, password: String) -> Self {
        Self::with_loopback_defaults(port, password)
    }

    pub fn with_runtime_options(
        mut self,
        profile: Option<String>,
        container_config: Option<ContainerConfig>,
    ) -> Self {
        self.profile = profile;
        self.container_config = container_config;
        self
    }

    pub fn with_remote_mode_policy(mut self, remote_mode_policy: RemoteModePolicy) -> Self {
        self.remote_mode_policy = remote_mode_policy;
        self
    }

    /// Shared builder used by both `try_new` (after validation) and
    /// `for_test` (which skips validation). Keeping the default bind address
    /// in one place guards against the two paths drifting.
    fn with_loopback_defaults(port: u16, password: String) -> Self {
        Self {
            bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            password: Secret::new(password),
            profile: None,
            container_config: None,
            remote_mode_policy: RemoteModePolicy::default(),
        }
    }

    pub(crate) fn bind_addr(&self) -> IpAddr {
        self.bind_addr
    }
}
