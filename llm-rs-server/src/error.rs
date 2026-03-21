//! Application error type with OpenAI-compatible JSON error responses.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized,
    LLMError(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::BadRequest(msg) => write!(f, "Bad request: {msg}"),
            AppError::Unauthorized => write!(f, "Unauthorized"),
            AppError::LLMError(msg) => write!(f, "LLM error: {msg}"),
            AppError::Internal(e) => write!(f, "Internal error: {e}"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::Internal(e) => e.source(),
            _ => None,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "invalid_request_error", msg),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Invalid API key".to_string(),
            ),
            AppError::LLMError(msg) => (StatusCode::BAD_GATEWAY, "api_error", msg),
            AppError::Internal(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                e.to_string(),
            ),
        };

        let body = serde_json::json!({
            "error": {
                "message": message,
                "type": error_type,
            }
        });

        (status, Json(body)).into_response()
    }
}
