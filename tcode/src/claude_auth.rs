use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

use auth::{CLIENT_ID, OAuthTokens};
pub use auth::{TokenManager, load_token_manager};

const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

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

    let json: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse token response")?;

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
        .expect("system clock before UNIX epoch")
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
    let manager = TokenManager::new(tokens.clone(), storage_path.clone());
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
