//! OAuth token management for LLM providers.
//!
//! Provides a shared generic [`BaseTokenManager`] that handles token persistence
//! (with 0600 permissions), expiry checking, and double-checked-locking refresh.
//! Provider-specific modules ([`claude`], [`openai`]) supply only the refresh
//! function via the [`TokenRefresher`] trait.

pub mod claude;
pub mod openai;

mod rate_limit_source;

#[cfg(test)]
mod rate_limit_tests;

#[cfg(test)]
mod token_storage_tests;

pub use rate_limit_source::rate_limit_reset_source;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

/// OAuth providers with file-backed token storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthProvider {
    Claude,
    OpenAi,
}

impl OAuthProvider {
    fn token_filename(self, profile: Option<&str>) -> String {
        let stem = match self {
            Self::Claude => "claude_tokens",
            Self::OpenAi => "openai_tokens",
        };

        match profile {
            Some(profile) => format!("{stem}-{profile}.json"),
            None => format!("{stem}.json"),
        }
    }
}

/// Resolve the OAuth token storage path for a provider and optional profile.
pub fn oauth_token_storage_path(provider: OAuthProvider, profile: Option<&str>) -> PathBuf {
    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    oauth_token_storage_path_in(&home_dir, provider, profile)
}

fn oauth_token_storage_path_in(
    home_dir: &Path,
    provider: OAuthProvider,
    profile: Option<&str>,
) -> PathBuf {
    home_dir
        .join(".tcode")
        .join("auth")
        .join(provider.token_filename(profile))
}

// ---------------------------------------------------------------------------
// Shared OAuthTokens
// ---------------------------------------------------------------------------

/// OAuth token set shared across providers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    /// Provider-specific extra data (e.g. OpenAI's `chatgpt_account_id`).
    /// Preserved across refreshes. Absent for providers that don't need it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// Buffer time before expiry to trigger refresh (5 minutes in seconds).
const REFRESH_BUFFER_SECS: u64 = 300;

impl OAuthTokens {
    /// Check if the token is expired or will expire within [`REFRESH_BUFFER_SECS`].
    pub fn is_expired_or_expiring(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.expires_at <= now + REFRESH_BUFFER_SECS
    }
}

// ---------------------------------------------------------------------------
// TokenRefresher trait — implemented per provider
// ---------------------------------------------------------------------------

/// Provider-specific token refresh logic.
///
/// Implementors supply the HTTP call to the provider's token endpoint and
/// return a new [`OAuthTokens`].
#[async_trait::async_trait]
pub trait TokenRefresher: Send + Sync + 'static {
    /// Refresh tokens using the given HTTP client and current refresh token.
    async fn refresh(&self, client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens>;
}

// ---------------------------------------------------------------------------
// Generic TokenManager
// ---------------------------------------------------------------------------

/// Token manager that handles automatic token refresh with double-checked locking.
///
/// Generic over a [`TokenRefresher`] so each provider only needs to supply the
/// refresh logic. Thread-safe and cheaply cloneable.
///
/// Provider modules expose this as `TokenManager` via a type alias.
#[derive(Clone)]
pub struct BaseTokenManager<R: TokenRefresher> {
    tokens: Arc<RwLock<OAuthTokens>>,
    storage_path: PathBuf,
    client: reqwest::Client,
    refresher: Arc<R>,
}

impl<R: TokenRefresher> BaseTokenManager<R> {
    /// Create a token manager with file-based persistence.
    pub fn new(tokens: OAuthTokens, path: PathBuf, refresher: R) -> Self {
        Self {
            tokens: Arc::new(RwLock::new(tokens)),
            storage_path: path,
            client: reqwest::Client::new(),
            refresher: Arc::new(refresher),
        }
    }

    /// Return a reference to the shared HTTP client.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Return a reference to the tokens lock for provider-specific reads.
    pub fn tokens(&self) -> &Arc<RwLock<OAuthTokens>> {
        &self.tokens
    }

    /// Load tokens from a file, or return `None` if it doesn't exist or is invalid.
    pub fn load_from_file(path: &Path, refresher: R) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let tokens: OAuthTokens = serde_json::from_str(&content).ok()?;
        Some(Self::new(tokens, path.to_path_buf(), refresher))
    }

    /// Save current tokens to the storage file with **0600** permissions.
    pub async fn save_tokens(&self) -> Result<()> {
        let content = {
            let tokens = self.tokens.read().await;
            serde_json::to_string_pretty(&*tokens)?
        };
        let path = self.storage_path.clone();

        // Write in a blocking task so we can use std::fs for permission control.
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Open with create | write | truncate and mode 0600.
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .with_context(|| format!("failed to open {}", path.display()))?;
            file.write_all(content.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            Ok(())
        })
        .await
        .context("save_tokens task panicked")??;

        Ok(())
    }

    /// Get a valid access token, refreshing if necessary.
    pub async fn get_access_token(&self) -> Result<String> {
        let needs_refresh = {
            let tokens = self.tokens.read().await;
            tokens.is_expired_or_expiring()
        };

        if needs_refresh {
            let mut tokens = self.tokens.write().await;
            // Double-check after acquiring write lock (another task may have refreshed).
            if tokens.is_expired_or_expiring() {
                tracing::info!("Access token expired or expiring, refreshing...");
                let mut new_tokens = self
                    .refresher
                    .refresh(&self.client, &tokens.refresh_token)
                    .await?;
                // Preserve account_id from old tokens if the refresher didn't set one.
                if new_tokens.account_id.is_none() {
                    new_tokens.account_id = tokens.account_id.clone();
                }
                *tokens = new_tokens;
                // Release lock before saving.
                drop(tokens);
                self.save_tokens().await?;
                tracing::info!("Token refreshed successfully");
            }
        }

        let tokens = self.tokens.read().await;
        Ok(tokens.access_token.clone())
    }
}

