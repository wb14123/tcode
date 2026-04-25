use std::path::{Path, PathBuf};

use axum::response::IntoResponse;

use super::api::{
    empty_permission_state, ensure_session_resumable, heartbeat_interval_seconds,
    read_appended_utf8, session_dir_for,
};

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/tcode-web-api")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
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

#[tokio::test]
async fn read_appended_utf8_leaves_partial_utf8_for_retry() -> anyhow::Result<()> {
    let dir = temp_dir();
    let path = dir.join("display.jsonl");
    let mut bytes = b"ok".to_vec();
    bytes.push(0xE2);
    tokio::fs::write(&path, bytes).await?;

    let first = read_appended_utf8(&path, 0).await?;
    assert_eq!(first, Some((2, "ok".to_string())));

    tokio::fs::write(&path, b"ok\xE2\x82\xAC\n").await?;
    let second = read_appended_utf8(&path, 2).await?;
    assert_eq!(second, Some((6, "€\n".to_string())));
    Ok(())
}
