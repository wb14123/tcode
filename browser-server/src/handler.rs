use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::error::AppError;
use crate::{
    HealthResponse, WebFetchRequest, WebFetchResponse, WebSearchRequest, WebSearchResponse,
};

/// Shared application state.
pub struct AppState {
    /// Epoch-seconds timestamp of the last request (for idle timeout).
    pub last_activity: AtomicU64,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            last_activity: AtomicU64::new(now_secs()),
        }
    }

    pub fn touch(&self) {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Build the Axum router with all endpoints.
pub fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/web_search", post(handle_web_search))
        .route("/web_fetch", post(handle_web_fetch))
        .route("/health", get(handle_health))
        .with_state(state)
}

async fn handle_web_search(
    State(state): State<Arc<AppState>>,
    Json(req): Json<WebSearchRequest>,
) -> Result<Json<WebSearchResponse>, AppError> {
    state.touch();
    let query = req.query;
    let results = tokio::task::spawn_blocking(move || {
        crate::web_search::search_and_extract(&query)
    })
    .await
    .map_err(|e| AppError(e.into()))??;

    state.touch();
    Ok(Json(WebSearchResponse { results }))
}

async fn handle_web_fetch(
    State(state): State<Arc<AppState>>,
    Json(req): Json<WebFetchRequest>,
) -> Result<Json<WebFetchResponse>, AppError> {
    state.touch();
    let url = req.url;
    let content = tokio::task::spawn_blocking(move || {
        crate::web_fetch::fetch_and_extract(&url)
    })
    .await
    .map_err(|e| AppError(e.into()))??;

    state.touch();
    Ok(Json(WebFetchResponse { content }))
}

async fn handle_health(
    State(state): State<Arc<AppState>>,
) -> Json<HealthResponse> {
    state.touch();
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}
