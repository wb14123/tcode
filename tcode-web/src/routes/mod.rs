use std::sync::Arc;

use crate::state::AppState;

mod auth;

#[cfg(test)]
mod auth_tests;

pub(crate) fn build_router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/api/auth/session", axum::routing::get(auth::get_session))
        .with_state(state)
}
