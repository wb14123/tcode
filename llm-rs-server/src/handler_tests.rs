use std::sync::Arc;

use anyhow::Result;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use llm_rs::llm::{LLMEvent, StopReason, ToolCall};

use crate::handler::{AppState, create_router};
use crate::test_helpers::MockLLM;
use crate::types::ChatCompletionResponse;

const TEST_TOKEN: &str = "test-token";

fn make_state(events: Vec<LLMEvent>) -> Arc<AppState> {
    Arc::new(AppState {
        llm: Box::new(MockLLM { events }),
        auth_tokens: [TEST_TOKEN.to_string()].into(),
    })
}

fn chat_request(body: serde_json::Value) -> Result<Request<Body>> {
    Ok(Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .body(Body::from(serde_json::to_string(&body)?))?)
}

#[tokio::test]
async fn test_non_streaming_text_response() -> Result<()> {
    let state = make_state(vec![
        LLMEvent::MessageStart { input_tokens: 10 },
        LLMEvent::TextDelta("Hello, ".into()),
        LLMEvent::TextDelta("world!".into()),
        LLMEvent::MessageEnd {
            stop_reason: StopReason::EndTurn,
            input_tokens: 10,
            output_tokens: 5,
            reasoning_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            raw: None,
        },
    ]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": false
    }))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await?.to_bytes();
    let resp: ChatCompletionResponse = serde_json::from_slice(&body)?;
    assert_eq!(
        resp.choices[0].message.content.as_deref(),
        Some("Hello, world!")
    );
    assert_eq!(resp.choices[0].finish_reason, "stop");
    let usage = resp.usage.as_ref().expect("usage should be present");
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 5);
    Ok(())
}

#[tokio::test]
async fn test_non_streaming_tool_call_response() -> Result<()> {
    let state = make_state(vec![
        LLMEvent::MessageStart { input_tokens: 15 },
        LLMEvent::ToolCall(ToolCall {
            id: "call_abc".into(),
            name: "search".into(),
            arguments: "{\"q\":\"rust\"}".into(),
        }),
        LLMEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
            input_tokens: 15,
            output_tokens: 20,
            reasoning_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            raw: None,
        },
    ]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Search for rust"}],
        "stream": false
    }))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await?.to_bytes();
    let resp: ChatCompletionResponse = serde_json::from_slice(&body)?;
    assert_eq!(resp.choices[0].finish_reason, "tool_calls");
    assert!(resp.choices[0].message.content.is_none());
    let tc = &resp.choices[0]
        .message
        .tool_calls
        .as_ref()
        .expect("tool_calls should be present")[0];
    assert_eq!(tc.id, "call_abc");
    assert_eq!(tc.function.name, "search");
    Ok(())
}

#[tokio::test]
async fn test_streaming_response() -> Result<()> {
    let state = make_state(vec![
        LLMEvent::MessageStart { input_tokens: 5 },
        LLMEvent::TextDelta("Hi".into()),
        LLMEvent::MessageEnd {
            stop_reason: StopReason::EndTurn,
            input_tokens: 5,
            output_tokens: 1,
            reasoning_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            raw: None,
        },
    ]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": true
    }))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .expect("content-type header should be present"),
        "text/event-stream"
    );

    let body = response.into_body().collect().await?.to_bytes();
    let text = String::from_utf8(body.to_vec())?;
    assert!(text.contains("data: "));
    assert!(text.contains("[DONE]"));
    assert!(text.contains("\"role\":\"assistant\""));
    assert!(text.contains("\"content\":\"Hi\""));
    assert!(text.contains("\"finish_reason\":\"stop\""));
    Ok(())
}

#[tokio::test]
async fn test_models_endpoint() -> Result<()> {
    let state = make_state(vec![]);
    let app = create_router(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .body(Body::empty())?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await?.to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["object"], "list");
    assert_eq!(json["data"][0]["id"], "mock-model");
    Ok(())
}

#[tokio::test]
async fn test_llm_error_returns_502() -> Result<()> {
    let state = make_state(vec![LLMEvent::Error("upstream failure".into())]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": false
    }))?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    Ok(())
}

#[tokio::test]
async fn test_request_without_token_rejected() -> Result<()> {
    let state = make_state(vec![]);
    let app = create_router(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())?;

    let response = app.oneshot(req).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}
