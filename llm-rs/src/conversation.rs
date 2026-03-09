use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::llm::{ChatOptions, LLMEvent, LLMMessage, ModelInfo, StopReason, ToolCall, LLM};
use crate::tool::{CancellationToken, Tool, ToolContext};
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::task::AbortHandle;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

/// Shared prompt rules appended to both root and subagent system prompts.
pub const SUBAGENT_RULES: &str = "\
## Subagent Management Rules

1. **Prefer continuing over creating.** When a follow-up question relates \
to work already done by an existing subagent, use `continue_subagent` \
to query that subagent first. Only spawn a new subagent if the task is \
genuinely independent of all prior subagent work.

2. **Ask, don't inspect.** When you need to know what a subagent did, how \
it did it, or what data it saw, use `continue_subagent` to ask it directly.

3. **Follow the delegation chain recursively.** If a subagent says it \
delegated work to its own subagents and lacks certain details, ask it to \
query its subagents — don't accept \"I don't know\" as the final answer if \
there is still an agent in the chain that might know.

4. **Provenance over corroboration.** When asked \"what are your sources\" or \
\"where did that come from,\" the goal is to trace the ACTUAL source of the \
information — not to find new sources that agree with it. These are \
fundamentally different tasks. Finding new supporting evidence is not the \
same as citing your actual sources.

5. **Don't approximate what you can verify.** If precise information (e.g. \
word counts, exact sources, specific claims) exists somewhere in the \
subagent chain, pursue it through `continue_subagent` rather than giving \
estimates or hedging.

6. **Before spawning a new subagent, check:** Could an existing subagent \
answer this? If the question is about the process, sources, reasoning, \
or details behind a previous subagent's output, continue that subagent.

7. **Do not start unnecessary subagents.** Do not create a subagent just to \
do something similar as the current task. It will create unnecessary nested \
subagents and increase the context window.

8. **Do not use subagent to avoid block.** If some operations are blocked, \
do not try to use subagent to try the same thing. It will be blocked as well \
and the only thing you are doing is waste tokens.

## Context Window Management

Some tools (e.g. `web_fetch`) can return large amounts of text that consume your context window. \
**Delegate tasks that may produce large outputs to a child subagent** instead of calling them directly, \
unless your task is essentially just to perform that operation (i.e. you were spawned specifically for it). \
The child subagent will process the content and return only the relevant information to you. \
This keeps your context window small and allows you to handle more steps effectively. \
Never re-delegate: if your assigned task is already just needed a single tool call, just do it directly. \
Otherwise it will create an infinite loop of subagent calls.";

/// Serializable snapshot of a conversation's state for persistence and resume.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationState {
    pub id: String,
    pub model: String,
    pub llm_msgs: Vec<LLMMessage>,
    pub chat_options: ChatOptions,
    pub msg_id_counter: i32,
    pub total_input_tokens: i32,
    pub total_output_tokens: i32,
    pub single_turn: bool,
    pub subagent_depth: usize,
}

/// Fill in synthetic "cancelled" ToolResults for any tool calls that lack results.
///
/// LLM APIs require a tool_result after every tool_use. If the conversation was
/// interrupted mid-tool-call, some tool_calls may lack results. This function
/// finds the last Assistant message with tool_calls and adds "cancelled" results
/// for any tool_call_ids that don't have a corresponding ToolResult after it.
pub fn fill_cancelled_tool_results(llm_msgs: &mut Vec<LLMMessage>) {
    // Find the last Assistant message with tool_calls
    let last_assistant_with_tools = llm_msgs.iter().enumerate().rev().find_map(|(i, msg)| {
        if let LLMMessage::Assistant { tool_calls, .. } = msg {
            if !tool_calls.is_empty() {
                return Some((i, tool_calls.clone()));
            }
        }
        None
    });

    let Some((assistant_idx, tool_calls)) = last_assistant_with_tools else {
        return;
    };

    // Collect tool_call_ids that already have ToolResults after the assistant message
    let existing_result_ids: std::collections::HashSet<&str> = llm_msgs[assistant_idx + 1..]
        .iter()
        .filter_map(|msg| {
            if let LLMMessage::ToolResult { tool_call_id, .. } = msg {
                Some(tool_call_id.as_str())
            } else {
                None
            }
        })
        .collect();

    // Add synthetic "cancelled" results for missing ones
    let missing: Vec<ToolCall> = tool_calls
        .into_iter()
        .filter(|tc| !existing_result_ids.contains(tc.id.as_str()))
        .collect();

    for tc in missing {
        llm_msgs.push(LLMMessage::ToolResult {
            tool_call_id: tc.id,
            content: "Tool call was cancelled due to conversation interruption.".to_string(),
        });
    }
}

type MessageID = i32;

