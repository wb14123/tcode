//! LLM trait and provider implementations.
//!
//! This module contains the core LLM trait definition and implementations
//! for various providers (OpenAI, Claude, Gemini, etc.).

mod openai;

pub use openai::OpenAI;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;

use crate::tool::Tool;

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
#[derive(Clone, Debug)]
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

    /// Model requests a tool call.
    /// Maps to Claude's tool_use content block or OpenAI's delta.tool_calls.
    ToolCall(ToolCall),

    /// Response completed successfully.
    /// Maps to Claude's message_delta or OpenAI's final chunk with finish_reason.
    MessageEnd {
        stop_reason: StopReason,
        input_tokens: i32,
        output_tokens: i32,
    },

    /// Error occurred during generation.
    /// Maps to Claude's error event or OpenAI error responses.
    Error(String),
}

pub trait LLM: Send + Sync {
    /// Send a chat request to the LLM.
    ///
    /// # Arguments
    /// - `model`: Model identifier (e.g., "claude-3-opus", "gpt-4")
    /// - `tools`: Tools available for the model to call, keyed by tool name
    /// - `msgs`: Conversation history
    ///
    /// # Returns
    /// A stream of [`LLMEvent`]s representing the model's response.
    fn chat(
        &self,
        model: &str,
        tools: &HashMap<String, Arc<Tool>>,
        msgs: &[(LLMRole, String)],
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>>;
}
