//! Claude OAuth token management.
//!
//! Reuses token files created by `tcode claude-auth` at
//! `~/.tcode/auth/claude_tokens.json`. Handles automatic refresh
//! when the access token is expired or about to expire.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::RwLock;

/// Buffer time before expiry to trigger refresh (5 minutes).
const REFRESH_BUFFER_SECS: u64 = 300;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

impl OAuthTokens {
    fn is_expired_or_expiring(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.expires_at <= now + REFRESH_BUFFER_SECS
    }
}

async fn refresh_tokens(refresh_token: &str) -> Result<OAuthTokens> {
    let client = reqwest::Client::new();
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
        .unwrap()
        .as_secs()
        + expires_in;

    Ok(OAuthTokens {
        access_token,
        refresh_token: new_refresh_token,
        expires_at,
    })
}

/// Thread-safe token manager with auto-refresh and file persistence.
#[derive(Clone)]
pub struct TokenManager {
    tokens: Arc<RwLock<OAuthTokens>>,
    storage_path: PathBuf,
}

impl TokenManager {
    /// Default token path shared with tcode: `~/.tcode/auth/claude_tokens.json`.
    pub fn default_storage_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".tcode")
            .join("auth")
            .join("claude_tokens.json")
    }

    /// Load tokens from the given file. Returns `None` if file missing or invalid.
    pub fn load_from_file(path: &PathBuf) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let tokens: OAuthTokens = serde_json::from_str(&content).ok()?;
        Some(Self {
            tokens: Arc::new(RwLock::new(tokens)),
            storage_path: path.clone(),
        })
    }

    async fn save_tokens(&self) -> Result<()> {
        let tokens = self.tokens.read().await;
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&*tokens)?;
        std::fs::write(&self.storage_path, content)?;
        Ok(())
    }

    /// Return a valid access token, refreshing transparently if needed.
    pub async fn get_access_token(&self) -> Result<String> {
        let needs_refresh = {
            let tokens = self.tokens.read().await;
            tokens.is_expired_or_expiring()
        };

        if needs_refresh {
            let mut tokens = self.tokens.write().await;
            // Double-check after acquiring write lock
            if tokens.is_expired_or_expiring() {
                tracing::info!("Claude OAuth token expired or expiring, refreshing...");
                let new_tokens = refresh_tokens(&tokens.refresh_token).await?;
                *tokens = new_tokens;
                drop(tokens);
                self.save_tokens().await?;
                tracing::info!("Token refreshed successfully");
            }
        }

        let tokens = self.tokens.read().await;
        Ok(tokens.access_token.clone())
    }
}

/// Try loading the token manager from the default tcode token file.
pub fn load_token_manager() -> Option<TokenManager> {
    let path = TokenManager::default_storage_path();
    TokenManager::load_from_file(&path)
}
