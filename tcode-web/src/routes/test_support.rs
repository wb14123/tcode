//! Shared test helpers and probe routes for routing-layer tests.
//!
//! This module is `#[cfg(test)]`-gated at every level so the probe URIs
//! (`/api/_test/*`) can NEVER ship in a production binary. Do not add a
//! non-test caller to this module; do not relax the `#[cfg(test)]` gate.

use std::sync::Arc;

use axum::http::header;
use axum::response::Response;
use axum_extra::extract::cookie::Cookie;
use http_body_util::BodyExt;

use super::SESSION_COOKIE_NAME;
use super::auth::SessionStatus;
use crate::state::AppState;

pub(crate) const VALID_PASSWORD: &str = "valid-password-16chars!";

/// Robustly pull the `tcode_session` cookie out of all `Set-Cookie` headers.
/// Surfaces parse errors so a malformed `Set-Cookie` shows up as a test
/// failure rather than a silent "no cookie".
pub(crate) fn find_session_cookie(resp: &Response) -> anyhow::Result<Option<Cookie<'static>>> {
    for h in resp.headers().get_all(header::SET_COOKIE) {
        let s = h.to_str()?;
        let parsed = Cookie::parse(s.to_owned())
            .map_err(|e| anyhow::anyhow!("malformed Set-Cookie {s:?}: {e}"))?;
        if parsed.name() == SESSION_COOKIE_NAME {
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

pub(crate) async fn parse_session_body(resp: Response) -> anyhow::Result<SessionStatus> {
    let bytes = resp.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice::<SessionStatus>(&bytes)?)
}

pub(crate) fn login_body(secret: &str) -> String {
    serde_json::json!({ "secret": secret }).to_string()
}

/// Build a router that mirrors production sequencing (route registration
/// first, layer attachment last) and registers a small set of probe
/// routes on the protected subrouter so the routing-layer tests can
/// exercise the middleware without depending on any real protected
/// handler existing yet.
///
/// **Test-only**: the `_test` URIs registered here must NEVER be
/// reachable from a production binary. The `#[cfg(test)]` gate on this
/// function (belt-and-suspenders alongside the `#[cfg(test)] mod
/// test_support;` declaration in `routes/mod.rs`) ensures that.
#[cfg(test)]
pub(crate) fn build_router_with_protected_probes(state: Arc<AppState>) -> axum::Router {
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;

    // Public subrouter — same shape as `build_router`.
    let public = axum::Router::<Arc<AppState>>::new()
        .route(
            "/api/auth/login",
            axum::routing::post(super::auth::post_login),
        )
        .route(
            "/api/auth/logout",
            axum::routing::post(super::auth::post_logout),
        )
        .route(
            "/api/auth/session",
            axum::routing::get(super::auth::get_session),
        );

    // Protected subrouter — register probe routes first, then attach the
    // layer LAST. Mirrors the contract documented on `protected_routes()`.
    // Note: this test helper is the canonical demonstration of the
    // sequencing rule. Future tests that need additional protected probes
    // should add them above the `route_layer` call, never after — except
    // the deliberate `/api/_test/after_layer` probe below, which exists to
    // pin axum 0.8's "later-added routes are NOT gated" footgun.
    let protected = axum::Router::<Arc<AppState>>::new()
        .route("/api/_test/json", get(probe_json))
        .route("/api/_test/sse", get(probe_sse))
        .route("/api/_test/freshly_added", get(probe_fresh))
        .route_layer(from_fn_with_state(
            Arc::clone(&state),
            super::auth_middleware::require_auth,
        ))
        // Deliberately registered AFTER `route_layer` to pin the axum 0.8
        // semantic that later-added routes do NOT inherit the layer. The
        // corresponding test asserts this probe responds 200 with no
        // cookie. If a future axum version changes this, that test fails
        // and forces a rethink.
        .route("/api/_test/after_layer", get(probe_fresh));

    public.merge(protected).with_state(state)
}

async fn probe_json() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
}

async fn probe_fresh() -> &'static str {
    "hi"
}

async fn probe_sse() -> axum::response::sse::Sse<
    impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>
    + Send
    + 'static,
> {
    use axum::response::sse::{Event, Sse};
    // No keep-alive: current tests inspect only the response head
    // (status + content-type) and do not drain the body. Omitting
    // keep-alive is forward-compat insurance — if a future test does
    // call `body.collect()`, the single-event stream will terminate
    // cleanly instead of being held open indefinitely by axum's
    // default keep-alive interval.
    // `+ Send + 'static` is required by axum's `Handler` bound on `Sse`.
    Sse::new(tokio_stream::once(Ok::<_, std::convert::Infallible>(
        Event::default().data("ok"),
    )))
}
