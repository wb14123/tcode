use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::SESSION_COOKIE_NAME;
use super::auth::SessionStatus;
use super::test_support::{
    VALID_PASSWORD, build_router_with_protected_probes, find_session_cookie, login_body,
    parse_session_body,
};
use crate::state::{AppState, SESSION_TOKEN_B64_LEN};

/// Build a fresh router with the protected-probes test scaffold. Each call
/// gets an isolated `AppState`, mirroring the `auth_tests.rs` `fresh_app`
/// pattern.
fn fresh_app() -> axum::Router {
    let state = Arc::new(AppState::new(VALID_PASSWORD.into()));
    build_router_with_protected_probes(state)
}

/// Login against the test router and return the `name=value` Cookie pair
/// for the freshly minted session, ready to be used as a `cookie` header
/// value on a subsequent request.
async fn login_and_take_cookie_pair(app: &axum::Router) -> anyhow::Result<String> {
    let resp = app
        .clone()
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
        .ok_or_else(|| anyhow::anyhow!("Set-Cookie tcode_session missing after login"))?;
    Ok(format!("{}={}", cookie.name(), cookie.value()))
}

#[tokio::test]
async fn protected_json_without_cookie_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("application/json"),
        "unexpected content-type: {content_type}"
    );
    let cache_control = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(cache_control, "no-store");
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
async fn protected_json_with_valid_cookie_returns_200() -> anyhow::Result<()> {
    let app = fresh_app();
    let pair = login_and_take_cookie_pair(&app).await?;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await?.to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value, serde_json::json!({ "ok": true }));
    Ok(())
}

#[tokio::test]
async fn protected_sse_without_cookie_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/sse")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("application/json"),
        "unexpected content-type: {content_type}"
    );
    assert!(
        !content_type.starts_with("text/event-stream"),
        "SSE upgrade must not have started; content-type: {content_type}"
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
async fn protected_sse_with_valid_cookie_streams() -> anyhow::Result<()> {
    let app = fresh_app();
    let pair = login_and_take_cookie_pair(&app).await?;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/sse")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/event-stream"),
        "expected text/event-stream, got: {content_type}"
    );
    Ok(())
}

#[tokio::test]
async fn protected_head_without_cookie_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/api/_test/json")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn protected_options_without_cookie_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/_test/json")
                .body(Body::empty())?,
        )
        .await?;

    // Defensive pin against silent axum changes; not a security claim.
    // The plan flagged this as "run locally to confirm" — axum 0.8 routes
    // the OPTIONS through the path's matched route (the path matched, the
    // method did not), so `route_layer` runs and rejects with 401 BEFORE
    // the method router has a chance to emit its 405 fallback. This is
    // strictly safer than 405 (no method-existence side channel) and
    // matches the §4 behavior contract: gated-by-construction beats
    // method-aware response shaping. Renamed from `_returns_405` to
    // match the observed axum 0.8 behavior, per the plan's instruction
    // for this exact case.
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn protected_route_with_unknown_cookie_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();
    // base64url-shaped value of correct length that the server never minted.
    // The chance of a real CSPRNG mint colliding with this all-A value is
    // 2^-256, so this is safe to hardcode.
    let forged = "A".repeat(SESSION_TOKEN_B64_LEN);
    let pair = format!("{SESSION_COOKIE_NAME}={forged}");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn protected_route_with_wrong_length_cookie_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();
    let pair = format!("{SESSION_COOKIE_NAME}=short");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn protected_route_with_wrong_cookie_name_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();
    // Mint a real, valid session, then send it under the WRONG cookie name.
    let pair_real = login_and_take_cookie_pair(&app).await?;
    let value = pair_real
        .split_once('=')
        .map(|(_, v)| v.to_owned())
        .ok_or_else(|| anyhow::anyhow!("malformed cookie pair"))?;
    let pair_wrong_name = format!("session={value}");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &pair_wrong_name)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn protected_route_with_malformed_cookie_header_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", "garbage-no-equals")
                .body(Body::empty())?,
        )
        .await?;

    // axum-extra silently skips malformed cookie pairs; the middleware
    // then sees no `tcode_session` cookie and rejects with 401 (not 400).
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn protected_route_with_duplicate_invalid_cookies_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();
    let stale1 = "A".repeat(SESSION_TOKEN_B64_LEN);
    let stale2 = "B".repeat(SESSION_TOKEN_B64_LEN);
    let header_value = format!("{SESSION_COOKIE_NAME}={stale1}; {SESSION_COOKIE_NAME}={stale2}");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &header_value)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

