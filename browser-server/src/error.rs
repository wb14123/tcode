use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::ErrorResponse;
use crate::ErrorDetail;

/// Application error type that converts to an Axum response.
pub struct AppError(pub anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = ErrorResponse {
            error: ErrorDetail {
                message: self.0.to_string(),
                error_type: "browser_error".to_string(),
            },
        };
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
