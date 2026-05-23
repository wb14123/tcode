use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use llm_rs::tool::ContainerConfig;
use password_hash::phc::PasswordHash;
use serde::{Deserialize, Serialize};

/// Configuration for the `tcode remote` web backend.
///
/// Constructed via [`RemoteConfig::try_new`], which validates the port.
/// Port-range validation is enforced by clap's `value_parser!(u16).range(1..)`
/// at the argv layer, so `try_new` trusts `port >= 1`.
pub struct RemoteConfig {
    bind_addr: IpAddr,
    pub(crate) port: u16,
    pub(crate) profile: Option<String>,
    pub(crate) container_config: Option<ContainerConfig>,
    pub(crate) allow_insecure_http: bool,
}

impl RemoteConfig {
    pub fn try_new(port: u16) -> anyhow::Result<Self> {
        Ok(Self::with_loopback_defaults(port))
    }

    /// Test-only constructor that skips clap's port-range check and all
    /// validation / advisory logging. Gated behind `#[cfg(test)]` so
    /// production code cannot reach it.
    #[cfg(test)]
    pub(crate) fn for_test(port: u16) -> Self {
        Self::with_loopback_defaults(port)
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

    /// Override the IP address used by `bind_listener`.
    pub fn with_bind_addr(mut self, bind_addr: IpAddr) -> Self {
        self.bind_addr = bind_addr;
        self
    }

    /// Allow direct plain-HTTP browser sessions by omitting the `Secure`
    /// attribute from auth cookies.
    pub fn with_allow_insecure_http(mut self, allow_insecure_http: bool) -> Self {
        self.allow_insecure_http = allow_insecure_http;
        self
    }

    fn with_loopback_defaults(port: u16) -> Self {
        Self {
            bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            profile: None,
            container_config: None,
            allow_insecure_http: false,
        }
    }

    pub(crate) fn bind_addr(&self) -> IpAddr {
        self.bind_addr
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebUsersFile {
    pub(crate) users: HashMap<String, WebUser>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebUser {
    pub(crate) password_hash: String,
    pub(crate) session_dir: PathBuf,
}

pub(crate) fn load_web_users() -> anyhow::Result<HashMap<String, WebUser>> {
    let path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
        .join(".tcode")
        .join("web-users.toml");

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!(
                "no users configured; create `{}` or run `tcode add-web-user`.",
                path.display()
            );
        }
        Err(e) => return Err(e.into()),
    };

    // Check file permissions: warn if group/other readable
    if let Ok(metadata) = std::fs::metadata(&path) {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            tracing::warn!(
                "web-users.toml ({}) has group or other permissions (mode {:o}); \
                 consider `chmod 600 {0}` to protect user password hashes.",
                path.display(),
                mode & 0o777
            );
        }
    }

    let file: WebUsersFile = toml::from_str(&content)?;

    for (username, user) in &file.users {
        // Full PHC parse — catches malformed hashes at startup instead of
        // at login time (where they would surface as 500).
        if let Err(e) = PasswordHash::new(&user.password_hash) {
            anyhow::bail!("Invalid password_hash for user '{}': {}", username, e);
        }

        // Validate session_dir exists and is accessible
        if let Err(e) = std::fs::metadata(&user.session_dir) {
            anyhow::bail!(
                "session_dir '{}' for user '{}' is not accessible: {}",
                user.session_dir.display(),
                username,
                e
            );
        }
    }

    if file.users.is_empty() {
        anyhow::bail!(
            "no users configured in {}; add at least one user with `tcode add-web-user <username>`",
            path.display()
        );
    }

    Ok(file.users)
}
