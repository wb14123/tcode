//! Bearer token authentication middleware.
//!
//! All requests must carry a valid `Authorization: Bearer <token>` header.
//! Valid tokens are loaded from a token file on startup.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::handler::AppState;

/// Default token file location.
pub fn default_token_file() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("llm-rs-server")
        .join("tokens.json")
}

/// Load tokens from the file. Exits with an error if the file is missing or empty.
pub fn load_tokens(path: &Path) -> Result<HashSet<String>> {
    if !path.exists() {
        anyhow::bail!(
            "token file not found: {}\n\
             Create it with a JSON array of allowed bearer tokens, e.g.:\n  \
             mkdir -p {} && echo '[\"my-secret-token\"]' > {}",
            path.display(),
            path.parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            path.display(),
        );
    }
    let content = std::fs::read_to_string(path).context("failed to read token file")?;
    let tokens: Vec<String> =
        serde_json::from_str(&content).context("token file must be a JSON array of strings")?;
    if tokens.is_empty() {
        anyhow::bail!(
            "token file {} contains no tokens — add at least one",
            path.display()
        );
    }
    Ok(tokens.into_iter().collect())
}

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, AppError> {
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..];
            if state.auth_tokens.contains(token) {
                Ok(next.run(request).await)
            } else {
                Err(AppError::Unauthorized)
            }
        }
        _ => Err(AppError::Unauthorized),
    }
}
