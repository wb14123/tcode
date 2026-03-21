//! Bidirectional conversion between OpenAI wire types and llm-rs types.

use std::sync::Arc;

use llm_rs::llm::{ChatOptions, LLMMessage, ReasoningEffort, StopReason, ToolCall};
use llm_rs::tool::Tool;

use crate::error::AppError;
use crate::types::{MessageToolCall, MessageToolCallFunction, RequestMessage, RequestTool};

/// Convert OpenAI request messages to llm-rs LLMMessage values.
pub fn convert_request_messages(messages: &[RequestMessage]) -> Result<Vec<LLMMessage>, AppError> {
    messages
        .iter()
        .map(|msg| match msg.role.as_str() {
            "system" => Ok(LLMMessage::System(msg.content.clone().unwrap_or_default())),
            "user" => Ok(LLMMessage::User(msg.content.clone().unwrap_or_default())),
            "assistant" => {
                let tool_calls = msg
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter()
                            .map(|tc| ToolCall {
                                id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                arguments: tc.function.arguments.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(LLMMessage::Assistant {
                    content: msg.content.clone().unwrap_or_default(),
                    tool_calls,
                    raw: None,
                })
            }
            "tool" => {
                let tool_call_id = msg.tool_call_id.clone().ok_or_else(|| {
                    AppError::BadRequest("tool message missing tool_call_id".into())
                })?;
                Ok(LLMMessage::ToolResult {
                    tool_call_id,
                    content: msg.content.clone().unwrap_or_default(),
                })
            }
            other => Err(AppError::BadRequest(format!("unknown role: {other}"))),
        })
        .collect()
}

/// Convert OpenAI tool definitions to llm-rs sentinel Tool instances.
pub fn convert_request_tools(tools: &[RequestTool]) -> Result<Vec<Arc<Tool>>, AppError> {
    tools
        .iter()
        .map(|t| {
            let params = t
                .function
                .parameters
                .clone()
                .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));
            let schema: schemars::Schema = serde_json::from_value(params).map_err(|e| {
                AppError::BadRequest(format!("invalid tool parameters schema: {e}"))
            })?;
            Ok(Arc::new(Tool::new_sentinel(
                &t.function.name,
                t.function.description.as_deref().unwrap_or(""),
                schema,
            )))
        })
        .collect()
}

/// Convert an OpenAI ChatCompletionRequest into llm-rs ChatOptions.
pub fn convert_chat_options(
    max_tokens: Option<u32>,
    reasoning: Option<&crate::types::ReasoningRequest>,
) -> ChatOptions {
    let (reasoning_effort, reasoning_budget, exclude_reasoning) = match reasoning {
        Some(r) => {
            let effort = r.effort.as_deref().and_then(|e| match e {
                "xhigh" => Some(ReasoningEffort::XHigh),
                "high" => Some(ReasoningEffort::High),
                "medium" => Some(ReasoningEffort::Medium),
                "low" => Some(ReasoningEffort::Low),
                "minimal" => Some(ReasoningEffort::Minimal),
                _ => None,
            });
            (effort, r.max_tokens, r.exclude)
        }
        None => (None, None, false),
    };
    ChatOptions {
        max_tokens,
        reasoning_effort,
        reasoning_budget,
        exclude_reasoning,
    }
}

/// Map an llm-rs StopReason to the OpenAI finish_reason string.
pub fn stop_reason_to_finish_reason(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "stop",
        StopReason::ToolUse => "tool_calls",
        StopReason::MaxTokens => "length",
    }
}

/// Convert an llm-rs ToolCall to an OpenAI MessageToolCall.
pub fn tool_call_to_message(tc: &ToolCall) -> MessageToolCall {
    MessageToolCall {
        id: tc.id.clone(),
        call_type: "function".to_string(),
        function: MessageToolCallFunction {
            name: tc.name.clone(),
            arguments: tc.arguments.clone(),
        },
    }
}
