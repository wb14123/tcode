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

use super::sse;
use super::{
    ChatOptions, LLM, LLMEvent, LLMMessage, ModelInfo, ReasoningEffort, StopReason, ToolCall,
};
use crate::tool::Tool;

// ============================================================================
// Token getter type
// ============================================================================

/// Function type for getting an access token. Called before each API request.
/// For static tokens, returns the same token. For OAuth, may trigger refresh.
pub type GetTokenFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>;

/// Trait for types that can provide an access token (e.g. OAuth token managers).
/// Implement this to use [`Claude::with_token_provider`].
pub trait TokenProvider {
    fn get_access_token(&self) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
}

// ============================================================================
// Claude client
// ============================================================================

/// Claude (Anthropic) Messages API client.
pub struct Claude {
    client: Client,
    get_token: GetTokenFn,
    base_url: String,
    /// When true, use OAuth Bearer auth with beta headers.
    /// When false, use x-api-key header.
    use_oauth: bool,
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
            use_oauth: false,
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
            use_oauth: true,
            cached_tool_defs: None,
        }
    }

    /// Create a new Claude client with a token provider.
    ///
    /// Accepts any cloneable type with an async `get_access_token` method,
    /// wrapping it into the `GetTokenFn` boilerplate automatically.
    ///
    /// # Example
    /// ```ignore
    /// let manager = TokenManager::load(...);
    /// let claude = Claude::with_token_provider(manager, "https://api.anthropic.com");
    /// ```
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
}

// ============================================================================
// Request types
// ============================================================================

