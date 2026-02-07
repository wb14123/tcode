//! OpenAI Responses API LLM implementation.
//!
//! Uses the Responses API (`/v1/responses`) for better reasoning model support,
//! reasoning summaries, and encrypted reasoning persistence across turns.

use std::pin::Pin;
use std::sync::Arc;

use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{
    CreateResponseArgs, EasyInputContent, EasyInputMessage, FunctionCallOutput,
    FunctionCallOutputItemParam, FunctionToolCall, IncludeEnum, InputItem, InputParam, Item,
    OutputItem, Reasoning, ReasoningItem, ResponseStreamEvent, Role, Status,
    Tool as OAITool, FunctionTool,
};
use async_stream::stream;
use futures::StreamExt;
use tokio_stream::Stream;

use super::openai_common::{effort_to_str, normalize_schema_for_openai};
use super::{ChatOptions, LLMEvent, LLMMessage, ReasoningDetail, StopReason, ToolCall, LLM};
use crate::tool::Tool;

// ============================================================================
// Responses API ReasoningDetail implementation
// ============================================================================

/// Reasoning detail from the Responses API — wraps a full `ReasoningItem` as JSON.
///
/// Preserves `encrypted_content` for round-tripping reasoning across turns
/// in stateless mode (store=false or ZDR).
#[derive(Debug)]
struct ResponsesReasoningDetail {
    raw: serde_json::Value,
}

impl ResponsesReasoningDetail {
    fn from_reasoning_item(item: &ReasoningItem) -> Self {
        Self {
            raw: serde_json::to_value(item).unwrap_or_default(),
        }
    }
}

impl ReasoningDetail for ResponsesReasoningDetail {
    fn text(&self) -> Option<&str> {
        // Extract from summary[0].text
        self.raw
            .get("summary")
            .and_then(|s| s.as_array())
            .and_then(|a| a.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
    }

    fn to_json(&self) -> serde_json::Value {
        self.raw.clone()
    }
}

// ============================================================================
// OpenAI client (Responses API)
// ============================================================================

/// OpenAI LLM client using the Responses API.
///
/// Provides full reasoning support including:
/// - Reasoning summaries (streamed as `ThinkingDelta` events)
/// - Encrypted reasoning persistence across function call turns
/// - Better model intelligence vs Chat Completions API
pub struct OpenAI {
    client: async_openai::Client<OpenAIConfig>,
    cached_tools: Option<Vec<OAITool>>,
}

impl OpenAI {
    /// Create a new OpenAI client with the default API base URL.
    pub fn new(api_key: impl Into<String>) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key);
        Self {
            client: async_openai::Client::with_config(config),
            cached_tools: None,
        }
    }

    /// Create a new OpenAI client with a custom base URL.
    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(base_url);
        Self {
            client: async_openai::Client::with_config(config),
            cached_tools: None,
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
                reasoning,
            } => {
                // Pass back reasoning items first (includes encrypted_content for persistence)
                for detail in reasoning {
                    let json = detail.to_json();
                    if let Ok(reasoning_item) = serde_json::from_value::<ReasoningItem>(json) {
                        items.push(InputItem::Item(Item::Reasoning(reasoning_item)));
                    }
                }

                // Assistant text as a message
                if !content.is_empty() {
                    items.push(InputItem::EasyMessage(EasyInputMessage {
                        role: Role::Assistant,
                        content: EasyInputContent::Text(content.clone()),
                        r#type: Default::default(),
                    }));
                }

                // Tool calls as FunctionCall items
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
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                items.push(InputItem::Item(Item::FunctionCallOutput(
                    FunctionCallOutputItemParam {
                        call_id: tool_call_id.clone(),
                        output: FunctionCallOutput::Text(content.clone()),
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
            builder.include(vec![IncludeEnum::ReasoningEncryptedContent]);

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
            let mut accumulated_reasoning: Vec<Arc<dyn ReasoningDetail>> = Vec::new();

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

                    // Output item done — collect reasoning items and function calls
                    ResponseStreamEvent::ResponseOutputItemDone(e) => {
                        match e.item {
                            OutputItem::FunctionCall(fc) => {
                                yield LLMEvent::ToolCall(ToolCall {
                                    id: fc.call_id,
                                    name: fc.name,
                                    arguments: fc.arguments,
                                });
                            }
                            OutputItem::Reasoning(ref reasoning_item) => {
                                accumulated_reasoning.push(Arc::new(
                                    ResponsesReasoningDetail::from_reasoning_item(reasoning_item),
                                ));
                            }
                            _ => {}
                        }
                    }

                    // Response completed — emit MessageEnd with usage
                    ResponseStreamEvent::ResponseCompleted(e) => {
                        let response = e.response;

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
                            reasoning: std::mem::take(&mut accumulated_reasoning),
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
                            reasoning: std::mem::take(&mut accumulated_reasoning),
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
                    reasoning: std::mem::take(&mut accumulated_reasoning),
                };
            }
        })
    }
}
