//! OpenAI-compatible LLM implementation (works with OpenRouter).

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use super::{ChatOptions, LLMEvent, LLMMessage, ReasoningDetail, ReasoningEffort, StopReason, ToolCall, LLM};
use crate::tool::Tool;

// ============================================================================
// OpenAI ReasoningDetail implementation
// ============================================================================

/// OpenAI/OpenRouter reasoning detail — wraps raw JSON from the provider.
#[derive(Debug)]
struct OpenAIReasoningDetail {
    raw: serde_json::Value,
}

impl OpenAIReasoningDetail {
    fn from_json(value: serde_json::Value) -> Self {
        Self { raw: value }
    }

    fn from_text(text: String) -> Self {
        Self {
            raw: serde_json::json!({"type": "reasoning.text", "text": text}),
        }
    }
}

impl ReasoningDetail for OpenAIReasoningDetail {
    fn text(&self) -> Option<&str> {
        // "text" for reasoning.text, "summary" for reasoning.summary
        self.raw
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| self.raw.get("summary").and_then(|v| v.as_str()))
    }

    fn to_json(&self) -> serde_json::Value {
        self.raw.clone()
    }
}

// ============================================================================
// OpenAI client
// ============================================================================

/// OpenAI-compatible LLM client.
///
/// Works with OpenAI API, OpenRouter, and other compatible providers.
pub struct OpenAI {
    client: Client,
    api_key: String,
    base_url: String,
    /// Cached tool definitions for API requests.
    cached_tool_defs: Option<Vec<ToolDefinition>>,
}

impl OpenAI {
    /// Create a new OpenAI client.
    ///
    /// # Arguments
    /// - `api_key`: API key for authentication
    /// - `base_url`: Base URL for the API (e.g., "https://api.openai.com/v1" or "https://openrouter.ai/api/v1")
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            cached_tool_defs: None,
        }
    }

    /// Create a new OpenRouter client.
    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://openrouter.ai/api/v1")
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
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// OpenRouter format: nested object `reasoning: { effort, max_tokens, exclude }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningRequest>,
    /// OpenAI Chat Completions format: top-level string `reasoning_effort: "medium"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Serialize)]
struct ReasoningRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "is_false")]
    exclude: bool,
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

/// Tool call in assistant message format for OpenAI API.
#[derive(Serialize)]
struct ChatMessageToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: &'static str,
    function: ChatMessageToolCallFunction,
}

#[derive(Serialize)]
struct ChatMessageToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Clone, Serialize)]
struct ToolDefinition {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: FunctionDefinition,
}

#[derive(Clone, Serialize)]
struct FunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
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
    /// Structured reasoning details (OpenRouter unified format).
    #[serde(default)]
    reasoning_details: Option<Vec<serde_json::Value>>,
    /// Simple reasoning content string (some providers).
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

/// Convert `ReasoningEffort` enum to the API string representation.
fn effort_to_str(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::XHigh => "xhigh",
        ReasoningEffort::High => "high",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Minimal => "minimal",
    }
}

/// Build a `ReasoningRequest` from `ChatOptions`, or None if no reasoning config is set.
fn build_reasoning_request(options: &ChatOptions) -> Option<ReasoningRequest> {
    if options.reasoning_effort.is_none() && options.reasoning_budget.is_none() && !options.exclude_reasoning {
        return None;
    }
    Some(ReasoningRequest {
        effort: options.reasoning_effort.as_ref().map(effort_to_str),
        max_tokens: options.reasoning_budget,
        exclude: options.exclude_reasoning,
    })
}

/// Normalize a schemars Schema into OpenAI-compatible JSON.
/// OpenAI requires `type: "object"` and `properties` even for empty params.
fn normalize_schema_for_openai(schema: &schemars::Schema) -> serde_json::Value {
    let mut value = serde_json::to_value(schema).unwrap_or(serde_json::json!({}));

    if let Some(obj) = value.as_object_mut() {
        // Ensure type is "object"
        if !obj.contains_key("type") {
            obj.insert("type".to_string(), serde_json::json!("object"));
        }
        // Ensure properties exists
        if !obj.contains_key("properties") {
            obj.insert("properties".to_string(), serde_json::json!({}));
        }
    }

    value
}

