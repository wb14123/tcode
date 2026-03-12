//! Axum request handlers and router construction.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::cors::CorsLayer;

use llm_rs::llm::LLM;

use crate::auth::auth_middleware;
use crate::convert::{convert_chat_options, convert_request_messages, convert_request_tools};
use crate::error::AppError;
use crate::stream::{non_streaming_response, streaming_response};
use crate::types::{ChatCompletionRequest, ModelObject, ModelsResponse};

pub struct AppState {
    pub llm: Box<dyn LLM>,
    pub auth_tokens: std::collections::HashSet<String>,
}

pub async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Response, AppError> {
    let messages = convert_request_messages(&req.messages)?;
    let options = convert_chat_options(req.max_tokens, req.reasoning.as_ref());
    let model = req.model.clone();
    let stream_requested = req.stream;
    let stream_options = req.stream_options;

    let mut llm = state.llm.clone_box();
    if let Some(ref tools) = req.tools {
        let sentinel_tools = convert_request_tools(tools)?;
        llm.register_tools(sentinel_tools);
    }

    let llm_stream = llm.chat(&model, &messages, &options);

    if stream_requested {
        Ok(streaming_response(llm_stream, model, stream_options).into_response())
    } else {
        let resp = non_streaming_response(llm_stream, model).await?;
        Ok(Json(resp).into_response())
    }
}

pub async fn list_models(State(state): State<Arc<AppState>>) -> Json<ModelsResponse> {
    let models = state.llm.available_models();
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Json(ModelsResponse {
        object: "list".into(),
        data: models
            .into_iter()
            .map(|m| ModelObject {
                id: m.id,
                object: "model".into(),
                created,
                owned_by: "llm-rs".into(),
            })
            .collect(),
    })
}

/// Build the Axum router with all routes and middleware.
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(CorsLayer::permissive())
        .with_state(state)
}
