//! LLM trait and provider implementations.
//!
//! This module contains the core LLM trait definition and implementations
//! for various providers (OpenAI, Claude, Gemini, etc.).

mod claude;
mod openai;
mod openai_common;
mod openrouter;
mod sse;

#[cfg(test)]
mod openai_tests;

pub use claude::Claude;
pub use openai::OpenAI;
pub use openrouter::OpenRouter;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;

use serde::{Deserialize, Serialize};

use crate::tool::Tool;

// ============================================================================
// Shared auth types (used by Claude, OpenAI, and auth crate)
// ============================================================================

/// Function type for getting an access token. Called before each API request.
/// For static tokens, returns the same token. For OAuth, may trigger refresh.
pub type GetTokenFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>;

/// Trait for types that can provide an access token (e.g. OAuth token managers).
/// Implement this to use [`Claude::with_token_provider`] or [`OpenAI::with_token_provider`].
pub trait TokenProvider: Send + Sync {
    fn get_access_token(&self) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
}

// ============================================================================
// Reasoning / Thinking types
// ============================================================================

/// Reasoning effort level for thinking models.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    XHigh,
    High,
    Medium,
    Low,
    Minimal,
}

/// Information about an available model.
#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub description: String,
}

/// Options for LLM chat requests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatOptions {
    /// Maximum tokens for the response output. If not set, provider defaults are used.
    pub max_tokens: Option<u32>,
    /// Reasoning effort level (mutually exclusive with `reasoning_budget`).
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Explicit reasoning token budget (mutually exclusive with `reasoning_effort`).
    pub reasoning_budget: Option<u32>,
    /// If true, model uses reasoning internally but doesn't return it in the response.
    pub exclude_reasoning: bool,
}

// ============================================================================
// Message types
// ============================================================================

/// Message content for LLM conversations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LLMMessage {
    System(String),
    User(String),
    /// Assistant message with optional tool calls.
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
        /// Raw provider response for round-tripping. If present, send this to LLM
        /// instead of reconstructing from other fields.
        raw: Option<serde_json::Value>,
    },
    /// Tool result message. Includes the tool_call_id from the original ToolCall.
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Clone, Debug)]
pub enum LLMRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Reason why the model stopped generating.
#[derive(Clone, Debug, PartialEq)]
pub enum StopReason {
    /// Normal completion (Claude: end_turn, OpenAI: stop)
    EndTurn,
    /// Model wants to call one or more tools (Claude: tool_use, OpenAI: tool_calls)
    ToolUse,
    /// Hit the token limit (Claude/OpenAI: max_tokens/length)
    MaxTokens,
}

/// A tool call requested by the model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this tool call (used to match tool results)
    pub id: String,
    /// Name of the tool to call
    pub name: String,
    /// JSON-encoded arguments for the tool
    pub arguments: String,
}

/// Events emitted by the LLM during streaming response.
#[derive(Clone, Debug)]
pub enum LLMEvent {
    /// Response started. Contains initial token count.
    /// Maps to Claude's message_start or OpenAI's first chunk.
    MessageStart { input_tokens: i32 },

    /// A chunk of text content.
    /// Maps to Claude's text_delta or OpenAI's delta.content.
    TextDelta(String),

    /// A chunk of reasoning/thinking text for streaming display.
    /// Maps to OpenRouter's delta.reasoning_details or delta.reasoning_content.
    ThinkingDelta(String),

    /// Model requests a tool call.
    /// Maps to Claude's tool_use content block or OpenAI's delta.tool_calls.
    ToolCall(ToolCall),

    /// Response completed successfully.
    /// Maps to Claude's message_delta or OpenAI's final chunk with finish_reason.
    MessageEnd {
        stop_reason: StopReason,
        input_tokens: i32,
        output_tokens: i32,
        /// Reasoning tokens used (subset of output_tokens). 0 if not reported.
        reasoning_tokens: i32,
        /// Tokens charged for writing new cache entries (Claude only).
        cache_creation_input_tokens: i32,
        /// Tokens served from cache at reduced rate (Claude only).
        cache_read_input_tokens: i32,
        /// Raw provider response for round-tripping.
        raw: Option<serde_json::Value>,
    },

    /// A tool call block has started streaming (name and id are known, args not yet complete).
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },

    /// A partial JSON fragment of a tool call's arguments.
    ToolCallDelta { index: usize, partial_json: String },

    /// Error occurred during generation.
    /// Maps to Claude's error event or OpenAI error responses.
    Error(String),
}

pub trait LLM: Send + Sync {
    /// Register tools available for the model to call.
    ///
    /// This method allows implementations to cache tool schemas instead of
    /// regenerating them on every chat call. The tools are stored internally
    /// and used in subsequent `chat` calls.
    ///
    /// # Arguments
    /// - `tools`: List of tools to register
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>);

    /// Send a chat request to the LLM.
    ///
    /// # Arguments
    /// - `model`: Model identifier (e.g., "claude-3-opus", "gpt-4")
    /// - `msgs`: Conversation history using LLMMessage enum
    /// - `options`: Chat options including reasoning configuration
    ///
    /// # Returns
    /// A stream of [`LLMEvent`]s representing the model's response.
    fn chat(
        &self,
        model: &str,
        msgs: &[LLMMessage],
        options: &ChatOptions,
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>>;

    /// Clone this LLM instance into a new boxed trait object.
    /// Used to create LLM instances for subagent conversations.
    fn clone_box(&self) -> Box<dyn LLM>;

    /// Return the list of models available from this provider.
    /// Used to generate the subagent tool description.
    fn available_models(&self) -> Vec<ModelInfo>;
}