/// Get current timestamp in milliseconds since Unix epoch
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageEndStatus {
    Succeeded,
    Failed,
    Cancelled,
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemMessageLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    UserMessage {
        msg_id: MessageID,
        created_at: u64,
        content: Arc<String>,
    },

    AssistantMessageStart {
        msg_id: MessageID,
        created_at: u64,
    },

    AssistantMessageChunk {
        msg_id: MessageID,
        content: Arc<String>,
    },

    AssistantThinkingChunk {
        msg_id: MessageID,
        content: Arc<String>,
    },

    AssistantMessageEnd {
        msg_id: MessageID,
        end_status: MessageEndStatus,
        error: Option<String>,
        input_tokens: i32,
        output_tokens: i32,
        reasoning_tokens: i32,
    },

    ToolMessageStart {
        msg_id: MessageID,
        tool_call_id: String,
        created_at: u64,
        tool_name: String,
        tool_args: String,
    },

    ToolOutputChunk {
        msg_id: MessageID,
        tool_call_id: String,
        tool_name: String,
        content: Arc<String>,
    },

    ToolMessageEnd {
        msg_id: MessageID,
        tool_call_id: String,
        end_status: MessageEndStatus,
        input_tokens: i32,
        output_tokens: i32,
    },

    SubAgentStart {
        msg_id: MessageID,
        conversation_id: String,
        description: String,
    },

    SubAgentEnd {
        msg_id: MessageID,
        conversation_id: String,
        end_status: MessageEndStatus,
        response: Arc<String>,
        input_tokens: i32,
        output_tokens: i32,
    },

    /// A sub-agent turn completed but the conversation is still alive (idle).
    SubAgentTurnEnd {
        msg_id: MessageID,
        conversation_id: String,
        end_status: MessageEndStatus,
        response: Arc<String>,
        input_tokens: i32,
        output_tokens: i32,
    },

    /// A sub-agent is being resumed with a follow-up message.
    SubAgentContinue {
        msg_id: MessageID,
        conversation_id: String,
        description: String,
    },

    // assistant request to end the conversation, useful for sub agents
    AssistantRequestEnd {
        total_input_tokens: i32,
        total_output_tokens: i32,
    },

    /// System-level message (info, warning, error)
    SystemMessage {
        msg_id: MessageID,
        created_at: u64,
        level: SystemMessageLevel,
        message: String,
    },
}

// ============================================================================
// Subagent tool parameter types
// ============================================================================

#[derive(Deserialize, JsonSchema)]
struct SubAgentParams {
    /// Description of the task for the subagent to perform
    task: String,
    /// Model ID to use for the subagent (see available models in tool description)
    model: String,
}

#[derive(Deserialize, JsonSchema)]
struct ContinueSubAgentParams {
    /// The conversation ID of the subagent to continue (from the [subagent_id: ...] prefix in previous results)
    conversation_id: String,
    /// The follow-up message to send to the subagent
    message: String,
}

/// Cloneable conversation environment passed to spawned tool-execution tasks.
#[derive(Clone)]
struct ConversationEnv {
    client: Arc<ConversationClient>,
    conversation_manager: Arc<ConversationManager>,
    tools: HashMap<String, Arc<Tool>>,
    chat_options: ChatOptions,
    subagent_max_iterations: usize,
    subagent_depth: usize,
    max_subagent_depth: usize,
    state_dir: Option<PathBuf>,
}

/// Create the `subagent` tool with a dynamic description listing available models.
pub fn create_subagent_tool(model_descriptions: &[ModelInfo]) -> Tool {
    let models_list: Vec<String> = model_descriptions
        .iter()
        .map(|m| format!("  - `{}`: {}", m.id, m.description))
        .collect();

    let description = format!(
        "Spawn a subagent to handle a task in its own context window. \
         The subagent has access to all tools and will return its final answer. \
         Subagents may also spawn their own subagents up to the configured depth limit. \
         Use this for tasks that produce large outputs \
         (web fetches, research, multi-step tool use) so the results are summarized \
         in the subagent's context rather than consuming your context window.\n\
         Always start the prompt to the subagent with \"You are a subagent.\" so that it knows its a subagent.\n\
         Give sub tasks to sub agents, do not just give the same task you received to a subagent.\n\n
         Available models:\n{}",
        models_list.join("\n")
    );

    let schema = schemars::schema_for!(SubAgentParams);
    Tool::new_sentinel("subagent", description, schema)
}

/// Create the `continue_subagent` tool.
pub fn create_continue_subagent_tool() -> Tool {
    let schema = schemars::schema_for!(ContinueSubAgentParams);
    Tool::new_sentinel(
        "continue_subagent",
        "Send a follow-up message to an existing idle subagent. \
         Use this to continue a conversation with a subagent that has already responded. \
         The conversation_id is found in the [subagent_id: ...] prefix of previous subagent results.",
        schema,
    )
}

/// Prepare tools and channels common to both new and resumed conversations.
fn prepare_conversation(
    llm: &mut dyn LLM,
    tools: Vec<Arc<Tool>>,
    msg_id_start: i32,
) -> (HashMap<String, Arc<Tool>>, mpsc::Receiver<String>, Arc<ConversationClient>) {
    llm.register_tools(tools.clone());
    let tools_map = tools.into_iter().map(|t| (t.name.clone(), t)).collect();
    let (input_tx, input_rx) = mpsc::channel(10);
    let (notify_tx, _) = broadcast::channel(100);
    let client = Arc::new(ConversationClient {
        msg_id_counter: AtomicI32::new(msg_id_start),
        msgs: RwLock::new(Vec::new()),
        input_channel_tx: input_tx,
        new_msg_notify_tx: notify_tx,
        tool_cancel_tokens: std::sync::Mutex::new(HashMap::new()),
    });
    (tools_map, input_rx, client)
}

// ============================================================================
// ConversationManager
// ============================================================================

