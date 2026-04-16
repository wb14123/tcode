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
use crate::state::AppState;

/// Reject requests that do not present a live `tcode_session` cookie.
///
/// Wired via `axum::middleware::from_fn_with_state` + `Router::route_layer`
/// on the protected subrouter in `routes::build_router`. Auth endpoints
/// live on a separate public subrouter and never reach this layer.
///
/// Why cookies and not a header: the browser `EventSource` API does not
/// allow attaching custom headers, so SSE auth must travel via cookies.
/// Switching to `Authorization: Bearer ...` later would break SSE.
///
/// Cookie selection: per RFC 6265 §5.3 a browser stores at most one
/// cookie per (name, domain, path) tuple, so a real client only ever
/// sends one `tcode_session` cookie. We delegate parsing to `CookieJar`
/// and consult `jar.get(SESSION_COOKIE_NAME)`. If a synthetic client
/// sends multiple `tcode_session` cookies, `cookie::CookieJar` keeps the
/// last-added one — accepting "the cookie the client most recently
/// asserted" is the conservative default and avoids the cookie-injection
/// edge cases of an "any-valid wins" policy.
///
/// Response contract on rejection:
/// - status `401 Unauthorized`
/// - `content-type: application/json`
/// - `cache-control: no-store`
/// - body `{"authenticated":false}` (mirrors `SessionStatus` shape so
///   the SPA's bootstrap probe can use a single deserializer)
///
/// Logging contract:
/// - logs `tracing::trace!` with method + path on rejection (silent at
///   default log level; raising to debug/info is a denial-of-disk-space
///   vector once the server is exposed remotely)
/// - NEVER logs the cookie value
/// - NEVER logs the full request headers
pub(crate) async fn require_auth(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    request: Request,
    next: Next,
) -> Response {
    let authenticated = jar
        .get(SESSION_COOKIE_NAME)
        .map(|c| state.verify_session(c.value()))
        .unwrap_or(false);

    if authenticated {
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
        Json(SessionStatus {
            authenticated: false,
        }),
    )
        .into_response()
}
