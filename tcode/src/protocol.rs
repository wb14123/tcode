use serde::{Deserialize, Serialize};

/// Client -> Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Send a user message to a conversation (main if conversation_id is None)
    SendMessage { conversation_id: Option<String>, content: String },
    /// Notify that the user finished interacting with a subagent (/done command)
    UserRequestEnd { conversation_id: String },
    /// Cancel a specific tool call by its ID
    CancelTool { tool_call_id: String },
    /// Cancel an entire conversation (cascades to all tools and child subagents)
    CancelConversation { conversation_id: String },
    /// Resolve a pending permission request
    ResolvePermission {
        key: llm_rs::permission::PermissionKey,
        decision: llm_rs::permission::PermissionDecision,
    },
    /// Revoke a saved permission
    RevokePermission {
        key: llm_rs::permission::PermissionKey,
    },
    /// Query the current permission state (pending, session, project)
    GetPermissionState,
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
    /// Full permission state snapshot
    PermissionState(llm_rs::permission::PermissionState),
}
