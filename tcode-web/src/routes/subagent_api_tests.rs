use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::test_support::{HomeGuard, VALID_PASSWORD, find_session_cookie, login_body};
use crate::state::AppState;

fn fresh_app() -> axum::Router {
    let state = Arc::new(AppState::new(VALID_PASSWORD.into()));
    super::build_router(state)
}

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

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/tcode-web-subagent-api")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

fn create_session_dir(session_id: &str) -> anyhow::Result<PathBuf> {
    let session_dir = tcode_runtime::session::base_path()?.join(session_id);
    std::fs::create_dir_all(&session_dir)?;
    Ok(session_dir)
}

async fn response_text(response: axum::response::Response) -> anyhow::Result<String> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(String::from_utf8(bytes.to_vec())?)
}

fn subagent_write_endpoints(
    session_id: &str,
    subagent_id: &str,
) -> Vec<(String, Option<&'static str>)> {
    vec![
        (
            format!("/api/sessions/{session_id}/subagents/{subagent_id}/messages"),
            Some(r#"{"text":"hello subagent"}"#),
        ),
        (
            format!("/api/sessions/{session_id}/subagents/{subagent_id}/finish"),
            None,
        ),
        (
            format!("/api/sessions/{session_id}/subagents/{subagent_id}/cancel"),
            None,
        ),
    ]
}

fn build_post_request(
    uri: &str,
    body: Option<&str>,
    cookie_pair: Option<&str>,
    origin: Option<&str>,
) -> anyhow::Result<Request<Body>> {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(cookie_pair) = cookie_pair {
        builder = builder.header("cookie", cookie_pair);
    }
    if let Some(origin) = origin {
        builder = builder
            .header("origin", origin)
            .header("host", "app.example");
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    Ok(builder.body(match body {
        Some(body) => Body::from(body.to_owned()),
        None => Body::empty(),
    })?)
}

#[tokio::test]
async fn subagent_write_routes_are_auth_gated() -> anyhow::Result<()> {
    let app = fresh_app();

    for (uri, body) in subagent_write_endpoints("missing-session", "child") {
        let resp = app
            .clone()
            .oneshot(build_post_request(&uri, body, None, None)?)
            .await?;
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "unexpected status for {uri}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn subagent_write_routes_require_same_origin() -> anyhow::Result<()> {
    let app = fresh_app();
    let pair = login_and_take_cookie_pair(&app).await?;

    for (uri, body) in subagent_write_endpoints("missing-session", "child") {
        let resp = app
            .clone()
            .oneshot(build_post_request(
                &uri,
                body,
                Some(&pair),
                Some("https://evil.example"),
            )?)
            .await?;
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "unexpected status for {uri}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn subagent_write_routes_validate_subagent_existence() -> anyhow::Result<()> {
    let home_dir = temp_dir();
    let _home_guard = HomeGuard::set(&home_dir);
    create_session_dir("abc123xy")?;

    let app = fresh_app();
    let pair = login_and_take_cookie_pair(&app).await?;

    for (uri, body) in subagent_write_endpoints("abc123xy", "missing-child") {
        let resp = app
            .clone()
            .oneshot(build_post_request(&uri, body, Some(&pair), None)?)
            .await?;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "unexpected status for {uri}"
        );
        let body = response_text(resp).await?;
        assert!(
            body.contains("subagent not found"),
            "unexpected body for {uri}: {body}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn nested_subagent_reads_still_work() -> anyhow::Result<()> {
    let home_dir = temp_dir();
    let _home_guard = HomeGuard::set(&home_dir);
    let session_dir = create_session_dir("def456uv")?;
    let nested_subagent_dir = session_dir.join("subagent-parent").join("subagent-child");
    std::fs::create_dir_all(&nested_subagent_dir)?;
    std::fs::write(nested_subagent_dir.join("status.txt"), "nested ok")?;

    let app = fresh_app();
    let pair = login_and_take_cookie_pair(&app).await?;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/sessions/def456uv/subagents/child/status.txt")
                .header("cookie", &pair)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(response_text(resp).await?, "nested ok");
    Ok(())
}
