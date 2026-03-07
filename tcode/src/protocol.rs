use serde::{Deserialize, Serialize};

/// Client -> Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Send a user message to the conversation
    SendMessage { content: String },
    /// Cancel a specific tool call by its ID
    CancelTool { tool_call_id: String },
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
