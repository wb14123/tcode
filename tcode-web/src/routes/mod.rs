use std::sync::Arc;

use axum::middleware::from_fn_with_state;

use crate::state::AppState;

mod auth;
mod auth_middleware;

use auth_middleware::require_auth;

// Re-export so the middleware (and any future caller) names the cookie
// const at one canonical path. Visibility-equivalent to `auth::SESSION_COOKIE_NAME`;
// purely a discoverability convention.
pub(crate) use auth::SESSION_COOKIE_NAME;

#[cfg(test)]
mod auth_tests;
#[cfg(test)]
mod enforcement_tests;
#[cfg(test)]
pub(crate) mod test_support;

/// All routes that must be behind the `require_auth` middleware.
///
/// **Invariants** (do not violate without revisiting §2 of plan.md):
/// 1. This is the ONLY function that registers protected routes.
/// 2. The `route_layer` attachment is the function's terminal expression.
///    Anything added after it would silently bypass auth.
/// 3. Production callers must consume the returned router opaquely; do
///    not chain additional `.route(...)` calls on it.
/// 4. Returns `Router<Arc<AppState>>` (state still missing). Do NOT call
///    `.with_state(state)` here — `build_router` calls it once after
///    `merge`-ing with the public router so both subrouters share the
///    same state allocation (the same-`Arc` invariant in `build_router`).
///
/// Empty-router guard: `route_layer` panics on a router with zero
/// routes. The `has_routes()` check below makes this function safe to
/// call during this PR (when the protected route set is empty); it
/// becomes a no-op once milestone 2 adds the first real route.
pub(crate) fn protected_routes(state: Arc<AppState>) -> axum::Router<Arc<AppState>> {
    // Future protected routes go HERE (above the layer attachment), e.g.:
    //   .route("/api/sessions", axum::routing::get(get_sessions))
    //   .route("/api/sessions/{sid}/display.jsonl", axum::routing::get(stream_display))
    let routes = axum::Router::<Arc<AppState>>::new();

    if routes.has_routes() {
        routes.route_layer(from_fn_with_state(state, require_auth))
    } else {
        // No protected routes registered yet — return the empty router
        // unchanged. No layer to attach, nothing to gate. Once milestone 2
        // adds the first real route, this branch becomes dead code and the
        // `state` arg is consumed by the `route_layer` arm above.
        // SAFETY-FOLLOWUP(milestone-2): delete this `else` branch (and the
        // `has_routes()` guard) once the first real protected route lands;
        // ALSO update `build_router_with_protected_probes` in
        // `routes::test_support` to chain off `protected_routes()` so the
        // same-`Arc` invariant in `build_router` is exercised end-to-end
        // by the `enforcement_tests` suite (until then, that invariant is
        // covered only by the test-helper's own wiring).
        drop(state); // explicitly consume to avoid `unused_variables` lint
        routes
    }
}

pub(crate) fn build_router(state: Arc<AppState>) -> axum::Router {
    // TODO(milestone-1): origin-check middleware on state-changing routes
    // is the NEXT todo item; see §2 of plan.md for how it composes.

    // Public subrouter: only the auth endpoints. Reachable without a
    // session cookie. Add new public routes here ONLY after explicit
    // security review. Turbofish the state type so inference is robust
    // even when `protected_routes()` returns an empty router that does
    // not constrain `S` via the merge.
    let public = axum::Router::<Arc<AppState>>::new()
        .route("/api/auth/login", axum::routing::post(auth::post_login))
        .route("/api/auth/logout", axum::routing::post(auth::post_logout))
        .route("/api/auth/session", axum::routing::get(auth::get_session));

    // Same-Arc invariant: the state passed to `from_fn_with_state`
    // (inside `protected_routes`) and the state passed to `with_state`
    // below MUST be the same allocation. If they were separate
    // `Arc::new(...)` instances, sessions minted by the login handler
    // (state-via-`with_state`) would be invisible to the middleware
    // (state-via-`from_fn_with_state`), and every authenticated request
    // would 401. Cloning the same `Arc` here keeps the invariant local
    // and obvious — do not rewrite this to take state by reference.
    let protected = protected_routes(Arc::clone(&state));

    public.merge(protected).with_state(state)
}
