use std::sync::Arc;

use argon2::{Argon2, password_hash::PasswordVerifier};
use axum::{Json, extract::State, http::StatusCode};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use password_hash::phc::PasswordHash;
#[cfg(test)]
use serde::Deserialize;
use serde::Serialize;

use crate::state::{AppState, SESSION_TTL};

/// Request body for `POST /api/auth/login`.
///
/// The final deserialized `password` buffer is zeroized on drop via
/// `zeroize::ZeroizeOnDrop`. Intermediate parser and transport buffers
/// (serde_json unescape scratch, axum/hyper body bytes) are NOT covered
/// and may linger in freed heap until reuse; that residue is out of
/// scope for this struct.
#[derive(serde::Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRequest {
    pub(crate) username: String,
    password: String,
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
    pub(crate) secure_session_cookie: bool,
    pub(crate) username: Option<String>,
}

impl SessionStatus {
    pub(crate) fn new(authenticated: bool, state: &AppState, username: Option<String>) -> Self {
        Self {
            authenticated,
            secure_session_cookie: state.secure_session_cookie(),
            username,
        }
    }
}

/// Cookie name used for the server-side session token.
pub(crate) const SESSION_COOKIE_NAME: &str = "tcode_session";

/// Helper: a cookie that clears `tcode_session`. Its attribute set
/// (name, Path=/, HttpOnly, SameSite=Strict, and optional Secure) must mirror
/// the cookie issued by `post_login`. Browsers store cookies keyed by
/// (name, domain, path) per RFC 6265, but the `Secure`, `HttpOnly`,
/// and `SameSite` attributes gate *which* request a cookie applies
/// to: a clearing `Set-Cookie` that diverges on those attributes can
/// be filtered out on requests where the original is still attached,
/// so the clear fails to take effect. Mirroring all attributes keeps
/// logout reliable.
///
/// The `Secure` attribute is set by default. Chromium, Firefox,
/// and Safari treat `http://localhost`, `http://127.0.0.1`, and
/// `http://[::1]` as potentially-trustworthy / secure contexts per
/// the W3C Secure Contexts spec, so `Secure` cookies still round-trip
/// over plain HTTP on the default loopback binding (see
/// `RemoteConfig::with_loopback_defaults`). Non-loopback direct HTTP
/// requires the explicit `--allow-insecure-http` opt-in, which omits
/// `Secure` and sends the password and session cookie in cleartext.
fn clear_cookie(secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::build((SESSION_COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict);
    if secure {
        cookie = cookie.secure(true);
    }
    cookie.build()
}

/// `POST /api/auth/login`
///
/// Verifies `body.password` against the stored argon2id hash for
/// `body.username`. Uses constant-time argon2id verification. On success
/// mints a fresh session token and returns it as a session cookie.
/// On failure returns 401 without touching any existing session.
///
/// Username enumeration prevention: when the user is not found, the handler
/// still runs a full argon2id verify against a dummy hash (the first user's
/// hash) so timing is identical to a real attempt.
pub(crate) async fn post_login(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> (StatusCode, CookieJar, Json<SessionStatus>) {
    let argon2 = Argon2::default();
    let user = state.users.get(&body.username);
    let dummy_hash = state
        .users
        .values()
        .next()
        .expect("users map must not be empty");

    let hash_to_verify = user.map_or(&dummy_hash.password_hash, |u| &u.password_hash);

    let parsed = match PasswordHash::new(hash_to_verify) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to parse stored password hash");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                jar,
                Json(SessionStatus::new(false, &state, None)),
            );
        }
    };

    if argon2
        .verify_password(body.password.as_bytes(), &parsed)
        .is_err()
        || user.is_none()
    {
        return (
            StatusCode::UNAUTHORIZED,
            jar,
            Json(SessionStatus::new(false, &state, None)),
        );
    }

    let username = body.username.clone();
    let token = match state.mint_session(username.clone()) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "failed to mint session token");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                jar,
                Json(SessionStatus::new(false, &state, None)),
            );
        }
    };

    let max_age = time::Duration::seconds(SESSION_TTL.as_secs() as i64);
    let mut cookie = Cookie::build((SESSION_COOKIE_NAME, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .max_age(max_age);
    if state.secure_session_cookie() {
        cookie = cookie.secure(true);
    }
    let cookie = cookie.build();
    (
        StatusCode::OK,
        jar.add(cookie),
        Json(SessionStatus::new(true, &state, Some(username))),
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
    if let Some(c) = jar.get(SESSION_COOKIE_NAME) {
        state.revoke_session(c.value());
        return (
            StatusCode::OK,
            jar.remove(clear_cookie(state.secure_session_cookie())),
        );
    }
    (StatusCode::OK, jar)
}

/// `GET /api/auth/session`
///
/// SPA bootstrap probe. Returns authentication status and cookie security
/// mode. Status is always 200 — the real 401 semantics come from the
/// middleware that will gate every other endpoint.
pub(crate) async fn get_session(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Json<SessionStatus> {
    let (authenticated, username) = jar
        .get(SESSION_COOKIE_NAME)
        .and_then(|c| state.verify_session(c.value()))
        .map(|username| (true, Some(username)))
        .unwrap_or((false, None));
    Json(SessionStatus::new(authenticated, &state, username))
}
