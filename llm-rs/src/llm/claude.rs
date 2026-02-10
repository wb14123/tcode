//! Claude (Anthropic) Messages API LLM implementation.
//!
//! Uses the Anthropic Messages API with OAuth authentication.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use super::{ChatOptions, LLMEvent, LLMMessage, StopReason, ToolCall, LLM};
use crate::tool::Tool;

// ============================================================================
// Claude client
// ============================================================================

/// Claude (Anthropic) Messages API client.
pub struct Claude {
    client: Client,
    access_token: String,
    base_url: String,
    cached_tool_defs: Option<Vec<ClaudeToolDefinition>>,
}

impl Claude {
    /// Create a new Claude client with the default Anthropic API base URL.
    pub fn new(access_token: impl Into<String>) -> Self {
        Self::with_base_url(access_token, "https://api.anthropic.com")
    }

    /// Create a new Claude client with a custom base URL.
    pub fn with_base_url(access_token: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            access_token: access_token.into(),
            base_url: base_url.into(),
            cached_tool_defs: None,
        }
    }
}

// ============================================================================
// Request types
// ============================================================================

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ClaudeMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ClaudeToolDefinition>>,
    // TODO: Future extended thinking support
    // Add: thinking: Option<ThinkingConfig>
    // where ThinkingConfig = { type: "enabled", budget_tokens: N }
    // The ChatOptions.reasoning_budget can map to budget_tokens
}

/// Claude tool definition (note: uses `input_schema` not `parameters`).
#[derive(Clone, Serialize)]
struct ClaudeToolDefinition {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

/// Tool name prefix required for OAuth authentication.
const TOOL_PREFIX: &str = "mcp_";

/// Claude message format.
#[derive(Serialize)]
struct ClaudeMessage {
    role: &'static str,
    content: ClaudeContent,
}

/// Claude message content - either a simple string or array of content blocks.
#[derive(Serialize)]
#[serde(untagged)]
enum ClaudeContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Content block types for Claude messages.
#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    // TODO: Future extended thinking support
    // #[serde(rename = "thinking")]
    // Thinking { thinking: String, signature: String },
}

// ============================================================================
// SSE Response types
// ============================================================================

/// Message start event payload.
#[derive(Deserialize, Debug)]
struct MessageStartData {
    message: MessageInfo,
}

#[derive(Deserialize, Debug)]
struct MessageInfo {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    model: String,
    usage: Option<UsageInfo>,
}

#[derive(Deserialize, Debug, Default)]
struct UsageInfo {
    #[serde(default)]
    input_tokens: i32,
    #[serde(default)]
    output_tokens: i32,
}

/// Content block start event payload.
#[derive(Deserialize, Debug)]
struct ContentBlockStartData {
    index: usize,
    content_block: ContentBlockInfo,
}