#[derive(Clone, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    cache_type: &'static str,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
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
    Thinking { thinking: String, signature: String },
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
    /// Tokens charged for writing new cache entries
    #[serde(default)]
    cache_creation_input_tokens: i32,
    /// Tokens served from cache (charged at ~10% rate)
    #[serde(default)]
    cache_read_input_tokens: i32,
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
                .map(|t| ClaudeToolDefinition {
                    // Prefix tool name for OAuth
                    name: format!("{}{}", TOOL_PREFIX, t.name),
                    description: t.description.clone(),
                    input_schema: crate::tool::normalize_schema(&t.param_schema),
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
                    if let Some(blocks) = raw_value.get("content")
                        && let Ok(mut content_blocks) =
                            serde_json::from_value::<Vec<ContentBlock>>(blocks.clone())
                    {
                        // Ensure tool_use blocks have the mcp_ prefix
                        for block in &mut content_blocks {
                            if let ContentBlock::ToolUse { name, .. } = block
                                && !name.starts_with(TOOL_PREFIX)
                            {
                                *name = format!("{}{}", TOOL_PREFIX, name);
                            }
                        }
                        claude_messages.push(ClaudeMessage {
                            role: "assistant",
                            content: ClaudeContent::Blocks(content_blocks),
                        });
                        continue;
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

impl LLM for Claude {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        self.cached_tool_defs = build_claude_tool_defs(&tools);
    }

    fn clone_box(&self) -> Box<dyn LLM> {
        Box::new(Claude {
            client: self.client.clone(),
            get_token: self.get_token.clone(),
            base_url: self.base_url.clone(),
            use_oauth: self.use_oauth,
            cached_tool_defs: None,
        })
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "claude-opus-4-6".into(),
                description: "Most capable, best for complex tasks".into(),
            },
            ModelInfo {
                id: "claude-sonnet-4-6".into(),
                description: "Balanced speed and capability".into(),
            },
            ModelInfo {
                id: "claude-haiku-4-5".into(),
                description: "Fast and cost-effective".into(),
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
        let use_oauth = self.use_oauth;
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
                cache_control: Some(CacheControl { cache_type: "ephemeral" }),
            };

            let url = if use_oauth {
                // OAuth requires ?beta=true query param
                format!("{}/v1/messages?beta=true", base_url)
            } else {
                format!("{}/v1/messages", base_url)
            };

            let mut req = client
                .post(&url)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json");

            if use_oauth {
                req = req
                    .header("Authorization", format!("Bearer {}", access_token))
                    .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14")
                    .header("User-Agent", "claude-cli/2.1.2 (external, cli)")
                    .header("x-app", "cli")
                    .header("anthropic-dangerous-direct-browser-access", "true");
            } else {
                req = req.header("x-api-key", &access_token);
            }

            let response = match sse::check_response(
                req.json(&request_body).send().await
            ).await {
                Ok(r) => r,
                Err(e) => {
                    yield LLMEvent::Error(e);
                    return;
                }
            };

            // State for accumulating the response
            let mut input_tokens = 0i32;
            let mut output_tokens = 0i32;
            let mut emitted_start = false;
            let mut stop_reason: Option<StopReason> = None;
            let mut accumulated_content: Vec<ContentBlock> = Vec::new();

            // Track tool_use blocks being built (by index)
            let mut tool_blocks: HashMap<usize, ToolBlockAccumulator> = HashMap::new();

            // Track thinking blocks being built (by index)
            let mut thinking_blocks: HashMap<usize, ThinkingBlockAccumulator> = HashMap::new();
            // Track text blocks being built (by index)
            let mut text_blocks: HashMap<usize, String> = HashMap::new();
            let reasoning_tokens = 0i32;
            let mut cache_creation_input_tokens = 0i32;
            let mut cache_read_input_tokens = 0i32;

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
                            "message_start" => {
                                if let Ok(parsed) = serde_json::from_str::<MessageStartData>(data) {
                                    if let Some(usage) = parsed.message.usage {
                                        input_tokens = usage.input_tokens;
                                        cache_creation_input_tokens += usage.cache_creation_input_tokens;
                                        cache_read_input_tokens += usage.cache_read_input_tokens;
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
                                            let id = parsed.content_block.id.unwrap_or_default();
                                            let raw_name = parsed.content_block.name.unwrap_or_default();
                                            let name = strip_tool_prefix(&raw_name);
                                            yield LLMEvent::ToolCallStart {
                                                index: parsed.index,
                                                id: id.clone(),
                                                name: name.clone(),
                                            };
                                            tool_blocks.insert(
                                                parsed.index,
                                                ToolBlockAccumulator {
                                                    id,
                                                    name: raw_name,
                                                    input_json: String::new(),
                                                },
                                            );
                                        }
                                        "text" => {
                                            // Text block started, initial text may be present
                                            let initial = parsed.content_block.text.unwrap_or_default();
                                            if !initial.is_empty() {
                                                yield LLMEvent::TextDelta(initial.clone());
                                            }
                                            text_blocks.insert(parsed.index, initial);
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
                                            if let Some(text) = parsed.delta.text
                                                && !text.is_empty()
                                            {
                                                if let Some(acc) = text_blocks.get_mut(&parsed.index) {
                                                    acc.push_str(&text);
                                                } else {
                                                    tracing::warn!("text_delta for unknown block index {}", parsed.index);
                                                }
                                                yield LLMEvent::TextDelta(text);
                                            }
                                        }
                                        "input_json_delta" => {
                                            // Accumulate partial JSON for tool_use input
                                            if let Some(partial) = parsed.delta.partial_json
                                                && let Some(acc) = tool_blocks.get_mut(&parsed.index)
                                            {
                                                acc.input_json.push_str(&partial);
                                                yield LLMEvent::ToolCallDelta {
                                                    index: parsed.index,
                                                    partial_json: partial,
                                                };
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
                                            if let Some(sig) = parsed.delta.signature.as_ref()
                                                && let Some(acc) = thinking_blocks.get_mut(&parsed.index)
                                            {
                                                acc.signature.push_str(sig);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            "content_block_stop" => {
                                // When a content block stops, finalize it
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data)
                                    && let Some(index) = parsed.get("index").and_then(|v| v.as_u64())
                                {
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
                                    } else if let Some(acc) = thinking_blocks.remove(&index) {
                                        // Check if this was a thinking block
                                        // Store for raw round-tripping (with signature for verification)
                                        accumulated_content.push(ContentBlock::Thinking {
                                            thinking: acc.thinking_text,
                                            signature: acc.signature,
                                        });
                                    } else if let Some(text) = text_blocks.remove(&index) {
                                        // Check if this was a text block
                                        if !text.is_empty() {
                                            accumulated_content.push(ContentBlock::Text {
                                                text,
                                            });
                                        }
                                    }
                                }
                            }
                            "message_delta" => {
                                if let Ok(parsed) = serde_json::from_str::<MessageDeltaData>(data) {
                                    if let Some(usage) = parsed.usage {
                                        output_tokens = usage.output_tokens;
                                        cache_creation_input_tokens += usage.cache_creation_input_tokens;
                                        cache_read_input_tokens += usage.cache_read_input_tokens;
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
                                // Safety fallback: drain any text blocks that never got content_block_stop
                                let mut remaining_text: Vec<_> = text_blocks.drain().collect();
                                remaining_text.sort_by_key(|(idx, _)| *idx);
                                for (_, text) in remaining_text {
                                    if !text.is_empty() {
                                        raw_content.push(ContentBlock::Text { text });
                                    }
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
                                    cache_creation_input_tokens,
                                    cache_read_input_tokens,
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

            // Stream ended without message_stop (shouldn't happen normally)
            let mut raw_content = accumulated_content;
            let mut remaining_text: Vec<_> = text_blocks.drain().collect();
            remaining_text.sort_by_key(|(idx, _)| *idx);
            for (_, text) in remaining_text {
                if !text.is_empty() {
                    raw_content.push(ContentBlock::Text { text });
                }
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
                cache_creation_input_tokens,
                cache_read_input_tokens,
                raw: Some(raw),
            };
        })
    }
}
