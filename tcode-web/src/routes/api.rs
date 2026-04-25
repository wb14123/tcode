use std::convert::Infallible;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::{StatusCode, header},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use base64::Engine;
use llm_rs::{
    conversation::{ConversationState, Message, SessionMeta},
    permission::{PermissionDecision, PermissionKey, PermissionScope, PermissionState},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tcode_runtime::{
    protocol::{ClientMessage, DEFAULT_LEASE_TIMEOUT_SECONDS, ServerMessage, SessionRuntimeInfo},
    session::{Session, base_path, generate_session_id, list_sessions, validate_session_id},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_stream::{Stream, wrappers::ReceiverStream};

use crate::state::{AppState, SessionRuntimeStatus};

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Serialize)]
pub(crate) struct SessionsResponse {
    sessions: Vec<SessionSummary>,
}

#[derive(Serialize)]
struct SessionSummary {
    id: String,
    description: Option<String>,
    created_at: Option<u64>,
    last_active_at: Option<u64>,
    status: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateSessionRequest {
    initial_prompt: String,
}

#[derive(Serialize)]
pub(crate) struct CreateSessionResponse {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageRequest {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvePermissionRequest {
    key: PermissionKey,
    decision: PermissionDecision,
    request_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddPermissionRequest {
    key: PermissionKey,
    scope: PermissionScope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterLeaseRequest {
    #[serde(default)]
    client_label: Option<String>,
    #[serde(default)]
    resume: bool,
}

#[derive(Serialize)]
pub(crate) struct LeaseResponse {
    active: bool,
    client_id: Option<String>,
    lease_timeout_seconds: u64,
    heartbeat_interval_seconds: u64,
    runtime_info: SessionRuntimeInfo,
}

#[derive(Serialize)]
pub(crate) struct RuntimeInfoResponse {
    runtime_info: SessionRuntimeInfo,
}

#[derive(Serialize)]
pub(crate) struct SubagentMetaResponse {
    meta: Value,
    parent: ParentContext,
}

#[derive(Serialize)]
struct ParentContext {
    kind: String,
    conversation_id: String,
    tool_call_id: Option<String>,
}

pub(crate) async fn get_sessions() -> ApiResult<Json<SessionsResponse>> {
    let mut sessions = Vec::new();
    for session_id in list_sessions().map_err(|e| ApiError::internal(e.to_string()))? {
        let session_dir = session_dir_for(&session_id)?;
        let meta =
            read_json_optional::<SessionMeta>(&session_dir.join("session-meta.json")).await?;
        let status = read_optional_text_file(&session_dir.join("status.txt"))
            .await?
            .unwrap_or_default();
        sessions.push(SessionSummary {
            id: session_id,
            description: meta.as_ref().and_then(|m| m.description.clone()),
            created_at: meta.as_ref().and_then(|m| m.created_at),
            last_active_at: meta.as_ref().and_then(|m| m.last_active_at),
            status,
        });
    }
    sessions.sort_by(|a, b| {
        b.last_active_at
            .unwrap_or(0)
            .cmp(&a.last_active_at.unwrap_or(0))
    });
    Ok(Json(SessionsResponse { sessions }))
}

pub(crate) async fn post_sessions(
    State(state): State<std::sync::Arc<AppState>>,
    Json(body): Json<CreateSessionRequest>,
) -> ApiResult<Json<CreateSessionResponse>> {
    let session_id = create_unique_session_id().map_err(|e| ApiError::internal(e.to_string()))?;
    Session::new(session_id.clone()).map_err(|e| ApiError::internal(e.to_string()))?;
    state
        .ensure_runtime(&session_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::SendMessage {
            conversation_id: None,
            content: body.initial_prompt,
        },
    )
    .await?;

    Ok(Json(CreateSessionResponse { id: session_id }))
}

pub(crate) async fn get_session_meta(
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let session_dir = existing_session_dir(&session_id)?;
    Ok(Json(
        read_json_value(&session_dir.join("session-meta.json")).await?,
    ))
}

pub(crate) async fn get_conversation_state(
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let session_dir = existing_session_dir(&session_id)?;
    Ok(Json(
        read_json_value(&session_dir.join("conversation-state.json")).await?,
    ))
}

pub(crate) async fn get_session_status(
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<impl IntoResponse> {
    let session_dir = existing_session_dir(&session_id)?;
    Ok(text_response(
        read_optional_text_file(&session_dir.join("status.txt")).await?,
    ))
}

pub(crate) async fn post_session_lease(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<RegisterLeaseRequest>,
) -> ApiResult<Json<LeaseResponse>> {
    let session_dir = existing_session_dir(&session_id)?;
    if body.resume {
        ensure_session_resumable(&session_dir).await?;
    }
    let lease = state
        .register_web_client_lease(&session_id, body.client_label, body.resume)
        .await
        .map_err(|e| ApiError::conflict(e.to_string()))?;

    if let Some(lease) = lease {
        let runtime_info = lease.runtime_info.clone();
        return Ok(Json(LeaseResponse {
            active: runtime_info.active,
            client_id: Some(lease.client_id),
            lease_timeout_seconds: lease.lease_timeout_seconds,
            heartbeat_interval_seconds: heartbeat_interval_seconds(lease.lease_timeout_seconds),
            runtime_info,
        }));
    }

    Ok(Json(LeaseResponse {
        active: false,
        client_id: None,
        lease_timeout_seconds: DEFAULT_LEASE_TIMEOUT_SECONDS,
        heartbeat_interval_seconds: heartbeat_interval_seconds(DEFAULT_LEASE_TIMEOUT_SECONDS),
        runtime_info: SessionRuntimeInfo::inactive(),
    }))
}

pub(crate) async fn post_session_lease_heartbeat(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, client_id)): AxumPath<(String, String)>,
) -> ApiResult<Json<RuntimeInfoResponse>> {
    existing_session_dir(&session_id)?;
    let runtime_info = state
        .heartbeat_client_lease(&session_id, client_id)
        .await
        .map_err(|e| ApiError::conflict(e.to_string()))?;
    Ok(Json(RuntimeInfoResponse { runtime_info }))
}

pub(crate) async fn delete_session_lease(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, client_id)): AxumPath<(String, String)>,
) -> ApiResult<StatusCode> {
    existing_session_dir(&session_id)?;
    state.detach_client_lease(&session_id, client_id).await;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_session_usage(
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<impl IntoResponse> {
    let session_dir = existing_session_dir(&session_id)?;
    Ok(text_response(
        read_optional_text_file(&session_dir.join("usage.txt")).await?,
    ))
}

pub(crate) async fn get_session_token_usage(
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<impl IntoResponse> {
    let session_dir = existing_session_dir(&session_id)?;
    Ok(text_response(
        read_optional_text_file(&session_dir.join("token_usage.txt")).await?,
    ))
}

pub(crate) async fn stream_session_display(
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let session_dir = existing_session_dir(&session_id)?;
    let path = session_dir.join("display.jsonl");
    sse_jsonl(path, true)
}

pub(crate) async fn stream_session_tool_call(
    AxumPath((session_id, tool_call_file)): AxumPath<(String, String)>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let session_dir = existing_session_dir(&session_id)?;
    let tool_call_id = decode_jsonl_file_id(&tool_call_file)?;
    let path = session_dir.join(format!("tool-call-{tool_call_id}.jsonl"));
    sse_jsonl(path, false)
}

pub(crate) async fn get_session_tool_call_status(
    AxumPath((session_id, tool_call_id)): AxumPath<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let session_dir = existing_session_dir(&session_id)?;
    Ok(text_response(Some(
        read_required_text_file(&session_dir.join(format!("tool-call-{tool_call_id}-status.txt")))
            .await?,
    )))
}

pub(crate) async fn get_subagent_meta(
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
) -> ApiResult<Json<SubagentMetaResponse>> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    let meta = read_json_value(&subagent_dir.join("session-meta.json")).await?;
    let parent = subagent_parent_context(&session_dir, &subagent_dir, &subagent_id).await?;
    Ok(Json(SubagentMetaResponse { meta, parent }))
}

pub(crate) async fn get_subagent_conversation_state(
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    Ok(Json(
        read_json_value(&subagent_dir.join("conversation-state.json")).await?,
    ))
}

pub(crate) async fn get_subagent_status(
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    Ok(text_response(
        read_optional_text_file(&subagent_dir.join("status.txt")).await?,
    ))
}

pub(crate) async fn get_subagent_token_usage(
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    Ok(text_response(
        read_optional_text_file(&subagent_dir.join("token_usage.txt")).await?,
    ))
}

pub(crate) async fn stream_subagent_display(
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    let path = subagent_dir.join("display.jsonl");
    sse_jsonl(path, true)
}

pub(crate) async fn stream_subagent_tool_call(
    AxumPath((session_id, subagent_id, tool_call_file)): AxumPath<(String, String, String)>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    let tool_call_id = decode_jsonl_file_id(&tool_call_file)?;
    let path = subagent_dir.join(format!("tool-call-{tool_call_id}.jsonl"));
    sse_jsonl(path, false)
}

pub(crate) async fn get_subagent_tool_call_status(
    AxumPath((session_id, subagent_id, tool_call_id)): AxumPath<(String, String, String)>,
) -> ApiResult<impl IntoResponse> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    Ok(text_response(Some(
        read_required_text_file(&subagent_dir.join(format!("tool-call-{tool_call_id}-status.txt")))
            .await?,
    )))
}

pub(crate) async fn post_session_message(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<MessageRequest>,
) -> ApiResult<StatusCode> {
    existing_session_dir(&session_id)?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::SendMessage {
            conversation_id: None,
            content: body.text,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_subagent_message(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
    Json(body): Json<MessageRequest>,
) -> ApiResult<StatusCode> {
    let session_dir = existing_session_dir(&session_id)?;
    find_subagent_dir(&session_dir, &subagent_id)?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::SendMessage {
            conversation_id: Some(subagent_id),
            content: body.text,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_session_finish(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<StatusCode> {
    let session_dir = existing_session_dir(&session_id)?;
    let root_id = root_conversation_id(&session_dir).await?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::UserRequestEnd {
            conversation_id: root_id,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_subagent_finish(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
) -> ApiResult<StatusCode> {
    let session_dir = existing_session_dir(&session_id)?;
    find_subagent_dir(&session_dir, &subagent_id)?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::UserRequestEnd {
            conversation_id: subagent_id,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_session_cancel(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<StatusCode> {
    let session_dir = existing_session_dir(&session_id)?;
    let root_id = root_conversation_id(&session_dir).await?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::CancelConversation {
            conversation_id: root_id,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_subagent_cancel(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
) -> ApiResult<StatusCode> {
    let session_dir = existing_session_dir(&session_id)?;
    find_subagent_dir(&session_dir, &subagent_id)?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::CancelConversation {
            conversation_id: subagent_id,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_session_tool_call_cancel(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, tool_call_id)): AxumPath<(String, String)>,
) -> ApiResult<StatusCode> {
    let session_dir = existing_session_dir(&session_id)?;
    let tool_path = session_dir.join(format!("tool-call-{tool_call_id}.jsonl"));
    if !tool_path.is_file() {
        return Err(ApiError::not_found("tool call not found"));
    }
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::CancelTool { tool_call_id },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_subagent_tool_call_cancel(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, subagent_id, tool_call_id)): AxumPath<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    let tool_path = subagent_dir.join(format!("tool-call-{tool_call_id}.jsonl"));
    if !tool_path.is_file() {
        return Err(ApiError::not_found("subagent tool call not found"));
    }
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::CancelTool { tool_call_id },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_permissions(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult<Json<PermissionState>> {
    existing_session_dir(&session_id)?;
    let response =
        send_runtime_message_if_active(&state, &session_id, ClientMessage::GetPermissionState)
            .await?;
    match response {
        Some(ServerMessage::PermissionState(state)) => Ok(Json(state)),
        Some(ServerMessage::Ack) => Err(ApiError::internal("unexpected ack from permission query")),
        Some(ServerMessage::Error { message }) => Err(map_runtime_error(message)),
        Some(ServerMessage::ClientLeaseRegistered(_))
        | Some(ServerMessage::SessionRuntimeInfo(_)) => Err(ApiError::internal(
            "unexpected runtime response from permission query",
        )),
        None => Ok(Json(empty_permission_state())),
    }
}

pub(crate) async fn post_permissions_resolve(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<ResolvePermissionRequest>,
) -> ApiResult<StatusCode> {
    existing_session_dir(&session_id)?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::ResolvePermission {
            key: body.key,
            decision: body.decision,
            request_id: body.request_id,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_permissions_add(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<AddPermissionRequest>,
) -> ApiResult<StatusCode> {
    existing_session_dir(&session_id)?;
    send_runtime_message(
        &state,
        &session_id,
        ClientMessage::AddPermission {
            key: body.key,
            scope: body.scope,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_permission(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((session_id, permission_id)): AxumPath<(String, String)>,
) -> ApiResult<StatusCode> {
    existing_session_dir(&session_id)?;
    let key = decode_permission_id(&permission_id)?;
    send_runtime_message(&state, &session_id, ClientMessage::RevokePermission { key }).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_runtime_message(
    state: &AppState,
    session_id: &str,
    message: ClientMessage,
) -> ApiResult<ServerMessage> {
    match state
        .runtime_status(session_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        SessionRuntimeStatus::Active => {}
        SessionRuntimeStatus::Inactive => {
            return Err(ApiError::conflict(
                "session runtime is inactive; reconnect/resume the session before sending commands",
            ));
        }
        SessionRuntimeStatus::Unresponsive => {
            return Err(ApiError::conflict(
                SessionRuntimeStatus::unavailable_message(),
            ));
        }
    }

    let response = state
        .send_socket_message(session_id, message)
        .await
        .map_err(|e| ApiError::conflict(format!("session runtime is unavailable: {e}")))?;
    if let ServerMessage::Error { message } = &response {
        return Err(map_runtime_error(message.clone()));
    }
    Ok(response)
}

async fn send_runtime_message_if_active(
    state: &AppState,
    session_id: &str,
    message: ClientMessage,
) -> ApiResult<Option<ServerMessage>> {
    let Some(response) = state
        .send_runtime_message_if_active(session_id, message)
        .await
        .map_err(|e| ApiError::conflict(e.to_string()))?
    else {
        return Ok(None);
    };

    if let ServerMessage::Error { message } = &response {
        return Err(map_runtime_error(message.clone()));
    }
    Ok(Some(response))
}

pub(crate) fn heartbeat_interval_seconds(lease_timeout_seconds: u64) -> u64 {
    (lease_timeout_seconds / 4).clamp(5, 15)
}

pub(crate) fn empty_permission_state() -> PermissionState {
    PermissionState {
        pending: Vec::new(),
        session: Vec::new(),
        project: Vec::new(),
    }
}

fn map_runtime_error(message: String) -> ApiError {
    if message.to_ascii_lowercase().contains("not found") {
        ApiError::not_found(message)
    } else {
        ApiError::conflict(message)
    }
}

fn create_unique_session_id() -> std::io::Result<String> {
    let base = base_path().map_err(std::io::Error::other)?;
    for _ in 0..64 {
        let session_id = generate_session_id();
        if !base.join(&session_id).exists() {
            return Ok(session_id);
        }
    }
    Err(std::io::Error::other(
        "failed to generate a unique session id",
    ))
}

pub(crate) fn session_dir_for(session_id: &str) -> ApiResult<PathBuf> {
    validate_session_id(session_id).map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(base_path()
        .map_err(|e| ApiError::internal(e.to_string()))?
        .join(session_id))
}

fn existing_session_dir(session_id: &str) -> ApiResult<PathBuf> {
    let session_dir = session_dir_for(session_id)?;
    if !session_dir.is_dir() {
        return Err(ApiError::not_found("session not found"));
    }
    Ok(session_dir)
}

pub(crate) async fn ensure_session_resumable(session_dir: &Path) -> ApiResult<()> {
    let path = session_dir.join("conversation-state.json");
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::conflict(
                "cannot resume historical session: conversation-state.json is missing",
            )
        } else {
            ApiError::conflict(format!(
                "cannot resume historical session: conversation-state.json is not readable: {e}"
            ))
        }
    })?;

    let state: ConversationState = serde_json::from_slice(&bytes).map_err(|e| {
        ApiError::conflict(format!(
            "cannot resume historical session: conversation-state.json is invalid: {e}"
        ))
    })?;
    if state.id.trim().is_empty() {
        return Err(ApiError::conflict(
            "cannot resume historical session: conversation-state.json has no conversation id",
        ));
    }
    Ok(())
}

async fn read_json_value(path: &Path) -> ApiResult<Value> {
    let bytes = tokio::fs::read(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(format!("resource {:?} not found", path.file_name()))
        } else {
            ApiError::internal(e.to_string())
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|e| ApiError::internal(e.to_string()))
}

async fn read_json_optional<T: serde::de::DeserializeOwned>(path: &Path) -> ApiResult<Option<T>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| ApiError::internal(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::internal(e.to_string())),
    }
}

async fn read_required_text_file(path: &Path) -> ApiResult<String> {
    tokio::fs::read_to_string(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(format!("resource {:?} not found", path.file_name()))
        } else {
            ApiError::internal(e.to_string())
        }
    })
}

async fn read_optional_text_file(path: &Path) -> ApiResult<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::internal(e.to_string())),
    }
}

fn text_response(text: Option<String>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        text.unwrap_or_default(),
    )
        .into_response()
}

const JSONL_READ_CHUNK_BYTES: usize = 16 * 1024;

fn sse_jsonl(
    path: PathBuf,
    wait_for_file: bool,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    if !wait_for_file && !path.is_file() {
        return Err(ApiError::not_found("event stream not found"));
    }

    let (tx, rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(async move {
        let mut offset = 0u64;
        let mut partial_line = Vec::new();
        loop {
            if tx.is_closed() {
                return;
            }
            if let Err(e) =
                send_appended_jsonl_events(&path, &mut offset, &mut partial_line, &tx).await
            {
                tracing::warn!(path = %path.display(), error = %e, "jsonl stream reader error; retrying");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(
        KeepAlive::default()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

pub(crate) async fn send_appended_jsonl_events(
    path: &Path,
    offset: &mut u64,
    partial_line: &mut Vec<u8>,
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), std::io::Error> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata().await?;
    let snapshot_len = metadata.len();
    if snapshot_len < *offset {
        tracing::debug!(
            path = %path.display(),
            previous_offset = *offset,
            snapshot_len,
            "jsonl stream source shrank; restarting from beginning"
        );
        *offset = 0;
        partial_line.clear();
    }
    if snapshot_len == *offset {
        return Ok(());
    }

    file.seek(SeekFrom::Start(*offset)).await?;
    let mut remaining = snapshot_len.saturating_sub(*offset);
    let mut read_buf = vec![0u8; JSONL_READ_CHUNK_BYTES];
    while remaining > 0 {
        if tx.is_closed() {
            return Ok(());
        }
        let bytes_to_read = read_buf.len().min(remaining as usize);
        let bytes_read = file.read(&mut read_buf[..bytes_to_read]).await?;
        if bytes_read == 0 {
            return Ok(());
        }
        remaining = remaining.saturating_sub(bytes_read as u64);

        if !send_jsonl_events_from_chunk(&read_buf[..bytes_read], offset, partial_line, tx).await? {
            return Ok(());
        }
    }
    Ok(())
}

async fn send_jsonl_events_from_chunk(
    chunk: &[u8],
    offset: &mut u64,
    partial_line: &mut Vec<u8>,
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
) -> Result<bool, std::io::Error> {
    let mut start = 0;
    let mut consumed = 0;
    while let Some(relative_newline_idx) = chunk[start..].iter().position(|byte| *byte == b'\n') {
        let newline_idx = start + relative_newline_idx;
        let line_bytes = &chunk[start..newline_idx];
        let sent = if partial_line.is_empty() {
            send_jsonl_event(line_bytes, tx).await?
        } else {
            let mut completed_line = Vec::with_capacity(partial_line.len() + line_bytes.len());
            completed_line.extend_from_slice(partial_line);
            completed_line.extend_from_slice(line_bytes);
            send_jsonl_event(&completed_line, tx).await?
        };
        if !sent {
            return Ok(false);
        }

        partial_line.clear();
        let consumed_through_line = newline_idx + 1;
        *offset += (consumed_through_line - consumed) as u64;
        consumed = consumed_through_line;
        start = consumed_through_line;
    }

    if start < chunk.len() {
        partial_line.extend_from_slice(&chunk[start..]);
        *offset += (chunk.len() - consumed) as u64;
    }
    Ok(true)
}

async fn send_jsonl_event(
    line_bytes: &[u8],
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
) -> Result<bool, std::io::Error> {
    let line = jsonl_line_from_bytes(line_bytes)?;
    Ok(tx.send(Ok(Event::default().data(line))).await.is_ok())
}

pub(crate) fn jsonl_line_from_bytes(line_bytes: &[u8]) -> Result<String, std::io::Error> {
    let line_bytes = trim_trailing_cr(line_bytes);
    std::str::from_utf8(line_bytes)
        .map(str::to_string)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

fn trim_trailing_cr(mut bytes: &[u8]) -> &[u8] {
    while bytes.last() == Some(&b'\r') {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn find_subagent_dir(session_dir: &Path, subagent_id: &str) -> ApiResult<PathBuf> {
    find_subagent_dir_inner(session_dir, subagent_id)
        .ok_or_else(|| ApiError::not_found("subagent not found"))
}

fn find_subagent_dir_inner(dir: &Path, subagent_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == format!("subagent-{subagent_id}") {
            return Some(path);
        }
        if name.starts_with("subagent-")
            && let Some(found) = find_subagent_dir_inner(&path, subagent_id)
        {
            return Some(found);
        }
    }
    None
}

async fn subagent_parent_context(
    session_dir: &Path,
    subagent_dir: &Path,
    subagent_id: &str,
) -> ApiResult<ParentContext> {
    let parent_dir = subagent_dir
        .parent()
        .ok_or_else(|| ApiError::internal("subagent has no parent directory"))?;
    let tool_call_id =
        find_subagent_tool_call_id(&parent_dir.join("display.jsonl"), subagent_id).await?;
    if parent_dir == session_dir {
        return Ok(ParentContext {
            kind: "session".to_string(),
            conversation_id: root_conversation_id(session_dir).await?,
            tool_call_id,
        });
    }

    let name = parent_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ApiError::internal("invalid parent directory name"))?;
    let parent_subagent_id = name
        .strip_prefix("subagent-")
        .ok_or_else(|| ApiError::internal("unexpected parent directory layout"))?;
    Ok(ParentContext {
        kind: "subagent".to_string(),
        conversation_id: parent_subagent_id.to_string(),
        tool_call_id,
    })
}

async fn root_conversation_id(session_dir: &Path) -> ApiResult<String> {
    let state: ConversationState = read_json_optional(&session_dir.join("conversation-state.json"))
        .await?
        .ok_or_else(|| ApiError::conflict("conversation state is not available yet"))?;
    Ok(state.id)
}

async fn find_subagent_tool_call_id(
    display_path: &Path,
    subagent_id: &str,
) -> ApiResult<Option<String>> {
    let text = match tokio::fs::read_to_string(display_path).await {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ApiError::internal(e.to_string())),
    };

    let mut last_match = None;
    for line in text.lines() {
        let event = match serde_json::from_str::<Message>(line) {
            Ok(event) => event,
            Err(_) => continue,
        };
        match event {
            Message::SubAgentStart {
                conversation_id,
                tool_call_id,
                ..
            }
            | Message::SubAgentContinue {
                conversation_id,
                tool_call_id,
                ..
            } if conversation_id == subagent_id => {
                last_match = Some(tool_call_id);
            }
            _ => {}
        }
    }
    Ok(last_match)
}

fn decode_jsonl_file_id(file_name: &str) -> ApiResult<&str> {
    file_name
        .strip_suffix(".jsonl")
        .ok_or_else(|| ApiError::not_found("event stream not found"))
}

fn decode_permission_id(permission_id: &str) -> ApiResult<PermissionKey> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(permission_id)
        .map_err(|e| ApiError::bad_request(format!("invalid permission id: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::bad_request(format!("invalid permission id payload: {e}")))
}
