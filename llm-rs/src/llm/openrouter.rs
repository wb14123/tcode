//! OpenRouter / Chat Completions API LLM implementation.
//!
//! Works with OpenRouter and other OpenAI Chat Completions-compatible providers.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use super::openai_common::{self, ReasoningRequest, ToolDefinition};
use super::sse;
use super::{ChatOptions, LLM, LLMEvent, LLMMessage, ModelInfo, StopReason, ToolCall};
use crate::tool::Tool;

// ============================================================================
// OpenRouter client
// ============================================================================

/// Chat Completions API client for OpenRouter and compatible providers.
pub struct OpenRouter {
    client: Client,
    api_key: String,
    base_url: String,
    cached_tool_defs: Option<Vec<ToolDefinition>>,
}

impl OpenRouter {
    /// Create a new OpenRouter client with the default OpenRouter base URL.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, "https://openrouter.ai/api/v1")
    }

    /// Create a new Chat Completions client with a custom base URL.
    ///
    /// Use this for other providers that implement the Chat Completions API.
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            cached_tool_defs: None,
        }
    }
}

// ============================================================================
// Request types
// ============================================================================

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningRequest>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatMessageToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_details: Option<Vec<serde_json::Value>>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessageToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ChatMessageToolCallFunction,
}

#[derive(Serialize, Deserialize)]
struct ChatMessageToolCallFunction {
    name: String,
    arguments: String,
}

// ============================================================================
// Response types (streaming)
// ============================================================================

#[derive(Deserialize, Debug)]
struct ChatCompletionChunk {
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Debug)]
struct ChunkChoice {
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
    #[serde(default)]
    reasoning_details: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize, Debug, Default)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct Usage {
    #[serde(default)]
    prompt_tokens: i32,
    #[serde(default)]
    completion_tokens: i32,
    #[serde(default)]
    output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Deserialize, Debug, Default)]
struct OutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: i32,
}

// ============================================================================
// Implementation
// ============================================================================

