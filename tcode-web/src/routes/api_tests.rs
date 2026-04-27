use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use tcode_runtime::session::{SessionMeta, SessionMode, read_session_mode};
use tower::ServiceExt;

use super::api::{
    empty_permission_state, ensure_session_resumable, heartbeat_interval_seconds,
    jsonl_line_from_bytes, send_appended_jsonl_events, session_dir_for,
};
use super::test_support::{HomeGuard, VALID_PASSWORD, find_session_cookie, login_body};
use crate::config::RemoteModePolicy;
use crate::state::AppState;

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/tcode-web-api")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

fn fresh_app_with_policy(policy: RemoteModePolicy) -> axum::Router {
    let state = Arc::new(AppState::new_with_policy(VALID_PASSWORD.into(), policy));
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

async fn response_json(response: axum::response::Response) -> anyhow::Result<serde_json::Value> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

fn create_session_with_mode(session_id: &str, mode: SessionMode) -> anyhow::Result<PathBuf> {
    let session_dir = tcode_runtime::session::base_path()?.join(session_id);
    std::fs::create_dir_all(&session_dir)?;
    let meta = SessionMeta {
        description: Some(format!("{session_id} description")),
        created_at: Some(1),
        last_active_at: Some(1),
        mode,
    };
    std::fs::write(
        session_dir.join("session-meta.json"),
        serde_json::to_vec(&meta)?,
    )?;
    Ok(session_dir)
}

#[test]
fn heartbeat_interval_is_shorter_than_default_lease_timeout() {
    assert_eq!(heartbeat_interval_seconds(60), 15);
}

#[test]
fn heartbeat_interval_has_lower_bound() {
    assert_eq!(heartbeat_interval_seconds(8), 5);
}

#[test]
fn empty_permission_state_contains_no_permissions() {
    let state = empty_permission_state();
    assert!(state.pending.is_empty());
    assert!(state.session.is_empty());
    assert!(state.project.is_empty());
}

#[test]
fn session_dir_for_rejects_path_like_session_ids() {
    for session_id in ["../abcde", "abc/1234", "ABC123XY", "subagent-foo"] {
        let err = session_dir_for(session_id).expect_err("session id should be rejected");
        let response = err.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }
}

#[test]
fn session_dir_for_accepts_generated_shape_session_ids() {
    let path = session_dir_for("abc123xy").expect("generated-shape session id should be accepted");
    assert!(path.ends_with("abc123xy"));
}

#[tokio::test]
async fn ensure_session_resumable_rejects_missing_conversation_state() -> anyhow::Result<()> {
    let dir = temp_dir();

    let err = ensure_session_resumable(&dir)
        .await
        .expect_err("missing state must not be resumable");
    let response = err.into_response();

    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    Ok(())
}

#[tokio::test]
async fn ensure_session_resumable_rejects_invalid_conversation_state() -> anyhow::Result<()> {
    let dir = temp_dir();
    tokio::fs::write(dir.join("conversation-state.json"), b"not json").await?;

    let err = ensure_session_resumable(&dir)
        .await
        .expect_err("invalid state must not be resumable");
    let response = err.into_response();

    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    Ok(())
}

#[tokio::test]
async fn ensure_session_resumable_accepts_valid_conversation_state() -> anyhow::Result<()> {
    let dir = temp_dir();
    let state = serde_json::json!({
        "id": "root-conversation",
        "model": "test-model",
        "llm_msgs": [],
        "chat_options": {
            "max_tokens": null,
            "reasoning_effort": null,
            "reasoning_budget": null,
            "exclude_reasoning": false
        },
        "msg_id_counter": 0,
        "total_input_tokens": 0,
        "total_output_tokens": 0,
        "total_cache_creation_tokens": 0,
        "total_cache_read_tokens": 0,
        "aggregate_input_tokens": 0,
        "aggregate_output_tokens": 0,
        "aggregate_cache_creation_tokens": 0,
        "aggregate_cache_read_tokens": 0,
        "single_turn": false,
        "subagent_depth": 0
    });
    tokio::fs::write(
        dir.join("conversation-state.json"),
        serde_json::to_vec(&state)?,
    )
    .await?;

    assert!(ensure_session_resumable(&dir).await.is_ok());
    Ok(())
}

#[test]
fn jsonl_line_from_bytes_decodes_utf8_and_trims_cr() -> anyhow::Result<()> {
    assert_eq!(jsonl_line_from_bytes(b"ok\xE2\x82\xAC\r")?, "ok€");
    assert_eq!(jsonl_line_from_bytes(b"plain")?, "plain");
    assert!(jsonl_line_from_bytes(b"bad\xFF").is_err());
    Ok(())
}

#[tokio::test]
async fn jsonl_stream_reader_retains_partial_utf8_line_between_polls() -> anyhow::Result<()> {
    let dir = temp_dir();
    let path = dir.join("display.jsonl");
    let mut bytes = b"ok".to_vec();
    bytes.push(0xE2);
    tokio::fs::write(&path, bytes).await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let mut offset = 0;
    let mut partial_line = Vec::new();

    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &tx).await?;
    assert_eq!(offset, 3);
    assert_eq!(partial_line, b"ok\xE2");
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    tokio::fs::write(&path, b"ok\xE2\x82\xAC\n").await?;
    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &tx).await?;
    assert_eq!(offset, 6);
    assert!(partial_line.is_empty());
    drop(rx.try_recv()?.map_err(|never| match never {})?);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    Ok(())
}

