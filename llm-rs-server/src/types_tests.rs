use crate::types::*;

#[test]
fn test_request_deserialization() {
    let json = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are helpful."},
            {"role": "user", "content": "Hi"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}
                }]
            },
            {"role": "tool", "tool_call_id": "call_1", "content": "result"}
        ],
        "stream": true,
        "max_tokens": 1024,
        "tools": [{
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search the web",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
            }
        }],
        "stream_options": {"include_usage": true},
        "reasoning": {"effort": "high", "max_tokens": 500, "exclude": false}
    });

    let req: ChatCompletionRequest = serde_json::from_value(json).unwrap();
    assert_eq!(req.model, "gpt-4");
    assert_eq!(req.messages.len(), 4);
    assert!(req.stream);
    assert_eq!(req.max_tokens, Some(1024));
    assert_eq!(req.tools.as_ref().unwrap().len(), 1);
    assert!(req.stream_options.as_ref().unwrap().include_usage);
    assert_eq!(
        req.reasoning.as_ref().unwrap().effort.as_deref(),
        Some("high")
    );
}

#[test]
fn test_request_defaults() {
    let json = serde_json::json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}]
    });
    let req: ChatCompletionRequest = serde_json::from_value(json).unwrap();
    assert!(!req.stream);
    assert!(req.max_tokens.is_none());
    assert!(req.tools.is_none());
    assert!(req.stream_options.is_none());
    assert!(req.reasoning.is_none());
}

#[test]
fn test_response_serialization_roundtrip() {
    let resp = ChatCompletionResponse {
        id: "chatcmpl-123".into(),
        object: "chat.completion".into(),
        created: 1700000000,
        model: "gpt-4".into(),
        choices: vec![ResponseChoice {
            index: 0,
            message: ResponseMessage {
                role: "assistant".into(),
                content: Some("Hello!".into()),
                reasoning_content: None,
                tool_calls: None,
            },
            finish_reason: "stop".into(),
        }],
        usage: Some(UsageResponse {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            completion_tokens_details: None,
        }),
    };

    let json = serde_json::to_string(&resp).unwrap();
    let parsed: ChatCompletionResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, "chatcmpl-123");
    assert_eq!(parsed.choices[0].finish_reason, "stop");
    assert_eq!(parsed.choices[0].message.content.as_deref(), Some("Hello!"));
    assert_eq!(parsed.usage.as_ref().unwrap().total_tokens, 15);
}

#[test]
fn test_response_with_tool_calls() {
    let resp = ChatCompletionResponse {
        id: "chatcmpl-456".into(),
        object: "chat.completion".into(),
        created: 1700000000,
        model: "gpt-4".into(),
        choices: vec![ResponseChoice {
            index: 0,
            message: ResponseMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![MessageToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    function: MessageToolCallFunction {
                        name: "search".into(),
                        arguments: "{\"q\":\"rust\"}".into(),
                    },
                }]),
            },
            finish_reason: "tool_calls".into(),
        }],
        usage: None,
    };

    let json = serde_json::to_value(&resp).unwrap();
    assert!(json["choices"][0]["message"]["content"].is_null());
    assert_eq!(
        json["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "search"
    );
    // usage should be absent when None
    assert!(json.get("usage").is_none());
}

#[test]
fn test_chunk_serialization() {
    let chunk = ChatCompletionChunk {
        id: "chatcmpl-789".into(),
        object: "chat.completion.chunk".into(),
        created: 1700000000,
        model: "gpt-4".into(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: ChunkDelta {
                content: Some("Hi".into()),
                ..Default::default()
            },
            finish_reason: None,
        }],
        usage: None,
    };

    let json = serde_json::to_value(&chunk).unwrap();
    assert_eq!(json["choices"][0]["delta"]["content"], "Hi");
    // role should be absent when None
    assert!(json["choices"][0]["delta"].get("role").is_none());
    assert!(json["choices"][0]["finish_reason"].is_null());
}

#[test]
fn test_models_response_serialization() {
    let resp = ModelsResponse {
        object: "list".into(),
        data: vec![ModelObject {
            id: "gpt-4".into(),
            object: "model".into(),
            created: 1700000000,
            owned_by: "llm-rs".into(),
        }],
    };

    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["object"], "list");
    assert_eq!(json["data"][0]["id"], "gpt-4");
}
