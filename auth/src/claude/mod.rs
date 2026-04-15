//! Claude OAuth token management.
//!
//! Handles loading, refreshing, and persisting OAuth tokens from
//! the provider's profile-aware `~/.tcode/auth/claude_tokens*.json` files.

pub mod usage;

use std::path::PathBuf;

use anyhow::{Context, Result};

// Re-export so callers using `auth::claude::OAuthTokens` still work.
pub use crate::OAuthTokens;

/// OAuth client ID for the Claude Max OAuth flow.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Token refresh endpoint.
const REFRESH_URL: &str = "https://console.anthropic.com/v1/oauth/token";

// ---------------------------------------------------------------------------
// Refresher
// ---------------------------------------------------------------------------

/// Claude-specific token refresher.
#[derive(Debug, Clone, Copy)]
pub struct ClaudeRefresher;

#[async_trait::async_trait]
impl crate::TokenRefresher for ClaudeRefresher {
    async fn refresh(&self, client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens> {
        refresh_tokens(client, refresh_token).await
    }
}

/// Refresh the access token using the refresh token.
/// Returns new tokens with updated access_token and potentially new refresh_token.
pub async fn refresh_tokens(client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens> {
    let response = client
        .post(REFRESH_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .context("Failed to refresh token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Token refresh failed ({}): {}", status, body);
    }

    let json: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse refresh response")?;

    let access_token = json["access_token"]
        .as_str()
        .context("No access_token in refresh response")?
        .to_string();
    let new_refresh_token = json["refresh_token"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| refresh_token.to_string());
    let expires_in = json["expires_in"].as_u64().unwrap_or(3600);
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + expires_in;

    Ok(OAuthTokens {
        access_token,
        refresh_token: new_refresh_token,
        expires_at,
        account_id: None,
    })
}

// ---------------------------------------------------------------------------
// TokenManager type alias + convenience helpers
// ---------------------------------------------------------------------------

/// Claude-specific token manager (type alias over the shared generic).
pub type TokenManager = crate::BaseTokenManager<ClaudeRefresher>;

impl TokenManager {
    /// Resolve the token storage path for an optional profile.
    pub fn storage_path(profile: Option<&str>) -> PathBuf {
        crate::oauth_token_storage_path(crate::OAuthProvider::Claude, profile)
    }

    #[cfg(test)]
    pub(crate) fn storage_path_in(home_dir: &std::path::Path, profile: Option<&str>) -> PathBuf {
        crate::oauth_token_storage_path_in(home_dir, crate::OAuthProvider::Claude, profile)
    }

    /// Default token storage path: `~/.tcode/auth/claude_tokens.json`.
    pub fn default_storage_path() -> PathBuf {
        Self::storage_path(None)
    }

    /// Load a token manager for an optional profile.
    pub fn load(profile: Option<&str>) -> Option<Self> {
        let path = Self::storage_path(profile);
        Self::load_from_file(&path, ClaudeRefresher)
    }

    #[cfg(test)]
    pub(crate) fn load_in(home_dir: &std::path::Path, profile: Option<&str>) -> Option<Self> {
        let path = Self::storage_path_in(home_dir, profile);
        Self::load_from_file(&path, ClaudeRefresher)
    }
}

#[async_trait::async_trait]
impl crate::OAuthTokenManager for TokenManager {
    fn client(&self) -> &reqwest::Client {
        // Call the inherent method from the generic BaseTokenManager.
        crate::BaseTokenManager::client(self)
    }

    async fn fetch_formatted_usage(&self) -> anyhow::Result<Option<String>> {
        let token = self.get_access_token().await?;
        let client = crate::BaseTokenManager::client(self);
        let u = usage::fetch_usage(client, &token).await?;
        Ok(Some(usage::format_usage(&u)))
    }
}

/// Load a token manager from the default storage location.
/// Returns `None` if no stored tokens exist.
pub fn load_token_manager() -> Option<TokenManager> {
    TokenManager::load(None)
}
