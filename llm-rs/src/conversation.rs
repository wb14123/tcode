use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::llm::{ChatOptions, LLMEvent, LLMMessage, ModelInfo, StopReason, ToolCall, LLM};
use crate::tool::{CancellationToken, Tool, ToolContext};
use anyhow::{Context, Result};
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

fn build_system_prompt(subagent_depth: usize) -> String {
    let role = if subagent_depth == 0 {
        "You are a helpful assistant."
    } else {
        "You are a subagent spawned to perform a specific task."
    };
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to get current directory: {}", e);
            "unknown".to_string()
        });
    format!(
        "{role}\n\n{rules}\n\nCurrent directory: {cwd}\n\
         If you need the current date or time, use the `current_time` tool.",
        role = role,
        rules = SUBAGENT_RULES,
        cwd = cwd,
    )
}

/// Lightweight metadata written alongside conversation state for quick access
/// (e.g. session listing) without loading the full state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub description: Option<String>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub last_active_at: Option<u64>,
}

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
    let existing_result_ids: HashSet<&str> = llm_msgs[assistant_idx + 1..]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessageEndStatus {
    Succeeded,
    Failed,
    Cancelled,
    Timeout,
    UserDenied,
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

    /// Broadcast by subagent when the user types `/done` in its interactive edit window.
    /// Monitored by the parent's tool task to recover a cancelled subagent result.
    UserRequestEnd {
        msg_id: MessageID,
        conversation_id: String,
    },

    /// Sent by tool task through loop_tx when a cancelled subagent is recovered via `/done`.
    ToolCallResolved {
        msg_id: MessageID,
        tool_call_id: String,
        content: Arc<String>,
    },

    /// System-level message (info, warning, error)
    SystemMessage {
        msg_id: MessageID,
        created_at: u64,
        level: SystemMessageLevel,
        message: String,
    },

    /// Signal that permission state has changed. UI should re-query for full state.
    PermissionUpdated {
        msg_id: MessageID,
    },

    /// Signal that a tool is waiting for user permission approval.
    ToolRequestPermission {
        msg_id: MessageID,
        tool_call_id: String,
    },

    /// Signal that a previously requested permission was approved and the tool is resuming.
    ToolPermissionApproved {
        msg_id: MessageID,
        tool_call_id: String,
    },

    /// A subagent (or one of its descendants) is waiting for user permission.
    SubAgentWaitingPermission {
        msg_id: MessageID,
        conversation_id: String,
    },

    /// A subagent's pending permission was approved.
    SubAgentPermissionApproved {
        msg_id: MessageID,
        conversation_id: String,
    },

    /// A subagent's tool was denied by the user.
    SubAgentPermissionDenied {
        msg_id: MessageID,
        conversation_id: String,
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
    conversation_id: String,
    client: Arc<ConversationClient>,
    conversation_manager: Arc<ConversationManager>,
    tools: HashMap<String, Arc<Tool>>,
    chat_options: ChatOptions,
    subagent_max_iterations: usize,
    subagent_depth: usize,
    max_subagent_depth: usize,
    state_dir: Option<PathBuf>,
    /// Permission manager shared across all conversations.
    permission_manager: Arc<crate::permission::PermissionManager>,
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
) -> (HashMap<String, Arc<Tool>>, mpsc::Receiver<Message>, Arc<ConversationClient>) {
    llm.register_tools(tools.clone());
    let tools_map = tools.into_iter().map(|t| (t.name.clone(), t)).collect();
    let (input_tx, input_rx) = mpsc::channel(100);
    let (notify_tx, _) = broadcast::channel(100);
    let client = Arc::new(ConversationClient {
        msg_id_counter: AtomicI32::new(msg_id_start),
        msgs: RwLock::new(Vec::new()),
        input_channel_tx: input_tx,
        new_msg_notify_tx: notify_tx,
        tool_cancel_tokens: std::sync::Mutex::new(HashMap::new()),
        cancel_token: std::sync::Mutex::new(CancellationToken::new()),
        children: std::sync::Mutex::new(HashMap::new()),
    });
    (tools_map, input_rx, client)
}

// ============================================================================
// ConversationManager
// ============================================================================

pub struct ConversationManager {
    conversations: RwLock<HashMap<String, (Arc<ConversationClient>, AbortHandle)>>,
    /// Maps subagent_conv_id → (parent_conv_id, tool_call_id).
    /// Used by the server to route `/done` recovery to the correct parent.
    subagent_parents: std::sync::Mutex<HashMap<String, (String, String)>>,
    /// Permission manager shared across all conversations.
    permission_manager: Arc<crate::permission::PermissionManager>,
}

/// Manages conversations so that any new client can attach to an existing conversation.
impl ConversationManager {
    pub fn new(permissions_path: PathBuf) -> Arc<Self> {
        let permission_manager = Arc::new(crate::permission::PermissionManager::new(permissions_path));
        Arc::new(Self {
            conversations: RwLock::new(HashMap::new()),
            subagent_parents: std::sync::Mutex::new(HashMap::new()),
            permission_manager,
        })
    }

    /// Get the permission manager.
    pub fn permission_manager(&self) -> &Arc<crate::permission::PermissionManager> {
        &self.permission_manager
    }

    /// Create a new conversation. The new conversation will be kept in the manager's
    /// memory until it ends.
    ///
    /// Returns `(conversation_id, client)`.
    pub fn new_conversation(
        self: &Arc<Self>,
        llm: Box<dyn LLM>,
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
        let system_prompt = build_system_prompt(subagent_depth);
        let llm_msgs = vec![LLMMessage::System(system_prompt)];
        let conversation = Conversation {
            id: conversation_id.clone(),
            llm,
            model: model.to_string(),
            llm_msgs,
            input_channel_rx: input_rx,
            total_input_tokens: 0,
            total_output_tokens: 0,
            single_turn,
            pending_tools: HashSet::new(),
            cancelled_tools: HashSet::new(),
            accumulated_tool_content: HashMap::new(),
            llm_calls: 0,
            description: None,
            created_at: Some(now_millis()),
            env: ConversationEnv {
                conversation_id: conversation_id.clone(),
                client,
                conversation_manager: Arc::clone(self),
                tools: tools_map,
                chat_options,
                subagent_max_iterations,
                subagent_depth,
                max_subagent_depth,
                state_dir,
                permission_manager: Arc::clone(&self.permission_manager),
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

    /// Register a subagent → parent mapping for `/done` recovery.
    pub fn register_subagent_parent(&self, subagent_conv_id: &str, parent_conv_id: &str, tool_call_id: &str) {
        self.subagent_parents.lock().unwrap()
            .insert(subagent_conv_id.to_string(), (parent_conv_id.to_string(), tool_call_id.to_string()));
    }

    /// Look up the parent conversation and tool_call_id for a subagent.
    pub fn get_subagent_parent(&self, subagent_conv_id: &str) -> Option<(String, String)> {
        self.subagent_parents.lock().unwrap().get(subagent_conv_id).cloned()
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

        // Load session metadata (description, created_at) from session-meta.json
        let meta = state_dir.as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join("session-meta.json")).ok())
            .and_then(|json| serde_json::from_str::<SessionMeta>(&json).ok());

        // Back-fill description from first user message for old sessions without metadata
        let description = meta.as_ref().and_then(|m| m.description.clone()).or_else(|| {
            state.llm_msgs.iter().find_map(|msg| {
                if let LLMMessage::User(text) = msg {
                    Some(truncate_preview(text, 80))
                } else {
                    None
                }
            })
        });
        let created_at = meta.and_then(|m| m.created_at).or(Some(now_millis()));

        let (tools_map, input_rx, client) = prepare_conversation(&mut *llm, tools, state.msg_id_counter);
        let conv_id = state.id.clone();
        let conversation = Conversation {
            id: state.id.clone(),
            llm,
            model: state.model,
            llm_msgs: state.llm_msgs,
            input_channel_rx: input_rx,
            total_input_tokens: state.total_input_tokens,
            total_output_tokens: state.total_output_tokens,
            single_turn: state.single_turn,
            pending_tools: HashSet::new(),
            cancelled_tools: HashSet::new(),
            accumulated_tool_content: HashMap::new(),
            llm_calls: 0,
            description,
            created_at,
            env: ConversationEnv {
                conversation_id: conv_id,
                client,
                conversation_manager: Arc::clone(self),
                tools: tools_map,
                chat_options: state.chat_options,
                subagent_max_iterations,
                subagent_depth: state.subagent_depth,
                max_subagent_depth,
                state_dir,
                permission_manager: Arc::clone(&self.permission_manager),
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

        // Reconstruct parent mapping from all conversation states' llm_msgs
        {
            let mut all_states: Vec<(&str, &[LLMMessage])> = vec![(&state.id, &state.llm_msgs)];
            for (_, sa_state) in &subagent_states {
                all_states.push((&sa_state.id, &sa_state.llm_msgs));
            }
            for (sa_dir, _) in &subagent_states {
                let sa_conv_id = sa_dir.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix("subagent-"));
                if let Some(sa_conv_id) = sa_conv_id {
                    for &(parent_id, llm_msgs) in &all_states {
                        if let Some(tool_call_id) = find_tool_call_for_subagent(llm_msgs, sa_conv_id) {
                            self.register_subagent_parent(sa_conv_id, parent_id, &tool_call_id);
                            break;
                        }
                    }
                }
            }
        }

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

/// Find the tool_call_id in `llm_msgs` that produced a ToolResult for the given subagent.
fn find_tool_call_for_subagent(llm_msgs: &[LLMMessage], subagent_conv_id: &str) -> Option<String> {
    let prefix = format!("[subagent_id: {}]", subagent_conv_id);
    for msg in llm_msgs {
        if let LLMMessage::ToolResult { tool_call_id, content } = msg {
            if content.starts_with(&prefix) {
                return Some(tool_call_id.clone());
            }
        }
    }
    None
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

/// Collect a subagent's first-turn response and publish results to the parent via `loop_tx`.
/// `/done` recovery after cancellation is handled by the server's `UserRequestEnd` handler.
async fn collect_subagent_response(
    sub_stream: &mut (impl Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> + Unpin),
    cancel_token: &CancellationToken,
    subagent_client: &ConversationClient,
    parent_client: &Arc<ConversationClient>,
    subagent_conv_id: &str,
    tool_call_id: &str,
    loop_tx: &mpsc::Sender<Message>,
) -> Result<()> {
    let mut resp = SubagentResponse {
        text: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        end_status: MessageEndStatus::Succeeded,
    };
    let mut cancel_sent = false;

    loop {
        let msg = tokio::select! {
            biased;
            _ = cancel_token.cancelled(), if !cancel_sent => {
                subagent_client.cancel();
                cancel_sent = true;
                continue;
            }
            result = sub_stream.next() => {
                match result {
                    Some(Ok(msg)) => msg,
                    Some(Err(_)) => continue,
                    None => break,
                }
            }
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
            Message::AssistantMessageEnd {
                end_status: MessageEndStatus::Cancelled, ..
            } => {
                resp.end_status = MessageEndStatus::Cancelled;
            }
            Message::AssistantRequestEnd { total_input_tokens, total_output_tokens } => {
                resp.input_tokens = *total_input_tokens;
                resp.output_tokens = *total_output_tokens;

                // Publish first-turn result to parent
                let text = match Conversation::broadcast_subagent_turn_end(
                    parent_client, subagent_conv_id, &resp,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to broadcast SubAgentTurnEnd");
                        format_subagent_result(subagent_conv_id, &resp.text, &resp.end_status)
                    }
                };
                let cancelled = cancel_token.is_cancelled()
                    || resp.end_status == MessageEndStatus::Cancelled;
                loop_tx.send(Message::ToolOutputChunk {
                    msg_id: 0,
                    tool_call_id: tool_call_id.to_string(),
                    tool_name: "subagent".to_string(),
                    content: Arc::new(text),
                }).await
                    .context("failed to send ToolOutputChunk")?;
                loop_tx.send(Message::ToolMessageEnd {
                    msg_id: 0,
                    tool_call_id: tool_call_id.to_string(),
                    end_status: if cancelled { MessageEndStatus::Cancelled } else { MessageEndStatus::Succeeded },
                    input_tokens: 0,
                    output_tokens: 0,
                }).await
                    .context("failed to send ToolMessageEnd")?;

                break; // First turn done — exit regardless of cancel status
            }
            // Tool in subagent requests permission → bubble SubAgent status to parent
            Message::ToolRequestPermission { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentWaitingPermission {
                    msg_id: parent_client.next_msg_id(),
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to send SubAgentWaitingPermission to parent");
                }
            }
            // Tool permission approved → bubble up
            Message::ToolPermissionApproved { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionApproved {
                    msg_id: parent_client.next_msg_id(),
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to send SubAgentPermissionApproved to parent");
                }
            }
            // Tool denied → bubble up
            Message::ToolMessageEnd { end_status: MessageEndStatus::UserDenied, .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionDenied {
                    msg_id: parent_client.next_msg_id(),
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to send SubAgentPermissionDenied to parent");
                }
            }
            // Forward permission signals to parent so the UI sees them
            Message::PermissionUpdated { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::PermissionUpdated {
                    msg_id: parent_client.next_msg_id(),
                }) {
                    tracing::error!(error = %e, "failed to forward PermissionUpdated to parent");
                }
            }
            // Recursive bubble-up from nested subagents: re-emit with THIS subagent's conversation_id
            Message::SubAgentWaitingPermission { .. } => {
                // Also forward PermissionUpdated so the permission UI works at all ancestor levels
                if let Err(e) = parent_client.notify_msg(Message::PermissionUpdated {
                    msg_id: parent_client.next_msg_id(),
                }) {
                    tracing::error!(error = %e, "failed to forward PermissionUpdated to parent");
                }
                if let Err(e) = parent_client.notify_msg(Message::SubAgentWaitingPermission {
                    msg_id: parent_client.next_msg_id(),
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to re-emit SubAgentWaitingPermission to parent");
                }
            }
            Message::SubAgentPermissionApproved { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionApproved {
                    msg_id: parent_client.next_msg_id(),
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to re-emit SubAgentPermissionApproved to parent");
                }
            }
            Message::SubAgentPermissionDenied { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionDenied {
                    msg_id: parent_client.next_msg_id(),
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to re-emit SubAgentPermissionDenied to parent");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Format a subagent result with the conversation ID prefix.
pub fn format_subagent_result(conversation_id: &str, text: &str, end_status: &MessageEndStatus) -> String {
    if matches!(end_status, MessageEndStatus::Cancelled) {
        format!(
            "[subagent_id: {}]\nSubagent was cancelled by the user. \
             Do not retry or continue this subagent unless the user explicitly asks.",
            conversation_id
        )
    } else if text.is_empty() {
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

    /// Event loop receiver. Receives user messages and tool completion signals.
    input_channel_rx: mpsc::Receiver<Message>,

    /// Accumulated token usage.
    total_input_tokens: i32,
    total_output_tokens: i32,

    /// When true, the conversation exits after one user message + LLM response cycle.
    single_turn: bool,

    /// Outstanding tool_call_ids waiting for completion.
    pending_tools: HashSet<String>,

    /// Tool_call_ids that completed with `Cancelled` status in the current turn.
    /// When all pending tools finish and any were cancelled, the LLM is NOT called
    /// automatically — instead a SystemMessage is broadcast and the turn pauses.
    cancelled_tools: HashSet<String>,

    /// Accumulated tool output per tool_call_id (chunks joined).
    accumulated_tool_content: HashMap<String, String>,

    /// Number of LLM calls since the last user message (for subagent max_iterations).
    llm_calls: usize,

    /// Truncated first user input used as session description.
    description: Option<String>,

    /// Timestamp (millis since epoch) when the conversation was created.
    created_at: Option<u64>,

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

            // Write lightweight session-meta.json for quick access (e.g. session listing)
            let meta = SessionMeta {
                description: self.description.clone(),
                created_at: self.created_at,
                last_active_at: Some(now_millis()),
            };
            let meta_json = serde_json::to_string_pretty(&meta)?;
            let meta_tmp = dir.join("session-meta.json.tmp");
            let meta_target = dir.join("session-meta.json");
            std::fs::write(&meta_tmp, &meta_json)?;
            std::fs::rename(&meta_tmp, &meta_target)?;
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
        let result_text = format_subagent_result(conversation_id, &response.text, &response.end_status);

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

    /// Single event loop for the conversation. Should only be called once.
    ///
    /// Receives user messages and tool completion signals through the same channel.
    /// Tool tasks are fire-and-forget, sending results back through `input_channel_tx`.
    /// When a user sends a message while tools run, cancel tokens fire, partial results
    /// are accumulated, remaining tools get synthetic cancelled results, and the LLM is
    /// called with the new user message.
    async fn start(&mut self) -> Result<()> {
        loop {
            let cancel_token = self.env.client.current_cancel_token();
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    if self.pending_tools.is_empty() {
                        // Cancelled while idle — reset token and continue waiting
                        self.env.client.reset_cancel_token();
                        continue;
                    }
                    // Cancelled with pending tools (external cancel, not user message)
                    self.fill_remaining_cancelled(false)?;
                    self.cancelled_tools.clear();
                    self.broadcast_msg(Message::AssistantMessageEnd {
                        msg_id: self.next_msg_id(),
                        end_status: MessageEndStatus::Cancelled,
                        error: None,
                        input_tokens: 0,
                        output_tokens: 0,
                        reasoning_tokens: 0,
                    })?;
                    self.finish_turn()?;
                }
                msg = self.input_channel_rx.recv() => {
                    let Some(msg) = msg else { break };
                    match msg {
                        Message::UserMessage { content, .. } => {
                            // If tools are pending, cancel them and fill synthetic results
                            if !self.pending_tools.is_empty() {
                                self.env.client.cancel_silent();
                                self.fill_remaining_cancelled(true)?;
                                self.env.client.reset_cancel_token();
                            }
                            self.cancelled_tools.clear();
                            self.broadcast_msg(Message::UserMessage {
                                msg_id: self.next_msg_id(),
                                created_at: now_millis(),
                                content: Arc::clone(&content),
                            })?;
                            if self.description.is_none() {
                                self.description = Some(truncate_preview(&content, 80));
                            }
                            self.push_llm_msg(LLMMessage::User(content.to_string()))?;
                            self.llm_calls = 0;
                            self.call_llm().await?;
                            self.maybe_finish_turn()?;
                        }
                        Message::ToolOutputChunk { tool_call_id, content, .. } => {
                            if self.pending_tools.contains(&tool_call_id) {
                                self.accumulated_tool_content
                                    .entry(tool_call_id).or_default()
                                    .push_str(&content);
                            }
                            // else: stale message from cancelled tool, ignore
                        }
                        Message::ToolMessageEnd { tool_call_id, end_status, .. } => {
                            if self.pending_tools.remove(&tool_call_id) {
                                if end_status == MessageEndStatus::Cancelled {
                                    self.cancelled_tools.insert(tool_call_id.clone());
                                }
                                let raw_content = self.accumulated_tool_content
                                    .remove(&tool_call_id).unwrap_or_default();
                                let content = if end_status == MessageEndStatus::UserDenied {
                                    format!(
                                        "The user denied permission for this tool call. This is not a technical error — \
                                         the human operator chose not to allow this action. Do not retry this tool call. \
                                         Instead, ask the user what they would like to do.\n\
                                         Original tool output: {}",
                                        raw_content
                                    )
                                } else {
                                    raw_content
                                };
                                self.push_llm_msg(LLMMessage::ToolResult {
                                    tool_call_id,
                                    content,
                                })?;
                                if self.pending_tools.is_empty() {
                                    if self.cancelled_tools.is_empty() {
                                        self.call_llm().await?;
                                        self.maybe_finish_turn()?;
                                    } else {
                                        // Some tools were cancelled — pause and let the user decide
                                        self.cancelled_tools.clear();
                                        log_and_broadcast_system_message(
                                            &self.env.client,
                                            SystemMessageLevel::Info,
                                            "Some tools/subagents were cancelled. Send a new message to continue the conversation.".to_string(),
                                        );
                                        self.maybe_finish_turn()?;
                                    }
                                }
                            }
                            // else: stale message from cancelled tool, ignore
                        }
                        Message::ToolCallResolved { tool_call_id, content, .. } => {
                            // A cancelled subagent was recovered via /done — replace
                            // the cancelled ToolResult in llm_msgs and re-call the LLM.
                            let mut found = false;
                            for msg in self.llm_msgs.iter_mut().rev() {
                                if let LLMMessage::ToolResult { tool_call_id: id, content: c } = msg {
                                    if *id == tool_call_id {
                                        *c = content.to_string();
                                        found = true;
                                        break;
                                    }
                                }
                            }
                            if found {
                                self.llm_calls = 0;
                                self.save_state()?;
                                self.call_llm().await?;
                                self.maybe_finish_turn()?;
                            }
                            // If not found (e.g. parent moved on), silently ignore
                        }
                        other => {
                            tracing::error!("unexpected message type in event loop: {:?}", other);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Call the LLM if allowed by max_iterations and not cancelled.
    /// Streams the response, handles tool spawning and cancellation internally.
    async fn call_llm(&mut self) -> Result<()> {
        let max = if self.single_turn { self.env.subagent_max_iterations } else { usize::MAX };
        if self.llm_calls >= max {
            log_and_broadcast_system_message(
                &self.env.client,
                SystemMessageLevel::Warning,
                format!("Subagent reached maximum iterations limit ({})", max),
            );
            return Ok(());
        }
        let cancel_token = self.env.client.current_cancel_token();
        if cancel_token.is_cancelled() { return Ok(()); }

        let mut response_stream =
            self.llm
                .chat(self.model.as_str(), &self.llm_msgs, &self.env.chat_options);
        let mut accumulated_text = String::new();
        let mut pending_tool_calls = Vec::new();

        self.broadcast_msg(Message::AssistantMessageStart {
            msg_id: self.next_msg_id(),
            created_at: now_millis(),
        })?;

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    self.broadcast_msg(Message::AssistantMessageEnd {
                        msg_id: self.next_msg_id(),
                        end_status: MessageEndStatus::Cancelled,
                        error: None,
                        input_tokens: 0,
                        output_tokens: 0,
                        reasoning_tokens: 0,
                    })?;
                    self.llm_calls += 1;
                    return Ok(());
                }
                event = response_stream.next() => {
                    match event {
                        Some(e) => e,
                        None => { self.llm_calls += 1; return Ok(()); }
                    }
                }
            };

            match event {
                LLMEvent::MessageStart { input_tokens } => {
                    self.total_input_tokens += input_tokens;
                }
                LLMEvent::TextDelta(text) => {
                    accumulated_text.push_str(&text);
                    self.broadcast_msg(Message::AssistantMessageChunk {
                        msg_id: self.next_msg_id(),
                        content: Arc::new(text),
                    })?;
                }
                LLMEvent::ThinkingDelta(text) => {
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

                    if stop_reason == StopReason::ToolUse && !pending_tool_calls.is_empty() {
                        let tool_calls = std::mem::take(&mut pending_tool_calls);
                        self.push_llm_msg(LLMMessage::Assistant {
                            content: accumulated_text,
                            tool_calls: tool_calls.clone(),
                            raw,
                        })?;
                        self.spawn_tool_tasks(tool_calls);
                    } else if !accumulated_text.is_empty() || raw.is_some() {
                        self.push_llm_msg(LLMMessage::Assistant {
                            content: accumulated_text,
                            tool_calls: vec![],
                            raw,
                        })?;
                    }
                    self.llm_calls += 1;
                    return Ok(());
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
                    self.llm_calls += 1;
                    return Ok(());
                }
            }
        }
    }

    /// Finish a turn if no tools are pending (normal path).
    fn maybe_finish_turn(&mut self) -> Result<()> {
        if !self.pending_tools.is_empty() { return Ok(()); }
        self.finish_turn()
    }

    /// Force finish a turn (called from cancel path).
    fn finish_turn(&mut self) -> Result<()> {
        if self.single_turn {
            self.broadcast_msg(Message::AssistantRequestEnd {
                total_input_tokens: self.total_input_tokens,
                total_output_tokens: self.total_output_tokens,
            })?;
        }
        self.env.client.reset_cancel_token();
        Ok(())
    }

    /// Fill synthetic results for all pending tools. No waiting.
    fn fill_remaining_cancelled(&mut self, user_interrupted: bool) -> Result<()> {
        for id in std::mem::take(&mut self.pending_tools) {
            let raw = self.accumulated_tool_content.remove(&id).unwrap_or_default();
            let content = if user_interrupted {
                if raw.is_empty() {
                    "Tool execution was interrupted because the user sent a new message.".into()
                } else {
                    format!(
                        "Tool execution was interrupted because the user sent \
                         a new message. Partial result:\n{}", raw
                    )
                }
            } else if raw.is_empty() {
                "Tool call was cancelled due to conversation interruption.".into()
            } else {
                format!("Tool call was cancelled. Partial result:\n{}", raw)
            };
            self.push_llm_msg(LLMMessage::ToolResult {
                tool_call_id: id,
                content,
            })?;
        }
        Ok(())
    }

    /// Spawn fire-and-forget tasks for each tool call. Cancel tokens are created
    /// here (before any reset) so they're children of the current conversation token.
    fn spawn_tool_tasks(&mut self, tool_calls: Vec<ToolCall>) {
        let loop_tx = self.env.client.input_channel_tx.clone();

        for tool_call in tool_calls {
            self.pending_tools.insert(tool_call.id.clone());
            let cancel_token = self.env.client.register_tool_token(&tool_call.id);
            let env = self.env.clone();
            let tx = loop_tx.clone();

            let client = env.client.clone();
            match tool_call.name.as_str() {
                "subagent" => {
                    let llm = self.llm.clone_box();
                    spawn_tool_task(client, async move {
                        execute_subagent(tool_call, env, llm, tx, cancel_token).await
                    });
                }
                "continue_subagent" => {
                    spawn_tool_task(client, async move {
                        execute_continue_subagent(tool_call, env, tx, cancel_token).await
                    });
                }
                _ => {
                    spawn_tool_task(client, async move {
                        execute_regular_tool(tool_call, env, tx, cancel_token).await
                    });
                }
            }
        }
    }
}

/// Spawn a tool task future, logging and broadcasting any error it returns.
fn spawn_tool_task(
    client: Arc<ConversationClient>,
    fut: impl Future<Output = Result<()>> + Send + 'static,
) {
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            let message = format!("Tool task failed: {}", e);
            tracing::error!(%message);
            if let Err(e2) = client.notify_msg(Message::SystemMessage {
                msg_id: client.next_msg_id(),
                created_at: now_millis(),
                level: SystemMessageLevel::Error,
                message,
            }) {
                tracing::error!(error = %e2, "failed to broadcast tool task error");
            }
        }
    });
}

/// Drop guard that sends a `ToolMessageEnd(Failed)` through the event loop channel
/// if the tool task panics. Safety net so the main loop never gets stuck waiting.
struct ToolCompleteGuard {
    tool_call_id: String,
    loop_tx: mpsc::Sender<Message>,
    defused: bool,
}

impl ToolCompleteGuard {
    fn new(tool_call_id: String, loop_tx: mpsc::Sender<Message>) -> Self {
        Self { tool_call_id, loop_tx, defused: false }
    }
    fn defuse(&mut self) { self.defused = true; }
}

impl Drop for ToolCompleteGuard {
    fn drop(&mut self) {
        if !self.defused {
            // Best-effort send — the channel is bounded so we use try_send.
            if let Err(e) = self.loop_tx.try_send(Message::ToolMessageEnd {
                msg_id: 0,
                tool_call_id: self.tool_call_id.clone(),
                end_status: MessageEndStatus::Failed,
                input_tokens: 0,
                output_tokens: 0,
            }) {
                tracing::error!(tool_call_id = %self.tool_call_id, error = %e,
                    "ToolCompleteGuard: failed to send ToolMessageEnd on panic");
            }
        }
    }
}

/// Execute a regular (non-subagent) tool call as a standalone async function.
/// Sends results through `loop_tx` for the main event loop.
async fn execute_regular_tool(
    tool_call: ToolCall,
    env: ConversationEnv,
    loop_tx: mpsc::Sender<Message>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let mut guard = ToolCompleteGuard::new(tool_call.id.clone(), loop_tx.clone());

    let tool_arc = env.tools.get(&tool_call.name).cloned();

    tracing::info!(
        tool_call_id = %tool_call.id,
        tool_name = %tool_call.name,
        args = %tool_call.arguments,
        "executing tool call"
    );

    env.client.notify_msg(Message::ToolMessageStart {
        msg_id: env.client.next_msg_id(),
        tool_call_id: tool_call.id.clone(),
        created_at: now_millis(),
        tool_name: tool_call.name.clone(),
        tool_args: tool_call.arguments.clone(),
    })?;

    let client_clone = Arc::clone(&env.client);
    let tc_id = tool_call.id.clone();
    let client_clone2 = Arc::clone(&env.client);
    let tc_id2 = tool_call.id.clone();
    let scoped_pm = crate::permission::ScopedPermissionManager::new(
        &tool_call.name,
        Arc::clone(&env.permission_manager),
        Arc::new(move || {
            if let Err(e) = client_clone.notify_msg(Message::ToolRequestPermission {
                msg_id: client_clone.next_msg_id(),
                tool_call_id: tc_id.clone(),
            }) {
                tracing::error!(error = %e, "failed to send ToolRequestPermission");
            }
            if let Err(e) = client_clone.notify_msg(Message::PermissionUpdated {
                msg_id: client_clone.next_msg_id(),
            }) {
                tracing::error!(error = %e, "failed to send PermissionUpdated");
            }
        }),
        Arc::new(move || {
            if let Err(e) = client_clone2.notify_msg(Message::ToolPermissionApproved {
                msg_id: client_clone2.next_msg_id(),
                tool_call_id: tc_id2.clone(),
            }) {
                tracing::error!(error = %e, "failed to send ToolPermissionApproved");
            }
        }),
        env.state_dir.clone(),
    );
    let scoped_pm_ref = scoped_pm.clone();
    let tool_ctx = ToolContext {
        cancel_token: cancel_token.clone(),
        permission: scoped_pm,
    };
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
        let status = if cancel_token.is_cancelled() {
            MessageEndStatus::Cancelled
        } else if scoped_pm_ref.was_denied() {
            MessageEndStatus::UserDenied
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

    // Send full result to main event loop
    loop_tx.send(Message::ToolOutputChunk {
        msg_id: 0,
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        content: Arc::new(tool_result),
    }).await?;

    // Broadcast ToolMessageEnd for UI
    env.client.notify_msg(Message::ToolMessageEnd {
        msg_id: env.client.next_msg_id(),
        tool_call_id: tool_call.id.clone(),
        end_status: end_status.clone(),
        input_tokens: 0,
        output_tokens: 0,
    })?;

    // Send ToolMessageEnd to event loop
    loop_tx.send(Message::ToolMessageEnd {
        msg_id: 0,
        tool_call_id: tool_call.id.clone(),
        end_status,
        input_tokens: 0,
        output_tokens: 0,
    }).await?;

    guard.defuse();
    Ok(())
}

/// Spawn a background task that monitors the subagent stream: collects the first-turn
/// response, publishes results to the parent, and — if cancelled — keeps watching for
/// `UserRequestEnd` (the user typing `/done`) to recover the subagent result.
fn spawn_subagent_stream_handler(
    mut sub_stream: impl Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> + Unpin + Send + 'static,
    cancel_token: CancellationToken,
    subagent_client: Arc<ConversationClient>,
    parent_client: Arc<ConversationClient>,
    subagent_conv_id: String,
    tool_call_id: String,
    loop_tx: mpsc::Sender<Message>,
) {
    tokio::spawn(async move {
        let mut guard = ToolCompleteGuard::new(tool_call_id.clone(), loop_tx.clone());
        if let Err(e) = collect_subagent_response(
            &mut sub_stream, &cancel_token, &subagent_client,
            &parent_client, &subagent_conv_id, &tool_call_id, &loop_tx,
        ).await {
            tracing::error!(error = %e, "subagent stream handler failed");
        }
        parent_client.unregister_tool_token(&tool_call_id);
        guard.defuse();
    });
}

/// Execute a subagent tool call. Sets up the subagent conversation, sends the task,
/// and spawns a stream handler to monitor results (including post-cancel recovery).
async fn execute_subagent(
    tool_call: ToolCall,
    env: ConversationEnv,
    llm: Box<dyn LLM>,
    loop_tx: mpsc::Sender<Message>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let params: SubAgentParams = match serde_json::from_str(&tool_call.arguments) {
        Ok(p) => p,
        Err(e) => {
            let error = format!("Error: Failed to parse subagent arguments: {}", e);
            loop_tx.send(Message::ToolOutputChunk {
                msg_id: 0, tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(), content: Arc::new(error),
            }).await?;
            loop_tx.send(Message::ToolMessageEnd {
                msg_id: 0, tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
            }).await?;
            return Ok(());
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
    let subagent_state_dir = match env.state_dir.as_ref().map(|d| {
        let dir = d.join(format!("subagent-{}", subagent_conv_id_pre));
        std::fs::create_dir_all(&dir)?;
        Ok::<_, anyhow::Error>(dir)
    }).transpose() {
        Ok(d) => d,
        Err(e) => {
            let error = format!("Error: Failed to create subagent state dir: {}", e);
            loop_tx.send(Message::ToolOutputChunk {
                msg_id: 0, tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(), content: Arc::new(error),
            }).await?;
            loop_tx.send(Message::ToolMessageEnd {
                msg_id: 0, tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
            }).await?;
            return Ok(());
        }
    };

    // Create the subagent conversation
    let (subagent_conv_id, subagent_client) = match env.conversation_manager.new_conversation_with_id(
        subagent_conv_id_pre,
        llm,
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
            let error = format!("Error: Failed to create subagent conversation: {}", e);
            loop_tx.send(Message::ToolOutputChunk {
                msg_id: 0, tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(), content: Arc::new(error),
            }).await?;
            loop_tx.send(Message::ToolMessageEnd {
                msg_id: 0, tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
            }).await?;
            return Ok(());
        }
    };

    // Register subagent as a child for cascading cancellation
    env.client.register_child(subagent_conv_id.clone(), Arc::clone(&subagent_client));

    // Register parent mapping for /done recovery
    env.conversation_manager.register_subagent_parent(
        &subagent_conv_id, &env.conversation_id, &tool_call.id,
    );

    let task_preview = truncate_preview(&params.task, 100);
    env.client.notify_msg(Message::SubAgentStart {
        msg_id: env.client.next_msg_id(),
        conversation_id: subagent_conv_id.clone(),
        description: task_preview,
    }).context("failed to broadcast SubAgentStart")?;

    let sub_stream = subagent_client.subscribe();

    if let Err(e) = subagent_client.send_chat(&params.task).await {
        let error = format!("Error: Failed to send task to subagent: {}", e);
        env.client.notify_msg(Message::SubAgentEnd {
            msg_id: env.client.next_msg_id(),
            conversation_id: subagent_conv_id.clone(),
            end_status: MessageEndStatus::Failed,
            response: Arc::new(error.clone()),
            input_tokens: 0,
            output_tokens: 0,
        }).context("failed to broadcast SubAgentEnd")?;
        loop_tx.send(Message::ToolOutputChunk {
            msg_id: 0, tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(), content: Arc::new(error),
        }).await?;
        loop_tx.send(Message::ToolMessageEnd {
            msg_id: 0, tool_call_id: tool_call.id,
            end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
        }).await?;
        return Ok(());
    }

    spawn_subagent_stream_handler(
        sub_stream, cancel_token, subagent_client,
        env.client, subagent_conv_id, tool_call.id, loop_tx,
    );
    Ok(())
}

/// Execute continue_subagent tool call. Resumes an existing subagent conversation
/// and spawns a stream handler to monitor results (including post-cancel recovery).
async fn execute_continue_subagent(
    tool_call: ToolCall,
    env: ConversationEnv,
    loop_tx: mpsc::Sender<Message>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let params: ContinueSubAgentParams = match serde_json::from_str(&tool_call.arguments) {
        Ok(p) => p,
        Err(e) => {
            let error = format!("Error: Failed to parse continue_subagent arguments: {}", e);
            loop_tx.send(Message::ToolOutputChunk {
                msg_id: 0, tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(), content: Arc::new(error),
            }).await?;
            loop_tx.send(Message::ToolMessageEnd {
                msg_id: 0, tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
            }).await?;
            return Ok(());
        }
    };

    let subagent_client = match env.conversation_manager.get_conversation(&params.conversation_id) {
        Ok(Some(client)) => client,
        Ok(None) => {
            let error = format!("Error: Subagent conversation '{}' not found", params.conversation_id);
            loop_tx.send(Message::ToolOutputChunk {
                msg_id: 0, tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(), content: Arc::new(error),
            }).await?;
            loop_tx.send(Message::ToolMessageEnd {
                msg_id: 0, tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
            }).await?;
            return Ok(());
        }
        Err(e) => {
            let error = format!("Error: Failed to get subagent conversation: {}", e);
            loop_tx.send(Message::ToolOutputChunk {
                msg_id: 0, tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(), content: Arc::new(error),
            }).await?;
            loop_tx.send(Message::ToolMessageEnd {
                msg_id: 0, tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
            }).await?;
            return Ok(());
        }
    };

    // Register subagent as a child for cascading cancellation (idempotent via HashMap)
    env.client.register_child(params.conversation_id.clone(), Arc::clone(&subagent_client));

    // Register parent mapping for /done recovery (idempotent)
    env.conversation_manager.register_subagent_parent(
        &params.conversation_id, &env.conversation_id, &tool_call.id,
    );

    let msg_preview = truncate_preview(&params.message, 100);

    env.client.notify_msg(Message::SubAgentContinue {
        msg_id: env.client.next_msg_id(),
        conversation_id: params.conversation_id.clone(),
        description: msg_preview,
    }).context("failed to broadcast SubAgentContinue")?;

    let sub_stream = subagent_client.subscribe_new();

    if let Err(e) = subagent_client.send_chat(&params.message).await {
        let error = format!("Error: Failed to send follow-up to subagent: {}", e);
        env.client.notify_msg(Message::SubAgentTurnEnd {
            msg_id: env.client.next_msg_id(),
            conversation_id: params.conversation_id,
            end_status: MessageEndStatus::Failed,
            response: Arc::new(error.clone()),
            input_tokens: 0,
            output_tokens: 0,
        }).context("failed to broadcast SubAgentTurnEnd")?;
        loop_tx.send(Message::ToolOutputChunk {
            msg_id: 0, tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(), content: Arc::new(error),
        }).await?;
        loop_tx.send(Message::ToolMessageEnd {
            msg_id: 0, tool_call_id: tool_call.id,
            end_status: MessageEndStatus::Failed, input_tokens: 0, output_tokens: 0,
        }).await?;
        return Ok(());
    }

    spawn_subagent_stream_handler(
        sub_stream, cancel_token, subagent_client,
        env.client, params.conversation_id, tool_call.id, loop_tx,
    );
    Ok(())
}


/// Use for the client to send chat messages and subscribe to the conversation's messages.
pub struct ConversationClient {
    msg_id_counter: AtomicI32,
    msgs: RwLock<Vec<Arc<Message>>>,
    input_channel_tx: mpsc::Sender<Message>,
    new_msg_notify_tx: broadcast::Sender<Arc<Message>>,
    tool_cancel_tokens: std::sync::Mutex<HashMap<String, CancellationToken>>,
    /// Conversation-level cancellation token. Cancelling this cancels all child tool tokens.
    cancel_token: std::sync::Mutex<CancellationToken>,
    /// Child subagent clients, keyed by conversation_id. Used for cascading cancellation.
    children: std::sync::Mutex<HashMap<String, Arc<ConversationClient>>>,
}

impl ConversationClient {
    /// Allocate the next unique message ID.
    pub fn next_msg_id(&self) -> MessageID {
        self.msg_id_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Read the current counter value (for snapshotting state).
    pub(crate) fn msg_id_counter_value(&self) -> i32 {
        self.msg_id_counter.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Cancel the entire conversation: cancels the conversation-level token (which cascades
    /// to all child tool tokens), recursively cancels all child subagent conversations,
    /// and broadcasts a system warning.
    pub fn cancel(&self) {
        self.cancel_silent();

        // Broadcast a system message so subscribers know
        log_and_broadcast_system_message(
            self,
            SystemMessageLevel::Warning,
            "Conversation cancelled".to_string(),
        );
    }

    /// Cancel the conversation token and all children, without broadcasting a system message.
    /// Used internally when a user sends a new message while tools are running.
    pub(crate) fn cancel_silent(&self) {
        // Cancel our token (idempotent — safe to call multiple times)
        self.cancel_token.lock().unwrap().cancel();

        // Recursively cancel all child subagent conversations
        let children = self.children.lock().unwrap();
        for child in children.values() {
            child.cancel_silent();
        }
    }

    /// Register a child subagent client for cascading cancellation.
    pub fn register_child(&self, conversation_id: String, client: Arc<ConversationClient>) {
        self.children.lock().unwrap().insert(conversation_id, client);
    }


    /// Get a clone of the current cancel token for use in `tokio::select!`.
    pub(crate) fn current_cancel_token(&self) -> CancellationToken {
        self.cancel_token.lock().unwrap().clone()
    }

    /// Replace the cancel token with a fresh one so the conversation can accept new work.
    pub(crate) fn reset_cancel_token(&self) {
        *self.cancel_token.lock().unwrap() = CancellationToken::new();
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

    /// Register a cancellation token for a tool call. The token is a child of the
    /// conversation-level cancel token, so cancelling the conversation cancels all tools.
    pub(crate) fn register_tool_token(&self, tool_call_id: &str) -> CancellationToken {
        let token = self.cancel_token.lock().unwrap().child_token();
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
        self.input_channel_tx.send(Message::UserMessage {
            msg_id: self.next_msg_id(),
            created_at: now_millis(),
            content: Arc::new(content.to_string()),
        }).await?;
        Ok(())
    }

    /// Used for conversation to notify a new message if available
    pub fn notify_msg(&self, msg: Message) -> Result<()> {
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

    /// Extract the latest assistant response text from this conversation's message history.
    /// Walks backward from the last `AssistantMessageEnd`, collecting `AssistantMessageChunk`s
    /// until `AssistantMessageStart` is found.
    pub fn extract_latest_response(&self) -> Option<String> {
        let msgs = match self.msgs.read() {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "msgs RwLock poisoned in extract_latest_response");
                return None;
            }
        };
        let mut chunks = Vec::new();
        let mut found_end = false;
        for msg in msgs.iter().rev() {
            match &**msg {
                Message::AssistantMessageEnd { .. } if !found_end => {
                    found_end = true;
                }
                Message::AssistantMessageChunk { content, .. } if found_end => {
                    chunks.push(content.as_str().to_owned());
                }
                Message::AssistantThinkingChunk { .. } if found_end => continue,
                Message::AssistantMessageStart { .. } if found_end => break,
                _ if found_end => continue,
                _ => continue,
            }
        }
        if chunks.is_empty() {
            return None;
        }
        chunks.reverse();
        Some(chunks.join(""))
    }

    /// Send a `ToolCallResolved` message to the conversation's input channel.
    /// Used by the server to deliver `/done` recovery results to the parent conversation.
    pub async fn send_tool_call_resolved(&self, tool_call_id: String, content: Arc<String>) -> Result<()> {
        self.input_channel_tx.send(Message::ToolCallResolved {
            msg_id: 0,
            tool_call_id,
            content,
        }).await?;
        Ok(())
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
            cancel_token: std::sync::Mutex::new(CancellationToken::new()),
            children: std::sync::Mutex::new(HashMap::new()),
        }
    }
}