/// Real browsers store at most one cookie per (name, domain, path) tuple
/// (RFC 6265 §5.3), so multiple `tcode_session` cookies should never reach
/// the server in practice. If a synthetic client does send multiple,
/// `cookie::CookieJar` keeps the last-added one — pinned here so a future
/// `axum-extra` change to that selection rule surfaces in CI rather than
/// silently changing auth behavior.
#[tokio::test]
async fn protected_route_with_duplicate_cookies_uses_last_added() -> anyhow::Result<()> {
    let app = fresh_app();
    let valid_pair = login_and_take_cookie_pair(&app).await?;
    let valid_value = valid_pair
        .split_once('=')
        .map(|(_, v)| v.to_owned())
        .ok_or_else(|| anyhow::anyhow!("malformed cookie pair"))?;
    let stale = "A".repeat(SESSION_TOKEN_B64_LEN);

    // valid first, stale second → stale wins (last-added) → 401
    let stale_last = format!("{SESSION_COOKIE_NAME}={valid_value}; {SESSION_COOKIE_NAME}={stale}");
    let resp_stale_last = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &stale_last)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp_stale_last.status(), StatusCode::UNAUTHORIZED);

    // stale first, valid second → valid wins (last-added) → 200
    let valid_last = format!("{SESSION_COOKIE_NAME}={stale}; {SESSION_COOKIE_NAME}={valid_value}");
    let resp_valid_last = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &valid_last)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp_valid_last.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn protected_route_after_logout_returns_401() -> anyhow::Result<()> {
    let app = fresh_app();
    let pair = login_and_take_cookie_pair(&app).await?;

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

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/json")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn public_login_endpoint_is_not_gated_by_require_auth() -> anyhow::Result<()> {
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
    Ok(())
}

#[tokio::test]
async fn public_logout_endpoint_is_not_gated_by_require_auth() -> anyhow::Result<()> {
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
    Ok(())
}

#[tokio::test]
async fn public_session_endpoint_is_not_gated_by_require_auth() -> anyhow::Result<()> {
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/session")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
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
async fn additional_protected_probe_route_is_gated() -> anyhow::Result<()> {
    let app = fresh_app();

    let no_cookie = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/freshly_added")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(no_cookie.status(), StatusCode::UNAUTHORIZED);

    let pair = login_and_take_cookie_pair(&app).await?;
    let with_cookie = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/freshly_added")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(with_cookie.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
#[allow(non_snake_case)]
async fn route_added_after_route_layer_is_NOT_gated() -> anyhow::Result<()> {
    // Documented-footgun test, NOT a security claim. Pins axum 0.8's
    // semantics that routes added AFTER `route_layer` do not inherit the
    // layer. If a future axum version changes this, this test fails and
    // forces revisiting `protected_routes()` doc invariants in §3.4 of
    // plan.md.
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/_test/after_layer")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn unmatched_path_without_cookie_returns_404_not_401() -> anyhow::Result<()> {
    // This pins `route_layer` semantics: unauthenticated requests to
    // unmatched paths return 404, not the middleware's 401. The test
    // exercises the helper router (which has registered protected probes
    // alongside `route_layer`) so the assertion is meaningful — against
    // a router with zero protected routes the unmatched-path response
    // would always be 404 trivially.
    let app = fresh_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/this-route-does-not-exist")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    Ok(())
}

/// Production `protected_routes()` now contains the real authenticated API surface.
#[tokio::test]
async fn protected_routes_register_real_endpoints() {
    let state = Arc::new(AppState::new(VALID_PASSWORD.into()));
    let router = super::protected_routes(state);
    assert!(
        router.has_routes(),
        "protected_routes() should register the authenticated API endpoints"
    );
}
