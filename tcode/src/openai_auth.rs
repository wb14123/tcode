use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write as _};
use std::net::TcpListener;

use auth::openai::{CLIENT_ID, OAuthTokens, TokenManager, parse_jwt_exp};

const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const LISTEN_ADDR: &str = "127.0.0.1:1455";

/// Generate a PKCE code verifier (64 random bytes → base64url-no-pad, 86 chars).
fn generate_code_verifier() -> String {
    let random_bytes: [u8; 64] = rand::rng().random();
    URL_SAFE_NO_PAD.encode(random_bytes)
}

/// Generate the PKCE code challenge (SHA256 of verifier → base64url-no-pad).
fn generate_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Generate a random state parameter (32 random bytes → base64url-no-pad).
fn generate_state() -> String {
    let random_bytes: [u8; 32] = rand::rng().random();
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn build_auth_url(challenge: &str, state: &str) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
    ];

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{}?{}", AUTHORIZE_URL, query)
}

/// HTML page returned to the browser on success.
const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html>
<head><title>Authentication Successful</title></head>
<body style="font-family: sans-serif; text-align: center; padding-top: 50px;">
<h1>&#x2705; Authentication Successful</h1>
<p>You can close this window and return to the terminal.</p>
</body>
</html>"#;

/// Escape HTML special characters to prevent injection.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// HTML page returned to the browser on error.
fn error_html(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head><title>Authentication Error</title></head>
<body style="font-family: sans-serif; text-align: center; padding-top: 50px;">
<h1>&#x274C; Authentication Error</h1>
<p>{}</p>
</body>
</html>"#,
        html_escape(message)
    )
}

/// Send a minimal HTTP response on a TCP stream.
fn send_http_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        body.len(),
        body
    );
    if let Err(e) = stream.write_all(response.as_bytes()) {
        tracing::warn!("Failed to write HTTP response to browser: {e}");
        return;
    }
    if let Err(e) = stream.flush() {
        tracing::warn!("Failed to flush HTTP response to browser: {e}");
    }
}

/// Parse the query parameters from the first line of an HTTP request.
/// E.g., `GET /auth/callback?code=abc&state=xyz HTTP/1.1` → `[("code","abc"), ("state","xyz")]`
fn parse_callback_request(request_line: &str) -> Option<Vec<(String, String)>> {
    // Expected: "GET /auth/callback?... HTTP/1.1"
    let path = request_line.split_whitespace().nth(1)?;
    if !path.starts_with("/auth/callback") {
        return None;
    }
    let query = path.split_once('?').map(|(_, q)| q)?;
    let params = query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((
                urlencoding::decode(k).ok()?.into_owned(),
                urlencoding::decode(v).ok()?.into_owned(),
            ))
        })
        .collect();
    Some(params)
}

/// Exchange authorization code for tokens via form-urlencoded POST.
async fn exchange_code(code: &str, verifier: &str) -> Result<OAuthTokens> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ];
    let response = client
        .post(TOKEN_URL)
        .form(&params)
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

    // Extract chatgpt_account_id from the id_token JWT.
    // This is required as the ChatGPT-Account-ID header for API calls
    // routed through the ChatGPT backend proxy.
    let account_id = json["id_token"]
        .as_str()
        .and_then(auth::openai::parse_jwt_account_id);
    if account_id.is_none() {
        tracing::warn!("No chatgpt_account_id found in id_token — API calls may fail");
    }

    // Compute expires_at from JWT exp claim, fallback to now + 3600
    let expires_at = parse_jwt_exp(&access_token).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 3600
    });

    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at,
        account_id,
    })
}

