pub mod browser;
pub mod web_fetch;
pub mod web_search;

pub mod auth;
mod error;
pub mod handler;

pub use handler::AppState;
pub use handler::build_app;
pub use handler::build_app_unix;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngineKind {
    Kagi,
    #[default]
    Google,
}

/// Request for the /web_search endpoint.
#[derive(Debug, Serialize, Deserialize)]
pub struct WebSearchRequest {
    pub query: String,
    #[serde(default)]
    pub engine: SearchEngineKind,
}

/// Response from the /web_search endpoint.
#[derive(Debug, Serialize, Deserialize)]
pub struct WebSearchResponse {
    pub content: String,
}

/// Request for the /web_fetch endpoint.
#[derive(Debug, Serialize, Deserialize)]
pub struct WebFetchRequest {
    pub url: String,
    /// Maximum number of characters to return (default: 20000).
    #[serde(default)]
    pub max_length: Option<u32>,
    /// Number of characters to skip from the start of the content (default: 0).
    #[serde(default)]
    pub skip_chars: Option<u32>,
}

/// Response from the /web_fetch endpoint.
#[derive(Debug, Serialize, Deserialize)]
pub struct WebFetchResponse {
    pub content: String,
    /// Total length of the full content before truncation.
    pub total_length: u32,
    /// Whether the content was truncated (by skip_chars or max_length).
    pub is_truncated: bool,
}

/// Error response body.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

/// Detail inside an error response.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
}

/// Health check response.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

#[cfg(test)]
mod browser_tests;

#[cfg(test)]
mod web_fetch_tests;
