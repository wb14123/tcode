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
    bootstrap::send_socket_message,
    protocol::{ClientMessage, ServerMessage},
    session::{Session, base_path, generate_session_id, list_sessions},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_stream::{Stream, wrappers::ReceiverStream};

use crate::state::AppState;

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
        let session_dir =
            session_dir_for(&session_id).map_err(|e| ApiError::internal(e.to_string()))?;
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
    sse_jsonl(path)
}

pub(crate) async fn stream_session_tool_call(
    AxumPath((session_id, tool_call_file)): AxumPath<(String, String)>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let session_dir = existing_session_dir(&session_id)?;
    let tool_call_id = decode_jsonl_file_id(&tool_call_file)?;
    let path = session_dir.join(format!("tool-call-{tool_call_id}.jsonl"));
    sse_jsonl(path)
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
    sse_jsonl(path)
}

pub(crate) async fn stream_subagent_tool_call(
    AxumPath((session_id, subagent_id, tool_call_file)): AxumPath<(String, String, String)>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let session_dir = existing_session_dir(&session_id)?;
    let subagent_dir = find_subagent_dir(&session_dir, &subagent_id)?;
    let tool_call_id = decode_jsonl_file_id(&tool_call_file)?;
    let path = subagent_dir.join(format!("tool-call-{tool_call_id}.jsonl"));
    sse_jsonl(path)
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
    state
        .ensure_runtime(&session_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
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
    state
        .ensure_runtime(&session_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
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
        send_runtime_message(&state, &session_id, ClientMessage::GetPermissionState).await?;
    match response {
        ServerMessage::PermissionState(state) => Ok(Json(state)),
        ServerMessage::Ack => Err(ApiError::internal("unexpected ack from permission query")),
        ServerMessage::Error { message } => Err(map_runtime_error(message)),
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
    state
        .ensure_runtime(session_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let socket_path = session_dir_for(session_id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .join("server.sock");
    let response = send_socket_message(socket_path, &message)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let response =
        response.ok_or_else(|| ApiError::internal("runtime closed socket without responding"))?;
    if let ServerMessage::Error { message } = &response {
        return Err(map_runtime_error(message.clone()));
    }
    Ok(response)
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

fn session_dir_for(session_id: &str) -> std::io::Result<PathBuf> {
    Ok(base_path().map_err(std::io::Error::other)?.join(session_id))
}

fn existing_session_dir(session_id: &str) -> ApiResult<PathBuf> {
    let session_dir = session_dir_for(session_id).map_err(|e| ApiError::internal(e.to_string()))?;
    if !session_dir.is_dir() {
        return Err(ApiError::not_found("session not found"));
    }
    Ok(session_dir)
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

fn sse_jsonl(path: PathBuf) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    if !path.is_file() {
        return Err(ApiError::not_found("event stream not found"));
    }

    let (tx, rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(async move {
        let mut offset = 0u64;
        let mut pending = String::new();
        loop {
            if tx.is_closed() {
                return;
            }
            match read_appended_utf8(&path, offset).await {
                Ok(Some((new_offset, chunk))) => {
                    offset = new_offset;
                    pending.push_str(&chunk);
                    while let Some(newline_idx) = pending.find('\n') {
                        let line = pending[..newline_idx].trim_end_matches('\r').to_string();
                        pending.drain(..=newline_idx);
                        if tx.send(Ok(Event::default().data(line))).await.is_err() {
                            return;
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "jsonl stream reader stopped");
                    return;
                }
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

async fn read_appended_utf8(
    path: &Path,
    offset: u64,
) -> Result<Option<(u64, String)>, std::io::Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let metadata = file.metadata().await?;
    if metadata.len() <= offset {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset)).await?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await?;
    let text = String::from_utf8(buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Some((metadata.len(), text)))
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