pub struct ConversationManager {
    conversations: RwLock<HashMap<String, (Arc<ConversationClient>, AbortHandle)>>,
}

impl Default for ConversationManager {
    fn default() -> Self {
        Self {
            conversations: RwLock::new(HashMap::new()),
        }
    }
}

/// Manages conversations so that any new client can attach to an existing conversation.
impl ConversationManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Create a new conversation. The new conversation will be kept in the manager's
    /// memory until it ends.
    ///
    /// Returns `(conversation_id, client)`.
    pub fn new_conversation(
        self: &Arc<Self>,
        llm: Box<dyn LLM>,
        system_prompt: &str,
        model: &str,
        tools: Vec<Arc<Tool>>,
        chat_options: ChatOptions,
        single_turn: bool,
        subagent_max_iterations: usize,
        subagent_depth: usize,
        max_subagent_depth: usize,
        state_dir: Option<PathBuf>,
    ) -> Result<(String, Arc<ConversationClient>)> {
        let conversation_id = Uuid::new_v4().to_string();
        self.new_conversation_with_id(
            conversation_id,
            llm,
            system_prompt,
            model,
            tools,
            chat_options,
            single_turn,
            subagent_max_iterations,
            subagent_depth,
            max_subagent_depth,
            state_dir,
        )
    }

    pub fn new_conversation_with_id(
        self: &Arc<Self>,
        conversation_id: String,
        mut llm: Box<dyn LLM>,
        system_prompt: &str,
        model: &str,
        tools: Vec<Arc<Tool>>,
        chat_options: ChatOptions,
        single_turn: bool,
        subagent_max_iterations: usize,
        subagent_depth: usize,
        max_subagent_depth: usize,
        state_dir: Option<PathBuf>,
    ) -> Result<(String, Arc<ConversationClient>)> {
        let (tools_map, input_rx, client) = prepare_conversation(&mut *llm, tools, 0);
        let llm_msgs = if system_prompt.is_empty() {
            vec![]
        } else {
            vec![LLMMessage::System(system_prompt.to_string())]
        };
        let conversation = Conversation {
            id: conversation_id.clone(),
            llm,
            model: model.to_string(),
            llm_msgs,
            input_channel_rx: input_rx,
            total_input_tokens: 0,
            total_output_tokens: 0,
            single_turn,
            env: ConversationEnv {
                client,
                conversation_manager: Arc::clone(self),
                tools: tools_map,
                chat_options,
                subagent_max_iterations,
                subagent_depth,
                max_subagent_depth,
                state_dir,
            },
        };
        self.spawn_conversation(conversation)
    }

    /// Spawn a conversation task with panic recovery and register it in the manager.
    fn spawn_conversation(
        self: &Arc<Self>,
        conversation: Conversation,
    ) -> Result<(String, Arc<ConversationClient>)> {
        let conversation_id = conversation.id.clone();
        let client = conversation.env.client.clone();
        let watcher_client = client.clone();
        let task = tokio::spawn(async move {
            let mut conv = conversation;
            if let Err(e) = conv.start().await {
                log_and_broadcast_system_message(
                    &conv.env.client,
                    SystemMessageLevel::Error,
                    format!("Conversation ended with error: {}", e),
                );
            }
        });
        let abort_handle = task.abort_handle();

        // Watcher task: monitors the conversation task for panics/cancellation
        tokio::spawn(async move {
            if let Err(e) = task.await {
                let msg = if e.is_panic() {
                    format!("Internal error (panic): {}", e)
                } else {
                    "Conversation task cancelled".to_string()
                };
                log_and_broadcast_system_message(
                    &watcher_client,
                    SystemMessageLevel::Error,
                    msg,
                );
            }
        });

        self.conversations
            .write()
            .map_err(|e| anyhow::anyhow!("failed to acquire conversations write lock: {e}"))?
            .insert(conversation_id.clone(), (client.clone(), abort_handle));
        Ok((conversation_id, client))
    }

    /// Get a conversation by its id. It will try to load it from the manager's memory.
    /// If not found, load it from storage and put into the manager's memory.
    pub fn get_conversation(&self, conversation_id: &str) -> Result<Option<Arc<ConversationClient>>> {
        Ok(self
            .conversations
            .read()
            .map_err(|e| anyhow::anyhow!("failed to acquire conversations read lock: {e}"))?
            .get(conversation_id)
            .map(|x| x.0.clone())
        )
    }

    /// Remove the conversation from the manager's memory. The conversation should be
    /// cleared if there is no reference to the Arc anymore.
    pub fn end_conversation(&self, _conversation_id: &str) -> Result<()> {
        Ok(()) // placeholder
    }

    /// Resume a conversation from a persisted `ConversationState`.
    ///
    /// Calls `fill_cancelled_tool_results()` on the loaded llm_msgs,
    /// creates a `Conversation` with pre-populated state, and spawns it.
    pub fn resume_conversation(
        self: &Arc<Self>,
        mut state: ConversationState,
        mut llm: Box<dyn LLM>,
        tools: Vec<Arc<Tool>>,
        subagent_max_iterations: usize,
        max_subagent_depth: usize,
        state_dir: Option<PathBuf>,
    ) -> Result<(String, Arc<ConversationClient>)> {
        fill_cancelled_tool_results(&mut state.llm_msgs);
        let (tools_map, input_rx, client) = prepare_conversation(&mut *llm, tools, state.msg_id_counter);
        let conversation = Conversation {
            id: state.id.clone(),
            llm,
            model: state.model,
            llm_msgs: state.llm_msgs,
            input_channel_rx: input_rx,
            total_input_tokens: state.total_input_tokens,
            total_output_tokens: state.total_output_tokens,
            single_turn: state.single_turn,
            env: ConversationEnv {
                client,
                conversation_manager: Arc::clone(self),
                tools: tools_map,
                chat_options: state.chat_options,
                subagent_max_iterations,
                subagent_depth: state.subagent_depth,
                max_subagent_depth,
                state_dir,
            },
        };
        self.spawn_conversation(conversation)
    }

    /// Resume a full conversation tree from persisted state.
    ///
    /// Scans `state_dir` for `subagent-*/conversation-state.json`, resumes those
    /// first (so they're registered in the manager for `continue_subagent`), then
    /// resumes the root conversation.
    ///
    /// Returns the root client and a list of all resumed subagent conversations
    /// (so the caller can attach event writers or other UI).
    pub fn resume_conversation_tree(
        self: &Arc<Self>,
        state: ConversationState,
        llm: Box<dyn LLM>,
        tools: Vec<Arc<Tool>>,
        subagent_max_iterations: usize,
        max_subagent_depth: usize,
        state_dir: PathBuf,
    ) -> Result<(String, Arc<ConversationClient>, Vec<ResumedSubagent>)> {
        // Find all subagent states (depth-first: nested before parent)
        let subagent_states = find_subagent_states(&state_dir);
        let mut resumed_subagents = Vec::new();

        for (sa_dir, sa_state) in subagent_states {
            let sa_llm = llm.clone_box();
            let sa_tools = tools.clone();
            let (sa_id, sa_client) = self.resume_conversation(
                sa_state,
                sa_llm,
                sa_tools,
                subagent_max_iterations,
                max_subagent_depth,
                Some(sa_dir.clone()),
            )?;
            resumed_subagents.push(ResumedSubagent {
                conversation_id: sa_id,
                client: sa_client,
                state_dir: sa_dir,
            });
        }

        // Resume root conversation
        let (root_id, root_client) = self.resume_conversation(
            state,
            llm,
            tools,
            subagent_max_iterations,
            max_subagent_depth,
            Some(state_dir),
        )?;

        Ok((root_id, root_client, resumed_subagents))
    }
}

