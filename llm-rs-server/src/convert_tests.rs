use anyhow::Result;
use llm_rs::llm::{LLMMessage, ReasoningEffort, StopReason};

use crate::convert::*;
use crate::types::*;

#[test]
fn test_convert_system_message() -> Result<()> {
    let msgs = vec![RequestMessage {
        role: "system".into(),
        content: Some("You are helpful.".into()),
        tool_call_id: None,
        tool_calls: None,
    }];
    let result = convert_request_messages(&msgs)?;
    assert!(matches!(&result[0], LLMMessage::System(s) if s == "You are helpful."));
    Ok(())
}

#[test]
fn test_convert_user_message() -> Result<()> {
    let msgs = vec![RequestMessage {
        role: "user".into(),
        content: Some("Hello".into()),
        tool_call_id: None,
        tool_calls: None,
    }];
    let result = convert_request_messages(&msgs)?;
    assert!(matches!(&result[0], LLMMessage::User(s) if s == "Hello"));
    Ok(())
}

#[test]
fn test_convert_assistant_message_with_tool_calls() -> Result<()> {
    let msgs = vec![RequestMessage {
        role: "assistant".into(),
        content: None,
        tool_call_id: None,
        tool_calls: Some(vec![MessageToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: MessageToolCallFunction {
                name: "search".into(),
                arguments: "{\"q\":\"test\"}".into(),
            },
        }]),
    }];
    let result = convert_request_messages(&msgs)?;
    match &result[0] {
        LLMMessage::Assistant { tool_calls, .. } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].name, "search");
            assert_eq!(tool_calls[0].arguments, "{\"q\":\"test\"}");
        }
        other => panic!("expected Assistant, got {:?}", other),
    }
    Ok(())
}

#[test]
fn test_convert_tool_result_message() -> Result<()> {
    let msgs = vec![RequestMessage {
        role: "tool".into(),
        content: Some("result data".into()),
        tool_call_id: Some("call_1".into()),
        tool_calls: None,
    }];
    let result = convert_request_messages(&msgs)?;
    match &result[0] {
        LLMMessage::ToolResult {
            tool_call_id,
            content,
        } => {
            assert_eq!(tool_call_id, "call_1");
            assert_eq!(content, "result data");
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }
    Ok(())
}

#[test]
fn test_convert_tool_result_missing_id() {
    let msgs = vec![RequestMessage {
        role: "tool".into(),
        content: Some("data".into()),
        tool_call_id: None,
        tool_calls: None,
    }];
    let result = convert_request_messages(&msgs);
    assert!(result.is_err());
}

#[test]
fn test_convert_unknown_role() {
    let msgs = vec![RequestMessage {
        role: "developer".into(),
        content: Some("hi".into()),
        tool_call_id: None,
        tool_calls: None,
    }];
    let result = convert_request_messages(&msgs);
    assert!(result.is_err());
}

#[test]
fn test_convert_request_tools() -> Result<()> {
    let tools = vec![RequestTool {
        tool_type: "function".into(),
        function: RequestFunctionDef {
            name: "get_weather".into(),
            description: Some("Get current weather".into()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"]
            })),
        },
    }];
    let result = convert_request_tools(&tools)?;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "get_weather");
    assert_eq!(result[0].description, "Get current weather");
    Ok(())
}

#[test]
fn test_convert_request_tools_no_params() -> Result<()> {
    let tools = vec![RequestTool {
        tool_type: "function".into(),
        function: RequestFunctionDef {
            name: "noop".into(),
            description: None,
            parameters: None,
        },
    }];
    let result = convert_request_tools(&tools)?;
    assert_eq!(result[0].name, "noop");
    Ok(())
}

#[test]
fn test_convert_chat_options_with_reasoning() {
    let reasoning = ReasoningRequest {
        effort: Some("high".into()),
        max_tokens: Some(500),
        exclude: true,
    };
    let opts = convert_chat_options(Some(1024), Some(&reasoning));
    assert_eq!(opts.max_tokens, Some(1024));
    assert_eq!(opts.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(opts.reasoning_budget, Some(500));
    assert!(opts.exclude_reasoning);
}

#[test]
fn test_convert_chat_options_no_reasoning() {
    let opts = convert_chat_options(Some(256), None);
    assert_eq!(opts.max_tokens, Some(256));
    assert!(opts.reasoning_effort.is_none());
    assert!(!opts.exclude_reasoning);
}

#[test]
fn test_stop_reason_mapping() {
    assert_eq!(stop_reason_to_finish_reason(&StopReason::EndTurn), "stop");
    assert_eq!(
        stop_reason_to_finish_reason(&StopReason::ToolUse),
        "tool_calls"
    );
    assert_eq!(
        stop_reason_to_finish_reason(&StopReason::MaxTokens),
        "length"
    );
}
