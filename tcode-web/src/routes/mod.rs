use std::sync::Arc;

use axum::middleware::{from_fn, from_fn_with_state};

use crate::state::AppState;

mod api;
mod auth;
mod auth_middleware;
mod origin_middleware;
mod spa;

use auth_middleware::require_auth;

pub(crate) use auth::SESSION_COOKIE_NAME;

#[cfg(test)]
mod auth_tests;
#[cfg(test)]
mod enforcement_tests;
#[cfg(test)]
mod subagent_api_tests;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) fn protected_routes(state: Arc<AppState>) -> axum::Router<Arc<AppState>> {
    let reads = axum::Router::<Arc<AppState>>::new()
        .route("/api/sessions", axum::routing::get(api::get_sessions))
        .route(
            "/api/sessions/{session_id}/session-meta.json",
            axum::routing::get(api::get_session_meta),
        )
        .route(
            "/api/sessions/{session_id}/conversation-state.json",
            axum::routing::get(api::get_conversation_state),
        )
        .route(
            "/api/sessions/{session_id}/status.txt",
            axum::routing::get(api::get_session_status),
        )
        .route(
            "/api/sessions/{session_id}/usage.txt",
            axum::routing::get(api::get_session_usage),
        )
        .route(
            "/api/sessions/{session_id}/token_usage.txt",
            axum::routing::get(api::get_session_token_usage),
        )
        .route(
            "/api/sessions/{session_id}/display.jsonl",
            axum::routing::get(api::stream_session_display),
        )
        .route(
            "/api/sessions/{session_id}/tool-calls/{tool_call_file}",
            axum::routing::get(api::stream_session_tool_call),
        )
        .route(
            "/api/sessions/{session_id}/tool-calls/{tool_call_id}/status.txt",
            axum::routing::get(api::get_session_tool_call_status),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/session-meta.json",
            axum::routing::get(api::get_subagent_meta),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/conversation-state.json",
            axum::routing::get(api::get_subagent_conversation_state),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/status.txt",
            axum::routing::get(api::get_subagent_status),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/token_usage.txt",
            axum::routing::get(api::get_subagent_token_usage),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/display.jsonl",
            axum::routing::get(api::stream_subagent_display),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/tool-calls/{tool_call_file}",
            axum::routing::get(api::stream_subagent_tool_call),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/tool-calls/{tool_call_id}/status.txt",
            axum::routing::get(api::get_subagent_tool_call_status),
        )
        .route(
            "/api/sessions/{session_id}/permissions",
            axum::routing::get(api::get_permissions),
        );

    let writes = axum::Router::<Arc<AppState>>::new()
        .route("/api/sessions", axum::routing::post(api::post_sessions))
        .route(
            "/api/sessions/{session_id}/messages",
            axum::routing::post(api::post_session_message),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/messages",
            axum::routing::post(api::post_subagent_message),
        )
        .route(
            "/api/sessions/{session_id}/finish",
            axum::routing::post(api::post_session_finish),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/finish",
            axum::routing::post(api::post_subagent_finish),
        )
        .route(
            "/api/sessions/{session_id}/cancel",
            axum::routing::post(api::post_session_cancel),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/cancel",
            axum::routing::post(api::post_subagent_cancel),
        )
        .route(
            "/api/sessions/{session_id}/tool-calls/{tool_call_id}/cancel",
            axum::routing::post(api::post_session_tool_call_cancel),
        )
        .route(
            "/api/sessions/{session_id}/subagents/{subagent_id}/tool-calls/{tool_call_id}/cancel",
            axum::routing::post(api::post_subagent_tool_call_cancel),
        )
        .route(
            "/api/sessions/{session_id}/permissions/resolve",
            axum::routing::post(api::post_permissions_resolve),
        )
        .route(
            "/api/sessions/{session_id}/permissions",
            axum::routing::post(api::post_permissions_add),
        )
        .route(
            "/api/sessions/{session_id}/permissions/{permission_id}",
            axum::routing::delete(api::delete_permission),
        )
        .route_layer(from_fn(origin_middleware::require_same_origin));

    reads
        .merge(writes)
        .route_layer(from_fn_with_state(state, require_auth))
}

pub(crate) fn build_router(state: Arc<AppState>) -> axum::Router {
    let public = axum::Router::<Arc<AppState>>::new()
        .route(
            "/api/auth/login",
            axum::routing::post(auth::post_login)
                .route_layer(from_fn(origin_middleware::require_same_origin)),
        )
        .route(
            "/api/auth/logout",
            axum::routing::post(auth::post_logout)
                .route_layer(from_fn(origin_middleware::require_same_origin)),
        )
        .route("/api/auth/session", axum::routing::get(auth::get_session));

    let protected = protected_routes(Arc::clone(&state));
    let frontend = axum::Router::<Arc<AppState>>::new()
        .route("/", axum::routing::any(spa::serve_frontend))
        .route("/{*path}", axum::routing::any(spa::serve_frontend));

    public.merge(protected).merge(frontend).with_state(state)
}
