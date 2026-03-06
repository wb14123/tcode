use serde::{Deserialize, Serialize};

/// Client -> Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Send a user message to the conversation
    SendMessage { content: String },
    /// Cancel the currently running tool call / LLM response
    Cancel,
    /// Request server shutdown (broadcasts to all clients)
    Shutdown,
}

/// Server -> Client messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Acknowledgment
    Ack,
    /// Error
    Error { message: String },
}