/// Info about a resumed subagent conversation, returned by
/// [`ConversationManager::resume_conversation_tree`].
pub struct ResumedSubagent {
    pub conversation_id: String,
    pub client: Arc<ConversationClient>,
    pub state_dir: PathBuf,
}

/// Recursively find subagent conversation states in a directory.
///
/// Returns entries depth-first (nested subagents before their parents)
/// so they can be resumed in dependency order.
fn find_subagent_states(dir: &Path) -> Vec<(PathBuf, ConversationState)> {
    let mut results = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("subagent-") {
            continue;
        }

        // Recurse into nested subagents first
        results.extend(find_subagent_states(&path));

        let state_file = path.join("conversation-state.json");
        let Ok(json) = std::fs::read_to_string(&state_file) else {
            continue;
        };
        let Ok(state) = serde_json::from_str::<ConversationState>(&json) else {
            continue;
        };
        results.push((path, state));
    }
    results
}

/// Log a message and broadcast it as a SystemMessage to the conversation client.
fn log_and_broadcast_system_message(
    client: &ConversationClient,
    level: SystemMessageLevel,
    message: String,
) {
    match &level {
        SystemMessageLevel::Error => tracing::error!(%message),
        SystemMessageLevel::Warning => tracing::warn!(%message),
        SystemMessageLevel::Info => tracing::info!(%message),
    }
    if let Err(e) = client.notify_msg(Message::SystemMessage {
        msg_id: client.next_msg_id(),
        created_at: now_millis(),
        level,
        message,
    }) {
        tracing::warn!(error = %e, "failed to broadcast system message");
    }
}

/// Truncate a string for preview display, appending "..." if truncated.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        let end = s.floor_char_boundary(max_len);
        format!("{}...", &s[..end])
    } else {
        s.to_string()
    }
}

/// Collected response from a subagent message stream.
struct SubagentResponse {
    text: String,
    input_tokens: i32,
    output_tokens: i32,
    end_status: MessageEndStatus,
}

/// Collect a subagent's response from its message stream until AssistantRequestEnd.
async fn collect_subagent_response(
    sub_stream: &mut (impl Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> + Unpin),
) -> SubagentResponse {
    let mut resp = SubagentResponse {
        text: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        end_status: MessageEndStatus::Succeeded,
    };

    while let Some(result) = sub_stream.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(_) => continue,
        };

        match &*msg {
            Message::AssistantMessageChunk { content, .. } => {
                resp.text.push_str(content);
            }
            Message::AssistantMessageEnd {
                end_status: MessageEndStatus::Failed, error: Some(err), ..
            } if resp.text.is_empty() => {
                resp.text = format!("Error: Subagent failed: {}", err);
                resp.end_status = MessageEndStatus::Failed;
            }
            Message::AssistantRequestEnd { total_input_tokens, total_output_tokens } => {
                resp.input_tokens = *total_input_tokens;
                resp.output_tokens = *total_output_tokens;
                break;
            }
            _ => {}
        }
    }

    resp
}

