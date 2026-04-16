use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum_extra::extract::cookie::{Cookie, SameSite};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::auth::{SESSION_COOKIE_NAME, SessionStatus};
use super::build_router;
use crate::state::{AppState, SESSION_TOKEN_B64_LEN};

const VALID_PASSWORD: &str = "valid-password-16chars!";

/// Build a fresh router for each test. `Arc::new` + `build_router` avoids
/// cross-test state bleed; each call gets an isolated `AppState`.
fn fresh_app() -> axum::Router {
    let state = Arc::new(AppState::new(VALID_PASSWORD.into()));
    build_router(state)
}

/// Robustly pull the `tcode_session` cookie out of all `Set-Cookie` headers.
/// Surfaces parse errors so a malformed `Set-Cookie` shows up as a test
/// failure rather than a silent "no cookie".
fn find_session_cookie(resp: &Response) -> anyhow::Result<Option<Cookie<'static>>> {
    for h in resp.headers().get_all(axum::http::header::SET_COOKIE) {
        let s = h.to_str()?;
        let parsed = Cookie::parse(s.to_owned())
            .map_err(|e| anyhow::anyhow!("malformed Set-Cookie {s:?}: {e}"))?;
        if parsed.name() == SESSION_COOKIE_NAME {
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

async fn parse_session_body(resp: Response) -> anyhow::Result<SessionStatus> {
    let bytes = resp.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice::<SessionStatus>(&bytes)?)
}

fn login_body(secret: &str) -> String {
    serde_json::json!({ "secret": secret }).to_string()
}

#[tokio::test]
async fn get_session_returns_unauthenticated() -> anyhow::Result<()> {
    let app = fresh_app();

    let request = Request::builder()
        .uri("/api/auth/session")
        .body(Body::empty())?;

    let response = app.oneshot(request).await?;

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("application/json"),
        "unexpected content-type: {content_type}"
    );

    let parsed = parse_session_body(response).await?;
    assert_eq!(
        parsed,
        SessionStatus {
            authenticated: false
        }
    );

    Ok(())
}

#[tokio::test]
async fn login_with_correct_password_succeeds() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(login_body(VALID_PASSWORD)))?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);

    let cookie = find_session_cookie(&resp)?
        .ok_or_else(|| anyhow::anyhow!("Set-Cookie tcode_session missing"))?;

    assert_eq!(
        cookie.value().len(),
        SESSION_TOKEN_B64_LEN,
        "cookie value: {:?}",
        cookie.value()
    );
    let b64url_alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    assert!(
        cookie.value().bytes().all(|b| b64url_alphabet.contains(&b)),
        "cookie value has non-base64url bytes: {:?}",
        cookie.value()
    );
    assert_eq!(cookie.path(), Some("/"));
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(cookie.same_site(), Some(SameSite::Strict));
    assert_eq!(cookie.secure(), Some(true));
    assert!(
        cookie.domain().is_none(),
        "Domain= must not be set; a misconfigured Domain would broaden the cookie to sibling hosts"
    );
    assert!(cookie.max_age().is_none(), "expected session cookie");
    assert!(cookie.expires().is_none(), "expected session cookie");

    let body = parse_session_body(resp).await?;
    assert_eq!(
        body,
        SessionStatus {
            authenticated: true
        }
    );

    Ok(())
}

#[tokio::test]
async fn login_with_wrong_password_returns_401_and_no_set_cookie() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(login_body("wrong-password-totally")))?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        find_session_cookie(&resp)?.is_none(),
        "failed login must not emit a Set-Cookie for tcode_session"
    );

    let body = parse_session_body(resp).await?;
    assert_eq!(
        body,
        SessionStatus {
            authenticated: false
        }
    );

    Ok(())
}

#[tokio::test]
async fn login_with_wrong_password_preserves_existing_session() -> anyhow::Result<()> {
    let app = fresh_app();

    // Step 1: successful login to seed a cookie.
    let ok = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(login_body(VALID_PASSWORD)))?,
        )
        .await?;
    assert_eq!(ok.status(), StatusCode::OK);
    let cookie = find_session_cookie(&ok)?
        .ok_or_else(|| anyhow::anyhow!("initial Set-Cookie tcode_session missing"))?;
    let pair = format!("{}={}", cookie.name(), cookie.value());

    // Step 2: second login, this time with a wrong password, carrying the
    // existing cookie.
    let bad = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .header("cookie", &pair)
                .body(Body::from(login_body("wrong-password-totally")))?,
        )
        .await?;
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    assert!(
        find_session_cookie(&bad)?.is_none(),
        "failed login must not touch the existing session cookie"
    );

    // Step 3: confirm the original cookie still authenticates.
    let probe = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(probe.status(), StatusCode::OK);
    let body = parse_session_body(probe).await?;
    assert!(
        body.authenticated,
        "existing session must survive wrong-password login"
    );

    Ok(())
}

#[tokio::test]
async fn login_with_malformed_json_returns_400() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from("not json"))?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn login_with_missing_secret_field_returns_422() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}

