use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::{SinkExt, Stream, StreamExt};
use llm_rs::conversation::{
    ConversationManager, ConversationState, Message, MessageEndStatus,
    create_continue_subagent_tool, create_subagent_tool, format_subagent_result,
};
use llm_rs::llm::{ChatOptions, LLM};
use llm_rs::tool::Tool;
use sha2::Digest;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use llm_rs::conversation::ConversationClient;

use crate::protocol::{ClientMessage, ServerMessage};

/// Shared map from tool_call_id -> ConversationClient that owns the tool.
/// Populated by event writers on ToolMessageStart, cleaned up on ToolMessageEnd.
type ToolClientMap = Arc<parking_lot::Mutex<HashMap<String, Arc<ConversationClient>>>>;

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
    usage_file: PathBuf,
    session_dir: PathBuf,
    conversation_state_file: PathBuf,
    llm: Box<dyn LLM>,
    model: String,
    chat_options: ChatOptions,
    max_subagent_depth: usize,
    subagent_model_selection: bool,
    token_manager: Option<auth::TokenManager>,
}

impl Server {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        socket_path: PathBuf,
        display_file: PathBuf,
        status_file: PathBuf,
        usage_file: PathBuf,
        session_dir: PathBuf,
        conversation_state_file: PathBuf,
        llm: Box<dyn LLM>,
        model: String,
        chat_options: ChatOptions,
        max_subagent_depth: usize,
        subagent_model_selection: bool,
        token_manager: Option<auth::TokenManager>,
    ) -> Self {
        Self {
            socket_path,
            display_file,
            status_file,
            usage_file,
            session_dir,
            conversation_state_file,
            llm,
            model,
            chat_options,
            max_subagent_depth,
            subagent_model_selection,
            token_manager,
        }
    }

    pub async fn run(
        self,
        ready_tx: Option<tokio::sync::oneshot::Sender<anyhow::Result<()>>>,
    ) -> Result<()> {
        let bind_result: Result<UnixListener> = (|| {
            // Clean up existing socket file
            if self.socket_path.exists() {
                std::fs::remove_file(&self.socket_path).with_context(|| {
                    format!("Failed to remove existing socket {:?}", self.socket_path)
                })?;
            }

            UnixListener::bind(&self.socket_path)
                .with_context(|| format!("Failed to bind Unix socket at {:?}", self.socket_path))
        })();

        let listener = match bind_result {
            Ok(l) => {
                if let Some(tx) = ready_tx {
                    let _ = tx.send(Ok(())); // receiver dropped = main is gone, not actionable
                }
                l
            }
            Err(e) => {
                if let Some(tx) = ready_tx {
                    // Send actual error to main; recover error if receiver dropped
                    return match tx.send(Err(e)) {
                        Ok(()) => Ok(()),
                        Err(Err(e)) => Err(e),
                        Err(Ok(())) => unreachable!(),
                    };
                }
                return Err(e);
            }
        };

        let token_manager = self.token_manager.clone();
        let usage_file = self.usage_file.clone();

        // Create conversation manager (creates permission manager internally)
        // Store project-level permissions under ~/.tcode/projects/<sha256(cwd)>/
        // so they persist across sessions for the same working directory.
        let cwd = std::env::current_dir().context("Failed to get current working directory")?;
        let cwd_str = cwd.to_string_lossy();
        let mut hasher = sha2::Sha256::new();
        hasher.update(cwd_str.as_bytes());
        let digest = hasher.finalize();
        let mut hash = String::with_capacity(digest.len() * 2);
        use std::fmt::Write;
        for byte in digest.iter() {
            // write! to a String is infallible (fmt::Write for String never errors).
            write!(&mut hash, "{:02x}", byte).expect("writes to String are infallible");
        }
        let base =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
        let permissions_path = base
            .join(".tcode")
            .join("projects")
            .join(&hash)
            .join("permissions.json");
        let manager = ConversationManager::new(permissions_path);

        // Scan skills
        let (skills, skill_warnings) = llm_rs::skill::scan_skills();
        for warning in &skill_warnings {
            tracing::warn!("{}", warning);
        }
        let skills = std::sync::Arc::new(skills);

        // Start LSP config extraction in the background (runs headless nvim, can take seconds).
        // We await the result later, just before building the tools list, so it doesn't
        // block session file creation (display.jsonl, status.txt) which the display nvim
        // needs to watch on startup.
        let lsp_config_task = tokio::spawn(lsp_client::extract_config_from_nvim());

        // Build tools list including subagent tools
        let model_infos = if self.subagent_model_selection {
            self.llm.available_models()
        } else {
            vec![llm_rs::llm::ModelInfo {
                id: self.model.clone(),
                description: "Same model as parent conversation".into(),
            }]
        };
        let mut tools_list: Vec<Arc<Tool>> = vec![
            Arc::new(tools::bash_tool()),
            Arc::new(tools::current_time_tool()),
            Arc::new(tools::glob_tool()),
            Arc::new(tools::grep_tool()),
            Arc::new(tools::read_tool()),
            Arc::new(tools::write_tool()),
            Arc::new(tools::edit_tool()),
            Arc::new(tools::web_fetch_tool()),
            Arc::new(tools::web_search_tool()),
        ];
        tools_list.push(Arc::new(create_subagent_tool(&model_infos)));
        tools_list.push(Arc::new(create_continue_subagent_tool()));
        if !skills.is_empty() {
            tools_list.push(Arc::new(tools::skill_tool(Arc::clone(&skills))));
        }

        // Create session files early so the display nvim (which may already be running)
        // can start watching them before the LSP config await which can take seconds.
        let resuming = self.conversation_state_file.exists();
        if !resuming {
            tokio::fs::write(&self.display_file, "")
                .await
                .with_context(|| {
                    format!("Failed to initialize display file {:?}", self.display_file)
                })?;
            tokio::fs::write(&self.status_file, "Ready")
                .await
                .with_context(|| {
                    format!("Failed to initialize status file {:?}", self.status_file)
                })?;
        }

        // Await LSP config extraction result and conditionally add LSP tool
        let lsp_manager = match lsp_config_task.await {
            Ok(Ok(lsp_config)) => {
                if lsp_config.has_servers() {
                    let manager =
                        std::sync::Arc::new(lsp_client::LspManager::new(lsp_config, cwd.clone()));
                    // Pre-warm: detect project languages and start servers in background
                    let manager_clone = manager.clone();
                    let project_dir = cwd.clone();
                    let _pre_warm = tokio::spawn(async move {
                        manager_clone.pre_warm(&project_dir).await;
                    });
                    tools_list.push(Arc::new(tools::lsp_tool(manager.clone())));
                    Some(manager)
                } else {
                    tracing::info!("No LSP servers configured in nvim");
                    None
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("Failed to extract LSP config from nvim: {e}");
                None
            }
            Err(e) => {
                tracing::warn!("LSP config extraction task panicked: {e}");
                None
            }
        };

        // Write LSP hint file if no servers configured
        if lsp_manager.is_none() {
            let hint_path = self.session_dir.join("lsp-hint.txt");
            if let Err(e) = tokio::fs::write(
                &hint_path,
                "LSP tools not available. Configure LSP in Neovim for code intelligence.\n\
                 See: https://neovim.io/doc/user/lsp.html",
            )
            .await
            {
                tracing::warn!("Failed to write LSP hint file: {e}");
            }
        }

        let tool_clients: ToolClientMap = Arc::new(parking_lot::Mutex::new(HashMap::new()));

        let conversation_client = if resuming {
            tracing::info!(
                "Resuming conversation from {:?}",
                self.conversation_state_file
            );

            // Load root conversation state
            let state_json = std::fs::read_to_string(&self.conversation_state_file)
                .with_context(|| format!("Failed to read {:?}", self.conversation_state_file))?;
            let root_state: ConversationState = serde_json::from_str(&state_json)
                .with_context(|| "Failed to parse conversation-state.json")?;

            // Resume the full conversation tree (root + all subagents)
            let (_, client, resumed_subagents) = manager.resume_conversation_tree(
                root_state,
                self.llm,
                tools_list,
                self.max_subagent_depth,
                self.session_dir.clone(),
            )?;

            // Close stale pending permission requests from previous session
            manager.permission_manager().close_all_pending();

            // Close stale "running" tool calls and subagents in display files
            close_stale_running_items(&self.session_dir)
                .await
                .with_context(|| "Failed to close stale running items on resume")?;

            // Spawn event writers for resumed subagents (appending to existing files)
            for sa in &resumed_subagents {
                tracing::info!(conversation_id = %sa.conversation_id, "Resumed subagent conversation");
                let mgr_clone = Arc::clone(&manager);
                let sa_events = Box::pin(sa.client.subscribe_new());
                let sa_display = sa.state_dir.join("display.jsonl");
                let sa_status = sa.state_dir.join("status.txt");
                let sa_token_usage = sa.state_dir.join("token_usage.txt");
                let sa_dir_clone = sa.state_dir.clone();
                let sa_conv_client = Arc::clone(&sa.client);
                let sa_tool_clients = Arc::clone(&tool_clients);
                tokio::spawn(async move {
                    if let Err(e) = run_event_writer(
                        sa_events,
                        sa_display,
                        sa_status,
                        sa_token_usage,
                        sa_dir_clone,
                        Some(mgr_clone),
                        sa_conv_client,
                        sa_tool_clients,
                    )
                    .await
                    {
                        tracing::error!(error = %e, "Resumed subagent event writer failed");
                    }
                });
            }

            // Clear stale usage percentage so the status bar doesn't show an
            // outdated "X% used" while we wait for the fresh fetch.
            tokio::fs::write(&self.usage_file, "")
                .await
                .with_context(|| {
                    format!("Failed to clear stale usage file {:?}", self.usage_file)
                })?;

            // Do NOT truncate display.jsonl on resume; subscribe to new events only
            tokio::fs::write(&self.status_file, "Ready")
                .await
                .with_context(|| format!("Failed to write status file {:?}", self.status_file))?;

            let manager_clone = Arc::clone(&manager);
            let events = Box::pin(client.subscribe_new());
            let display_file = self.display_file.clone();
            let status_file = self.status_file.clone();
            let token_usage_file = self.session_dir.join("token_usage.txt");
            let session_dir = self.session_dir.clone();
            let root_client = Arc::clone(&client);
            let tc_map = Arc::clone(&tool_clients);
            tokio::spawn(run_event_writer(
                events,
                display_file,
                status_file,
                token_usage_file,
                session_dir,
                Some(manager_clone),
                root_client,
                tc_map,
            ));

            client
        } else {
            // New conversation path (display.jsonl and status.txt already created above)
            let (_, client) = manager.new_conversation(
                self.llm,
                &self.model,
                tools_list,
                self.chat_options.clone(),
                false,
                0, // root conversation depth
                self.max_subagent_depth,
                Some(self.session_dir.clone()),
            )?;

            let manager_clone = Arc::clone(&manager);
            let events = Box::pin(client.subscribe());
            let display_file = self.display_file.clone();
            let status_file = self.status_file.clone();
            let token_usage_file = self.session_dir.join("token_usage.txt");
            let session_dir = self.session_dir.clone();
            let root_client = Arc::clone(&client);
            let tc_map = Arc::clone(&tool_clients);
            tokio::spawn(run_event_writer(
                events,
                display_file,
                status_file,
                token_usage_file,
                session_dir,
                Some(manager_clone),
                root_client,
                tc_map,
            ));

            client
        };

        // Broadcast skill shadow warnings to the UI
        for warning in skill_warnings {
            conversation_client.broadcast_system_warning(warning);
        }

        // Shutdown signal
        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        let shutdown_tx = Arc::new(shutdown_tx);
        let socket_path = self.socket_path.clone();

        // Periodic usage polling (OAuth only, every 5 min)
        if let Some(ref tm) = token_manager {
            let tm = tm.clone();
            let uf = usage_file.clone();
            let mut usage_rx = shutdown_tx.subscribe();
            tokio::spawn(async move {
                // Initial fetch
                write_usage_file(&tm, &uf).await;
                loop {
                    tokio::select! {
                        biased;
                        _ = usage_rx.recv() => break,
                        _ = tokio::time::sleep(tokio::time::Duration::from_secs(300)) => {
                            write_usage_file(&tm, &uf).await;
                        }
                    }
                }
            });
        }

        // Accept loop
        let mut shutdown_rx = shutdown_tx.subscribe();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => break,
                _ = tokio::signal::ctrl_c() => break,
                result = listener.accept() => {
                    let (stream, _) = result?;
                    let conv_client = Arc::clone(&conversation_client);
                    let tc_map = Arc::clone(&tool_clients);
                    let shutdown_tx = Arc::clone(&shutdown_tx);
                    let mgr = Arc::clone(&manager);
                    tokio::spawn(handle_client(stream, conv_client, tc_map, shutdown_tx, mgr));
                }
            }
        }

        // Signal display nvim to quit via status file
        tokio::fs::write(&self.status_file, "Shutdown")
            .await
            .with_context(|| {
                format!("Failed to write shutdown status to {:?}", self.status_file)
            })?;

        // Shutdown LSP servers
        if let Some(manager) = lsp_manager {
            manager.shutdown_all().await;
        }

        std::fs::remove_file(&socket_path)
            .with_context(|| format!("Failed to remove socket {:?}", socket_path))?;
        Ok(())
    }
}