impl LLM for OpenAI {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        self.cached_tool_defs = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|t| ToolDefinition {
                        tool_type: "function",
                        function: FunctionDefinition {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: normalize_schema_for_openai(&t.param_schema),
                        },
                    })
                    .collect(),
            )
        };
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
        let is_openrouter = base_url.contains("openrouter.ai");
        let reasoning_request = build_reasoning_request(options);
        // OpenAI Chat Completions uses top-level `reasoning_effort` string;
        // OpenRouter uses nested `reasoning: { effort, ... }` object.
        let (reasoning, reasoning_effort) = if is_openrouter {
            (reasoning_request, None)
        } else {
            let effort_str = reasoning_request.as_ref().and_then(|r| r.effort);
            (None, effort_str)
        };

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
                    reasoning,
                } => {
                    let tc = if tool_calls.is_empty() {
                        None
                    } else {
                        Some(
                            tool_calls
                                .iter()
                                .map(|tc| ChatMessageToolCall {
                                    id: tc.id.clone(),
                                    call_type: "function",
                                    function: ChatMessageToolCallFunction {
                                        name: tc.name.clone(),
                                        arguments: tc.arguments.clone(),
                                    },
                                })
                                .collect(),
                        )
                    };
                    let rd = if reasoning.is_empty() {
                        None
                    } else {
                        Some(reasoning.iter().map(|r| r.to_json()).collect())
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
                        reasoning_details: rd,
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

        // Use cached tool definitions
        let tool_defs = self.cached_tool_defs.clone();

        Box::pin(stream! {
            let request_body = ChatRequest {
                model: &model,
                messages,
                stream: true,
                tools: tool_defs,
                stream_options: Some(StreamOptions { include_usage: true }),
                reasoning,
                reasoning_effort,
            };

            let url = format!("{}/chat/completions", base_url);
            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await;

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    yield LLMEvent::Error(format!("Request failed: {}", e));
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                yield LLMEvent::Error(format!("API error {}: {}", status, body));
                return;
            }

            // Track tool calls being built across chunks
            let mut tool_calls: HashMap<usize, (String, String, String)> = HashMap::new();
            let mut input_tokens = 0i32;
            let mut output_tokens = 0i32;
            let mut reasoning_tokens = 0i32;
            let mut emitted_start = false;
            let mut stop_reason: Option<StopReason> = None;

            // Accumulate reasoning details for round-tripping
            let mut accumulated_reasoning: Vec<Arc<dyn ReasoningDetail>> = Vec::new();
            // Accumulate simple reasoning_content text (some providers use this instead of reasoning_details)
            let mut accumulated_reasoning_text = String::new();

            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        yield LLMEvent::Error(format!("Stream error: {}", e));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE lines
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if !line.starts_with("data: ") {
                        continue;
                    }

                    let data = &line[6..];

                    if data == "[DONE]" {
                        // Convert any accumulated reasoning_content text to a detail
                        if !accumulated_reasoning_text.is_empty() {
                            let text = std::mem::take(&mut accumulated_reasoning_text);
                            accumulated_reasoning.push(Arc::new(
                                OpenAIReasoningDetail::from_text(text),
                            ));
                        }

                        yield LLMEvent::MessageEnd {
                            stop_reason: stop_reason.unwrap_or(StopReason::EndTurn),
                            input_tokens,
                            output_tokens,
                            reasoning_tokens,
                            reasoning: std::mem::take(&mut accumulated_reasoning),
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

                    // Handle usage info (sent with stream_options.include_usage)
                    if let Some(usage) = chunk.usage {
                        input_tokens = usage.prompt_tokens;
                        output_tokens = usage.completion_tokens;
                        reasoning_tokens = usage
                            .output_tokens_details
                            .map(|d| d.reasoning_tokens)
                            .unwrap_or(0);
                    }

                    for choice in chunk.choices {
                        // Emit MessageStart on first content
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }

                        // Handle reasoning details (structured array format — OpenRouter)
                        if let Some(details) = choice.delta.reasoning_details {
                            for detail_json in details {
                                // Extract text for streaming display
                                let text = detail_json
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| {
                                        detail_json.get("summary").and_then(|v| v.as_str())
                                    });
                                if let Some(text) = text {
                                    if !text.is_empty() {
                                        yield LLMEvent::ThinkingDelta(text.to_string());
                                    }
                                }
                                // Accumulate full detail for round-tripping
                                accumulated_reasoning.push(Arc::new(
                                    OpenAIReasoningDetail::from_json(detail_json),
                                ));
                            }
                        }

                        // Handle reasoning content (simple string format — some providers)
                        if let Some(ref reasoning_text) = choice.delta.reasoning_content {
                            if !reasoning_text.is_empty() {
                                yield LLMEvent::ThinkingDelta(reasoning_text.clone());
                                accumulated_reasoning_text.push_str(reasoning_text);
                            }
                        }

                        // Handle text content
                        if let Some(content) = choice.delta.content {
                            if !content.is_empty() {
                                yield LLMEvent::TextDelta(content);
                            }
                        }

                        // Handle tool calls
                        if let Some(tc_deltas) = choice.delta.tool_calls {
                            for tc_delta in tc_deltas {
                                let entry = tool_calls
                                    .entry(tc_delta.index)
                                    .or_insert_with(|| (String::new(), String::new(), String::new()));

                                if let Some(id) = tc_delta.id {
                                    entry.0 = id;
                                }
                                if let Some(func) = tc_delta.function {
                                    if let Some(name) = func.name {
                                        entry.1 = name;
                                    }
                                    if let Some(args) = func.arguments {
                                        entry.2.push_str(&args);
                                    }
                                }
                            }
                        }

                        // Handle finish reason — don't emit MessageEnd yet;
                        // the usage chunk arrives after this, before [DONE].
                        if let Some(reason) = choice.finish_reason {
                            stop_reason = Some(match reason.as_str() {
                                "tool_calls" => {
                                    // Emit completed tool calls
                                    for (_, (id, name, args)) in tool_calls.drain() {
                                        yield LLMEvent::ToolCall(ToolCall {
                                            id,
                                            name,
                                            arguments: args,
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
            }

            // Stream ended without [DONE]
            if !tool_calls.is_empty() {
                for (_, (id, name, args)) in tool_calls.drain() {
                    yield LLMEvent::ToolCall(ToolCall {
                        id,
                        name,
                        arguments: args,
                    });
                }
            }

            // Convert any accumulated reasoning_content text to a detail
            if !accumulated_reasoning_text.is_empty() {
                let text = std::mem::take(&mut accumulated_reasoning_text);
                accumulated_reasoning.push(Arc::new(
                    OpenAIReasoningDetail::from_text(text),
                ));
            }

            yield LLMEvent::MessageEnd {
                stop_reason: stop_reason.unwrap_or(StopReason::EndTurn),
                input_tokens,
                output_tokens,
                reasoning_tokens,
                reasoning: std::mem::take(&mut accumulated_reasoning),
            };
        })
    }
}
