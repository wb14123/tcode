use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use llm_rs::llm::{LLMEvent, StopReason, ToolCall};

use crate::handler::{create_router, AppState};
use crate::test_helpers::MockLLM;
use crate::types::ChatCompletionResponse;

const TEST_TOKEN: &str = "test-token";

fn make_state(events: Vec<LLMEvent>) -> Arc<AppState> {
    Arc::new(AppState {
        llm: Box::new(MockLLM { events }),
        auth_tokens: [TEST_TOKEN.to_string()].into(),
    })
}

fn chat_request(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn test_non_streaming_text_response() {
    let state = make_state(vec![
        LLMEvent::MessageStart { input_tokens: 10 },
        LLMEvent::TextDelta("Hello, ".into()),
        LLMEvent::TextDelta("world!".into()),
        LLMEvent::MessageEnd {
            stop_reason: StopReason::EndTurn,
            input_tokens: 10,
            output_tokens: 5,
            reasoning_tokens: 0,
            raw: None,
        },
    ]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": false
    }));

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let resp: ChatCompletionResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello, world!"));
    assert_eq!(resp.choices[0].finish_reason, "stop");
    assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, 10);
    assert_eq!(resp.usage.as_ref().unwrap().completion_tokens, 5);
}

#[tokio::test]
async fn test_non_streaming_tool_call_response() {
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
            raw: None,
        },
    ]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Search for rust"}],
        "stream": false
    }));

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let resp: ChatCompletionResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(resp.choices[0].finish_reason, "tool_calls");
    assert!(resp.choices[0].message.content.is_none());
    let tc = &resp.choices[0].message.tool_calls.as_ref().unwrap()[0];
    assert_eq!(tc.id, "call_abc");
    assert_eq!(tc.function.name, "search");
}

#[tokio::test]
async fn test_streaming_response() {
    let state = make_state(vec![
        LLMEvent::MessageStart { input_tokens: 5 },
        LLMEvent::TextDelta("Hi".into()),
        LLMEvent::MessageEnd {
            stop_reason: StopReason::EndTurn,
            input_tokens: 5,
            output_tokens: 1,
            reasoning_tokens: 0,
            raw: None,
        },
    ]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": true
    }));

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: "));
    assert!(text.contains("[DONE]"));
    assert!(text.contains("\"role\":\"assistant\""));
    assert!(text.contains("\"content\":\"Hi\""));
    assert!(text.contains("\"finish_reason\":\"stop\""));
}

#[tokio::test]
async fn test_models_endpoint() {
    let state = make_state(vec![]);
    let app = create_router(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["object"], "list");
    assert_eq!(json["data"][0]["id"], "mock-model");
}

#[tokio::test]
async fn test_llm_error_returns_502() {
    let state = make_state(vec![LLMEvent::Error("upstream failure".into())]);
    let app = create_router(state);

    let req = chat_request(serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": false
    }));

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn test_request_without_token_rejected() {
    let state = make_state(vec![]);
    let app = create_router(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