/// Format a subagent result with the conversation ID prefix.
fn format_subagent_result(conversation_id: &str, text: &str) -> String {
    if text.is_empty() {
        format!("[subagent_id: {}]\nSubagent completed but produced no output.", conversation_id)
    } else {
        format!("[subagent_id: {}]\n{}", conversation_id, text)
    }
}

// ============================================================================
// Conversation
// ============================================================================

pub struct Conversation {
    pub id: String,

    /// LLM API.
    llm: Box<dyn LLM>,

    model: String,

    /// LLM messages so far. Used to keep tracking the current messages and send the next message
    /// to LLM.
    llm_msgs: Vec<LLMMessage>,

    /// Chat input receiver. Used for Conversation to poll the messages.
    input_channel_rx: mpsc::Receiver<String>,

    /// Accumulated token usage.
    total_input_tokens: i32,
    total_output_tokens: i32,

    /// When true, the conversation exits after one user message + LLM response cycle.
    single_turn: bool,

    /// Cloneable environment passed to spawned tool-execution tasks.
    env: ConversationEnv,
}

/// Multi round LLM conversation. Thread and async safe.
impl Conversation {
    fn next_msg_id(&self) -> MessageID {
        self.env.client.next_msg_id()
    }

    fn broadcast_msg(&self, msg: Message) -> Result<()> {
        self.env.client.notify_msg(msg)
    }

    fn snapshot_state(&self) -> ConversationState {
        ConversationState {
            id: self.id.clone(),
            model: self.model.clone(),
            llm_msgs: self.llm_msgs.clone(),
            chat_options: self.env.chat_options.clone(),
            msg_id_counter: self.env.client.msg_id_counter_value(),
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            single_turn: self.single_turn,
            subagent_depth: self.env.subagent_depth,
        }
    }

    fn save_state(&self) -> Result<()> {
        if let Some(ref dir) = self.env.state_dir {
            let state = self.snapshot_state();
            let json = serde_json::to_string_pretty(&state)?;
            let tmp = dir.join("conversation-state.json.tmp");
            let target = dir.join("conversation-state.json");
            std::fs::write(&tmp, &json)?;
            std::fs::rename(&tmp, &target)?;
        }
        Ok(())
    }

    fn push_llm_msg(&mut self, msg: LLMMessage) -> Result<()> {
        self.llm_msgs.push(msg);
        self.save_state()
    }

    /// Broadcast SubAgentTurnEnd for a subagent response (without pushing to llm_msgs).
    /// Returns `(tool_call_id, result_text)` for the caller to push.
    fn broadcast_subagent_turn_end(
        client: &ConversationClient,
        conversation_id: &str,
        response: &SubagentResponse,
    ) -> Result<String> {
        let result_text = format_subagent_result(conversation_id, &response.text);

        client.notify_msg(Message::SubAgentTurnEnd {
            msg_id: client.next_msg_id(),
            conversation_id: conversation_id.to_string(),
            end_status: response.end_status.clone(),
            response: Arc::new(result_text.clone()),
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
        })?;

        Ok(result_text)
    }

    /// Start the worker loop for the conversation. Should only be called once.
    ///
    /// For single-turn (subagent) conversations, broadcasts `AssistantRequestEnd` after
    /// each turn but keeps looping so the parent can send follow-up messages via
    /// `continue_subagent`.
    async fn start(&mut self) -> Result<()> {
        while let Some(user_input) = self.input_channel_rx.recv().await {
            let user_msg_id = self.next_msg_id();
            self.broadcast_msg(Message::UserMessage {
                msg_id: user_msg_id,
                created_at: now_millis(),
                content: Arc::new(user_input.clone()),
            })?;

            self.push_llm_msg(LLMMessage::User(user_input))?;
            self.call_llm().await?;

            // Single-turn conversations broadcast end-of-turn but keep looping
            // so the parent can resume them with continue_subagent.
            if self.single_turn {
                self.broadcast_msg(Message::AssistantRequestEnd {
                    total_input_tokens: self.total_input_tokens,
                    total_output_tokens: self.total_output_tokens,
                })?;
            }
        }
        Ok(())
    }

