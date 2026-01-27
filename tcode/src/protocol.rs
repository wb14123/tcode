use llm_rs::conversation::Message;
use serde::{Deserialize, Serialize};

/// Client -> Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Send a user message to the conversation
    SendMessage { content: String },
    /// Subscribe to conversation events (display client sends this)
    Subscribe,
}

/// Server -> Client messages
/// Reuses llm_rs::Message directly - no conversion needed!
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Acknowledgment
    Ack,
    /// Conversation event - directly uses llm_rs::Message
    Event(Message),
    /// Error
    Error { message: String },
}
