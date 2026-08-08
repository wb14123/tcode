use std::path::{Path, PathBuf};

use axum::response::IntoResponse;

use super::api::{
    empty_permission_state, ensure_session_resumable, find_subagent_tool_call_id,
    heartbeat_interval_seconds, jsonl_line_from_bytes, send_appended_jsonl_events,
};

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/tcode-web-api")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

async fn resolve_tool_call_id(
    display_path: &Path,
    subagent_id: &str,
) -> anyhow::Result<Option<String>> {
    find_subagent_tool_call_id(display_path, subagent_id)
        .await
        .map_err(|e| anyhow::anyhow!("find_subagent_tool_call_id failed: {:?}", e))
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

    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &mut 0u64, &tx).await?;
    assert_eq!(offset, 3);
    assert_eq!(partial_line, b"ok\xE2");
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    tokio::fs::write(&path, b"ok\xE2\x82\xAC\n").await?;
    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &mut 0u64, &tx).await?;
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

    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &mut 0u64, &tx).await?;
    assert_eq!(offset, old_partial.len() as u64);
    assert_eq!(partial_line, old_partial);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    tokio::fs::write(&path, b"new\n").await?;
    send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &mut 0u64, &tx).await?;
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

    let err = send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &mut 0u64, &tx)
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
async fn find_subagent_tool_call_id_resolves_legacy_and_envelope_lines() -> anyhow::Result<()> {
    let dir = temp_dir();
    let display = dir.join("display.jsonl");
    let legacy = serde_json::json!({
        "SubAgentStart": {
            "msg_id": 1,
            "tool_call_id": "tool-1",
            "conversation_id": "sub-1",
            "description": "d"
        }
    });
    let envelope = serde_json::json!({
        "id": 4_294_967_296i64,
        "msg": {
            "SubAgentContinue": {
                "tool_call_id": "tool-2",
                "conversation_id": "sub-1",
                "description": "d"
            }
        }
    });
    let other = serde_json::json!({
        "id": 4_294_967_297i64,
        "msg": {
            "SubAgentStart": {
                "tool_call_id": "tool-3",
                "conversation_id": "sub-other",
                "description": "d"
            }
        }
    });
    tokio::fs::write(
        &display,
        format!("{}\nnot json\n{}\n{}\n", legacy, envelope, other),
    )
    .await?;

    assert_eq!(
        resolve_tool_call_id(&display, "sub-1").await?.as_deref(),
        Some("tool-2")
    );
    assert_eq!(
        resolve_tool_call_id(&display, "sub-other")
            .await?
            .as_deref(),
        Some("tool-3")
    );
    Ok(())
}

#[tokio::test]
async fn find_subagent_tool_call_id_legacy_only_missing_and_nonmatching() -> anyhow::Result<()> {
    let dir = temp_dir();
    let display = dir.join("display.jsonl");
    let legacy = serde_json::json!({
        "SubAgentStart": {
            "msg_id": 1,
            "tool_call_id": "tool-1",
            "conversation_id": "sub-1",
            "description": "d"
        }
    });
    tokio::fs::write(&display, format!("{}\n", legacy)).await?;

    assert_eq!(
        resolve_tool_call_id(&display, "sub-1").await?.as_deref(),
        Some("tool-1")
    );
    assert_eq!(
        resolve_tool_call_id(&display, "sub-other")
            .await?
            .as_deref(),
        None
    );
    assert_eq!(
        resolve_tool_call_id(&dir.join("does-not-exist.jsonl"), "sub-1")
            .await?
            .as_deref(),
        None
    );
    Ok(())
}