    /// Call the LLM and handle the response.
    /// It handles the tool call and continues the loop when there is no tool call request anymore.
    async fn call_llm(&mut self) -> Result<()> {
        let max_iterations = if self.single_turn { self.env.subagent_max_iterations } else { usize::MAX };
        let mut iteration = 0;

        loop {
            iteration += 1;
            if iteration > max_iterations {
                log_and_broadcast_system_message(
                    &self.env.client,
                    SystemMessageLevel::Warning,
                    format!("Subagent reached maximum iterations limit ({})", max_iterations),
                );
                break;
            }

            // TODO: LLM stream cancellation will be restored with cancel_all()
            let mut response_stream =
                self.llm
                    .chat(self.model.as_str(), &self.llm_msgs, &self.env.chat_options);
            let mut accumulated_text = String::new();
            let mut pending_tool_calls = Vec::new();
            let mut should_continue = false;

            // Broadcast assistant message start
            self.broadcast_msg(Message::AssistantMessageStart {
                msg_id: self.next_msg_id(),
                created_at: now_millis(),
            })?;

            while let Some(event) = response_stream.next().await {
                match event {
                    LLMEvent::MessageStart { input_tokens } => {
                        self.total_input_tokens += input_tokens;
                    }
                    LLMEvent::TextDelta(text) => {
                        accumulated_text.push_str(&text);
                        let chunk_msg_id = self.next_msg_id();
                        self.broadcast_msg(Message::AssistantMessageChunk {
                            msg_id: chunk_msg_id,
                            content: Arc::new(text),
                        })?;
                    }
                    LLMEvent::ThinkingDelta(text) => {
                        // Reasoning is streamed for display; raw field handles round-tripping
                        self.broadcast_msg(Message::AssistantThinkingChunk {
                            msg_id: self.next_msg_id(),
                            content: Arc::new(text),
                        })?;
                    }
                    LLMEvent::ToolCall(tool_call) => {
                        pending_tool_calls.push(tool_call);
                    }
                    LLMEvent::MessageEnd {
                        stop_reason,
                        input_tokens,
                        output_tokens,
                        reasoning_tokens,
                        raw,
                    } => {
                        self.total_input_tokens += input_tokens;
                        self.total_output_tokens += output_tokens;

                        let (end_status, error) = if stop_reason == StopReason::MaxTokens {
                            (
                                MessageEndStatus::Failed,
                                Some("Response truncated: maximum token limit reached".to_string()),
                            )
                        } else {
                            (MessageEndStatus::Succeeded, None)
                        };

                        self.broadcast_msg(Message::AssistantMessageEnd {
                            msg_id: self.next_msg_id(),
                            end_status,
                            error,
                            input_tokens,
                            output_tokens,
                            reasoning_tokens,
                        })?;

                        // Handle tool calls if any
                        if stop_reason == StopReason::ToolUse && !pending_tool_calls.is_empty() {
                            // Add assistant message with tool calls to llm_msgs
                            let tool_calls = std::mem::take(&mut pending_tool_calls);
                            self.push_llm_msg(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: tool_calls.clone(),
                                raw: raw.clone(),
                            })?;
                            self.execute_tool_calls(tool_calls).await?;
                            should_continue = true;
                        } else if !accumulated_text.is_empty() || raw.is_some() {
                            // Add assistant response to llm_msgs for context
                            self.push_llm_msg(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: vec![],
                                raw: raw.clone(),
                            })?;
                        }
                        break;
                    }
                    LLMEvent::Error(error) => {
                        self.broadcast_msg(Message::AssistantMessageEnd {
                            msg_id: self.next_msg_id(),
                            end_status: MessageEndStatus::Failed,
                            error: Some(error),
                            input_tokens: 0,
                            output_tokens: 0,
                            reasoning_tokens: 0,
                        })?;
                        return Ok(());
                    }
                }
            }

            if !should_continue {
                break;
            }
        }
        Ok(())
    }

    async fn execute_tool_calls(&mut self, tool_calls: Vec<ToolCall>) -> Result<()> {
        let mut join_set = tokio::task::JoinSet::new();

        for tool_call in tool_calls {
            let env = self.env.clone();
            match tool_call.name.as_str() {
                "subagent" => {
                    let llm = self.llm.clone_box();
                    join_set.spawn(async move {
                        execute_subagent_standalone(tool_call, env, llm).await
                    });
                }
                "continue_subagent" => {
                    join_set.spawn(async move {
                        execute_continue_subagent_standalone(tool_call, env).await
                    });
                }
                _ => {
                    join_set.spawn(async move {
                        execute_regular_tool(tool_call, env).await
                    });
                }
            }
        }

        // Collect all results and push them sequentially
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(inner) => {
                    let (tool_call_id, content) = inner?;
                    self.push_llm_msg(LLMMessage::ToolResult {
                        tool_call_id,
                        content,
                    })?;
                }
                Err(e) => {
                    log_and_broadcast_system_message(
                        &self.env.client,
                        SystemMessageLevel::Error,
                        format!("Tool call task failed: {}", e),
                    );
                }
            }
        }
        Ok(())
    }

}

