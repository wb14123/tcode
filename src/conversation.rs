use std::fmt::Display;
use std::sync::Arc;
use std::time::Instant;

use crate::llm::{Tool, LLM};
use anyhow::Result;
use tokio_stream::Stream;

type MessageID = i32;

pub enum MessageEndStatus {
    SUCCEEDED,
    FAILED,
    CANCELLED,
    TIMEOUT,
}

pub enum Message {
    UserMessage {
        msg_id: MessageID,
        created_at: Instant,
        content: Arc<String>,
    },

    AssistantMessageStart {
        msg_id: MessageID,
        created_at: Instant,
    },

    MessageChunk {
        msg_id: MessageID,
        content: Arc<String>,
    },

    AssistantMessageEnd {
        msg_id: MessageID,
        end_status: MessageEndStatus,
        error: Option<String>,
        input_tokens: i32,
        output_tokens: i32,
    },

    ToolMessageStart {
        msg_id: MessageID,
        created_at: Instant,
        tool_name: String,
        tool_args: Box<dyn Display>,
    },

    ToolMessageEnd {
        msg_id: MessageID,
        end_status: MessageEndStatus,
        input_tokens: i32,
        output_tokens: i32,
    },

    SubAgentStart {
        msg_id: MessageID,
        conversation_id: String,
        description: String,
        conversation: Arc<Conversation>,
    },

    SubAgentEnd {
        msg_id: MessageID,
        conversation_id: String,
        end_status: MessageEndStatus,
        input_tokens: i32,
        output_tokens: i32,
    },

    // assistant request to end the conversation, useful for sub agents
    AssistantRequestEnd {
        total_input_tokens: i32,
        total_output_tokens: i32,
    }
}

pub struct ConversationManager {
}

/// Manages conversations so that any new client can attach to an existing conversation.
impl ConversationManager {
    /// Create a new conversation. The new conversation will be kept in the manager's
    /// memory until it ends.
    pub fn new_conversation(&self, system_prompt: &str, tools: Vec<Arc<Tool<>>>, llm: Box<dyn LLM>) -> Result<Arc<Conversation>> {
        Ok(Arc::new(Conversation {llm})) // placeholder
    }

    /// Get a conversation by its id. It will try to load it from the manager's memory.
    /// If not found, load it from storage and put into the manager's memory.
    pub fn get_conversation(&self, conversation_id: &str) -> Option<Arc<Conversation>> {
        None // placeholder
    }

    /// Remove the conversation from the manager's memory. The conversation should be
    /// cleared if there is no reference to the Arc anymore.
    pub fn end_conversation(&self, conversation_id: &str) -> Result<()> {
        Ok(()) // placeholder
    }
}

pub struct Conversation {
    llm: Box<dyn LLM>,
}

/// Multi round LLM conversation. Thread and async safe.
impl Conversation {

    /// Send a chat to the conversation. Returns after the message is queued. The message
    /// will be sent to the LLM in the background when the current LLM response finished.
    pub async fn send_chat(&self, content: &str) -> Result<()> {
        Ok(()) // placeholder
    }

    /// Stop the current LLM response and all the messages in the queue.
    pub async fn break_conversation(&self) -> Result<()> {
        Ok(()) // placeholder
    }

    /// Subscribe to the conversation's messages.
    pub fn subscribe(&self) -> impl Stream<Item = Message> {
        tokio_stream::empty() // placeholder
    }
}