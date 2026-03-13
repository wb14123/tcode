use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::{ErrorDetail, ErrorResponse};

/// Shared set of valid bearer tokens.
pub struct TokenSet {
    tokens: Vec<String>,
}

impl TokenSet {
    /// Load tokens from a JSON file. The file should contain a JSON array of strings.
    pub fn from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let tokens: Vec<String> = serde_json::from_str(&data)?;
        Ok(Self { tokens })
    }

    pub fn contains(&self, token: &str) -> bool {
        self.tokens.iter().any(|t| t == token)
    }
}

/// Axum middleware that validates bearer tokens for TCP mode.
pub async fn bearer_auth(
    request: Request,
    next: Next,
) -> Response {
    let token_set = request.extensions().get::<Arc<TokenSet>>();
    let token_set = match token_set {
        Some(ts) => ts.clone(),
        None => return next.run(request).await,
    };

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..];
            if token_set.contains(token) {
                next.run(request).await
            } else {
                unauthorized("Invalid bearer token")
            }
        }
        _ => unauthorized("Missing or malformed Authorization header"),
    }
}

fn unauthorized(message: &str) -> Response {
    let body = ErrorResponse {
        error: ErrorDetail {
            message: message.to_string(),
            error_type: "auth_error".to_string(),
        },
    };
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}
