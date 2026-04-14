//! OpenAI OAuth token management.
//!
//! Handles loading, refreshing, and persisting OAuth tokens from
//! `~/.tcode/auth/openai_tokens.json`.

pub mod usage;

use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;

// Re-export so callers using `auth::openai::OAuthTokens` still work.
pub use crate::OAuthTokens;

/// OAuth client ID for the OpenAI Codex OAuth flow.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Token refresh endpoint.
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";

// ---------------------------------------------------------------------------
// JWT helper
// ---------------------------------------------------------------------------

/// Decode a JWT's payload segment and extract the `exp` claim.
///
/// Performs **no** signature verification — we only need the expiry time
/// and the server validates the token on each request.
///
/// Manually pads the payload to a multiple of 4 before decoding, so both
/// padded and unpadded JWT implementations are handled correctly.
pub fn parse_jwt_exp(token: &str) -> Option<u64> {
    let json = decode_jwt_payload(token)?;
    json["exp"].as_u64()
}

/// Decode a JWT's payload segment and extract the `chatgpt_account_id` claim.
///
/// This is present in the `id_token` returned by OpenAI's OAuth flow and
/// is required as the `ChatGPT-Account-ID` header for API calls via the
/// ChatGPT backend proxy.
pub fn parse_jwt_account_id(token: &str) -> Option<String> {
    let json = decode_jwt_payload(token)?;
    json["chatgpt_account_id"].as_str().map(|s| s.to_string())
}

/// Decode a JWT's payload (second segment) into a JSON value.
fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    // Standard JWTs use base64url without padding (RFC 7515 §2), but some
    // implementations include padding. Manually pad to handle both.
    let mut payload = parts[1].to_string();
    match payload.len() % 4 {
        2 => payload.push_str("=="),
        3 => payload.push('='),
        _ => {}
    }
    let decoded = base64::engine::general_purpose::URL_SAFE
        .decode(&payload)
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

// ---------------------------------------------------------------------------
// Refresher
// ---------------------------------------------------------------------------

/// OpenAI-specific token refresher.
#[derive(Debug, Clone, Copy)]
pub struct OpenAiRefresher;

#[async_trait::async_trait]
impl crate::TokenRefresher for OpenAiRefresher {
    async fn refresh(&self, client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens> {
        refresh_tokens(client, refresh_token).await
    }
}

/// Refresh the access token using the refresh token.
///
/// POSTs a **JSON** body (not form-urlencoded) to the OpenAI token endpoint.
/// Returns new tokens with updated `access_token` and potentially new `refresh_token`.
pub async fn refresh_tokens(client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens> {
    let response = client
        .post(REFRESH_URL)
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .send()
        .await
        .context("Failed to refresh OpenAI token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI token refresh failed ({}): {}", status, body);
    }

    let json: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse OpenAI refresh response")?;

    let access_token = json["access_token"]
        .as_str()
        .context("No access_token in OpenAI refresh response")?
        .to_string();
    let new_refresh_token = json["refresh_token"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| refresh_token.to_string());

    // Determine expiry from the JWT `exp` claim; fall back to now + 1 hour.
    let expires_at = parse_jwt_exp(&access_token).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 3600
    });

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

/// OpenAI-specific token manager (type alias over the shared generic).
pub type TokenManager = crate::BaseTokenManager<OpenAiRefresher>;

impl TokenManager {
    /// Default token storage path: `~/.tcode/auth/openai_tokens.json`.
    pub fn default_storage_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".tcode")
            .join("auth")
            .join("openai_tokens.json")
    }

    /// Get the stored ChatGPT account ID (if present).
    pub async fn account_id(&self) -> Option<String> {
        self.tokens().read().await.account_id.clone()
    }
}

#[async_trait::async_trait]
impl crate::OAuthTokenManager for TokenManager {
    fn client(&self) -> &reqwest::Client {
        crate::BaseTokenManager::client(self)
    }

    async fn fetch_formatted_usage(&self) -> anyhow::Result<Option<String>> {
        let token = self.get_access_token().await?;
        let account_id = self.account_id().await;
        let client = crate::BaseTokenManager::client(self);
        let payload = usage::fetch_usage(client, &token, account_id.as_deref()).await?;
        Ok(Some(usage::format_usage(&payload)))
    }
}

/// Load a token manager from the default storage location.
/// Returns `None` if no stored tokens exist.
pub fn load_token_manager() -> Option<TokenManager> {
    let path = TokenManager::default_storage_path();
    TokenManager::load_from_file(&path, OpenAiRefresher)
}