#[derive(Deserialize, Debug)]
struct ContentBlockInfo {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// Content block delta event payload.
#[derive(Deserialize, Debug)]
struct ContentBlockDeltaData {
    index: usize,
    delta: DeltaInfo,
}

#[derive(Deserialize, Debug)]
struct DeltaInfo {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
    // TODO: Future extended thinking support
    // #[serde(default)]
    // thinking: Option<String>,
}

/// Message delta event payload.
#[derive(Deserialize, Debug)]
struct MessageDeltaData {
    delta: MessageDeltaInfo,
    usage: Option<UsageInfo>,
}

#[derive(Deserialize, Debug)]
struct MessageDeltaInfo {
    #[serde(default)]
    stop_reason: Option<String>,
}

/// Error event payload.
#[derive(Deserialize, Debug)]
struct ErrorData {
    error: ErrorInfo,
}

#[derive(Deserialize, Debug)]
struct ErrorInfo {
    message: String,
}

// ============================================================================
// Tool block accumulator
// ============================================================================

/// Tracks tool_use content blocks being built across deltas.
struct ToolBlockAccumulator {
    id: String,
    name: String,
    input_json: String,
}

// ============================================================================
// Helper functions
// ============================================================================

/// Build Claude tool definitions from registered tools.
/// Tool names are prefixed with `mcp_` as required by OAuth authentication.
fn build_claude_tool_defs(tools: &[Arc<Tool>]) -> Option<Vec<ClaudeToolDefinition>> {
    if tools.is_empty() {
        None
    } else {
        Some(
            tools
                .iter()
                .map(|t| {
                    // Normalize schema for Claude (ensure type: object and properties exist)
                    let mut schema = serde_json::to_value(&t.param_schema).unwrap_or_default();
                    if let Some(obj) = schema.as_object_mut() {
                        if !obj.contains_key("type") {
                            obj.insert("type".to_string(), serde_json::json!("object"));
                        }
                        if !obj.contains_key("properties") {
                            obj.insert("properties".to_string(), serde_json::json!({}));
                        }
                    }
                    ClaudeToolDefinition {
                        // Prefix tool name for OAuth
                        name: format!("{}{}", TOOL_PREFIX, t.name),
                        description: t.description.clone(),
                        input_schema: schema,
                    }
                })
                .collect(),
        )
    }
}

/// Strip the mcp_ prefix from a tool name if present.
fn strip_tool_prefix(name: &str) -> String {
    name.strip_prefix(TOOL_PREFIX).unwrap_or(name).to_string()
}

/// Convert LLMMessage list to Claude message format.
/// Returns (system_prompt, messages).
fn convert_messages(msgs: &[LLMMessage]) -> (Option<String>, Vec<ClaudeMessage>) {
    let mut system_prompt: Option<String> = None;
    let mut claude_messages: Vec<ClaudeMessage> = Vec::new();

    for msg in msgs {
        match msg {
            LLMMessage::System(content) => {
                // Claude uses top-level system parameter, not a message role
                system_prompt = Some(content.clone());
            }
            LLMMessage::User(content) => {
                claude_messages.push(ClaudeMessage {
                    role: "user",
                    content: ClaudeContent::Text(content.clone()),
                });
            }
            LLMMessage::Assistant {
                content,
                tool_calls,
                raw,
            } => {
                if let Some(raw_value) = raw {
                    // Use raw content blocks if available for round-tripping
                    if let Some(blocks) = raw_value.get("content") {
                        if let Ok(mut content_blocks) =
                            serde_json::from_value::<Vec<ContentBlock>>(blocks.clone())
                        {
                            // Ensure tool_use blocks have the mcp_ prefix
                            for block in &mut content_blocks {
                                if let ContentBlock::ToolUse { name, .. } = block {
                                    if !name.starts_with(TOOL_PREFIX) {
                                        *name = format!("{}{}", TOOL_PREFIX, name);
                                    }
                                }
                            }
                            claude_messages.push(ClaudeMessage {
                                role: "assistant",
                                content: ClaudeContent::Blocks(content_blocks),
                            });
                            continue;
                        }
                    }
                }

                // Fallback: reconstruct from fields
                if tool_calls.is_empty() {
                    claude_messages.push(ClaudeMessage {
                        role: "assistant",
                        content: ClaudeContent::Text(content.clone()),
                    });
                } else {
                    // Build content blocks for text + tool_use
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    if !content.is_empty() {
                        blocks.push(ContentBlock::Text {
                            text: content.clone(),
                        });
                    }
                    for tc in tool_calls {
                        let input: serde_json::Value =
                            serde_json::from_str(&tc.arguments).unwrap_or_default();
                        blocks.push(ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            // Prefix tool name for OAuth
                            name: format!("{}{}", TOOL_PREFIX, tc.name),
                            input,
                        });
                    }
                    claude_messages.push(ClaudeMessage {
                        role: "assistant",
                        content: ClaudeContent::Blocks(blocks),
                    });
                }
            }
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                // Claude requires tool_result in a user message as content block
                claude_messages.push(ClaudeMessage {
                    role: "user",
                    content: ClaudeContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tool_call_id.clone(),
                        content: content.clone(),
                    }]),
                });
            }
        }
    }

    (system_prompt, claude_messages)
}

// ============================================================================
// LLM trait implementation
// ============================================================================

