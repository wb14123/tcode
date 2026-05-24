use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;

use super::SESSION_COOKIE_NAME;
use super::auth::SessionStatus;
use super::session_path::SessionRoot;
use crate::state::AppState;

/// Reject requests that do not present a live `tcode_session` cookie.
///
/// Wired via `axum::middleware::from_fn_with_state` + `Router::route_layer`
/// on the protected subrouter in `routes::build_router`. Auth endpoints
/// live on a separate public subrouter and never reach this layer.
///
/// When authenticated, looks up the user in `state.users`, canonicalizes
/// their `session_dir`, and injects a `SessionRoot` into request extensions
/// so handlers can safely access session files.
///
/// Response contract on rejection:
/// - status `401 Unauthorized`
/// - `content-type: application/json`
/// - `cache-control: no-store`
/// - body `{"authenticated":false,"secure_session_cookie":...,"username":null}`
///
/// Logging contract:
/// - logs `tracing::trace!` with method + path on rejection (silent at
///   default log level)
/// - NEVER logs the cookie value
/// - NEVER logs the full request headers
pub(crate) async fn require_auth(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    mut request: Request,
    next: Next,
) -> Response {
    let username = jar
        .get(SESSION_COOKIE_NAME)
        .and_then(|c| state.verify_session(c.value()));

    if let Some(username) = username
        && let Some(user) = state.users.get(&username)
    {
        let path = match std::fs::canonicalize(&user.session_dir) {
            Ok(path) => path,
            Err(_) => {
                // Session directory may not exist yet; create it and retry.
                std::fs::create_dir_all(&user.session_dir)
                    .inspect_err(|e| {
                        tracing::warn!(
                            user = %username,
                            session_dir = %user.session_dir.display(),
                            error = %e,
                            "failed to create user session directory"
                        );
                    })
                    .ok();
                match std::fs::canonicalize(&user.session_dir) {
                    Ok(path) => path,
                    Err(e) => {
                        tracing::error!(
                            user = %username,
                            session_dir = %user.session_dir.display(),
                            error = %e,
                            "failed to canonicalize user session directory"
                        );
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            [(header::CACHE_CONTROL, "no-store")],
                            Json(SessionStatus::new(false, &state, None)),
                        )
                            .into_response();
                    }
                }
            }
        };
        let root = SessionRoot {
            path,
            trash_dir: user.trash_dir.clone(),
        };
        request.extensions_mut().insert(root);
        return next.run(request).await;
    }

    tracing::trace!(
        method = %request.method(),
        path = %request.uri().path(),
        "rejecting unauthenticated request"
    );

    (
        StatusCode::UNAUTHORIZED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(SessionStatus::new(false, &state, None)),
    )
        .into_response()
}
