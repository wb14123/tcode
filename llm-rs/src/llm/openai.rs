//! OpenAI Responses API LLM implementation.
//!
//! Uses the Responses API (`/v1/responses`) for reasoning model support.
//! Uses `reqwest` + `sse.rs` directly (same pattern as Claude/OpenRouter),
//! enabling dynamic OAuth token refresh via `GetTokenFn`.
//!
//! Reasoning is streamed via `ThinkingDelta` events for display but not
//! persisted across turns in stateless mode. For full reasoning persistence,
//! use server-managed mode with `previous_response_id`.

use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use super::openai_common::effort_to_str;
use super::sse;
use super::{
    ChatOptions, GetTokenFn, LLM, LLMEvent, LLMMessage, ModelInfo, StopReason, TokenProvider,
    ToolCall,
};
use crate::tool::Tool;
use crate::tool::normalize_schema;

// ============================================================================
// Request types (OpenAI Responses API)
// ============================================================================

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    input: Vec<InputItem>,
    stream: bool,
    /// Must be `false` for the ChatGPT backend proxy. Defaults to `true` on
    /// the standard API, so we always set it explicitly.
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<FunctionToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

/// An input item for the Responses API.
/// Can be either a simple message or a structured item (function call, output, etc.).
#[derive(Serialize)]
#[serde(untagged)]
enum InputItem {
    EasyMessage(EasyInputMessage),
    Item(serde_json::Value),
}

#[derive(Serialize)]
struct EasyInputMessage {
    role: &'static str,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<serde_json::Value>,
}

#[derive(Clone, Serialize)]
struct FunctionToolDef {
    r#type: &'static str,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'static str>,
}

// ============================================================================
// Response / SSE event types (OpenAI Responses API)
// ============================================================================

/// Payload for `response.completed` / `response.failed` / `response.incomplete` events.
#[derive(Deserialize, Debug)]
struct ResponsePayload {
    response: ResponseData,
}

#[derive(Deserialize, Debug)]
struct ResponseData {
    #[serde(default)]
    output: Vec<serde_json::Value>,
    #[serde(default)]
    usage: Option<UsageData>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Deserialize, Debug)]
struct UsageData {
    #[serde(default)]
    input_tokens: i32,
    #[serde(default)]
    output_tokens: i32,
    #[serde(default)]
    input_tokens_details: Option<InputTokensDetails>,
    #[serde(default)]
    output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Deserialize, Debug, Default)]
struct InputTokensDetails {
    #[serde(default)]
    cached_tokens: i32,
}

#[derive(Deserialize, Debug, Default)]
struct OutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: i32,
}

#[derive(Deserialize, Debug)]
struct ResponseError {
    #[serde(default)]
    message: String,
}

/// Payload for `response.output_text.delta` events.
#[derive(Deserialize, Debug)]
struct TextDeltaPayload {
    #[serde(default)]
    delta: String,
}

/// Payload for `response.reasoning_summary_text.delta` and `response.reasoning_text.delta`.
#[derive(Deserialize, Debug)]
struct ReasoningDeltaPayload {
    #[serde(default)]
    delta: String,
}

/// Payload for `response.output_item.done` events.
#[derive(Deserialize, Debug)]
struct OutputItemDonePayload {
    item: serde_json::Value,
}

/// Payload for error events.
#[derive(Deserialize, Debug)]
struct ErrorEventPayload {
    #[serde(default)]
    message: String,
}

// ============================================================================
// OpenAI client (Responses API)
// ============================================================================

/// OpenAI LLM client using the Responses API.
///
/// Provides reasoning support:
/// - Reasoning summaries streamed as `ThinkingDelta` events for display
/// - Reasoning tokens tracked separately in usage stats
///
/// Supports both API key and OAuth authentication via `GetTokenFn`.
pub struct OpenAI {
    client: Client,
    get_token: GetTokenFn,
    base_url: String,
    /// ChatGPT account ID for OAuth requests via the ChatGPT backend proxy.
    account_id: Option<String>,
    /// Opaque key for server-side prompt caching. Each clone (= each conversation)
    /// gets a fresh UUID so the server can cache prompt prefixes per conversation.
    cache_key: String,
    cached_tools: Option<Vec<FunctionToolDef>>,
}