impl LLM for Claude {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        self.cached_tool_defs = build_claude_tool_defs(&tools);
    }

    fn chat(
        &self,
        model: &str,
        msgs: &[LLMMessage],
        _options: &ChatOptions,
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
        let client = self.client.clone();
        let access_token = self.access_token.clone();
        let base_url = self.base_url.clone();
        let model = model.to_string();
        let tool_defs = self.cached_tool_defs.clone();

        // Convert messages
        let (system_prompt, messages) = convert_messages(msgs);

        // TODO: Future extended thinking support
        // Map ChatOptions to Claude thinking config:
        // if let Some(budget) = options.reasoning_budget {
        //     thinking = Some(ThinkingConfig { type: "enabled", budget_tokens: budget });
        // }

        Box::pin(stream! {
            let request_body = MessagesRequest {
                model: &model,
                max_tokens: 8192, // Default max tokens
                system: system_prompt,
                messages,
                stream: true,
                tools: tool_defs,
            };

            // OAuth requires ?beta=true query param and additional beta headers
            let url = format!("{}/v1/messages?beta=true", base_url);
            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", access_token))
                .header("anthropic-beta", "oauth-2025-04-20,interleaved-thinking-2025-05-14")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .header("User-Agent", "claude-cli/2.1.2 (external, cli)")
                .json(&request_body)
                .send()
                .await;

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    yield LLMEvent::Error(format!("Request failed: {:?}", e));
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                yield LLMEvent::Error(format!("API error {}: {}", status, body));
                return;
            }

            // State for accumulating the response
            let mut input_tokens = 0i32;
            let mut output_tokens = 0i32;
            let mut emitted_start = false;
            let mut stop_reason: Option<StopReason> = None;
            let mut accumulated_content: Vec<ContentBlock> = Vec::new();
            let mut accumulated_text = String::new();

            // Track tool_use blocks being built (by index)
            let mut tool_blocks: HashMap<usize, ToolBlockAccumulator> = HashMap::new();

            // SSE parsing state
            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut current_event_type: Option<String> = None;

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        yield LLMEvent::Error(format!("Stream error: {:?}", e));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Parse SSE format: "event: <type>\ndata: <json>\n\n"
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim_end().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() {
                        // Empty line means end of event
                        current_event_type = None;
                        continue;
                    }

                    if let Some(event_name) = line.strip_prefix("event: ") {
                        current_event_type = Some(event_name.to_string());
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data: ") {
                        let event_type = current_event_type.as_deref().unwrap_or("unknown");

                        match event_type {
                            "message_start" => {
                                if let Ok(parsed) = serde_json::from_str::<MessageStartData>(data) {
                                    if let Some(usage) = parsed.message.usage {
                                        input_tokens = usage.input_tokens;
                                    }
                                    if !emitted_start {
                                        yield LLMEvent::MessageStart { input_tokens };
                                        emitted_start = true;
                                    }
                                }
                            }
                            "content_block_start" => {
                                if let Ok(parsed) = serde_json::from_str::<ContentBlockStartData>(data) {
                                    match parsed.content_block.block_type.as_str() {
                                        "tool_use" => {
                                            // Start tracking a new tool_use block
                                            tool_blocks.insert(
                                                parsed.index,
                                                ToolBlockAccumulator {
                                                    id: parsed.content_block.id.unwrap_or_default(),
                                                    name: parsed.content_block.name.unwrap_or_default(),
                                                    input_json: String::new(),
                                                },
                                            );
                                        }
                                        "text" => {
                                            // Text block started, initial text may be present
                                            if let Some(text) = parsed.content_block.text {
                                                if !text.is_empty() {
                                                    accumulated_text.push_str(&text);
                                                    yield LLMEvent::TextDelta(text);
                                                }
                                            }
                                        }
                                        // TODO: Future extended thinking support
                                        // "thinking" => { /* handle thinking block start */ }
                                        _ => {}
                                    }
                                }
                            }
                            "content_block_delta" => {
                                if let Ok(parsed) = serde_json::from_str::<ContentBlockDeltaData>(data) {
                                    match parsed.delta.delta_type.as_str() {
                                        "text_delta" => {
                                            if let Some(text) = parsed.delta.text {
                                                if !text.is_empty() {
                                                    accumulated_text.push_str(&text);
                                                    yield LLMEvent::TextDelta(text);
                                                }
                                            }
                                        }
                                        "input_json_delta" => {
                                            // Accumulate partial JSON for tool_use input
                                            if let Some(partial) = parsed.delta.partial_json {
                                                if let Some(acc) = tool_blocks.get_mut(&parsed.index) {
                                                    acc.input_json.push_str(&partial);
                                                }
                                            }
                                        }
                                        // TODO: Future extended thinking support
                                        // "thinking_delta" => {
                                        //     if let Some(thinking) = parsed.delta.thinking {
                                        //         yield LLMEvent::ThinkingDelta(thinking);
                                        //     }
                                        // }
                                        _ => {}
                                    }
                                }
                            }
                            "content_block_stop" => {
                                // When a content block stops, finalize it
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                                    if let Some(index) = parsed.get("index").and_then(|v| v.as_u64()) {
                                        let index = index as usize;
                                        // Check if this was a tool_use block
                                        if let Some(acc) = tool_blocks.remove(&index) {
                                            // Parse the accumulated JSON
                                            let input: serde_json::Value =
                                                serde_json::from_str(&acc.input_json)
                                                    .unwrap_or(serde_json::json!({}));

                                            // Store for raw round-tripping
                                            accumulated_content.push(ContentBlock::ToolUse {
                                                id: acc.id.clone(),
                                                name: acc.name.clone(),
                                                input: input.clone(),
                                            });

                                            // Emit the tool call event (strip mcp_ prefix)
                                            yield LLMEvent::ToolCall(ToolCall {
                                                id: acc.id,
                                                name: strip_tool_prefix(&acc.name),
                                                arguments: acc.input_json,
                                            });
                                        }
                                    }
                                }
                            }
                            "message_delta" => {
                                if let Ok(parsed) = serde_json::from_str::<MessageDeltaData>(data) {
                                    if let Some(usage) = parsed.usage {
                                        output_tokens = usage.output_tokens;
                                    }
                                    if let Some(reason) = parsed.delta.stop_reason {
                                        stop_reason = Some(match reason.as_str() {
                                            "end_turn" => StopReason::EndTurn,
                                            "tool_use" => StopReason::ToolUse,
                                            "max_tokens" => StopReason::MaxTokens,
                                            _ => StopReason::EndTurn,
                                        });
                                    }
                                }
                            }
                            "message_stop" => {
                                // Build raw content for round-tripping
                                let mut raw_content = accumulated_content.clone();
                                if !accumulated_text.is_empty() {
                                    // Insert text at the beginning if we have it
                                    raw_content.insert(0, ContentBlock::Text {
                                        text: accumulated_text.clone(),
                                    });
                                }

                                let raw = serde_json::json!({
                                    "role": "assistant",
                                    "content": raw_content
                                });

                                yield LLMEvent::MessageEnd {
                                    stop_reason: stop_reason.clone().unwrap_or(StopReason::EndTurn),
                                    input_tokens,
                                    output_tokens,
                                    reasoning_tokens: 0, // TODO: Track from thinking blocks
                                    raw: Some(raw),
                                };
                                return;
                            }
                            "ping" => {
                                // Ignore ping events
                            }
                            "error" => {
                                if let Ok(parsed) = serde_json::from_str::<ErrorData>(data) {
                                    yield LLMEvent::Error(parsed.error.message);
                                    return;
                                }
                            }
                            _ => {
                                // Unknown event type - ignore per API versioning policy
                            }
                        }
                    }
                }
            }

            // Stream ended without message_stop (shouldn't happen normally)
            let mut raw_content = accumulated_content;
            if !accumulated_text.is_empty() {
                raw_content.insert(0, ContentBlock::Text {
                    text: accumulated_text,
                });
            }

            let raw = serde_json::json!({
                "role": "assistant",
                "content": raw_content
            });

            yield LLMEvent::MessageEnd {
                stop_reason: stop_reason.unwrap_or(StopReason::EndTurn),
                input_tokens,
                output_tokens,
                reasoning_tokens: 0,
                raw: Some(raw),
            };
        })
    }
}
