//! Claude (Anthropic) Messages API LLM implementation.
//!
//! Uses the Anthropic Messages API with OAuth authentication.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use super::{ChatOptions, LLMEvent, LLMMessage, ReasoningEffort, StopReason, ToolCall, LLM};
use crate::tool::Tool;

// ============================================================================
// Token getter type
// ============================================================================

/// Function type for getting an access token. Called before each API request.
/// For static tokens, returns the same token. For OAuth, may trigger refresh.
pub type GetTokenFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>;

// ============================================================================
// Claude client
// ============================================================================

/// Claude (Anthropic) Messages API client.
pub struct Claude {
    client: Client,
    get_token: GetTokenFn,
    base_url: String,
    cached_tool_defs: Option<Vec<ClaudeToolDefinition>>,
}

impl Claude {
    /// Create a new Claude client with a static API key/token.
    pub fn new(access_token: impl Into<String>) -> Self {
        Self::with_base_url(access_token, "https://api.anthropic.com")
    }

    /// Create a new Claude client with a static token and custom base URL.
    pub fn with_base_url(access_token: impl Into<String>, base_url: impl Into<String>) -> Self {
        let token = access_token.into();
        Self {
            client: Client::new(),
            get_token: Arc::new(move || {
                let t = token.clone();
                Box::pin(async move { Ok(t) })
            }),
            base_url: base_url.into(),
            cached_tool_defs: None,
        }
    }

    /// Create a new Claude client with a custom token getter function.
    /// Use this for OAuth tokens with auto-refresh.
    ///
    /// # Example
    /// ```ignore
    /// let manager = TokenManager::load(...);
    /// let get_token: GetTokenFn = Arc::new(move || {
    ///     let m = manager.clone();
    ///     Box::pin(async move { m.get_access_token().await.map_err(|e| e.to_string()) })
    /// });
    /// let claude = Claude::with_get_token(get_token, "https://api.anthropic.com");
    /// ```
    pub fn with_get_token(get_token: GetTokenFn, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            get_token,
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
    system: Option<Vec<SystemBlock>>,
    messages: Vec<ClaudeMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ClaudeToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

/// System prompt content block for Claude API.
#[derive(Serialize)]
struct SystemBlock {
    #[serde(rename = "type")]
    block_type: &'static str,
    text: String,
}

/// Claude tool definition (note: uses `input_schema` not `parameters`).
#[derive(Clone, Serialize)]
struct ClaudeToolDefinition {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

/// Extended thinking configuration for Claude.
#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    thinking_type: &'static str,
    budget_tokens: u32,
}

/// Tool name prefix required for OAuth authentication.
const TOOL_PREFIX: &str = "mcp_";

/// Required system prompt prefix for Claude Code OAuth authentication.
/// Without this prefix, the OAuth token will be rejected with:
/// "This credential is only authorized for use with Claude Code"
const CLAUDE_CODE_SYSTEM_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

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
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        signature: String,
    },
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
    /// Tokens used for extended thinking (reasoning tokens)
    #[serde(default)]
    cache_creation_input_tokens: i32,
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
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    signature: Option<String>,
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

/// Tracks thinking content blocks being built across deltas.
struct ThinkingBlockAccumulator {
    thinking_text: String,
    signature: String,
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
/// Returns (system_blocks, messages).
/// The system prompt is prefixed with CLAUDE_CODE_SYSTEM_PREFIX for OAuth authentication.
fn convert_messages(msgs: &[LLMMessage]) -> (Option<Vec<SystemBlock>>, Vec<ClaudeMessage>) {
    let mut user_system_prompt: Option<String> = None;
    let mut claude_messages: Vec<ClaudeMessage> = Vec::new();

    for msg in msgs {
        match msg {
            LLMMessage::System(content) => {
                // Claude uses top-level system parameter, not a message role
                user_system_prompt = Some(content.clone());
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
                cached_summary,
            } => {
                // Use summary if available, otherwise full content
                let effective_content = cached_summary.as_ref().unwrap_or(content);
                // Claude requires tool_result in a user message as content block
                claude_messages.push(ClaudeMessage {
                    role: "user",
                    content: ClaudeContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tool_call_id.clone(),
                        content: effective_content.clone(),
                    }]),
                });
            }
        }
    }

    // Build final system blocks with required Claude Code prefix for OAuth
    // Using array format: [{"type": "text", "text": "..."}]
    let mut system_blocks = vec![SystemBlock {
        block_type: "text",
        text: CLAUDE_CODE_SYSTEM_PREFIX.to_string(),
    }];

    if let Some(user_prompt) = user_system_prompt {
        system_blocks.push(SystemBlock {
            block_type: "text",
            text: user_prompt,
        });
    }

    (Some(system_blocks), claude_messages)
}

// ============================================================================
// LLM trait implementation
// ============================================================================

/// Default model for tool summarization
const DEFAULT_SUMMARY_MODEL: &str = "claude-3-5-haiku-20241022";

/// Summarization prompt template
const SUMMARIZATION_PROMPT: &str = r#"Summarize the following tool output concisely.
Keep the summary under 500 chars. Output only the summary.

{tool_output}"#;