impl OpenAI {
    /// Create a new OpenAI client with the default API base URL.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, "https://api.openai.com/v1")
    }

    /// Create a new OpenAI client with a custom base URL.
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let token = api_key.into();
        Self {
            client: Client::new(),
            get_token: Arc::new(move || {
                let t = token.clone();
                Box::pin(async move { Ok(t) })
            }),
            base_url: base_url.into(),
            account_id: None,
            cache_key: uuid::Uuid::new_v4().to_string(),
            cached_tools: None,
        }
    }

    /// Create a new OpenAI client with a custom token getter function.
    /// Use this for OAuth tokens with auto-refresh.
    pub fn with_get_token(get_token: GetTokenFn, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            get_token,
            base_url: base_url.into(),
            account_id: None,
            cache_key: uuid::Uuid::new_v4().to_string(),
            cached_tools: None,
        }
    }

    /// Create a new OpenAI client with a token provider.
    ///
    /// Accepts any cloneable type with an async `get_access_token` method,
    /// wrapping it into the `GetTokenFn` boilerplate automatically.
    pub fn with_token_provider<T>(provider: T, base_url: impl Into<String>) -> Self
    where
        T: TokenProvider + Clone + Send + Sync + 'static,
    {
        let get_token: GetTokenFn = Arc::new(move || {
            let p = provider.clone();
            Box::pin(async move { p.get_access_token().await })
        });
        Self::with_get_token(get_token, base_url)
    }

    /// Set the ChatGPT account ID for OAuth requests via the ChatGPT backend proxy.
    pub fn with_account_id(mut self, account_id: Option<String>) -> Self {
        self.account_id = account_id;
        self
    }
}

// ============================================================================
// Message conversion: LLMMessage -> InputItem
// ============================================================================

/// Convert LLM messages into Responses API input items.
/// Returns `(instructions, input_items)` — the system message is extracted
/// as the top-level `instructions` field (required by the ChatGPT backend proxy),
/// and remaining messages become input items.
fn convert_messages(msgs: &[LLMMessage]) -> (Option<String>, Vec<InputItem>) {
    let mut items = Vec::new();
    let mut instructions: Option<String> = None;

    for msg in msgs {
        match msg {
            LLMMessage::System(content) => {
                // Use the last system message as instructions.
                // Also include as a developer input item for the standard API.
                instructions = Some(content.clone());
            }
            LLMMessage::User(content) => {
                items.push(InputItem::EasyMessage(EasyInputMessage {
                    role: "user",
                    content: content.clone(),
                    phase: None,
                }));
            }
            LLMMessage::Assistant {
                content,
                tool_calls,
                raw,
            } => {
                if let Some(raw_value) = raw {
                    // Use raw output items directly - preserves reasoning + message pairing
                    if let Some(arr) = raw_value.as_array() {
                        for item_json in arr {
                            items.push(InputItem::Item(item_json.clone()));
                        }
                    }
                } else {
                    // Fallback: reconstruct from fields (for messages not from OpenAI).
                    // For assistant turns that did NOT originate from an OpenAI response
                    // (so we have no phase to preserve); `None` is the only sensible value.
                    if !content.is_empty() {
                        items.push(InputItem::EasyMessage(EasyInputMessage {
                            role: "assistant",
                            content: content.clone(),
                            phase: None,
                        }));
                    }
                    for tc in tool_calls {
                        items.push(InputItem::Item(serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": tc.arguments,
                        })));
                    }
                }
            }
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                items.push(InputItem::Item(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content,
                })));
            }
        }
    }

    (instructions, items)
}

// ============================================================================
// LLM trait implementation
// ============================================================================