impl LLM for OpenRouter {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        self.cached_tool_defs = openai_common::build_tool_defs(&tools);
    }

    fn clone_box(&self) -> Box<dyn LLM> {
        Box::new(OpenRouter {
            client: self.client.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            cached_tool_defs: None,
        })
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "deepseek/deepseek-r1".into(),
                description: "DeepSeek R1 reasoning model".into(),
            },
            ModelInfo {
                id: "openai/gpt-5".into(),
                description: "OpenAI GPT-5".into(),
            },
            ModelInfo {
                id: "anthropic/claude-sonnet-4-6".into(),
                description: "Claude Sonnet 4.6".into(),
            },
        ]
    }

    fn chat(
        &self,
        model: &str,
        msgs: &[LLMMessage],
        options: &ChatOptions,
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let model = model.to_string();
        let max_tokens = options.max_tokens;
        let reasoning_request = openai_common::build_reasoning_request(options);

        // Convert messages
        let messages: Vec<ChatMessage> = msgs
            .iter()
            .map(|msg| match msg {
                LLMMessage::System(content) => ChatMessage {
                    role: "system",
                    content: Some(content.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_details: None,
                },
                LLMMessage::User(content) => ChatMessage {
                    role: "user",
                    content: Some(content.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_details: None,
                },
                LLMMessage::Assistant {
                    content,
                    tool_calls,
                    raw,
                } => {
                    if let Some(raw_value) = raw {
                        // Use raw message directly if available
                        let content = raw_value
                            .get("content")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        let tc = raw_value.get("tool_calls").and_then(|v| {
                            serde_json::from_value::<Vec<ChatMessageToolCall>>(v.clone()).ok()
                        });
                        let rd = raw_value
                            .get("reasoning_details")
                            .and_then(|v| v.as_array().cloned());
                        ChatMessage {
                            role: "assistant",
                            content,
                            tool_call_id: None,
                            tool_calls: tc,
                            reasoning_details: rd,
                        }
                    } else {
                        // Fallback: reconstruct from fields
                        let tc = if tool_calls.is_empty() {
                            None
                        } else {
                            Some(
                                tool_calls
                                    .iter()
                                    .map(|tc| ChatMessageToolCall {
                                        id: tc.id.clone(),
                                        call_type: "function".to_string(),
                                        function: ChatMessageToolCallFunction {
                                            name: tc.name.clone(),
                                            arguments: tc.arguments.clone(),
                                        },
                                    })
                                    .collect(),
                            )
                        };
                        ChatMessage {
                            role: "assistant",
                            content: if content.is_empty() {
                                None
                            } else {
                                Some(content.clone())
                            },
                            tool_call_id: None,
                            tool_calls: tc,
                            reasoning_details: None,
                        }
                    }
                }
                LLMMessage::ToolResult {
                    tool_call_id,
                    content,
                } => ChatMessage {
                    role: "tool",
                    content: Some(content.clone()),
                    tool_call_id: Some(tool_call_id.clone()),
                    tool_calls: None,
                    reasoning_details: None,
                },
            })
            .collect();

        let tool_defs = self.cached_tool_defs.clone();

        Box::pin(stream! {
            let request_body = ChatRequest {
                model: &model,
                messages,
                stream: true,
                max_tokens,
                tools: tool_defs,
                stream_options: Some(StreamOptions { include_usage: true }),
                reasoning: reasoning_request,
            };

            let url = format!("{}/chat/completions", base_url);
            let response = match sse::check_response(
                client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json")
                    .json(&request_body)
                    .send()
                    .await
            ).await {
                Ok(r) => r,
                Err(e) => {
                    yield LLMEvent::Error(e);
                    return;
                }
            };

            let mut tool_calls: HashMap<usize, (String, String, String)> = HashMap::new();
            let mut input_tokens = 0i32;
            let mut output_tokens = 0i32;
            let mut reasoning_tokens = 0i32;
            let mut emitted_start = false;
            let mut stop_reason: Option<StopReason> = None;
            let mut accumulated_text = String::new();
            let mut accumulated_reasoning_details: Vec<serde_json::Value> = Vec::new();
            let mut accumulated_reasoning_text = String::new();

            let mut sse_events = sse::sse_stream(response);

            while let Some(event_result) = sse_events.next().await {
                let event = match event_result {
                    Ok(e) => e,
                    Err(e) => {
                        yield LLMEvent::Error(e);
                        return;
                    }
                };

                let data = &event.data;

                    if data == "[DONE]" {
                        // Build raw message for round-tripping
                        let mut raw_msg = serde_json::json!({ "role": "assistant" });
                        if !accumulated_text.is_empty() {
                            raw_msg["content"] = accumulated_text.clone().into();
                        }
                        if !tool_calls.is_empty() {
                            raw_msg["tool_calls"] = serde_json::json!(
                                tool_calls.values().map(|(id, name, args)| {
                                    serde_json::json!({
                                        "id": id,
                                        "type": "function",
                                        "function": { "name": name, "arguments": args }
                                    })
                                }).collect::<Vec<_>>()
                            );
                        }
                        // Combine reasoning_details and reasoning_content into reasoning_details
                        if !accumulated_reasoning_details.is_empty() || !accumulated_reasoning_text.is_empty() {
                            let mut all_details = accumulated_reasoning_details.clone();
                            if !accumulated_reasoning_text.is_empty() {
                                all_details.push(serde_json::json!({
                                    "type": "reasoning.text",
                                    "text": accumulated_reasoning_text
                                }));
                            }
                            raw_msg["reasoning_details"] = serde_json::Value::Array(all_details);
                        }

                        yield LLMEvent::MessageEnd {
                            stop_reason: stop_reason.unwrap_or(StopReason::EndTurn),
                            input_tokens,
                            output_tokens,
                            reasoning_tokens,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                            raw: Some(raw_msg),
                        };
                        return;
                    }

                    let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
                        Ok(c) => c,
                        Err(e) => {
                            yield LLMEvent::Error(format!("Parse error: {} - data: {}", e, data));
                            return;
                        }
                    };

                    if let Some(usage) = chunk.usage {
                        input_tokens = usage.prompt_tokens;
                        output_tokens = usage.completion_tokens;
                        reasoning_tokens = usage
                            .output_tokens_details
                            .map(|d| d.reasoning_tokens)
                            .unwrap_or(0);
                    }

                    for choice in chunk.choices {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }

                        if let Some(details) = choice.delta.reasoning_details {
                            for detail_json in details {
                                let text = detail_json
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| {
                                        detail_json.get("summary").and_then(|v| v.as_str())
                                    });
                                if let Some(text) = text
                                    && !text.is_empty()
                                {
                                    yield LLMEvent::ThinkingDelta(text.to_string());
                                }
                                accumulated_reasoning_details.push(detail_json);
                            }
                        }

                        if let Some(ref reasoning_text) = choice.delta.reasoning_content
                            && !reasoning_text.is_empty()
                        {
                            yield LLMEvent::ThinkingDelta(reasoning_text.clone());
                            accumulated_reasoning_text.push_str(reasoning_text);
                        }

                        if let Some(content) = choice.delta.content
                            && !content.is_empty()
                        {
                            accumulated_text.push_str(&content);
                            yield LLMEvent::TextDelta(content);
                        }

                        if let Some(tc_deltas) = choice.delta.tool_calls {
                            for tc_delta in tc_deltas {
                                let index = tc_delta.index;
                                let is_new = !tool_calls.contains_key(&index);
                                let entry = tool_calls
                                    .entry(index)
                                    .or_insert_with(|| (String::new(), String::new(), String::new()));

                                if let Some(id) = tc_delta.id {
                                    entry.0 = id;
                                }
                                if let Some(func) = tc_delta.function {
                                    if let Some(name) = func.name {
                                        entry.1 = name;
                                    }
                                    // Emit ToolCallStart before the first delta so
                                    // the renderer can set up state for this tool call.
                                    if is_new {
                                        yield LLMEvent::ToolCallStart {
                                            index,
                                            id: entry.0.clone(),
                                            name: entry.1.clone(),
                                        };
                                    }
                                    if let Some(args) = func.arguments {
                                        entry.2.push_str(&args);
                                        yield LLMEvent::ToolCallDelta {
                                            index,
                                            partial_json: args,
                                        };
                                    }
                                } else if is_new {
                                    // First delta has id but no function block — emit start anyway.
                                    yield LLMEvent::ToolCallStart {
                                        index,
                                        id: entry.0.clone(),
                                        name: entry.1.clone(),
                                    };
                                }
                            }
                        }

                        if let Some(reason) = choice.finish_reason {
                            stop_reason = Some(match reason.as_str() {
                                "tool_calls" => {
                                    for (_, (id, name, args)) in tool_calls.iter() {
                                        yield LLMEvent::ToolCall(ToolCall {
                                            id: id.clone(),
                                            name: name.clone(),
                                            arguments: args.clone(),
                                        });
                                    }
                                    StopReason::ToolUse
                                }
                                "length" => StopReason::MaxTokens,
                                _ => StopReason::EndTurn,
                            });
                        }
                    }
            }

            // Stream ended without [DONE]
            if !tool_calls.is_empty() {
                for (_, (id, name, args)) in tool_calls.iter() {
                    yield LLMEvent::ToolCall(ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: args.clone(),
                    });
                }
            }

            // Build raw message for round-tripping
            let mut raw_msg = serde_json::json!({ "role": "assistant" });
            if !accumulated_text.is_empty() {
                raw_msg["content"] = accumulated_text.into();
            }
            if !tool_calls.is_empty() {
                raw_msg["tool_calls"] = serde_json::json!(
                    tool_calls.values().map(|(id, name, args)| {
                        serde_json::json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": args }
                        })
                    }).collect::<Vec<_>>()
                );
            }
            if !accumulated_reasoning_details.is_empty() || !accumulated_reasoning_text.is_empty() {
                let mut all_details = accumulated_reasoning_details;
                if !accumulated_reasoning_text.is_empty() {
                    all_details.push(serde_json::json!({
                        "type": "reasoning.text",
                        "text": accumulated_reasoning_text
                    }));
                }
                raw_msg["reasoning_details"] = serde_json::Value::Array(all_details);
            }

            yield LLMEvent::MessageEnd {
                stop_reason: stop_reason.unwrap_or(StopReason::EndTurn),
                input_tokens,
                output_tokens,
                reasoning_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                raw: Some(raw_msg),
            };
        })
    }
}
