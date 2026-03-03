use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::{SinkExt, Stream, StreamExt};
use llm_rs::conversation::{ConversationManager, Message, create_subagent_tool, create_get_subagent_logs_tool};
use llm_rs::llm::{ChatOptions, LLM};
use llm_rs::tool::Tool;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::{ClientMessage, ServerMessage};

/// Per-tool-call tracking state used by the event writer.
struct ToolCallState {
    file_path: PathBuf,
    status_file_path: PathBuf,
    tool_name: String,
    accumulated_preview: String,
}

/// Per-subagent tracking state used by the event writer.
struct SubAgentState {
    status_file_path: PathBuf,
    task_handle: JoinHandle<()>,
}

const PREVIEW_MAX_CHARS: usize = 200;

pub struct Server {
    socket_path: PathBuf,
    display_file: PathBuf,
    status_file: PathBuf,
    session_dir: PathBuf,
    llm: Box<dyn LLM>,
    model: String,
    chat_options: ChatOptions,
}

impl Server {
    pub fn new(
        socket_path: PathBuf,
        display_file: PathBuf,
        status_file: PathBuf,
        session_dir: PathBuf,
        llm: Box<dyn LLM>,
        model: String,
        chat_options: ChatOptions,
    ) -> Self {
        Self {
            socket_path,
            display_file,
            status_file,
            session_dir,
            llm,
            model,
            chat_options,
        }
    }

    pub async fn run(self) -> Result<()> {
        // Clean up existing socket file
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)
                .with_context(|| format!("Failed to remove existing socket {:?}", self.socket_path))?;
        }

        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("Failed to bind Unix socket at {:?}", self.socket_path))?;

        // Initialize display files
        tokio::fs::write(&self.display_file, "").await
            .with_context(|| format!("Failed to initialize display file {:?}", self.display_file))?;
        tokio::fs::write(&self.status_file, "Ready").await
            .with_context(|| format!("Failed to initialize status file {:?}", self.status_file))?;

        // Create conversation manager
        let manager = ConversationManager::new();

        // Build tools list including subagent tools
        let model_infos = self.llm.available_models();
        let mut tools_list: Vec<Arc<Tool>> = vec![
            Arc::new(tools::web_fetch_tool()),
            Arc::new(tools::web_search_tool()),
        ];
        tools_list.push(Arc::new(create_subagent_tool(&model_infos)));
        tools_list.push(Arc::new(create_get_subagent_logs_tool()));

        let (_, conversation_client) = manager.new_conversation(
            self.llm,
            "You are a helpful assistant.",
            &self.model,
            tools_list,
            self.chat_options.clone(),
            false,
        )?;

        // Spawn background task to write conversation events to display files
        let manager_clone = Arc::clone(&manager);
        let events = Box::pin(conversation_client.subscribe());
        let display_file = self.display_file.clone();
        let status_file = self.status_file.clone();
        let session_dir = self.session_dir.clone();
        let mut event_writer = tokio::spawn(run_event_writer(
            events,
            display_file,
            status_file,
            session_dir,
            Some(manager_clone),
        ));

        // Shutdown signal
        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        let shutdown_tx = Arc::new(shutdown_tx);
        let socket_path = self.socket_path.clone();

        // Accept loop — also monitors event writer task
        let mut shutdown_rx = shutdown_tx.subscribe();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => break,
                result = &mut event_writer => {
                    result.context("Event writer task panicked")?
                        .context("Event writer failed")?;
                    break;
                }
                result = listener.accept() => {
                    let (stream, _) = result?;
                    let conv_client = Arc::clone(&conversation_client);
                    let shutdown_tx = Arc::clone(&shutdown_tx);
                    tokio::spawn(handle_client(stream, conv_client, shutdown_tx));
                }
            }
        }

        // Signal display nvim to quit via status file
        tokio::fs::write(&self.status_file, "Shutdown").await
            .with_context(|| format!("Failed to write shutdown status to {:?}", self.status_file))?;
        std::fs::remove_file(&socket_path)
            .with_context(|| format!("Failed to remove socket {:?}", socket_path))?;
        Ok(())
    }
}

/// Reusable event writer that processes conversation events and writes them to display files.
/// Used for both the main conversation (manager = Some) and subagent conversations (manager = None).
/// When manager is Some, SubAgentStart events trigger creation of sub-session directories
/// and spawning of nested event writers for each subagent.
type EventStream = Pin<Box<dyn Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> + Send>>;