impl LLM for OpenAI {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        self.cached_tools = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|t| FunctionToolDef {
                        r#type: "function",
                        name: t.name.clone(),
                        description: Some(t.description.clone()),
                        parameters: Some(normalize_schema(&t.param_schema)),
                    })
                    .collect(),
            )
        };
    }

    fn clone_box(&self) -> Box<dyn LLM> {
        Box::new(OpenAI {
            client: self.client.clone(),
            get_token: self.get_token.clone(),
            base_url: self.base_url.clone(),
            account_id: self.account_id.clone(),
            cache_key: uuid::Uuid::new_v4().to_string(),
            cached_tools: None,
        })
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "gpt-5".into(),
                description: "Most capable OpenAI model".into(),
            },
            ModelInfo {
                id: "gpt-5.4".into(),
                description: "Balanced capability and speed".into(),
            },
            ModelInfo {
                id: "gpt-5-nano".into(),
                description: "Fast and cost-effective".into(),
            },
            ModelInfo {
                id: "o3".into(),
                description: "Reasoning model".into(),
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
        let get_token = self.get_token.clone();
        let base_url = self.base_url.clone();
        let account_id = self.account_id.clone();
        let model = model.to_string();
        let (instructions, input_items) = convert_messages(msgs);
        let tools = self.cached_tools.clone();
        let max_output_tokens = options.max_tokens;
        let cache_key = self.cache_key.clone();

        // Build reasoning config
        let reasoning = {
            let has_reasoning =
                options.reasoning_effort.is_some() || options.reasoning_budget.is_some();
            if has_reasoning {
                let effort = options.reasoning_effort.as_ref().map(effort_to_str);
                Some(ReasoningConfig {
                    effort,
                    summary: Some("auto"),
                })
            } else {
                None
            }
        };

        Box::pin(stream! {
            // Get a valid access token (may trigger refresh if expired)
            let access_token = match get_token().await {
                Ok(token) => token,
                Err(e) => {
                    yield LLMEvent::Error(format!("Failed to get access token: {}", e));
                    return;
                }
            };

            let request_body = ResponsesRequest {
                model: &model,
                input: input_items,
                stream: true,
                store: false,
                instructions,
                max_output_tokens,
                tools,
                reasoning,
                prompt_cache_key: Some(cache_key.clone()),
            };

            let url = format!("{}/responses", base_url);
            let mut request = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", access_token))
                .header("Content-Type", "application/json")
                // Session affinity headers — tell the proxy to route requests
                // to the same backend server so the prompt cache is reused.
                .header("x-session-affinity", cache_key.as_str())
                .header("session_id", cache_key.as_str());
            if let Some(ref id) = account_id {
                request = request.header("ChatGPT-Account-ID", id.as_str());
            }
            let response = match sse::check_response(
                request
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

            let mut emitted_start = false;
            let mut tool_call_counter: usize = 0;
            let mut saw_function_calls = false;
            // Collect raw function_call items from output_item.done events,
            // in case the response.completed output array omits them (ChatGPT proxy).
            let mut streamed_function_calls: Vec<serde_json::Value> = Vec::new();
            let mut sse_events = sse::sse_stream(response);

            while let Some(event_result) = sse_events.next().await {
                let event = match event_result {
                    Ok(e) => e,
                    Err(e) => {
                        yield LLMEvent::Error(e);
                        return;
                    }
                };

                let event_type = event.event_type.as_deref().unwrap_or("unknown");
                let data = &event.data;

                match event_type {
                    // Response created — emit MessageStart
                    "response.created" => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                    }

                    // Text delta
                    "response.output_text.delta" => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                        if let Ok(parsed) = serde_json::from_str::<TextDeltaPayload>(data)
                            && !parsed.delta.is_empty()
                        {
                            yield LLMEvent::TextDelta(parsed.delta);
                        }
                    }

                    // Reasoning summary text delta → ThinkingDelta
                    "response.reasoning_summary_text.delta" => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                        if let Ok(parsed) = serde_json::from_str::<ReasoningDeltaPayload>(data)
                            && !parsed.delta.is_empty()
                        {
                            yield LLMEvent::ThinkingDelta(parsed.delta);
                        }
                    }

                    // Reasoning text delta (raw reasoning) → ThinkingDelta
                    "response.reasoning_text.delta" => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                        if let Ok(parsed) = serde_json::from_str::<ReasoningDeltaPayload>(data)
                            && !parsed.delta.is_empty()
                        {
                            yield LLMEvent::ThinkingDelta(parsed.delta);
                        }
                    }

                    // Output item done — handle function calls
                    "response.output_item.done" => {
                        if let Ok(parsed) = serde_json::from_str::<OutputItemDonePayload>(data) {
                            let item = &parsed.item;
                            let item_type = item.get("type").and_then(|t| t.as_str());
                            if item_type == Some("function_call") {
                                saw_function_calls = true;
                                streamed_function_calls.push(parsed.item.clone());
                                let call_id = item.get("call_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();
                                let name = item.get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();
                                let arguments = item.get("arguments")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();

                                // Emit streaming events for the UI (conversation layer
                                // uses ToolCallStart/ToolCallDelta to broadcast to display).
                                let tc_index = tool_call_counter;
                                tool_call_counter += 1;
                                yield LLMEvent::ToolCallStart {
                                    index: tc_index,
                                    id: call_id.clone(),
                                    name: name.clone(),
                                };
                                yield LLMEvent::ToolCallDelta {
                                    index: tc_index,
                                    partial_json: arguments.clone(),
                                };

                                // Emit the complete tool call (conversation layer
                                // uses this to build the pending_tool_calls list).
                                yield LLMEvent::ToolCall(ToolCall {
                                    id: call_id,
                                    name,
                                    arguments,
                                });
                            }
                        }
                    }

                    // Response completed — emit MessageEnd with usage
                    "response.completed" => {
                        if let Ok(parsed) = serde_json::from_str::<ResponsePayload>(data) {
                            let resp = parsed.response;

                            // Serialize full output for round-tripping.
                            // If the response.completed output array is missing function calls
                            // that we saw during streaming (ChatGPT proxy behavior), merge them in.
                            let mut output = resp.output.clone();
                            if saw_function_calls {
                                let has_fc_in_output = output.iter().any(|item| {
                                    item.get("type").and_then(|t| t.as_str()) == Some("function_call")
                                });
                                if !has_fc_in_output {
                                    output.extend(streamed_function_calls.iter().cloned());
                                }
                            }
                            let raw_output = serde_json::Value::Array(output);

                            let (input_tokens, output_tokens, reasoning_tokens, cached_tokens) =
                                extract_usage(&resp.usage);

                            // Determine stop reason — check both the response.completed
                            // output array AND function calls seen during streaming
                            // (the ChatGPT proxy may not include them in the output array).
                            let has_function_calls = saw_function_calls
                                || resp.output.iter().any(|item| {
                                    item.get("type").and_then(|t| t.as_str()) == Some("function_call")
                                });

                            let stop_reason = match resp.status.as_deref() {
                                _ if has_function_calls => StopReason::ToolUse,
                                Some("completed") => StopReason::EndTurn,
                                _ => StopReason::EndTurn,
                            };

                            if !emitted_start {
                                yield LLMEvent::MessageStart { input_tokens: 0 };
                            }
                            yield LLMEvent::MessageEnd {
                                stop_reason,
                                input_tokens,
                                output_tokens,
                                reasoning_tokens,
                                cache_creation_input_tokens: 0,
                                cache_read_input_tokens: cached_tokens,
                                raw: Some(raw_output),
                            };
                            return;
                        } else {
                            tracing::warn!("Failed to parse response.completed event: {}", data);
                            yield LLMEvent::Error(
                                "Failed to parse response.completed event".to_string(),
                            );
                            return;
                        }
                    }

                    // Response failed
                    "response.failed" => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                        }
                        if let Ok(parsed) = serde_json::from_str::<ResponsePayload>(data) {
                            let msg = parsed.response.error
                                .map(|err| err.message)
                                .unwrap_or_else(|| "Unknown error".to_string());
                            yield LLMEvent::Error(format!("Response failed: {}", msg));
                        } else {
                            yield LLMEvent::Error(format!("Response failed: {}", data));
                        }
                        return;
                    }

                    // Response incomplete
                    "response.incomplete" => {
                        if let Ok(parsed) = serde_json::from_str::<ResponsePayload>(data) {
                            let resp = parsed.response;

                            // Merge streamed function calls if missing from
                            // the output array (same as response.completed).
                            let mut output = resp.output.clone();
                            if saw_function_calls {
                                let has_fc_in_output = output.iter().any(|item| {
                                    item.get("type").and_then(|t| t.as_str()) == Some("function_call")
                                });
                                if !has_fc_in_output {
                                    output.extend(streamed_function_calls.iter().cloned());
                                }
                            }
                            let raw_output = serde_json::Value::Array(output);

                            let (input_tokens, output_tokens, reasoning_tokens, cached_tokens) =
                                extract_usage(&resp.usage);

                            let has_function_calls = saw_function_calls
                                || resp.output.iter().any(|item| {
                                    item.get("type").and_then(|t| t.as_str()) == Some("function_call")
                                });

                            let stop_reason = if has_function_calls {
                                StopReason::ToolUse
                            } else {
                                StopReason::MaxTokens
                            };

                            if !emitted_start {
                                yield LLMEvent::MessageStart { input_tokens: 0 };
                            }
                            yield LLMEvent::MessageEnd {
                                stop_reason,
                                input_tokens,
                                output_tokens,
                                reasoning_tokens,
                                cache_creation_input_tokens: 0,
                                cache_read_input_tokens: cached_tokens,
                                raw: Some(raw_output),
                            };
                            return;
                        } else {
                            tracing::warn!("Failed to parse response.incomplete event: {}", data);
                            yield LLMEvent::Error(
                                "Failed to parse response.incomplete event".to_string(),
                            );
                            return;
                        }
                    }

                    // Error event
                    "error" => {
                        if let Ok(parsed) = serde_json::from_str::<ErrorEventPayload>(data) {
                            yield LLMEvent::Error(format!("API error: {}", parsed.message));
                        } else {
                            yield LLMEvent::Error(format!("API error: {}", data));
                        }
                        return;
                    }

                    // All other events — ignore
                    _ => {}
                }
            }

            // Stream ended without a terminal event — shouldn't happen but handle gracefully
            if emitted_start {
                yield LLMEvent::MessageEnd {
                    stop_reason: StopReason::EndTurn,
                    input_tokens: 0,
                    output_tokens: 0,
                    reasoning_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    raw: None,
                };
            }
        })
    }
}

/// Extract (input_tokens, output_tokens, reasoning_tokens, cached_tokens) from optional usage data.
///
/// OpenAI's `input_tokens` includes cached tokens, but our `LLMEvent::MessageEnd`
/// convention (matching Anthropic) treats `input_tokens` and `cache_read_input_tokens`
/// as additive. So we subtract cached from input here.
fn extract_usage(usage: &Option<UsageData>) -> (i32, i32, i32, i32) {
    if let Some(usage) = usage {
        let reasoning = usage
            .output_tokens_details
            .as_ref()
            .map(|d| d.reasoning_tokens)
            .unwrap_or(0);
        let cached = usage
            .input_tokens_details
            .as_ref()
            .map(|d| d.cached_tokens)
            .unwrap_or(0);
        // Subtract cached from input so input + cached = total (Anthropic convention).
        (
            usage.input_tokens - cached,
            usage.output_tokens,
            reasoning,
            cached,
        )
    } else {
        (0, 0, 0, 0)
    }
}