impl<R: TokenRefresher> llm_rs::llm::TokenProvider for BaseTokenManager<R> {
    fn get_access_token(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send>> {
        let tokens = Arc::clone(&self.tokens);
        let client = self.client.clone();
        let refresher = Arc::clone(&self.refresher);
        let storage_path = self.storage_path.clone();
        Box::pin(async move {
            let helper = BaseTokenManager {
                tokens,
                storage_path,
                client,
                refresher,
            };
            BaseTokenManager::get_access_token(&helper)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

// ---------------------------------------------------------------------------
// Subscription rate-limit snapshots
// ---------------------------------------------------------------------------

/// Utilisation percentage at or above which a window is fully consumed.
pub(crate) const EXHAUSTED_UTILIZATION: f64 = 100.0;

/// A single rate-limit window of a subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitWindow {
    /// Window label as reported by the provider: `"5h"`, `"7d"`,
    /// `"7d sonnet"`, `"7d opus"`, `"primary"`, `"secondary"`.
    pub name: String,
    /// Percentage of the window capacity consumed (0–100).
    pub utilization: f64,
    /// Moment at which the window resets, if the provider reports one.
    pub reset_at: Option<DateTime<Utc>>,
}

/// Provider-neutral view of a subscription's rate-limit state.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitSnapshot {
    /// Windows reported by the provider, in the provider's order.
    pub windows: Vec<RateLimitWindow>,
    /// Whether the provider reports that the subscription is out of quota.
    pub limit_reached: bool,
}

impl RateLimitSnapshot {
    /// Look up one window by its label.
    pub fn window(&self, name: &str) -> Option<&RateLimitWindow> {
        self.windows.iter().find(|window| window.name == name)
    }

    /// Latest reset among windows that are at or over the limit, ignoring
    /// resets that have already passed `now`.
    ///
    /// A request only succeeds again once every exhausted window has reset, so
    /// the latest of them wins. When the provider reports `limit_reached` but no
    /// window reports full utilisation, every window's reset is considered. A
    /// usage snapshot can be minutes old, so a reset in the past says nothing
    /// about the next one and yields `None`.
    pub fn blocking_reset_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut exhausted = self
            .windows
            .iter()
            .filter(|window| window.utilization >= EXHAUSTED_UTILIZATION)
            .peekable();
        let blocking: &mut dyn Iterator<Item = &RateLimitWindow> = if exhausted.peek().is_some() {
            &mut exhausted
        } else if self.limit_reached {
            &mut self.windows.iter()
        } else {
            return None;
        };

        blocking
            .filter_map(|window| window.reset_at)
            .filter(|reset_at| *reset_at > now)
            .max()
    }
}

// ---------------------------------------------------------------------------
// OAuthTokenManager trait
// ---------------------------------------------------------------------------

/// Common trait for OAuth token managers across providers.
///
/// Extends [`llm_rs::llm::TokenProvider`] with provider-specific
/// HTTP client access and subscription usage fetching.
#[async_trait::async_trait]
pub trait OAuthTokenManager: llm_rs::llm::TokenProvider + Send + Sync {
    /// Return a reference to the shared HTTP client.
    fn client(&self) -> &reqwest::Client;

    /// Fetch the subscription's rate-limit windows.
    ///
    /// Returns `Ok(None)` if the provider exposes no usage data.
    async fn fetch_rate_limit_snapshot(&self) -> Result<Option<RateLimitSnapshot>>;

    /// Fetch usage data and format it as a human-readable one-line summary.
    ///
    /// Returns `Ok(None)` if usage data is unavailable.
    async fn fetch_formatted_usage(&self) -> Result<Option<String>>;
}

/// [`llm_rs::llm::TokenProvider`] over a token manager shared with the caller.
///
/// Lets the LLM client and the server state work on one token set, so a refresh
/// triggered by either side is visible to both.
#[derive(Clone)]
pub struct SharedTokenProvider(Arc<dyn OAuthTokenManager>);

impl SharedTokenProvider {
    pub fn new(token_manager: Arc<dyn OAuthTokenManager>) -> Self {
        Self(token_manager)
    }
}

impl llm_rs::llm::TokenProvider for SharedTokenProvider {
    fn get_access_token(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send>> {
        self.0.get_access_token()
    }
}

/// Format the time remaining until a usage window resets as a human-readable string.
///
/// Returns strings like `"2h 13m"`, `"45m"`, `"3h 0m"`, or `"now"` when the
/// reset time is already in the past.
pub(crate) fn format_duration_until(reset_time: DateTime<Utc>) -> String {
    let duration = reset_time.signed_duration_since(Utc::now());

    if duration.num_seconds() <= 0 {
        return "now".to_string();
    }

    let total_minutes = duration.num_minutes();
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}
