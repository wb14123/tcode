use axum::Json;
#[cfg(test)]
use serde::Deserialize;
use serde::Serialize;

/// Response body for `GET /api/auth/session`.
///
/// `Deserialize` is gated behind `#[cfg(test)]` so tests can round-trip
/// the JSON body without leaking a symmetric wire contract into the
/// public type — `SessionStatus` is a response-only DTO.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Deserialize))]
pub struct SessionStatus {
    pub authenticated: bool,
}

/// Stub: always reports unauthenticated.
///
/// The real cookie-based session check lands with the login/logout ticket,
/// together with the cookie library choice. The 401 semantics come with
/// that middleware; `GET /api/auth/session` itself stays unauthenticated-
/// accessible (it's the SPA's bootstrap probe).
pub async fn get_session() -> Json<SessionStatus> {
    Json(SessionStatus {
        authenticated: false,
    })
}