pub async fn run() -> Result<()> {
    let verifier = generate_code_verifier();
    let challenge = generate_code_challenge(&verifier);
    let state = generate_state();

    // Bind TCP listener before opening the browser
    let listener = TcpListener::bind(LISTEN_ADDR).context(
        "Failed to bind to 127.0.0.1:1455. Is another instance running? Close it and try again.",
    )?;

    let auth_url = build_auth_url(&challenge, &state);

    // Always print the URL
    println!("Opening browser for OpenAI authentication...");
    println!();
    println!("If the browser doesn't open automatically, visit this URL:");
    println!("{}", auth_url);
    println!();
    println!("Tip: If you're in a remote SSH session, set up port forwarding:");
    println!("  ssh -L 1455:localhost:1455 ...");
    println!();

    // Try to open browser
    if let Err(e) = webbrowser::open(&auth_url) {
        tracing::warn!("Could not open browser automatically: {}", e);
    }

    println!(
        "Waiting for authentication callback on {} (timeout: 5 minutes)...",
        LISTEN_ADDR
    );
    println!("Press Ctrl+C to cancel.");

    // Wait for the callback (state is validated inside)
    let code = wait_for_callback(&listener, &state)?;

    println!("Authorization code received. Exchanging for tokens...");
    let tokens = exchange_code(&code, &verifier).await?;

    // Save tokens
    let storage_path = TokenManager::default_storage_path();
    let manager = TokenManager::new(tokens, storage_path.clone(), auth::openai::OpenAiRefresher);
    manager.save_tokens().await?;

    println!();
    println!("Authentication successful!");
    println!("Tokens saved to: {}", storage_path.display());
    println!();
    println!("You can now use tcode with OpenAI OAuth (provider = \"open-ai-oauth\").");

    Ok(())
}

/// Wait for the OAuth callback on the TCP listener.
/// Validates the `state` parameter and returns the authorization `code`.
/// Times out after 5 minutes.
fn wait_for_callback(listener: &TcpListener, expected_state: &str) -> Result<String> {
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
    const POLL_INTERVAL: Duration = Duration::from_millis(250);

    let deadline = Instant::now() + TIMEOUT;

    listener
        .set_nonblocking(true)
        .context("Failed to set listener to non-blocking mode")?;

    loop {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "Timed out after 5 minutes waiting for OAuth callback. Please try again."
            );
        }

        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            Err(e) => return Err(anyhow::anyhow!(e).context("Failed to accept connection")),
        };

        // Set stream back to blocking with a read timeout so a half-open
        // connection (e.g. port scanner) doesn't hang the auth flow.
        stream
            .set_nonblocking(false)
            .context("Failed to set stream to blocking mode")?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .context("Failed to set stream read timeout")?;

        // Read the first line of the HTTP request
        let mut reader = std::io::BufReader::new(&stream);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .context("Failed to read HTTP request")?;

        // Parse callback parameters
        let params = match parse_callback_request(&request_line) {
            Some(p) => p,
            None => {
                // Not a callback request; send a simple 400 and continue listening
                drop(reader);
                send_http_response(&mut stream, 400, &error_html("Unexpected request."));
                continue;
            }
        };

        let find_param = |name: &str| -> Option<String> {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };

        // Drop the reader before writing to the stream
        drop(reader);

        // Check for OAuth error parameters first
        if let Some(error) = find_param("error") {
            let description = find_param("error_description")
                .unwrap_or_else(|| "No details provided.".to_string());
            let msg = format!("OAuth error: {} — {}", error, description);
            send_http_response(&mut stream, 400, &error_html(&msg));
            anyhow::bail!(msg);
        }

        let code = find_param("code");
        let received_state = find_param("state");

        if let (Some(code), Some(received_state)) = (code, received_state) {
            // Validate state before sending success
            if received_state != expected_state {
                let msg = "State mismatch: possible CSRF attack.";
                send_http_response(&mut stream, 400, &error_html(msg));
                anyhow::bail!("State mismatch: possible CSRF attack.");
            }
            send_http_response(&mut stream, 200, SUCCESS_HTML);
            return Ok(code);
        }

        // Missing parameters
        send_http_response(
            &mut stream,
            400,
            &error_html("Missing 'code' or 'state' parameter in callback."),
        );
    }
}