fn run_event_writer(
    mut events: EventStream,
    display_file: PathBuf,
    status_file: PathBuf,
    session_dir: PathBuf,
    manager: Option<Arc<ConversationManager>>,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(async move {
    let mut tool_calls: HashMap<String, ToolCallState> = HashMap::new();
    let mut subagents: HashMap<String, SubAgentState> = HashMap::new();
    let mut is_thinking = false;

    tracing::info!("event_writer started");

    while let Some(item) = events.next().await {
        let event = match item {
            Ok(event) => event,
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "broadcast lagged");
                continue;
            }
        };

        // Status file updates for assistant messages
        if matches!(&*event, Message::AssistantMessageStart { .. }) {
            is_thinking = false;
            tokio::fs::write(&status_file, "Streaming...").await
                .context("Failed to write status file")?;
        }
        if matches!(&*event, Message::AssistantThinkingChunk { .. }) && !is_thinking {
            is_thinking = true;
            tokio::fs::write(&status_file, "Thinking...").await
                .context("Failed to write status file")?;
        }
        if matches!(&*event, Message::AssistantMessageChunk { .. }) && is_thinking {
            is_thinking = false;
            tokio::fs::write(&status_file, "Streaming...").await
                .context("Failed to write status file")?;
        }
        if matches!(&*event, Message::AssistantMessageEnd { .. }) {
            is_thinking = false;
            tokio::fs::write(&status_file, "Ready").await
                .context("Failed to write status file")?;
        }

        match &*event {
            Message::ToolMessageStart { tool_call_id, tool_name, tool_args, .. } => {
                tracing::info!(
                    tool_call_id,
                    tool_name,
                    tool_args,
                    "ToolMessageStart received"
                );

                // Create per-tool-call files
                let tc_file = session_dir.join(format!("tool-call-{}.jsonl", tool_call_id));
                let tc_status = session_dir.join(format!("tool-call-{}-status.txt", tool_call_id));
                tokio::fs::write(&tc_file, "").await
                    .context("Failed to create tool call file")?;
                tokio::fs::write(&tc_status, "Running").await
                    .context("Failed to create tool call status file")?;

                // Write event to both main display and per-tool-call file
                append_event(&display_file, &event).await
                    .context("Failed to append display event")?;
                append_event(&tc_file, &event).await
                    .context("Failed to append tool call event")?;

                tool_calls.insert(tool_call_id.clone(), ToolCallState {
                    file_path: tc_file,
                    status_file_path: tc_status,
                    tool_name: tool_name.clone(),
                    accumulated_preview: String::new(),
                });
            }

            Message::ToolOutputChunk { tool_call_id, content, .. } => {
                let tracked = tool_calls.contains_key(tool_call_id.as_str());
                tracing::debug!(
                    tool_call_id,
                    tracked,
                    content_len = content.len(),
                    "ToolOutputChunk received"
                );
                if let Some(state) = tool_calls.get_mut(tool_call_id) {
                    // Write to per-tool-call file only (NOT main display)
                    append_event(&state.file_path, &event).await
                        .context("Failed to append tool call chunk")?;

                    // Accumulate first N chars for preview
                    if state.accumulated_preview.len() < PREVIEW_MAX_CHARS {
                        let remaining = PREVIEW_MAX_CHARS - state.accumulated_preview.len();
                        let chunk: String = content.chars().take(remaining).collect();
                        state.accumulated_preview.push_str(&chunk);
                    }
                } else {
                    tracing::warn!(
                        tool_call_id,
                        "ToolOutputChunk for untracked tool call — dropped"
                    );
                }
            }

            Message::ToolMessageEnd { tool_call_id, end_status, .. } => {
                tracing::info!(
                    tool_call_id,
                    ?end_status,
                    "ToolMessageEnd received"
                );
                if let Some(state) = tool_calls.remove(tool_call_id) {
                    // Write truncated preview to main display as a single ToolOutputChunk
                    if !state.accumulated_preview.is_empty() {
                        let mut preview = state.accumulated_preview;
                        if preview.len() >= PREVIEW_MAX_CHARS {
                            preview.push_str("...");
                        }
                        let preview_event = Message::ToolOutputChunk {
                            msg_id: 0,
                            tool_call_id: tool_call_id.clone(),
                            tool_name: state.tool_name,
                            content: Arc::new(preview),
                        };
                        append_event(&display_file, &preview_event).await
                            .context("Failed to append tool call preview")?;
                    }

                    // Write ToolMessageEnd to both files
                    append_event(&display_file, &event).await
                        .context("Failed to append display event")?;
                    append_event(&state.file_path, &event).await
                        .context("Failed to append tool call end")?;

                    // Mark the tool call as done
                    tokio::fs::write(&state.status_file_path, "Done").await
                        .context("Failed to write tool call status")?;
                    tracing::debug!(tool_call_id, "wrote Done to status file");
                } else {
                    tracing::warn!(
                        tool_call_id,
                        "ToolMessageEnd for untracked tool call — fallback to main display"
                    );
                    // Fallback: write to main display if we missed the start
                    append_event(&display_file, &event).await
                        .context("Failed to append display event")?;
                }
            }

            Message::SubAgentStart { conversation_id, description, .. } => {
                tracing::info!(
                    conversation_id,
                    description,
                    "SubAgentStart received"
                );
                append_event(&display_file, &event).await
                    .context("Failed to append subagent start to display")?;

                // When we have a manager, create a sub-session and spawn a nested event writer
                if let Some(ref mgr) = manager {
                    let sa_dir = session_dir.join(format!("subagent-{}", conversation_id));
                    tokio::fs::create_dir_all(&sa_dir).await
                        .context("Failed to create subagent directory")?;

                    let sa_display = sa_dir.join("display.jsonl");
                    let sa_status = sa_dir.join("status.txt");
                    tokio::fs::write(&sa_display, "").await
                        .context("Failed to initialize subagent display file")?;
                    tokio::fs::write(&sa_status, "Running").await
                        .context("Failed to initialize subagent status file")?;

                    match mgr.get_conversation(conversation_id) {
                        Ok(Some(sa_client)) => {
                            let sa_events = Box::pin(sa_client.subscribe());
                            let sa_status_clone = sa_status.clone();
                            let handle = tokio::spawn(async move {
                                if let Err(e) = run_event_writer(
                                    sa_events,
                                    sa_display,
                                    sa_status_clone,
                                    sa_dir,
                                    None,
                                ).await {
                                    tracing::error!(error = %e, "Subagent event writer failed");
                                }
                            });
                            subagents.insert(conversation_id.clone(), SubAgentState {
                                status_file_path: sa_status,
                                task_handle: handle,
                            });
                        }
                        Ok(None) => {
                            tracing::warn!(conversation_id, "Subagent conversation not found in manager");
                        }
                        Err(e) => {
                            tracing::error!(conversation_id, error = %e, "Failed to get subagent conversation");
                        }
                    }
                }
            }

            Message::SubAgentEnd { conversation_id, end_status, input_tokens, output_tokens, response, .. } => {
                tracing::info!(
                    conversation_id,
                    ?end_status,
                    response_len = response.len(),
                    input_tokens,
                    output_tokens,
                    "SubAgentEnd received"
                );

                // Clean up subagent event writer
                if let Some(state) = subagents.remove(conversation_id) {
                    tokio::fs::write(&state.status_file_path, "Done").await
                        .context("Failed to write subagent done status")?;
                    state.task_handle.abort();
                }

                append_event(&display_file, &event).await
                    .context("Failed to append subagent end to display")?;
            }

            _ => {
                // All other events: write to main display only
                append_event(&display_file, &event).await
                    .context("Failed to append display event")?;
            }
        }
    }
    tracing::info!("event_writer finished");
    Ok(())
    }) // Box::pin
}

