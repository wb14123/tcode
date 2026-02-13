use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Buffer time before expiry to trigger refresh (5 minutes in seconds)
const REFRESH_BUFFER_SECS: u64 = 300;

fn generate_code_verifier() -> String {
    let random_bytes: [u8; 32] = rand::rng().random();
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn generate_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

fn build_auth_url(challenge: &str, verifier: &str) -> String {
    let params = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", verifier),
    ];

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    // Use claude.ai for Claude Max OAuth flow
    format!("https://claude.ai/oauth/authorize?{}", query)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

impl OAuthTokens {
    /// Check if the token is expired or will expire soon (within REFRESH_BUFFER_SECS)
    fn is_expired_or_expiring(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.expires_at <= now + REFRESH_BUFFER_SECS
    }
}

/// Refresh the access token using the refresh token.
/// Returns new tokens with updated access_token and potentially new refresh_token.
pub async fn refresh_tokens(refresh_token: &str) -> Result<OAuthTokens> {
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

    let json: serde_json::Value = response.json().await.context("Failed to parse refresh response")?;

    let access_token = json["access_token"]
        .as_str()
        .context("No access_token in refresh response")?
        .to_string();
    // The refresh response may include a new refresh_token
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

/// Token manager that handles automatic token refresh.
/// Thread-safe and can be shared across async tasks.
#[derive(Clone)]
pub struct TokenManager {
    tokens: Arc<RwLock<OAuthTokens>>,
    storage_path: Option<PathBuf>,
}

impl TokenManager {
    /// Create a token manager with file-based persistence.
    fn with_storage(tokens: OAuthTokens, path: PathBuf) -> Self {
        Self {
            tokens: Arc::new(RwLock::new(tokens)),
            storage_path: Some(path),
        }
    }

    /// Load tokens from a file, or return None if file doesn't exist or is invalid.
    pub fn load_from_file(path: &PathBuf) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let tokens: OAuthTokens = serde_json::from_str(&content).ok()?;
        Some(Self::with_storage(tokens, path.clone()))
    }

    /// Get the default token storage path (~/.config/tcode/claude_tokens.json)
    pub fn default_storage_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".tcode")
            .join("auth")
            .join("claude_tokens.json")
    }

    /// Save current tokens to the storage file (if configured).
    pub async fn save_tokens(&self) -> Result<()> {
        if let Some(ref path) = self.storage_path {
            let tokens = self.tokens.read().await;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let content = serde_json::to_string_pretty(&*tokens)?;
            std::fs::write(path, content)?;
        }
        Ok(())
    }

    /// Get a valid access token, refreshing if necessary.
    /// This is the main method to use when making API requests.
    pub async fn get_access_token(&self) -> Result<String> {
        // First, check if we need to refresh (read lock)
        let needs_refresh = {
            let tokens = self.tokens.read().await;
            tokens.is_expired_or_expiring()
        };

        if needs_refresh {
            // Acquire write lock and refresh
            let mut tokens = self.tokens.write().await;
            // Double-check after acquiring write lock (another task may have refreshed)
            if tokens.is_expired_or_expiring() {
                tracing::info!("Access token expired or expiring, refreshing...");
                let new_tokens = refresh_tokens(&tokens.refresh_token).await?;
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

async fn exchange_code(code: &str, verifier: &str) -> Result<OAuthTokens> {
    let (auth_code, state) = code
        .split_once('#')
        .map(|(c, s)| (c.to_string(), s.to_string()))
        .unwrap_or_else(|| (code.to_string(), String::new()));

    let client = reqwest::Client::new();
    let response = client
        .post("https://console.anthropic.com/v1/oauth/token")
        .json(&serde_json::json!({
            "code": auth_code,
            "state": state,
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .send()
        .await
        .context("Failed to exchange authorization code")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed ({}): {}", status, body);
    }

    let json: serde_json::Value = response.json().await.context("Failed to parse token response")?;

    let access_token = json["access_token"]
        .as_str()
        .context("No access_token in response")?
        .to_string();
    let refresh_token = json["refresh_token"]
        .as_str()
        .context("No refresh_token in response")?
        .to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(3600);
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + expires_in;

    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at,
    })
}

fn read_line() -> Result<String> {
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("Failed to read input")?;
    Ok(input.trim().to_string())
}

pub async fn run() -> Result<()> {
    let verifier = generate_code_verifier();
    let challenge = generate_code_challenge(&verifier);

    let auth_url = build_auth_url(&challenge, &verifier);
    println!("Open this URL in your browser:");
    println!("{}", auth_url);
    println!();

    print!("Paste the authorization code here: ");
    io::stdout().flush()?;
    let code = read_line()?;

    if code.is_empty() {
        anyhow::bail!("No authorization code provided");
    }

    println!("Exchanging code for tokens...");
    let tokens = exchange_code(&code, &verifier).await?;

    // Save tokens to file for persistence
    let storage_path = TokenManager::default_storage_path();
    let manager = TokenManager::with_storage(tokens.clone(), storage_path.clone());
    manager.save_tokens().await?;

    println!();
    println!("Tokens saved to: {}", storage_path.display());
    println!();
    println!("Token details:");
    println!("{}", serde_json::to_string_pretty(&tokens)?);
    println!();
    println!("You can now use tcode with Claude. The tokens will auto-refresh when needed.");

    Ok(())
}

/// Load or create a token manager from the default storage location.
/// Returns None if no stored tokens exist.
pub fn load_token_manager() -> Option<TokenManager> {
    let path = TokenManager::default_storage_path();
    TokenManager::load_from_file(&path)
}
