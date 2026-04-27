use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
#[cfg(test)]
use serde::Deserialize;
use serde::Serialize;

use crate::state::{AppState, SESSION_TTL};

/// Request body for `POST /api/auth/login`.
///
/// The final deserialized `secret` buffer is zeroized on drop via
/// `zeroize::ZeroizeOnDrop`. Intermediate parser and transport buffers
/// (serde_json unescape scratch, axum/hyper body bytes) are NOT covered
/// and may linger in freed heap until reuse; that residue is out of
/// scope for this struct.
#[derive(serde::Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRequest {
    secret: String,
}

/// Response body for the auth endpoints.
///
/// `Deserialize` is gated behind `#[cfg(test)]` so tests can round-trip
/// the JSON body without leaking a symmetric wire contract into the
/// public type — `SessionStatus` is a response-only DTO.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Deserialize))]
pub(crate) struct SessionStatus {
    pub(crate) authenticated: bool,
}

/// Cookie name used for the server-side session token.
pub(crate) const SESSION_COOKIE_NAME: &str = "tcode_session";

/// Helper: a cookie that clears `tcode_session`. Its attribute set
/// (name, Path=/, HttpOnly, Secure, SameSite=Strict) must mirror the
/// cookie issued by `post_login`. Browsers store cookies keyed by
/// (name, domain, path) per RFC 6265, but the `Secure`, `HttpOnly`,
/// and `SameSite` attributes gate *which* request a cookie applies
/// to: a clearing `Set-Cookie` that diverges on those attributes can
/// be filtered out on requests where the original is still attached,
/// so the clear fails to take effect. Mirroring all attributes keeps
/// logout reliable.
///
/// The `Secure` attribute is set unconditionally. Chromium, Firefox,
/// and Safari treat `http://localhost`, `http://127.0.0.1`, and
/// `http://[::1]` as potentially-trustworthy / secure contexts per
/// the W3C Secure Contexts spec, so `Secure` cookies still round-trip
/// over plain HTTP on the default loopback binding (see
/// `RemoteConfig::with_loopback_defaults`). Any non-loopback
/// deployment is expected to terminate TLS at a proxy, at which
/// point `Secure` becomes load-bearing.
fn clear_cookie() -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .build()
}

/// `POST /api/auth/login`
///
/// Verifies `body.secret` against the configured shared secret in constant
/// time. On success mints a fresh session token, stores it, and returns it
/// to the client as a session cookie. On failure returns 401 without
/// touching any existing session — a failed login (wrong password or a
/// typo) must not log the user out.
pub(crate) async fn post_login(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> (StatusCode, CookieJar, Json<SessionStatus>) {
    if !state.password.verify(body.secret.as_bytes()) {
        // Industry-standard behavior: wrong password → 401, do not touch any
        // existing session. Revoking/clearing on failed login would create a
        // CSRF-amplifier / DoS (a typo or a cross-origin POST could log the
        // user out). Returning the jar unchanged means no `Set-Cookie` is
        // emitted when no cookie was present, and the user's existing valid
        // session survives a password typo.
        return (
            StatusCode::UNAUTHORIZED,
            jar,
            Json(SessionStatus {
                authenticated: false,
            }),
        );
    }
    let token = match state.mint_session() {
        Ok(t) => t,
        Err(e) => {
            // Log only the error code; never log derived buffers or the
            // eventual token. `getrandom::Error` has a stable Display impl.
            tracing::error!(error = %e, "failed to mint session token");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                jar,
                Json(SessionStatus {
                    authenticated: false,
                }),
            );
        }
    };
    // `Max-Age` mirrors the server-side `SESSION_TTL` so the browser
    // forgets the cookie in lockstep with server-side expiry. The
    // server is the authority — `verify_session` enforces TTL even if
    // a misbehaving client ignores `Max-Age`. The cast is safe because
    // `SESSION_TTL` is a hardcoded const well below `i64::MAX` seconds.
    let max_age = time::Duration::seconds(SESSION_TTL.as_secs() as i64);
    let cookie = Cookie::build((SESSION_COOKIE_NAME, token))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .max_age(max_age)
        .build();
    (
        StatusCode::OK,
        jar.add(cookie),
        Json(SessionStatus {
            authenticated: true,
        }),
    )
}

/// `POST /api/auth/logout`
///
/// Idempotent: always returns 200. When the client sent a session cookie,
/// revoke the server-side token and emit a clearing `Set-Cookie` that
/// matches the original name+path.
pub(crate) async fn post_logout(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> (StatusCode, CookieJar) {
    // We need the cookie value anyway in order to revoke the server-side
    // token, so the guard is load-bearing for that. Emitting (or not) a
    // clearing `Set-Cookie` when there was no incoming cookie is a
    // behavioral detail of `CookieJar::remove` (no-op when the original
    // jar is empty); the explicit if-let keeps the "no incoming cookie →
    // 200 with no `Set-Cookie`" contract readable.
    if let Some(c) = jar.get(SESSION_COOKIE_NAME) {
        state.revoke_session(c.value());
        return (StatusCode::OK, jar.remove(clear_cookie()));
    }
    (StatusCode::OK, jar)
}

/// `GET /api/auth/session`
///
/// SPA bootstrap probe. Returns `{authenticated: bool}` based on whether
/// the presented session cookie (if any) names a live session. Status is
/// always 200 — the real 401 semantics come from the middleware that will
/// gate every other endpoint.
pub(crate) async fn get_session(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Json<SessionStatus> {
    let authenticated = jar
        .get(SESSION_COOKIE_NAME)
        .map(|c| state.verify_session(c.value()))
        .unwrap_or(false);
    Json(SessionStatus { authenticated })
}