async fn handle_client(
    stream: UnixStream,
    conv_client: Arc<llm_rs::conversation::ConversationClient>,
    shutdown_tx: Arc<broadcast::Sender<()>>,
) {
    let shutdown_rx = shutdown_tx.subscribe();
    if let Err(e) = handle_client_inner(stream, conv_client, shutdown_tx, shutdown_rx).await {
        eprintln!("[Server] Client handler error: {}", e);
    }
}

async fn handle_client_inner(
    stream: UnixStream,
    conv_client: Arc<llm_rs::conversation::ConversationClient>,
    shutdown_tx: Arc<broadcast::Sender<()>>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let framed = Framed::new(stream, LengthDelimitedCodec::new());
    let (mut sink, mut stream) = framed.split();

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.recv() => break,
            result = stream.next() => {
                let Some(Ok(bytes)) = result else { break };
                let Ok(msg) = serde_json::from_slice::<ClientMessage>(&bytes) else { continue };

                match msg {
                    ClientMessage::SendMessage { content } => {
                        if let Err(e) = conv_client.send_chat(&content).await {
                            send_msg(&mut sink, &ServerMessage::Error {
                                message: format!("Chat error: {}", e),
                            }).await?;
                        } else {
                            send_msg(&mut sink, &ServerMessage::Ack).await?;
                        }
                    }
                    ClientMessage::Shutdown => {
                        let _ = shutdown_tx.send(());
                        return Ok(());
                    }
                }
            }
        }
    }

    Ok(())
}

async fn send_msg<S>(sink: &mut S, msg: &ServerMessage) -> Result<()>
where
    S: futures::Sink<Bytes, Error = std::io::Error> + Unpin,
{
    let json = serde_json::to_vec(msg)?;
    sink.send(Bytes::from(json)).await?;
    Ok(())
}

async fn append_event(file: &PathBuf, event: &Message) -> Result<()> {
    let mut line = serde_json::to_string(event)?;
    line.push('\n');

    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .await?;
    f.write_all(line.as_bytes()).await?;
    f.flush().await?;
    Ok(())
}