#[tokio::test]
async fn login_with_unknown_fields_returns_422() -> anyhow::Result<()> {
    let app = fresh_app();

    // `#[serde(deny_unknown_fields)]` rejects extra keys like `extra`. This
    // prevents silently ignoring typos (e.g. `{"secrret": "..."}`) that would
    // otherwise deserialize into a missing `secret` and yield an identical
    // 422, but without signaling the mistake. The rejection surfaces through
    // axum's `Json` extractor as a deserialization error -> 422.
    let payload = serde_json::json!({ "secret": VALID_PASSWORD, "extra": "x" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(payload))?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}

#[tokio::test]
async fn login_without_json_content_type_returns_415() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .body(Body::empty())?,
        )
        .await?;

    // Note: 415 here is axum's `Json` extractor behavior, not an `api.md`
    // contract. A future extractor swap may legitimately relax this.
    assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    Ok(())
}

#[tokio::test]
async fn session_with_valid_cookie_returns_authenticated() -> anyhow::Result<()> {
    let app = fresh_app();

    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(login_body(VALID_PASSWORD)))?,
        )
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = find_session_cookie(&login)?
        .ok_or_else(|| anyhow::anyhow!("Set-Cookie tcode_session missing after login"))?;
    let pair = format!("{}={}", cookie.name(), cookie.value());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = parse_session_body(resp).await?;
    assert!(body.authenticated);
    Ok(())
}

#[tokio::test]
async fn session_with_unknown_cookie_returns_unauthenticated() -> anyhow::Result<()> {
    let app = fresh_app();

    // base64url string of the correct length that the server never minted.
    let forged = "A".repeat(SESSION_TOKEN_B64_LEN);
    let pair = format!("{SESSION_COOKIE_NAME}={forged}");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = parse_session_body(resp).await?;
    assert!(!body.authenticated);
    Ok(())
}

#[tokio::test]
async fn session_with_wrong_length_cookie_returns_unauthenticated() -> anyhow::Result<()> {
    let app = fresh_app();
    let pair = format!("{SESSION_COOKIE_NAME}=short");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = parse_session_body(resp).await?;
    assert!(!body.authenticated);
    Ok(())
}

#[tokio::test]
async fn logout_clears_cookie_and_revokes_session() -> anyhow::Result<()> {
    let app = fresh_app();

    // Seed a valid session.
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(login_body(VALID_PASSWORD)))?,
        )
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let issued = find_session_cookie(&login)?
        .ok_or_else(|| anyhow::anyhow!("Set-Cookie tcode_session missing after login"))?;
    let pair = format!("{}={}", issued.name(), issued.value());

    // Logout with the cookie.
    let logout = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(logout.status(), StatusCode::OK);

    let cleared = find_session_cookie(&logout)?
        .ok_or_else(|| anyhow::anyhow!("logout must emit a clearing Set-Cookie"))?;
    assert!(
        cleared.value().is_empty(),
        "clearing cookie should have empty value, got {:?}",
        cleared.value()
    );
    assert_eq!(cleared.path(), Some("/"));
    assert_eq!(cleared.http_only(), Some(true));
    assert_eq!(cleared.secure(), Some(true));
    assert_eq!(cleared.same_site(), Some(SameSite::Strict));
    // The `cookie` crate's removal path sets `Max-Age=0`. Assert via the
    // parsed `max_age()` so we don't have to name the `time::Duration`
    // path directly.
    assert_eq!(
        cleared.max_age().map(|d| d.is_zero()),
        Some(true),
        "clearing Set-Cookie should have Max-Age=0, got {:?}",
        cleared.max_age()
    );

    // Reusing the original pre-logout cookie must now read as unauthenticated.
    let probe = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(probe.status(), StatusCode::OK);
    let body = parse_session_body(probe).await?;
    assert!(
        !body.authenticated,
        "session must be revoked after logout, got authenticated=true"
    );

    Ok(())
}

#[tokio::test]
async fn logout_without_cookie_is_noop() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        find_session_cookie(&resp)?.is_none(),
        "no Set-Cookie tcode_session should be emitted when no cookie was presented"
    );
    Ok(())
}

#[tokio::test]
async fn multi_tab_login_is_independent() -> anyhow::Result<()> {
    let app = fresh_app();

    let login_a = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(login_body(VALID_PASSWORD)))?,
        )
        .await?;
    assert_eq!(login_a.status(), StatusCode::OK);
    let cookie_a =
        find_session_cookie(&login_a)?.ok_or_else(|| anyhow::anyhow!("login A cookie missing"))?;

    let login_b = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(login_body(VALID_PASSWORD)))?,
        )
        .await?;
    assert_eq!(login_b.status(), StatusCode::OK);
    let cookie_b =
        find_session_cookie(&login_b)?.ok_or_else(|| anyhow::anyhow!("login B cookie missing"))?;

    assert_ne!(
        cookie_a.value(),
        cookie_b.value(),
        "two fresh 256-bit tokens must differ"
    );

    let pair_a = format!("{}={}", cookie_a.name(), cookie_a.value());
    let pair_b = format!("{}={}", cookie_b.name(), cookie_b.value());

    // Logout with cookie A only.
    let logout_a = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header("cookie", &pair_a)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(logout_a.status(), StatusCode::OK);

    // Cookie A must now read as unauthenticated.
    let probe_a = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .header("cookie", &pair_a)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(probe_a.status(), StatusCode::OK);
    let body_a = parse_session_body(probe_a).await?;
    assert!(!body_a.authenticated, "cookie A must be revoked");

    // Cookie B must still authenticate.
    let probe_b = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .header("cookie", &pair_b)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(probe_b.status(), StatusCode::OK);
    let body_b = parse_session_body(probe_b).await?;
    assert!(body_b.authenticated, "cookie B must survive A's logout");

    Ok(())
}