#[tokio::test]
async fn jsonl_stream_reader_restarts_after_truncation() -> anyhow::Result<()> {
    let dir = temp_dir();
    let path = dir.join("display.jsonl");
    let old_partial = b"old partial without newline";
    tokio::fs::write(&path, old_partial).await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let mut offset = 0;
    let mut partial_line = Vec::new();

    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &tx).await?;
    assert_eq!(offset, old_partial.len() as u64);
    assert_eq!(partial_line, old_partial);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    tokio::fs::write(&path, b"new\n").await?;
    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &tx).await?;
    assert_eq!(offset, 4);
    assert!(partial_line.is_empty());
    drop(rx.try_recv()?.map_err(|never| match never {})?);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    Ok(())
}

#[tokio::test]
async fn jsonl_stream_reader_does_not_advance_past_invalid_utf8_line() -> anyhow::Result<()> {
    let dir = temp_dir();
    let path = dir.join("display.jsonl");
    tokio::fs::write(&path, b"ok\nbad\xFF\nlater\n").await?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let mut offset = 0;
    let mut partial_line = Vec::new();

    let err = send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &tx)
        .await
        .expect_err("invalid UTF-8 line should fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(offset, 3);
    assert!(partial_line.is_empty());
    drop(rx.try_recv()?.map_err(|never| match never {})?);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    Ok(())
}

#[tokio::test]
async fn get_sessions_filters_to_web_only_under_restricted_policy() -> anyhow::Result<()> {
    let home_dir = temp_dir().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let _home = HomeGuard::set(&home_dir);
    create_session_with_mode("normal01", SessionMode::Normal)?;
    create_session_with_mode("webonly1", SessionMode::WebOnly)?;
    let app = fresh_app_with_policy(RemoteModePolicy::WebOnlyOnly);
    let cookie = login_and_take_cookie_pair(&app).await?;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/sessions")
                .header("cookie", cookie)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await?;
    let sessions = body["sessions"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("sessions response was not an array"))?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "webonly1");
    assert_eq!(sessions[0]["mode"], "web_only");
    Ok(())
}

#[tokio::test]
async fn web_only_policy_rejects_direct_normal_session_access() -> anyhow::Result<()> {
    let home_dir = temp_dir().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let _home = HomeGuard::set(&home_dir);
    create_session_with_mode("normal01", SessionMode::Normal)?;
    create_session_with_mode("webonly1", SessionMode::WebOnly)?;
    let app = fresh_app_with_policy(RemoteModePolicy::WebOnlyOnly);
    let cookie = login_and_take_cookie_pair(&app).await?;

    let normal_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/sessions/normal01/session-meta.json")
                .header("cookie", cookie.as_str())
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(normal_response.status(), StatusCode::NOT_FOUND);

    let web_only_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/sessions/webonly1/session-meta.json")
                .header("cookie", cookie)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(web_only_response.status(), StatusCode::OK);
    let body = response_json(web_only_response).await?;
    assert_eq!(body["mode"], "web_only");
    Ok(())
}

#[tokio::test]
async fn post_sessions_initializes_web_only_metadata_under_restricted_policy() -> anyhow::Result<()>
{
    let home_dir = temp_dir().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let _home = HomeGuard::set(&home_dir);
    let app = fresh_app_with_policy(RemoteModePolicy::WebOnlyOnly);
    let cookie = login_and_take_cookie_pair(&app).await?;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sessions")
                .header("cookie", cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "initial_prompt": "hello" }).to_string(),
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let sessions = tcode_runtime::session::list_sessions()?;
    assert_eq!(sessions.len(), 1);
    let session_dir = tcode_runtime::session::base_path()?.join(&sessions[0]);
    assert_eq!(read_session_mode(&session_dir)?, SessionMode::WebOnly);
    Ok(())
}

#[tokio::test]
async fn post_sessions_initializes_normal_metadata_under_all_policy() -> anyhow::Result<()> {
    let home_dir = temp_dir().join("home");
    std::fs::create_dir_all(&home_dir)?;
    let _home = HomeGuard::set(&home_dir);
    let app = fresh_app_with_policy(RemoteModePolicy::All);
    let cookie = login_and_take_cookie_pair(&app).await?;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sessions")
                .header("cookie", cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "initial_prompt": "hello" }).to_string(),
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let sessions = tcode_runtime::session::list_sessions()?;
    assert_eq!(sessions.len(), 1);
    let session_dir = tcode_runtime::session::base_path()?.join(&sessions[0]);
    assert_eq!(read_session_mode(&session_dir)?, SessionMode::Normal);
    Ok(())
}
