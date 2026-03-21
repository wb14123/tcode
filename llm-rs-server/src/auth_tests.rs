use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use llm_rs::llm::{LLMEvent, StopReason};

use crate::handler::{AppState, create_router};
use crate::test_helpers::MockLLM;

fn mock_events() -> Vec<LLMEvent> {
    vec![
        LLMEvent::MessageStart { input_tokens: 1 },
        LLMEvent::TextDelta("ok".into()),
        LLMEvent::MessageEnd {
            stop_reason: StopReason::EndTurn,
            input_tokens: 1,
            output_tokens: 1,
            reasoning_tokens: 0,
            raw: None,
        },
    ]
}

fn make_state(tokens: &[&str]) -> Arc<AppState> {
    Arc::new(AppState {
        llm: Box::new(MockLLM {
            events: mock_events(),
        }),
        auth_tokens: tokens.iter().map(|s| s.to_string()).collect(),
    })
}

fn chat_body() -> Result<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false
    }))?)
}

#[tokio::test]
async fn test_valid_token() -> Result<()> {
    let state = make_state(&["secret-key"]);
    let app = create_router(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer secret-key")
        .body(Body::from(chat_body()?))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn test_multiple_valid_tokens() -> Result<()> {
    let state = make_state(&["token-a", "token-b"]);
    let app = create_router(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer token-b")
        .body(Body::from(chat_body()?))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn test_invalid_token() -> Result<()> {
    let state = make_state(&["secret-key"]);
    let app = create_router(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer wrong-key")
        .body(Body::from(chat_body()?))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let body = response.into_body().collect().await?.to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["type"], "authentication_error");
    Ok(())
}

#[tokio::test]
async fn test_missing_token() -> Result<()> {
    let state = make_state(&["secret-key"]);
    let app = create_router(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Content-Type", "application/json")
        .body(Body::from(chat_body()?))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_load_tokens_missing_file() {
    let dir = std::env::temp_dir().join(format!("llm-rs-test-{}", uuid::Uuid::new_v4()));
    let path = dir.join("tokens.json");

    let result = crate::auth::load_tokens(&path);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("token file not found"));
}

#[tokio::test]
async fn test_load_tokens_reads_existing() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("llm-rs-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("tokens.json");
    std::fs::write(&path, r#"["aaa", "bbb"]"#)?;

    let tokens = crate::auth::load_tokens(&path)?;
    assert_eq!(tokens, HashSet::from(["aaa".into(), "bbb".into()]));

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

#[tokio::test]
async fn test_load_tokens_rejects_empty() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("llm-rs-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("tokens.json");
    std::fs::write(&path, "[]")?;

    let result = crate::auth::load_tokens(&path);
    assert!(result.is_err());

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
