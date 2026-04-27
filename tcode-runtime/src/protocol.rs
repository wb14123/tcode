use crate::session::SessionMode;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_LEASE_TIMEOUT_SECONDS: u64 = 60;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuntimeOwnerKind {
    Cli,
    Web,
    Serve,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClientKind {
    Cli,
    Web,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientLeaseInfo {
    pub client_id: String,
    pub lease_timeout_seconds: u64,
    pub runtime_info: SessionRuntimeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRuntimeInfo {
    pub active: bool,
    pub owner_kind: RuntimeOwnerKind,
    #[serde(default)]
    pub session_mode: SessionMode,
    pub active_lease_count: usize,
    pub lease_timeout_seconds: u64,
    pub runtime_id: String,
}

impl SessionRuntimeInfo {
    pub fn inactive() -> Self {
        Self {
            active: false,
            owner_kind: RuntimeOwnerKind::Cli,
            session_mode: SessionMode::Normal,
            active_lease_count: 0,
            lease_timeout_seconds: DEFAULT_LEASE_TIMEOUT_SECONDS,
            runtime_id: String::new(),
        }
    }
}

pub fn lease_timeout_duration() -> Duration {
    Duration::from_secs(DEFAULT_LEASE_TIMEOUT_SECONDS)
}

/// Client -> Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Send a user message to a conversation (main if conversation_id is None)
    SendMessage {
        conversation_id: Option<String>,
        content: String,
    },
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
        request_id: Option<String>,
    },
    /// Revoke a saved permission
    RevokePermission {
        key: llm_rs::permission::PermissionKey,
    },
    /// Add a permission directly (user-initiated, bypasses pending request flow)
    AddPermission {
        key: llm_rs::permission::PermissionKey,
        scope: llm_rs::permission::PermissionScope,
    },
    /// Query the current permission state (pending, session, project)
    GetPermissionState,
    /// Register an active UI/client lease with the runtime.
    RegisterClientLease {
        client_kind: ClientKind,
        client_label: Option<String>,
    },
    /// Renew a previously registered client lease.
    HeartbeatClientLease { client_id: String },
    /// Drop a previously registered client lease.
    DetachClientLease { client_id: String },
    /// Query active runtime metadata.
    GetSessionRuntimeInfo,
    /// Request server shutdown from the runtime owner/supervisor.
    AuthorizedShutdown { owner_token: String },
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
    /// Client lease registration response.
    ClientLeaseRegistered(ClientLeaseInfo),
    /// Runtime metadata snapshot.
    SessionRuntimeInfo(SessionRuntimeInfo),
}
