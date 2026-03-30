//! Shared Claude OAuth token management.
//!
//! Handles loading, refreshing, and persisting OAuth tokens from
//! `~/.tcode/auth/claude_tokens.json`.

pub mod usage;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::RwLock;

/// OAuth client ID for the Claude Max OAuth flow.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Buffer time before expiry to trigger refresh (5 minutes in seconds).
const REFRESH_BUFFER_SECS: u64 = 300;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

impl OAuthTokens {
    /// Check if the token is expired or will expire soon (within REFRESH_BUFFER_SECS).
    pub fn is_expired_or_expiring(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.expires_at <= now + REFRESH_BUFFER_SECS
    }
}

/// Refresh the access token using the refresh token.
/// Returns new tokens with updated access_token and potentially new refresh_token.
pub async fn refresh_tokens(client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens> {
    let response = client
        .post("https://console.anthropic.com/v1/oauth/token")
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
    })
}

/// Token manager that handles automatic token refresh.
/// Thread-safe and can be shared across async tasks.
#[derive(Clone)]
pub struct TokenManager {
    tokens: Arc<RwLock<OAuthTokens>>,
    storage_path: PathBuf,
    client: reqwest::Client,
}

impl TokenManager {
    /// Create a token manager with file-based persistence.
    pub fn new(tokens: OAuthTokens, path: PathBuf) -> Self {
        Self {
            tokens: Arc::new(RwLock::new(tokens)),
            storage_path: path,
            client: reqwest::Client::new(),
        }
    }

    /// Default token storage path: `~/.tcode/auth/claude_tokens.json`.
    pub fn default_storage_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".tcode")
            .join("auth")
            .join("claude_tokens.json")
    }

    /// Return a reference to the shared HTTP client.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Load tokens from a file, or return None if file doesn't exist or is invalid.
    pub fn load_from_file(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let tokens: OAuthTokens = serde_json::from_str(&content).ok()?;
        Some(Self::new(tokens, path.to_path_buf()))
    }

    /// Save current tokens to the storage file.
    pub async fn save_tokens(&self) -> Result<()> {
        let tokens = self.tokens.read().await;
        if let Some(parent) = self.storage_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(&*tokens)?;
        tokio::fs::write(&self.storage_path, content).await?;
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
            // Double-check after acquiring write lock (another task may have refreshed)
            if tokens.is_expired_or_expiring() {
                tracing::info!("Access token expired or expiring, refreshing...");
                let new_tokens = refresh_tokens(&self.client, &tokens.refresh_token).await?;
                *tokens = new_tokens;
                // Release lock before saving
                drop(tokens);
                self.save_tokens().await?;
                tracing::info!("Token refreshed successfully");
            }
        }

        let tokens = self.tokens.read().await;
        Ok(tokens.access_token.clone())
    }
}

impl llm_rs::llm::TokenProvider for TokenManager {
    fn get_access_token(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let this = self.clone();
        Box::pin(async move { this.get_access_token().await.map_err(|e| e.to_string()) })
    }
}

/// Load a token manager from the default storage location.
/// Returns None if no stored tokens exist.
pub fn load_token_manager() -> Option<TokenManager> {
    let path = TokenManager::default_storage_path();
    TokenManager::load_from_file(&path)
}