impl LLM for Claude {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        self.cached_tool_defs = build_claude_tool_defs(&tools);
    }

    fn summarize_tool_output(
        &self,
        model: Option<&str>,
        tool_output: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let model = model.unwrap_or(DEFAULT_SUMMARY_MODEL).to_string();
        let client = self.client.clone();
        let get_token = self.get_token.clone();
        let base_url = self.base_url.clone();

        // Build the prompt
        let prompt = SUMMARIZATION_PROMPT.replace("{tool_output}", tool_output);

        Box::pin(async move {
            // Get access token
            let access_token = get_token().await.map_err(|e| format!("Token error: {}", e))?;

            // Build non-streaming request
            let request_body = serde_json::json!({
                "model": model,
                "max_tokens": 1024,
                "messages": [{
                    "role": "user",
                    "content": prompt
                }]
            });

            let url = format!("{}/v1/messages", base_url);
            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", access_token))
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
                .header("x-app", "cli")
                .header("anthropic-dangerous-direct-browser-access", "true")
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

            // Extract text from response
            if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            return Ok(text.to_string());
                        }
                    }
                }
            }

            Err("No text content in response".to_string())
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
        let get_token = self.get_token.clone();
        let base_url = self.base_url.clone();
        let model = model.to_string();
        let tool_defs = self.cached_tool_defs.clone();

        // Convert messages
        let (system_blocks, messages) = convert_messages(msgs);

        // Capture max_tokens option
        let max_tokens_option = options.max_tokens;

        // Map ChatOptions to Claude thinking config
        // If reasoning_budget is set, use it directly
        // Otherwise, map reasoning_effort to a default budget
        let thinking = if let Some(budget) = options.reasoning_budget {
            Some(ThinkingConfig {
                thinking_type: "enabled",
                budget_tokens: budget,
            })
        } else if let Some(ref effort) = options.reasoning_effort {
            // Map reasoning effort to budget tokens for Claude
            let budget = match effort {
                ReasoningEffort::Minimal => 4000,
                ReasoningEffort::Low => 8000,
                ReasoningEffort::Medium => 16000,
                ReasoningEffort::High => 24000,
                ReasoningEffort::XHigh => 31999, // Max allowed
            };
            Some(ThinkingConfig {
                thinking_type: "enabled",
                budget_tokens: budget,
            })
        } else {
            None
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

            // Calculate max_tokens: must be greater than thinking.budget_tokens if thinking is enabled
            // Use provided max_tokens option, otherwise use defaults
            const DEFAULT_OUTPUT_TOKENS: u32 = 8192;
            let max_tokens = match (&thinking, max_tokens_option) {
                // User provided explicit max_tokens
                (_, Some(user_max)) => user_max,
                // Thinking enabled: budget + default output buffer
                (Some(config), None) => config.budget_tokens + DEFAULT_OUTPUT_TOKENS,
                // No thinking, no user override: use default
                (None, None) => DEFAULT_OUTPUT_TOKENS,
            };

            let request_body = MessagesRequest {
                model: &model,
                max_tokens,
                system: system_blocks,
                messages,
                stream: true,
                tools: tool_defs,
                thinking,
            };

            // OAuth requires ?beta=true query param and additional beta headers
            let url = format!("{}/v1/messages?beta=true", base_url);
            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", access_token))
                .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14")
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .header("User-Agent", "claude-cli/2.1.2 (external, cli)")
                // Additional headers required for Claude Code OAuth
                .header("x-app", "cli")
                .header("anthropic-dangerous-direct-browser-access", "true")
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

            // Track thinking blocks being built (by index)
            let mut thinking_blocks: HashMap<usize, ThinkingBlockAccumulator> = HashMap::new();
            let mut reasoning_tokens = 0i32;

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
                                        "thinking" => {
                                            // Start tracking a new thinking block
                                            thinking_blocks.insert(
                                                parsed.index,
                                                ThinkingBlockAccumulator {
                                                    thinking_text: String::new(),
                                                    signature: String::new(),
                                                },
                                            );
                                        }
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
                                        "thinking_delta" => {
                                            // Accumulate thinking text and emit delta
                                            if let Some(thinking) = parsed.delta.thinking {
                                                if let Some(acc) = thinking_blocks.get_mut(&parsed.index) {
                                                    acc.thinking_text.push_str(&thinking);
                                                }
                                                yield LLMEvent::ThinkingDelta(thinking);
                                            }
                                        }
                                        "signature_delta" => {
                                            // Accumulate signature for thinking block
                                            if let Some(sig) = parsed.delta.signature.as_ref() {
                                                if let Some(acc) = thinking_blocks.get_mut(&parsed.index) {
                                                    acc.signature.push_str(sig);
                                                }
                                            }
                                        }
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
                                        // Check if this was a thinking block
                                        if let Some(acc) = thinking_blocks.remove(&index) {
                                            // Store for raw round-tripping (with signature for verification)
                                            accumulated_content.push(ContentBlock::Thinking {
                                                thinking: acc.thinking_text,
                                                signature: acc.signature,
                                            });
                                        }
                                    }
                                }
                            }
                            "message_delta" => {
                                if let Ok(parsed) = serde_json::from_str::<MessageDeltaData>(data) {
                                    if let Some(usage) = parsed.usage {
                                        output_tokens = usage.output_tokens;
                                        // Track reasoning tokens from cache_creation_input_tokens
                                        if usage.cache_creation_input_tokens > 0 {
                                            reasoning_tokens = usage.cache_creation_input_tokens;
                                        }
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
                                    reasoning_tokens,
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
                reasoning_tokens,
                raw: Some(raw),
            };
        })
    }
}