/// Execute a regular (non-subagent) tool call as a standalone async function.
/// Returns `(tool_call_id, result_content)` for the caller to push into llm_msgs.
async fn execute_regular_tool(
    tool_call: ToolCall,
    env: ConversationEnv,
) -> Result<(String, String)> {
    let tool_arc = env.tools.get(&tool_call.name).cloned();
    let tool_token = env.client.register_tool_token(&tool_call.id);

    let tool_msg_id = env.client.next_msg_id();

    tracing::info!(
        tool_call_id = %tool_call.id,
        tool_name = %tool_call.name,
        args = %tool_call.arguments,
        "executing tool call"
    );

    env.client.notify_msg(Message::ToolMessageStart {
        msg_id: tool_msg_id,
        tool_call_id: tool_call.id.clone(),
        created_at: now_millis(),
        tool_name: tool_call.name.clone(),
        tool_args: tool_call.arguments.clone(),
    })?;

    let tool_ctx = ToolContext { cancel_token: tool_token.clone() };
    let (end_status, tool_result) = if let Some(tool) = tool_arc {
        tracing::debug!(tool_call_id = %tool_call.id, "tool found, starting stream");
        let mut output_stream = tool.execute(tool_ctx, tool_call.arguments.clone());
        let mut result_parts = Vec::new();
        while let Some(chunk) = output_stream.next().await {
            tracing::debug!(
                tool_call_id = %tool_call.id,
                chunk_len = chunk.len(),
                "tool output chunk"
            );
            env.client.notify_msg(Message::ToolOutputChunk {
                msg_id: env.client.next_msg_id(),
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                content: Arc::new(chunk.clone()),
            })?;
            result_parts.push(chunk);
        }
        tracing::info!(
            tool_call_id = %tool_call.id,
            result_len = result_parts.iter().map(|s| s.len()).sum::<usize>(),
            "tool stream finished"
        );
        let status = if tool_token.is_cancelled() {
            MessageEndStatus::Cancelled
        } else {
            MessageEndStatus::Succeeded
        };
        (status, result_parts.join(""))
    } else {
        let error_msg = format!("Error: Tool '{}' not found", tool_call.name);
        log_and_broadcast_system_message(
            &env.client,
            SystemMessageLevel::Error,
            format!("Tool '{}' not found", tool_call.name),
        );
        env.client.notify_msg(Message::ToolOutputChunk {
            msg_id: env.client.next_msg_id(),
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            content: Arc::new(error_msg.clone()),
        })?;
        (MessageEndStatus::Failed, error_msg)
    };

    env.client.unregister_tool_token(&tool_call.id);

    env.client.notify_msg(Message::ToolMessageEnd {
        msg_id: env.client.next_msg_id(),
        tool_call_id: tool_call.id.clone(),
        end_status,
        input_tokens: 0,
        output_tokens: 0,
    })?;

    Ok((tool_call.id, tool_result))
}

/// Execute a subagent tool call as a standalone async function.
/// Returns `(tool_call_id, result_content)` for the caller to push into llm_msgs.
async fn execute_subagent_standalone(
    tool_call: ToolCall,
    env: ConversationEnv,
    llm: Box<dyn LLM>,
) -> Result<(String, String)> {
    let params: SubAgentParams = match serde_json::from_str(&tool_call.arguments) {
        Ok(p) => p,
        Err(e) => {
            return Ok((tool_call.id, format!("Error: Failed to parse subagent arguments: {}", e)));
        }
    };

    // Collect parent's tools; include subagent tools only if depth allows nesting
    let child_depth = env.subagent_depth + 1;
    let allow_nesting = child_depth + 1 < env.max_subagent_depth;
    let subagent_tools: Vec<Arc<Tool>> = env.tools.values()
        .filter(|t| allow_nesting || t.name != "subagent")
        .cloned()
        .collect();

    // Pre-generate subagent conversation ID so we can create its state_dir
    let subagent_conv_id_pre = Uuid::new_v4().to_string();
    let subagent_state_dir = env.state_dir.as_ref().map(|d| {
        let dir = d.join(format!("subagent-{}", subagent_conv_id_pre));
        std::fs::create_dir_all(&dir)?;
        Ok::<_, anyhow::Error>(dir)
    }).transpose()?;

    // Create the subagent conversation
    let (subagent_conv_id, subagent_client) = match env.conversation_manager.new_conversation_with_id(
        subagent_conv_id_pre,
        llm,
        &format!("You are a subagent spawned to perform a specific task.\n\n{}", SUBAGENT_RULES),
        &params.model,
        subagent_tools,
        env.chat_options.clone(),
        true, // single_turn
        env.subagent_max_iterations,
        child_depth,
        env.max_subagent_depth,
        subagent_state_dir,
    ) {
        Ok(result) => result,
        Err(e) => {
            return Ok((tool_call.id, format!("Error: Failed to create subagent conversation: {}", e)));
        }
    };

    let task_preview = truncate_preview(&params.task, 100);
    env.client.notify_msg(Message::SubAgentStart {
        msg_id: env.client.next_msg_id(),
        conversation_id: subagent_conv_id.clone(),
        description: task_preview,
    })?;

    let mut sub_stream = subagent_client.subscribe();

    if let Err(e) = subagent_client.send_chat(&params.task).await {
        let error = format!("Error: Failed to send task to subagent: {}", e);
        env.client.notify_msg(Message::SubAgentEnd {
            msg_id: env.client.next_msg_id(),
            conversation_id: subagent_conv_id,
            end_status: MessageEndStatus::Failed,
            response: Arc::new(error.clone()),
            input_tokens: 0,
            output_tokens: 0,
        })?;
        return Ok((tool_call.id, error));
    }

    let response = collect_subagent_response(&mut sub_stream).await;
    let result_text = Conversation::broadcast_subagent_turn_end(
        &env.client, &subagent_conv_id, &response,
    )?;
    Ok((tool_call.id, result_text))
}

