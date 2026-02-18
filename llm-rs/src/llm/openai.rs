//! OpenAI Responses API LLM implementation.
//!
//! Uses the Responses API (`/v1/responses`) for reasoning model support.
//! Reasoning is streamed via `ThinkingDelta` events for display but not
//! persisted across turns in stateless mode. For full reasoning persistence,
//! use server-managed mode with `previous_response_id`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{
    CreateResponseArgs, EasyInputContent, EasyInputMessage, FunctionCallOutput,
    FunctionCallOutputItemParam, FunctionToolCall, InputItem, InputParam, Item,
    OutputItem, Reasoning, ResponseStreamEvent, Role, Status,
    Tool as OAITool, FunctionTool,
};
use async_stream::stream;
use futures::StreamExt;
use tokio_stream::Stream;

use super::openai_common::{effort_to_str, normalize_schema_for_openai};
use super::{ChatOptions, LLMEvent, LLMMessage, StopReason, ToolCall, LLM};
use crate::tool::Tool;

// ============================================================================
// OpenAI client (Responses API)
// ============================================================================

/// OpenAI LLM client using the Responses API.
///
/// Provides reasoning support:
/// - Reasoning summaries streamed as `ThinkingDelta` events for display
/// - Reasoning tokens tracked separately in usage stats
pub struct OpenAI {
    client: async_openai::Client<OpenAIConfig>,
    cached_tools: Option<Vec<OAITool>>,
    /// API key stored for summarization calls (uses Chat Completions API)
    api_key: String,
    /// Base URL stored for summarization calls
    base_url: String,
}

impl OpenAI {
    /// Create a new OpenAI client with the default API base URL.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, "https://api.openai.com/v1")
    }

    /// Create a new OpenAI client with a custom base URL.
    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let api_key = api_key.into();
        let base_url = base_url.into();
        let config = OpenAIConfig::new()
            .with_api_key(api_key.clone())
            .with_api_base(base_url.clone());
        Self {
            client: async_openai::Client::with_config(config),
            cached_tools: None,
            api_key,
            base_url,
        }
    }
}

// ============================================================================
// Message conversion: LLMMessage -> InputItem
// ============================================================================

/// Convert our `LLMMessage` slice into Responses API `InputItem` list.
fn convert_messages(msgs: &[LLMMessage]) -> Vec<InputItem> {
    let mut items = Vec::new();

    for msg in msgs {
        match msg {
            LLMMessage::System(content) => {
                items.push(InputItem::EasyMessage(EasyInputMessage {
                    role: Role::Developer,
                    content: EasyInputContent::Text(content.clone()),
                    r#type: Default::default(),
                }));
            }
            LLMMessage::User(content) => {
                items.push(InputItem::EasyMessage(EasyInputMessage {
                    role: Role::User,
                    content: EasyInputContent::Text(content.clone()),
                    r#type: Default::default(),
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
                            if let Ok(item) = serde_json::from_value::<Item>(item_json.clone()) {
                                items.push(InputItem::Item(item));
                            }
                        }
                    }
                } else {
                    // Fallback: reconstruct from fields (for messages not from OpenAI)
                    if !content.is_empty() {
                        items.push(InputItem::EasyMessage(EasyInputMessage {
                            role: Role::Assistant,
                            content: EasyInputContent::Text(content.clone()),
                            r#type: Default::default(),
                        }));
                    }
                    for tc in tool_calls {
                        items.push(InputItem::Item(Item::FunctionCall(FunctionToolCall {
                            call_id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                            id: None,
                            status: None,
                        })));
                    }
                }
            }
            LLMMessage::ToolResult {
                tool_call_id,
                content,
                cached_summary,
            } => {
                // Use summary if available, otherwise full content
                let effective_content = cached_summary.as_ref().unwrap_or(content);
                items.push(InputItem::Item(Item::FunctionCallOutput(
                    FunctionCallOutputItemParam {
                        call_id: tool_call_id.clone(),
                        output: FunctionCallOutput::Text(effective_content.clone()),
                        id: None,
                        status: None,
                    },
                )));
            }
        }
    }

    items
}

// ============================================================================
// Tool summarization constants
// ============================================================================