/// Fetch subscription usage and write a human-readable summary to the usage file.
async fn write_usage_file(tm: &auth::TokenManager, usage_file: &Path) {
    let result: Result<()> = async {
        let token = tm
            .get_access_token()
            .await
            .context("Failed to get access token")?;
        let usage = auth::usage::fetch_usage(tm.client(), &token).await?;
        let five_hour = usage.five_hour.context("No five_hour usage window")?;
        let mut text = format!("{:.0}% used", five_hour.utilization);
        if let Some(resets_in) = five_hour
            .resets_at
            .as_deref()
            .and_then(auth::usage::format_resets_in)
        {
            text.push_str(&format!(", resets in {}", resets_in));
        }
        tokio::fs::write(usage_file, &text)
            .await
            .context("Failed to write usage file")?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::warn!("Failed to update usage file: {}", e);
    }
}

/// Reusable event writer that processes conversation events and writes them to display files.
/// Used for both the main conversation (manager = Some) and subagent conversations (manager = None).
/// When manager is Some, SubAgentStart events trigger creation of sub-session directories
/// and spawning of nested event writers for each subagent.
type EventStream =
    Pin<Box<dyn Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> + Send>>;

fn format_token_count(n: i32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Write accumulated token usage to a file for the nvim status bar.
///
/// The Anthropic API splits input tokens into three non-overlapping buckets:
/// - `input_tokens`: tokens NOT involved in any cache (not read from, not written to)
/// - `cache_creation_input_tokens`: tokens fully processed AND written to a new cache entry (1.25x cost)
/// - `cache_read_input_tokens`: tokens served from an existing cache (0.1x cost, cheapest)
///
/// Total actual input = input_tokens + cache_creation_input_tokens + cache_read_input_tokens
///
/// We display:
/// - "in" = input_tokens + cache_creation_input_tokens (all tokens actually processed by the model)
/// - "cache read" = cache_read_input_tokens (tokens cheaply served from cache, not reprocessed)
/// - "out" = output_tokens
async fn write_token_usage(
    path: &Path,
    input: i32,
    output: i32,
    cache_creation: i32,
    cache_read: i32,
) -> Result<()> {
    // input + cache_creation = all tokens the model actually processed
    // cache_read = tokens served from cache without reprocessing
    let processed_input = input + cache_creation;
    let content = if cache_read > 0 {
        format!(
            "{} in │ {} cached │ {} out",
            format_token_count(processed_input),
            format_token_count(cache_read),
            format_token_count(output)
        )
    } else {
        format!(
            "{} in │ {} out",
            format_token_count(processed_input),
            format_token_count(output)
        )
    };
    tokio::fs::write(path, content)
        .await
        .context("Failed to write token usage file")
}

#[allow(clippy::too_many_arguments)]
fn run_event_writer(
    mut events: EventStream,
    display_file: PathBuf,
    status_file: PathBuf,
    token_usage_file: PathBuf,
    session_dir: PathBuf,
    manager: Option<Arc<ConversationManager>>,
    conv_client: Arc<ConversationClient>,
    tool_clients: ToolClientMap,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(async move {
        let mut tool_calls: HashMap<String, ToolCallState> = HashMap::new();
        let mut subagents: HashMap<String, SubAgentState> = HashMap::new();
        // Maps tool_call_index -> tool_call_id for AssistantToolCallArgChunk routing
        let mut tool_call_index_map: HashMap<usize, String> = HashMap::new();
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
                tokio::fs::write(&status_file, "Streaming...")
                    .await
                    .context("Failed to write status file")?;
            }
            if matches!(&*event, Message::AssistantThinkingChunk { .. }) && !is_thinking {
                is_thinking = true;
                tokio::fs::write(&status_file, "Thinking...")
                    .await
                    .context("Failed to write status file")?;
            }
            if matches!(&*event, Message::AssistantMessageChunk { .. }) && is_thinking {
                is_thinking = false;
                tokio::fs::write(&status_file, "Streaming...")
                    .await
                    .context("Failed to write status file")?;
            }
            match &*event {
                Message::AssistantToolCallStart {
                    tool_call_id,
                    tool_call_index,
                    tool_name,
                    ..
                } => {
                    tracing::info!(
                        tool_call_id,
                        tool_call_index,
                        tool_name,
                        "AssistantToolCallStart received"
                    );

                    // Create per-tool-call files early so the detail window can
                    // stream args as they arrive.
                    let tc_file = session_dir.join(format!("tool-call-{}.jsonl", tool_call_id));
                    let tc_status =
                        session_dir.join(format!("tool-call-{}-status.txt", tool_call_id));
                    tokio::fs::write(&tc_file, "")
                        .await
                        .context("Failed to create tool call file")?;
                    tokio::fs::write(&tc_status, "Generating")
                        .await
                        .context("Failed to create tool call status file")?;

                    tool_calls.insert(
                        tool_call_id.clone(),
                        ToolCallState {
                            file_path: tc_file.clone(),
                            status_file_path: tc_status,
                            tool_name: tool_name.clone(),
                            accumulated_preview: String::new(),
                        },
                    );
                    tool_call_index_map.insert(*tool_call_index, tool_call_id.clone());

                    // Write to both main display and per-tool-call file
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append display event")?;
                    append_event(&tc_file, &event)
                        .await
                        .context("Failed to append tool call start event")?;
                }

                Message::AssistantToolCallArgChunk {
                    tool_call_index, ..
                } => {
                    if let Some(tool_call_id) = tool_call_index_map.get(tool_call_index)
                        && let Some(state) = tool_calls.get(tool_call_id)
                    {
                        // Write to per-tool-call file for streaming display
                        append_event(&state.file_path, &event)
                            .await
                            .context("Failed to append tool call arg chunk")?;
                    }
                    // Always write to main display
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append display event")?;
                }

                Message::ToolMessageStart {
                    tool_call_id,
                    tool_name,
                    tool_args,
                    ..
                } => {
                    tracing::info!(
                        tool_call_id,
                        tool_name,
                        tool_args,
                        "ToolMessageStart received"
                    );

                    // If AssistantToolCallStart already created the per-tool-call
                    // files and state, just append the event and update status.
                    // Otherwise create them now (fallback for providers that don't
                    // stream tool call args).
                    let tc_file = session_dir.join(format!("tool-call-{}.jsonl", tool_call_id));
                    if !tool_calls.contains_key(tool_call_id.as_str()) {
                        let tc_status =
                            session_dir.join(format!("tool-call-{}-status.txt", tool_call_id));
                        tokio::fs::write(&tc_file, "")
                            .await
                            .context("Failed to create tool call file")?;
                        tokio::fs::write(&tc_status, "Running")
                            .await
                            .context("Failed to create tool call status file")?;
                        tool_calls.insert(
                            tool_call_id.clone(),
                            ToolCallState {
                                file_path: tc_file.clone(),
                                status_file_path: tc_status,
                                tool_name: tool_name.clone(),
                                accumulated_preview: String::new(),
                            },
                        );
                    } else if let Some(state) = tool_calls.get(tool_call_id) {
                        tokio::fs::write(&state.status_file_path, "Running")
                            .await
                            .context("Failed to update tool call status to Running")?;
                    }

                    // Write event to both main display and per-tool-call file
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append display event")?;
                    append_event(&tc_file, &event)
                        .await
                        .context("Failed to append tool call event")?;

                    tool_clients
                        .lock()
                        .insert(tool_call_id.clone(), Arc::clone(&conv_client));
                }

                Message::ToolOutputChunk {
                    tool_call_id,
                    content,
                    ..
                } => {
                    let tracked = tool_calls.contains_key(tool_call_id.as_str());
                    tracing::debug!(
                        tool_call_id,
                        tracked,
                        content_len = content.len(),
                        "ToolOutputChunk received"
                    );
                    if let Some(state) = tool_calls.get_mut(tool_call_id) {
                        // Write to per-tool-call file only (NOT main display)
                        append_event(&state.file_path, &event)
                            .await
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

                Message::ToolMessageEnd {
                    tool_call_id,
                    end_status,
                    ..
                } => {
                    tracing::info!(tool_call_id, ?end_status, "ToolMessageEnd received");
                    tool_clients.lock().remove(tool_call_id.as_str());
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
                            append_event(&display_file, &preview_event)
                                .await
                                .context("Failed to append tool call preview")?;
                        }

                        // Write ToolMessageEnd to both files
                        append_event(&display_file, &event)
                            .await
                            .context("Failed to append display event")?;
                        append_event(&state.file_path, &event)
                            .await
                            .context("Failed to append tool call end")?;

                        // Mark the tool call with final status
                        let status_text = match end_status {
                            MessageEndStatus::Succeeded => "Done",
                            MessageEndStatus::Failed => "Failed",
                            MessageEndStatus::Cancelled => "Cancelled",
                            MessageEndStatus::UserDenied => "Denied",
                            MessageEndStatus::Timeout => "Timeout",
                        };
                        tokio::fs::write(&state.status_file_path, status_text)
                            .await
                            .context("Failed to write tool call status")?;
                        tracing::debug!(tool_call_id, status_text, "wrote status to status file");
                    } else {
                        tracing::warn!(
                            tool_call_id,
                            "ToolMessageEnd for untracked tool call — fallback to main display"
                        );
                        // Fallback: write to main display if we missed the start
                        append_event(&display_file, &event)
                            .await
                            .context("Failed to append display event")?;
                    }
                }

                Message::SubAgentStart {
                    conversation_id,
                    description,
                    ..
                } => {
                    tracing::info!(conversation_id, description, "SubAgentStart received");
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent start to display")?;

                    // When we have a manager, create a sub-session and spawn a nested event writer
                    if let Some(ref mgr) = manager {
                        let sa_dir = session_dir.join(format!("subagent-{}", conversation_id));
                        tokio::fs::create_dir_all(&sa_dir)
                            .await
                            .context("Failed to create subagent directory")?;

                        let sa_display = sa_dir.join("display.jsonl");
                        let sa_status = sa_dir.join("status.txt");
                        let sa_token_usage = sa_dir.join("token_usage.txt");
                        tokio::fs::write(&sa_display, "")
                            .await
                            .context("Failed to initialize subagent display file")?;
                        tokio::fs::write(&sa_status, "Running")
                            .await
                            .context("Failed to initialize subagent status file")?;

                        match mgr.get_conversation(conversation_id) {
                            Ok(Some(sa_client)) => {
                                let sa_events = Box::pin(sa_client.subscribe());
                                let sa_status_clone = sa_status.clone();
                                let sa_mgr = Arc::clone(mgr);
                                let sa_tool_clients = Arc::clone(&tool_clients);
                                let sa_conv_client = Arc::clone(&sa_client);
                                let handle = tokio::spawn(async move {
                                    if let Err(e) = run_event_writer(
                                        sa_events,
                                        sa_display,
                                        sa_status_clone,
                                        sa_token_usage,
                                        sa_dir,
                                        Some(sa_mgr),
                                        sa_conv_client,
                                        sa_tool_clients,
                                    )
                                    .await
                                    {
                                        tracing::error!(error = %e, "Subagent event writer failed");
                                    }
                                });
                                subagents.insert(
                                    conversation_id.clone(),
                                    SubAgentState {
                                        status_file_path: sa_status,
                                        task_handle: handle,
                                    },
                                );
                            }
                            Ok(None) => {
                                tracing::warn!(
                                    conversation_id,
                                    "Subagent conversation not found in manager"
                                );
                            }
                            Err(e) => {
                                tracing::error!(conversation_id, error = %e, "Failed to get subagent conversation");
                            }
                        }
                    }
                }

                Message::SubAgentEnd {
                    conversation_id,
                    end_status,
                    input_tokens,
                    output_tokens,
                    response,
                    ..
                } => {
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
                        tokio::fs::write(&state.status_file_path, "Done")
                            .await
                            .context("Failed to write subagent done status")?;
                        state.task_handle.abort();
                    }

                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent end to display")?;
                }

                Message::SubAgentTurnEnd {
                    conversation_id,
                    end_status,
                    input_tokens,
                    output_tokens,
                    response,
                    ..
                } => {
                    tracing::info!(
                        conversation_id,
                        ?end_status,
                        response_len = response.len(),
                        input_tokens,
                        output_tokens,
                        "SubAgentTurnEnd received"
                    );

                    // Write status — do NOT abort the event writer or remove from subagents
                    if let Some(state) = subagents.get(conversation_id) {
                        let status_str = match end_status {
                            MessageEndStatus::Cancelled => "Cancelled",
                            _ => "Idle",
                        };
                        tokio::fs::write(&state.status_file_path, status_str)
                            .await
                            .context("Failed to write subagent status")?;
                    }

                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent turn end to display")?;
                }

                Message::SubAgentContinue {
                    conversation_id,
                    description,
                    ..
                } => {
                    tracing::info!(conversation_id, description, "SubAgentContinue received");

                    // Write "Running" status — subagent dir and event writer already exist
                    if let Some(state) = subagents.get(conversation_id) {
                        tokio::fs::write(&state.status_file_path, "Running")
                            .await
                            .context("Failed to write subagent running status")?;
                    }

                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent continue to display")?;
                }

                Message::ToolRequestPermission { tool_call_id, .. } => {
                    if let Some(state) = tool_calls.get(tool_call_id) {
                        tokio::fs::write(&state.status_file_path, "Permission")
                            .await
                            .context("Failed to write tool call permission status")?;
                    }
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append display event")?;
                }

                Message::ToolPermissionApproved { tool_call_id, .. } => {
                    if let Some(state) = tool_calls.get(tool_call_id) {
                        tokio::fs::write(&state.status_file_path, "Running")
                            .await
                            .context("Failed to write tool call running status after approval")?;
                    }
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append display event")?;
                }

                Message::SubAgentWaitingPermission {
                    conversation_id, ..
                } => {
                    if let Some(state) = subagents.get(conversation_id) {
                        tokio::fs::write(&state.status_file_path, "Permission")
                            .await
                            .context("Failed to write subagent permission status")?;
                    }
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent waiting permission to display")?;
                }

                // Both Approved and Denied resolve a pending permission request
                // and put the subagent back into the Running state. They are
                // handled identically here; the denial is still recorded in
                // `display.jsonl` via `append_event` for audit/replay.
                Message::SubAgentPermissionApproved {
                    conversation_id, ..
                }
                | Message::SubAgentPermissionDenied {
                    conversation_id, ..
                } => {
                    if let Some(state) = subagents.get(conversation_id) {
                        tokio::fs::write(&state.status_file_path, "Running")
                            .await
                            .context("Failed to write subagent running status after permission resolution")?;
                    }
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent permission resolution to display")?;
                }

                Message::SubAgentInputStart { .. } => {
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent input start to display")?;
                }

                Message::SubAgentInputChunk { .. } => {
                    // Just write to display.jsonl — no per-tool-call file for subagent inputs
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append subagent input chunk to display")?;
                }

                Message::AssistantMessageEnd {
                    aggregate_input_tokens,
                    aggregate_output_tokens,
                    aggregate_cache_creation_tokens,
                    aggregate_cache_read_tokens,
                    ..
                } => {
                    append_event(&display_file, &event)
                        .await
                        .context("Failed to append display event")?;
                    is_thinking = false;
                    tool_call_index_map.clear();
                    tokio::fs::write(&status_file, "Ready")
                        .await
                        .context("Failed to write status file")?;
                    write_token_usage(
                        &token_usage_file,
                        *aggregate_input_tokens,
                        *aggregate_output_tokens,
                        *aggregate_cache_creation_tokens,
                        *aggregate_cache_read_tokens,
                    )
                    .await?;
                }

                Message::AggregateTokenUpdate {
                    aggregate_input_tokens,
                    aggregate_output_tokens,
                    aggregate_cache_creation_tokens,
                    aggregate_cache_read_tokens,
                } => {
                    write_token_usage(
                        &token_usage_file,
                        *aggregate_input_tokens,
                        *aggregate_output_tokens,
                        *aggregate_cache_creation_tokens,
                        *aggregate_cache_read_tokens,
                    )
                    .await?;
                }

                _ => {
                    // All other events: write to main display only
                    append_event(&display_file, &event)
                        .await
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
    conv_client: Arc<ConversationClient>,
    tool_clients: ToolClientMap,
    shutdown_tx: Arc<broadcast::Sender<()>>,
    manager: Arc<ConversationManager>,
) {
    let shutdown_rx = shutdown_tx.subscribe();
    if let Err(e) = handle_client_inner(
        stream,
        conv_client,
        tool_clients,
        shutdown_tx,
        shutdown_rx,
        manager,
    )
    .await
    {
        eprintln!("[Server] Client handler error: {}", e);
    }
}

async fn handle_client_inner(
    stream: UnixStream,
    conv_client: Arc<ConversationClient>,
    tool_clients: ToolClientMap,
    shutdown_tx: Arc<broadcast::Sender<()>>,
    mut shutdown_rx: broadcast::Receiver<()>,
    manager: Arc<ConversationManager>,
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
                    ClientMessage::SendMessage { conversation_id, content } => {
                        let result = if let Some(conv_id) = conversation_id {
                            match manager.get_conversation(&conv_id) {
                                Ok(Some(client)) => client.send_chat(&content).await.map_err(|e| e.to_string()),
                                Ok(None) => Err(format!("Conversation '{}' not found", conv_id)),
                                Err(e) => Err(e.to_string()),
                            }
                        } else {
                            conv_client.send_chat(&content).await.map_err(|e| e.to_string())
                        };
                        match result {
                            Ok(()) => send_msg(&mut sink, &ServerMessage::Ack).await?,
                            Err(e) => send_msg(&mut sink, &ServerMessage::Error {
                                message: format!("Chat error: {}", e),
                            }).await?,
                        }
                    }
                    ClientMessage::UserRequestEnd { conversation_id } => {
                        match manager.get_conversation(&conversation_id) {
                            Ok(Some(client)) => {
                                // Broadcast UserRequestEnd on the subagent's channel (for UI)
                                let msg = Message::UserRequestEnd {
                                    msg_id: client.next_msg_id(),
                                    conversation_id: conversation_id.clone(),
                                };
                                client.notify_msg(msg)
                                    .context("failed to broadcast UserRequestEnd")?;

                                // Resolve: extract response and send ToolCallResolved to parent
                                if let Some((parent_conv_id, tool_call_id)) = manager.get_subagent_parent(&conversation_id)
                                    && let Ok(Some(parent_client)) = manager.get_conversation(&parent_conv_id)
                                    && let Some(response) = client.extract_latest_response()
                                {
                                    let formatted = format_subagent_result(
                                        &conversation_id, &response, &MessageEndStatus::Succeeded,
                                    );
                                    parent_client.send_tool_call_resolved(
                                        tool_call_id, Arc::new(formatted),
                                    ).await
                                        .context("failed to send ToolCallResolved to parent")?;
                                }

                                send_msg(&mut sink, &ServerMessage::Ack).await?;
                            }
                            Ok(None) => {
                                send_msg(&mut sink, &ServerMessage::Error {
                                    message: format!("Conversation '{}' not found", conversation_id),
                                }).await?;
                            }
                            Err(e) => {
                                send_msg(&mut sink, &ServerMessage::Error {
                                    message: format!("Error looking up conversation: {}", e),
                                }).await?;
                            }
                        }
                    }
                    ClientMessage::CancelTool { tool_call_id } => {
                        let client = tool_clients.lock().get(&tool_call_id).cloned();
                        if let Some(client) = client {
                            client.cancel_tool(&tool_call_id);
                            send_msg(&mut sink, &ServerMessage::Ack).await?;
                        } else {
                            send_msg(&mut sink, &ServerMessage::Error {
                                message: format!("Tool call '{}' not found", tool_call_id),
                            }).await?;
                        }
                    }
                    ClientMessage::CancelConversation { conversation_id } => {
                        match manager.get_conversation(&conversation_id) {
                            Ok(Some(client)) => {
                                client.cancel();
                                send_msg(&mut sink, &ServerMessage::Ack).await?;
                            }
                            Ok(None) => {
                                send_msg(&mut sink, &ServerMessage::Error {
                                    message: format!("Conversation '{}' not found", conversation_id),
                                }).await?;
                            }
                            Err(e) => {
                                send_msg(&mut sink, &ServerMessage::Error {
                                    message: format!("Error looking up conversation: {}", e),
                                }).await?;
                            }
                        }
                    }
                    ClientMessage::ResolvePermission { key, decision, request_id } => {
                        match manager.permission_manager().resolve(&key, &decision, request_id.as_deref()) {
                            Ok(()) => {
                                if let Err(e) = conv_client.notify_msg(
                                    Message::PermissionUpdated {
                                        msg_id: conv_client.next_msg_id(),
                                    },
                                ) {
                                    tracing::error!(error = %e, "failed to broadcast PermissionUpdated");
                                }
                                send_msg(&mut sink, &ServerMessage::Ack).await?;
                            }
                            Err(e) => send_msg(&mut sink, &ServerMessage::Error {
                                message: format!("Failed to resolve permission: {}", e),
                            }).await?,
                        }
                    }
                    ClientMessage::AddPermission { key, scope } => {
                        match manager.permission_manager().add_permission(key, scope) {
                            Ok(()) => {
                                if let Err(e) = conv_client.notify_msg(
                                    Message::PermissionUpdated {
                                        msg_id: conv_client.next_msg_id(),
                                    },
                                ) {
                                    tracing::error!(error = %e, "failed to broadcast PermissionUpdated");
                                }
                                send_msg(&mut sink, &ServerMessage::Ack).await?;
                            }
                            Err(e) => send_msg(&mut sink, &ServerMessage::Error {
                                message: format!("Failed to add permission: {}", e),
                            }).await?,
                        }
                    }
                    ClientMessage::RevokePermission { key } => {
                        match manager.permission_manager().revoke(&key) {
                            Ok(()) => {
                                if let Err(e) = conv_client.notify_msg(
                                    Message::PermissionUpdated {
                                        msg_id: conv_client.next_msg_id(),
                                    },
                                ) {
                                    tracing::error!(error = %e, "failed to broadcast PermissionUpdated");
                                }
                                send_msg(&mut sink, &ServerMessage::Ack).await?;
                            }
                            Err(e) => send_msg(&mut sink, &ServerMessage::Error {
                                message: format!("Failed to revoke permission: {}", e),
                            }).await?,
                        }
                    }
                    ClientMessage::GetPermissionState => {
                        let state = manager.permission_manager().snapshot();
                        send_msg(&mut sink, &ServerMessage::PermissionState(state)).await?;
                    }
                    ClientMessage::Shutdown => {
                        if shutdown_tx.send(()).is_err() {
                            tracing::warn!("shutdown receiver already dropped");
                        }
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

/// On resume, close any tool calls or subagents that were still "running"
/// when the previous session exited. Appends synthetic Cancelled end events
/// to display.jsonl files and updates status files.
async fn close_stale_running_items(session_dir: &PathBuf) -> Result<()> {
    close_stale_in_dir(session_dir).await
}

/// Process a single directory's display.jsonl, close stale items, and recurse into subagent dirs.
fn close_stale_in_dir(dir: &PathBuf) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    Box::pin(async move {
        let display_file = dir.join("display.jsonl");
        if !display_file.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&display_file)
            .await
            .with_context(|| format!("Failed to read {:?}", display_file))?;

        // Track open tool calls: tool_call_id -> tool_name
        let mut open_tools: HashMap<String, String> = HashMap::new();

        // Track subagent states: conversation_id -> is_running
        // true = Running (needs closing), false = Idle (already has turn end)
        let mut subagent_running: HashMap<String, bool> = HashMap::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let msg: Message = match serde_json::from_str(line) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "Skipping unparseable line in display.jsonl");
                    continue;
                }
            };
            match &msg {
                Message::ToolMessageStart {
                    tool_call_id,
                    tool_name,
                    ..
                } => {
                    open_tools.insert(tool_call_id.clone(), tool_name.clone());
                }
                Message::ToolMessageEnd { tool_call_id, .. } => {
                    open_tools.remove(tool_call_id);
                }
                Message::SubAgentStart {
                    conversation_id, ..
                } => {
                    subagent_running.insert(conversation_id.clone(), true);
                }
                Message::SubAgentTurnEnd {
                    conversation_id, ..
                } => {
                    if let Some(running) = subagent_running.get_mut(conversation_id) {
                        *running = false;
                    }
                }
                Message::SubAgentContinue {
                    conversation_id, ..
                } => {
                    if let Some(running) = subagent_running.get_mut(conversation_id) {
                        *running = true;
                    }
                }
                Message::SubAgentEnd {
                    conversation_id, ..
                } => {
                    subagent_running.remove(conversation_id);
                }
                _ => {}
            }
        }

        // Close stale tool calls
        for tool_call_id in open_tools.keys() {
            tracing::info!(tool_call_id, dir = ?dir, "Closing stale running tool call");

            let end_event = Message::ToolMessageEnd {
                msg_id: 0,
                tool_call_id: tool_call_id.clone(),
                end_status: MessageEndStatus::Cancelled,
                input_tokens: 0,
                output_tokens: 0,
            };

            append_event(&display_file, &end_event)
                .await
                .with_context(|| {
                    format!(
                        "Failed to append synthetic ToolMessageEnd for {}",
                        tool_call_id
                    )
                })?;

            // Also append to per-tool-call file
            let tc_file = dir.join(format!("tool-call-{}.jsonl", tool_call_id));
            if tc_file.exists() {
                append_event(&tc_file, &end_event).await.with_context(|| {
                    format!("Failed to append synthetic ToolMessageEnd to {:?}", tc_file)
                })?;
            }

            // Update status file
            let tc_status = dir.join(format!("tool-call-{}-status.txt", tool_call_id));
            tokio::fs::write(&tc_status, "Done")
                .await
                .with_context(|| format!("Failed to write tool call status {:?}", tc_status))?;
        }

        // Close stale running subagents (only those still in Running state)
        for (conversation_id, is_running) in &subagent_running {
            if !*is_running {
                continue;
            }

            tracing::info!(conversation_id, dir = ?dir, "Closing stale running subagent");

            let end_event = Message::SubAgentTurnEnd {
                msg_id: 0,
                conversation_id: conversation_id.clone(),
                end_status: MessageEndStatus::Cancelled,
                response: Arc::new(String::new()),
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            };

            append_event(&display_file, &end_event)
                .await
                .with_context(|| {
                    format!(
                        "Failed to append synthetic SubAgentTurnEnd for {}",
                        conversation_id
                    )
                })?;

            // Update subagent status file
            let sa_status = dir
                .join(format!("subagent-{}", conversation_id))
                .join("status.txt");
            if sa_status.exists() {
                tokio::fs::write(&sa_status, "Cancelled")
                    .await
                    .with_context(|| format!("Failed to write subagent status {:?}", sa_status))?;
            }
        }

        // Recurse into subagent directories
        let mut read_dir = tokio::fs::read_dir(dir)
            .await
            .with_context(|| format!("Failed to read directory {:?}", dir))?;
        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("subagent-") && entry.file_type().await?.is_dir() {
                let subdir = entry.path();
                close_stale_in_dir(&subdir)
                    .await
                    .with_context(|| format!("Failed to close stale items in {:?}", subdir))?;
            }
        }

        Ok(())
    }) // Box::pin
}
