use std::sync::Arc;

use crate::state::AppState;

mod auth;

#[cfg(test)]
mod auth_tests;

pub(crate) fn build_router(state: Arc<AppState>) -> axum::Router {
    // TODO(milestone-1): origin-check middleware for state-changing routes;
    // current CSRF mitigation leans entirely on SameSite=Strict until then.
    axum::Router::new()
        .route("/api/auth/login", axum::routing::post(auth::post_login))
        .route("/api/auth/logout", axum::routing::post(auth::post_logout))
        .route("/api/auth/session", axum::routing::get(auth::get_session))
        .with_state(state)
}