/// Execute continue_subagent tool call as a standalone async function.
/// Returns `(tool_call_id, result_content)` for the caller to push into llm_msgs.
async fn execute_continue_subagent_standalone(
    tool_call: ToolCall,
    env: ConversationEnv,
) -> Result<(String, String)> {
    let params: ContinueSubAgentParams = match serde_json::from_str(&tool_call.arguments) {
        Ok(p) => p,
        Err(e) => {
            return Ok((tool_call.id, format!("Error: Failed to parse continue_subagent arguments: {}", e)));
        }
    };

    let subagent_client = match env.conversation_manager.get_conversation(&params.conversation_id) {
        Ok(Some(client)) => client,
        Ok(None) => {
            return Ok((tool_call.id, format!("Error: Subagent conversation '{}' not found", params.conversation_id)));
        }
        Err(e) => {
            return Ok((tool_call.id, format!("Error: Failed to get subagent conversation: {}", e)));
        }
    };

    let msg_preview = truncate_preview(&params.message, 100);

    env.client.notify_msg(Message::SubAgentContinue {
        msg_id: env.client.next_msg_id(),
        conversation_id: params.conversation_id.clone(),
        description: msg_preview,
    })?;

    let mut sub_stream = subagent_client.subscribe_new();

    if let Err(e) = subagent_client.send_chat(&params.message).await {
        let error = format!("Error: Failed to send follow-up to subagent: {}", e);
        env.client.notify_msg(Message::SubAgentTurnEnd {
            msg_id: env.client.next_msg_id(),
            conversation_id: params.conversation_id,
            end_status: MessageEndStatus::Failed,
            response: Arc::new(error.clone()),
            input_tokens: 0,
            output_tokens: 0,
        })?;
        return Ok((tool_call.id, error));
    }

    let response = collect_subagent_response(&mut sub_stream).await;
    let result_text = Conversation::broadcast_subagent_turn_end(
        &env.client, &params.conversation_id, &response,
    )?;
    Ok((tool_call.id, result_text))
}


/// Use for the client to send chat messages and subscribe to the conversation's messages.
pub struct ConversationClient {
    msg_id_counter: AtomicI32,
    msgs: RwLock<Vec<Arc<Message>>>,
    input_channel_tx: mpsc::Sender<String>,
    new_msg_notify_tx: broadcast::Sender<Arc<Message>>,
    tool_cancel_tokens: std::sync::Mutex<HashMap<String, CancellationToken>>,
}

impl ConversationClient {
    /// Allocate the next unique message ID.
    pub(crate) fn next_msg_id(&self) -> MessageID {
        self.msg_id_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Read the current counter value (for snapshotting state).
    pub(crate) fn msg_id_counter_value(&self) -> i32 {
        self.msg_id_counter.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Cancel a specific tool call by its ID. Returns true if the tool was found and cancelled.
    pub fn cancel_tool(&self, tool_call_id: &str) -> bool {
        let tokens = self.tool_cancel_tokens.lock().unwrap();
        if let Some(token) = tokens.get(tool_call_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Register a cancellation token for a tool call. Returns the token for use during execution.
    pub(crate) fn register_tool_token(&self, tool_call_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let clone = token.clone();
        self.tool_cancel_tokens.lock().unwrap().insert(tool_call_id.to_string(), token);
        clone
    }

    /// Remove a tool's cancellation token after it completes.
    pub(crate) fn unregister_tool_token(&self, tool_call_id: &str) {
        self.tool_cancel_tokens.lock().unwrap().remove(tool_call_id);
    }

    /// Send a chat to the conversation. Returns after the message is queued. The message
    /// will be sent to the LLM in the background when the current LLM response finished.
    pub async fn send_chat(&self, content: &str) -> Result<()> {
        self.input_channel_tx.send(content.to_string()).await?;
        Ok(())
    }

    /// Used for conversation to notify a new message if available
    pub(crate) fn notify_msg(&self, msg: Message) -> Result<()> {
        let msg = Arc::new(msg);
        self.msgs.write()
            .map_err(|e| anyhow::anyhow!("failed to acquire msgs write lock: {e}"))?
            .push(Arc::clone(&msg));
        self.new_msg_notify_tx.send(msg)
            .map_err(|e| anyhow::anyhow!("failed to send msg to the notification broadcast: {e}"))?;
        Ok(())
    }

    /// Get a snapshot of all messages in the conversation.
    pub fn get_messages(&self) -> Vec<Arc<Message>> {
        self.msgs.read().unwrap().clone()
    }

    /// Subscribe to the conversation's messages.
    /// This will also send all the historical messages.
    /// If the consumer lagged too far behind, it will receive BroadcastStreamRecvError
    /// then the stream continues with normal messages.
    pub fn subscribe(&self) -> impl Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> + use<> {
        // TODO: handle error and return error in stream
        let msgs = self.msgs.read().unwrap();
        let tx = self.new_msg_notify_tx.subscribe();
        let stream = BroadcastStream::new(tx);
        tokio_stream::iter(msgs.clone().into_iter().map(Ok)).chain(stream)
    }

    /// Subscribe to only new messages (no history replay).
    /// Useful for continue_subagent to avoid reprocessing old messages.
    pub fn subscribe_new(&self) -> impl Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> + use<> {
        let tx = self.new_msg_notify_tx.subscribe();
        BroadcastStream::new(tx)
    }

    /// Create a test-only ConversationClient with dummy channels.
    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        let (input_tx, _input_rx) = mpsc::channel(10);
        let (notify_tx, _) = broadcast::channel(100);
        ConversationClient {
            msg_id_counter: AtomicI32::new(0),
            msgs: RwLock::new(Vec::new()),
            input_channel_tx: input_tx,
            new_msg_notify_tx: notify_tx,
            tool_cancel_tokens: std::sync::Mutex::new(HashMap::new()),
        }
    }
}