/// Default model for tool summarization
const DEFAULT_SUMMARY_MODEL: &str = "gpt-4o-mini";

/// Summarization prompt template
const SUMMARIZATION_PROMPT: &str = r#"Summarize the following tool output concisely.
Keep the summary under 500 chars. Output only the summary.

{tool_output}"#;

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
                    .map(|t| {
                        OAITool::Function(FunctionTool {
                            name: t.name.clone(),
                            description: Some(t.description.clone()),
                            parameters: Some(normalize_schema_for_openai(&t.param_schema)),
                            strict: None,
                        })
                    })
                    .collect(),
            )
        };
    }

    fn summarize_tool_output(
        &self,
        model: Option<&str>,
        tool_output: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let model = model.unwrap_or(DEFAULT_SUMMARY_MODEL).to_string();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();

        // Build the prompt
        let prompt = SUMMARIZATION_PROMPT.replace("{tool_output}", tool_output);

        Box::pin(async move {
            let http_client = reqwest::Client::new();

            let request_body = serde_json::json!({
                "model": model,
                "max_tokens": 1024,
                "messages": [{
                    "role": "user",
                    "content": prompt
                }]
            });

            let url = format!("{}/chat/completions", base_url);
            let response = http_client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await
                .map_err(|e| format!("Request failed: {}", e))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(format!("API error {}: {}", status, body));
            }

            let body: serde_json::Value = response
                .json()
                .await
                .map_err(|e| format!("JSON parse error: {}", e))?;

            // Extract content from chat completions response
            body.get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| "No content in response".to_string())
        })
    }

    fn default_tool_summary_model(&self) -> &'static str {
        DEFAULT_SUMMARY_MODEL
    }

    fn chat(
        &self,
        model: &str,
        msgs: &[LLMMessage],
        options: &ChatOptions,
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
        let client = self.client.clone();
        let model = model.to_string();
        let input_items = convert_messages(msgs);
        let tools = self.cached_tools.clone();
        let max_output_tokens = options.max_tokens;

        // Build reasoning config
        let reasoning = {
            let has_reasoning = options.reasoning_effort.is_some()
                || options.reasoning_budget.is_some();
            if has_reasoning {
                let effort = options
                    .reasoning_effort
                    .as_ref()
                    .map(|e| match effort_to_str(e) {
                        "xhigh" => async_openai::types::responses::ReasoningEffort::Xhigh,
                        "high" => async_openai::types::responses::ReasoningEffort::High,
                        "medium" => async_openai::types::responses::ReasoningEffort::Medium,
                        "low" => async_openai::types::responses::ReasoningEffort::Low,
                        "minimal" => async_openai::types::responses::ReasoningEffort::Minimal,
                        _ => async_openai::types::responses::ReasoningEffort::Medium,
                    });
                Some(Reasoning {
                    effort,
                    summary: Some(async_openai::types::responses::ReasoningSummary::Auto),
                })
            } else {
                None
            }
        };

        Box::pin(stream! {
            // Build request
            let mut builder = CreateResponseArgs::default();
            builder.model(&model);
            builder.input(InputParam::Items(input_items));
            builder.stream(true);

            if let Some(max_tokens) = max_output_tokens {
                builder.max_output_tokens(max_tokens);
            }
            if let Some(tools) = tools {
                builder.tools(tools);
            }
            if let Some(reasoning) = reasoning {
                builder.reasoning(reasoning);
            }

            let request = match builder.build() {
                Ok(r) => r,
                Err(e) => {
                    yield LLMEvent::Error(format!("Failed to build request: {:?}", e));
                    return;
                }
            };

            let stream_result = client.responses().create_stream(request).await;
            let mut event_stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    yield LLMEvent::Error(format!("Request failed: {:?}", e));
                    return;
                }
            };

            let mut emitted_start = false;

            while let Some(event_result) = event_stream.next().await {
                let event = match event_result {
                    Ok(e) => e,
                    Err(e) => {
                        yield LLMEvent::Error(format!("Stream error: {:?}", e));
                        return;
                    }
                };

                match event {
                    // Response created — emit MessageStart
                    ResponseStreamEvent::ResponseCreated(_) => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                    }

                    // Text delta
                    ResponseStreamEvent::ResponseOutputTextDelta(e) => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                        if !e.delta.is_empty() {
                            yield LLMEvent::TextDelta(e.delta);
                        }
                    }

                    // Reasoning summary text delta → ThinkingDelta
                    ResponseStreamEvent::ResponseReasoningSummaryTextDelta(e) => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                        if !e.delta.is_empty() {
                            yield LLMEvent::ThinkingDelta(e.delta);
                        }
                    }

                    // Reasoning text delta (raw reasoning) → ThinkingDelta
                    ResponseStreamEvent::ResponseReasoningTextDelta(e) => {
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }
                        if !e.delta.is_empty() {
                            yield LLMEvent::ThinkingDelta(e.delta);
                        }
                    }

                    // Output item done — handle function calls
                    ResponseStreamEvent::ResponseOutputItemDone(e) => {
                        if let OutputItem::FunctionCall(fc) = e.item {
                            yield LLMEvent::ToolCall(ToolCall {
                                id: fc.call_id,
                                name: fc.name,
                                arguments: fc.arguments,
                            });
                        }
                        // Note: Reasoning items are streamed via ThinkingDelta events,
                        // we don't accumulate them since we don't round-trip in stateless mode
                    }

                    // Response completed — emit MessageEnd with usage
                    ResponseStreamEvent::ResponseCompleted(e) => {
                        let response = e.response;

                        // Serialize full output for round-tripping (preserves reasoning + message pairing)
                        let raw_output: serde_json::Value = serde_json::Value::Array(
                            response.output
                                .iter()
                                .map(|item| serde_json::to_value(item).unwrap_or_default())
                                .collect()
                        );

                        let (input_tokens, output_tokens, reasoning_tokens) =
                            if let Some(usage) = &response.usage {
                                (
                                    usage.input_tokens as i32,
                                    usage.output_tokens as i32,
                                    usage.output_tokens_details.reasoning_tokens as i32,
                                )
                            } else {
                                (0, 0, 0)
                            };

                        // Determine stop reason
                        let has_function_calls = response.output.iter().any(|item| {
                            matches!(item, OutputItem::FunctionCall(_))
                        });

                        let stop_reason = match response.status {
                            Status::Completed if has_function_calls => StopReason::ToolUse,
                            Status::Completed => StopReason::EndTurn,
                            Status::Incomplete => StopReason::MaxTokens,
                            _ => StopReason::EndTurn,
                        };

                        yield LLMEvent::MessageEnd {
                            stop_reason,
                            input_tokens,
                            output_tokens,
                            reasoning_tokens,
                            raw: Some(raw_output),
                        };
                        return;
                    }

                    // Response failed
                    ResponseStreamEvent::ResponseFailed(e) => {
                        let msg = e.response.error
                            .map(|err| err.message)
                            .unwrap_or_else(|| "Unknown error".to_string());
                        yield LLMEvent::Error(format!("Response failed: {}", msg));
                        return;
                    }

                    // Response incomplete
                    ResponseStreamEvent::ResponseIncomplete(e) => {
                        let response = e.response;

                        // Serialize partial output for round-tripping
                        let raw_output: serde_json::Value = serde_json::Value::Array(
                            response.output
                                .iter()
                                .map(|item| serde_json::to_value(item).unwrap_or_default())
                                .collect()
                        );

                        let (input_tokens, output_tokens, reasoning_tokens) =
                            if let Some(usage) = &response.usage {
                                (
                                    usage.input_tokens as i32,
                                    usage.output_tokens as i32,
                                    usage.output_tokens_details.reasoning_tokens as i32,
                                )
                            } else {
                                (0, 0, 0)
                            };

                        yield LLMEvent::MessageEnd {
                            stop_reason: StopReason::MaxTokens,
                            input_tokens,
                            output_tokens,
                            reasoning_tokens,
                            raw: Some(raw_output),
                        };
                        return;
                    }

                    // Error event
                    ResponseStreamEvent::ResponseError(e) => {
                        yield LLMEvent::Error(format!("API error: {}", e.message));
                        return;
                    }

                    // All other events — skip
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
                    raw: None,
                };
            }
        })
    }
}
