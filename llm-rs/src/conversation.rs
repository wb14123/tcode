use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::llm::{ChatOptions, LLM, LLMEvent, LLMMessage, ModelInfo, StopReason, ToolCall};
use crate::media::{ContentPart, MediaData, media_type_from_extension};
use crate::tool::{CancellationToken, ContainerConfig, Tool, ToolContext};
use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::task::AbortHandle;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

/// Prefix that subagent prompts are expected to start with.
/// Used in subagent tool guidance and stripped from display descriptions.
const SUBAGENT_PROMPT_PREFIX: &str = "You are a subagent.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemPromptContext {
    pub subagent_depth: usize,
}

pub type SystemPromptBuilder = Arc<dyn Fn(SystemPromptContext) -> String + Send + Sync + 'static>;

pub fn default_system_prompt_builder() -> SystemPromptBuilder {
    Arc::new(|context| {
        if context.subagent_depth == 0 {
            "You are a helpful assistant.".to_string()
        } else {
            "You are a helpful assistant. Complete the delegated task and return a concise result."
                .to_string()
        }
    })
}

/// Generic lightweight summary of a conversation for outer applications that
/// keep their own session metadata.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationSummary {
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
    pub total_input_tokens: i32,
    pub total_output_tokens: i32,
    #[serde(default)]
    pub total_cache_creation_tokens: i32,
    #[serde(default)]
    pub total_cache_read_tokens: i32,
    #[serde(default)]
    pub aggregate_input_tokens: i32,
    #[serde(default)]
    pub aggregate_output_tokens: i32,
    #[serde(default)]
    pub aggregate_cache_creation_tokens: i32,
    #[serde(default)]
    pub aggregate_cache_read_tokens: i32,
    pub single_turn: bool,
    pub subagent_depth: usize,
}

impl ConversationState {
    pub fn summary(&self) -> ConversationSummary {
        ConversationSummary {
            description: first_user_description(&self.llm_msgs),
            created_at: None,
            last_active_at: None,
        }
    }
}

fn first_user_description(llm_msgs: &[LLMMessage]) -> Option<String> {
    llm_msgs.iter().find_map(|msg| {
        if let LLMMessage::User(parts) = msg {
            let first_text = parts.iter().find_map(|p| {
                if let ContentPart::Text(t) = p {
                    Some(t.as_str())
                } else {
                    None
                }
            });
            match first_text {
                Some(text) => Some(truncate_preview(text, 80)),
                None => Some("[Media]".to_string()),
            }
        } else {
            None
        }
    })
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
        if let LLMMessage::Assistant { tool_calls, .. } = msg
            && !tool_calls.is_empty()
        {
            return Some((i, tool_calls.clone()));
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
            content: vec![ContentPart::Text(
                "Tool call was cancelled due to conversation interruption.".to_string(),
            )],
        });
    }
}

/// A message id, unique per conversation across resumes.
///
/// id = (epoch << 32) | counter. The only ways to obtain a `UniqueId` are
/// `UniqueIdGenerator::get_unique_id` (minting) and serde deserialization
/// (reading persisted events); there is deliberately no public constructor.
///
/// Uniqueness is by construction: within one epoch the counter never repeats,
/// and the epoch strictly increases at every conversation start (new or
/// resumed), so no id is ever issued twice for the same conversation,
/// regardless of how stale any persisted counter is. Id order is NOT
/// guaranteed: ids are minted with a Relaxed atomic, so the order in which
/// ids appear in `msgs`/display.jsonl may differ from minting order. Old
/// legacy i32 ids are all < 2^32, i.e. implicitly epoch 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UniqueId(i64);

impl UniqueId {
    /// Raw i64 value (display, logging, comparing against legacy ids).
    pub fn as_i64(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for UniqueId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Mints ids for one session. Exactly one instance exists per session,
/// created from the ROOT session dir by
/// [`ConversationManager::ensure_id_generator`] and shared by the root
/// conversation and all subagents (every `ConversationClient` holds an `Arc`
/// to it). The shared Relaxed-atomic counter makes ids globally unique across
/// the whole session dir. `new(dir)` may also be called with `None` by test
/// helpers and for orphaned-subagent synthetic ids (epoch 0, no file).
pub struct UniqueIdGenerator {
    epoch: u32,         // immutable after construction
    counter: AtomicU32, // per-run, starts at 0
}

impl UniqueIdGenerator {
    /// Read `dir/msg-id-epoch` (missing -> 0), set epoch = value + 1, write
    /// `dir/msg-id-epoch` with the new epoch (created on first run; tmp +
    /// fsync + rename + dir fsync), counter 0. All of this completes before
    /// any id can be minted, which is the ordering invariant: no event with
    /// epoch E may be written to disk before the epoch file durably reads E.
    /// `None` (no state dir, e.g. test helper): epoch 0, counter 0, no file.
    /// On any read/bump/write error, propagate and fail the conversation
    /// start (do not continue with a stale epoch).
    pub fn new(dir: Option<&Path>) -> anyhow::Result<Self> {
        let (epoch, counter) = match dir {
            Some(dir) => {
                let saved = read_epoch_file(dir)?;
                let epoch = saved
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("msg-id-epoch overflow: saved value {saved}"))?;
                anyhow::ensure!(
                    epoch < (1 << 31),
                    "msg-id-epoch overflow: epoch {epoch} would make ids non-positive"
                );
                write_epoch_file(dir, epoch)?;
                (epoch, AtomicU32::new(0))
            }
            None => (0, AtomicU32::new(0)),
        };
        Ok(Self { epoch, counter })
    }

    /// Reserve the next unique id:
    /// `UniqueId(((self.epoch as i64) << 32) | self.counter.fetch_add(1, Ordering::Relaxed) as i64)`.
    /// Does NOT push to `msgs` and does NOT broadcast — it only reserves an
    /// id. This is the single minting point, used by `notify_msg` and by the
    /// synthetic stale-close events. `Relaxed` is sufficient: uniqueness is
    /// the only guarantee; id order in `msgs`/display.jsonl is NOT guaranteed
    /// (concurrent minting may invert it). On counter overflow (unreachable
    /// at 2^32 per run) panic with a documented invariant — a "saturate"
    /// would mint duplicate ids.
    pub fn get_unique_id(&self) -> UniqueId {
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        assert!(
            counter != u32::MAX,
            "UniqueId counter overflow: the 2^32nd mint in one run would \
             reuse counter u32::MAX after 2^32-1 ids. Ids are \
             (epoch << 32) | counter and must never repeat within an epoch; \
             wrapping the counter would mint a duplicate id. Unreachable at \
             realistic event volumes — resume the conversation to start a \
             fresh epoch."
        );
        UniqueId(((self.epoch as i64) << 32) | counter as i64)
    }

    /// Current epoch (diagnostics/tests).
    pub fn epoch(&self) -> u32 {
        self.epoch
    }
}

/// Path of the per-conversation epoch file inside a state dir.
fn epoch_file_path(dir: &Path) -> PathBuf {
    dir.join("msg-id-epoch")
}

/// Read `dir/msg-id-epoch` (missing -> 0). Content is `"{epoch}\n"`;
/// `trim()` tolerates the trailing newline. Strict UTF-8: an invalid-UTF-8
/// file is an error, not a lossy read.
fn read_epoch_file(dir: &Path) -> anyhow::Result<u32> {
    let path = epoch_file_path(dir);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let epoch = content
                .trim()
                .parse::<u32>()
                .with_context(|| format!("invalid msg-id-epoch content in {:?}", path))?;
            Ok(epoch)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).with_context(|| format!("failed to read {:?}", path)),
    }
}

/// Write `dir/msg-id-epoch` with `"{epoch}\n"`, durably: tmp + fsync +
/// rename + dir fsync. The fsyncs are mandatory so the epoch write is durable
/// before any epoch-E event can survive; without them a power loss could
/// persist an epoch-E append while losing the epoch rename.
fn write_epoch_file(dir: &Path, epoch: u32) -> anyhow::Result<()> {
    let target = epoch_file_path(dir);
    let tmp = dir.join("msg-id-epoch.tmp");
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("failed to create {:?}", tmp))?;
    std::io::Write::write_all(&mut file, format!("{epoch}\n").as_bytes())
        .with_context(|| format!("failed to write {:?}", tmp))?;
    file.sync_all()
        .with_context(|| format!("failed to fsync {:?}", tmp))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("failed to rename {:?} -> {:?}", tmp, target))?;
    // fsync the directory so the rename itself is durable.
    let dir_file =
        std::fs::File::open(dir).with_context(|| format!("failed to open dir {:?}", dir))?;
    dir_file
        .sync_all()
        .with_context(|| format!("failed to fsync dir {:?}", dir))?;
    Ok(())
}

/// Get current timestamp in milliseconds since Unix epoch
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
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

/// Wrap raw tool output content based on the tool call's final status.
///
/// For `UserDenied`, this prepends boilerplate instructing the LLM that the
/// denial was a deliberate human choice (not a technical error). The user's
/// reason (if any) is already baked into `raw_content` by
/// `ask_permission_inner`, so this wrapper does not interpolate it
/// separately. For every other status the raw content parts are returned unchanged.
///
/// Kept `pub(crate)` so sibling tests in `conversation_tests.rs` can call it
/// directly without driving the full conversation loop.
pub(crate) fn build_tool_result_content(
    end_status: &MessageEndStatus,
    raw_content: Vec<ContentPart>,
) -> Vec<ContentPart> {
    match end_status {
        MessageEndStatus::UserDenied => {
            let mut parts = vec![ContentPart::Text(
                "The user denied permission for this tool call. This is not a technical error — \
                 the human operator chose not to allow this action. Do not retry this tool call. \
                 Instead, ask the user what they would like to do.\n\
                 Original tool output: "
                    .to_string(),
            )];
            parts.extend(raw_content);
            parts
        }
        _ => raw_content,
    }
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
        created_at: u64,
        content: Arc<String>,
        /// Relative paths from media/ dir like ["uuid.png"].
        #[serde(default)]
        #[serde(alias = "images")]
        media_filenames: Vec<String>,
    },

    ConversationSaved {},

    AssistantMessageStart {
        created_at: u64,
    },

    AssistantMessageChunk {
        content: Arc<String>,
    },

    AssistantThinkingChunk {
        content: Arc<String>,
    },

    AssistantMessageEnd {
        end_status: MessageEndStatus,
        error: Option<String>,
        input_tokens: i32,
        output_tokens: i32,
        reasoning_tokens: i32,
        cache_creation_input_tokens: i32,
        cache_read_input_tokens: i32,
        aggregate_input_tokens: i32,
        aggregate_output_tokens: i32,
        aggregate_cache_creation_tokens: i32,
        aggregate_cache_read_tokens: i32,
        #[serde(default)]
        tool_call_count: usize,
    },

    ToolMessageStart {
        tool_call_id: String,
        created_at: u64,
        tool_name: String,
        tool_args: String,
    },

    ToolOutputChunk {
        tool_call_id: String,
        tool_name: String,
        content: Arc<ContentPart>,
    },

    ToolMessageEnd {
        tool_call_id: String,
        end_status: MessageEndStatus,
        input_tokens: i32,
        output_tokens: i32,
    },

    /// Fired when a subagent tool call input is beginning to stream (before the conversation is
    /// created). Allows the UI to show a pending node immediately.
    SubAgentInputStart {
        tool_call_index: usize,
        tool_call_id: String,
        tool_name: String,
        created_at: u64,
    },

    /// A partial chunk of subagent tool call input (task text).
    SubAgentInputChunk {
        tool_call_index: usize,
        tool_name: String,
        content: Arc<String>,
    },

    SubAgentStart {
        tool_call_id: String,
        conversation_id: String,
        description: String,
    },

    SubAgentEnd {
        conversation_id: String,
        end_status: MessageEndStatus,
        response: Arc<String>,
        input_tokens: i32,
        output_tokens: i32,
    },

    /// A sub-agent turn completed but the conversation is still alive (idle).
    SubAgentTurnEnd {
        conversation_id: String,
        end_status: MessageEndStatus,
        response: Arc<String>,
        input_tokens: i32,
        output_tokens: i32,
        cache_creation_input_tokens: i32,
        cache_read_input_tokens: i32,
    },

    /// A sub-agent is being resumed with a follow-up message.
    SubAgentContinue {
        tool_call_id: String,
        conversation_id: String,
        description: String,
    },

    // assistant request to end the conversation, useful for sub agents
    AssistantRequestEnd {
        total_input_tokens: i32,
        total_output_tokens: i32,
        total_cache_creation_tokens: i32,
        total_cache_read_tokens: i32,
    },

    /// A tool call block has started streaming; name and id are now known.
    AssistantToolCallStart {
        tool_call_index: usize,
        tool_call_id: String,
        tool_name: String,
        created_at: u64,
    },

    /// A partial JSON fragment of a tool call's arguments arrived.
    AssistantToolCallArgChunk {
        tool_call_index: usize,
        tool_name: String,
        content: Arc<String>,
    },

    /// Broadcast by subagent when the user types `/done` in its interactive edit window.
    /// Monitored by the parent's tool task to recover a cancelled subagent result.
    UserRequestEnd {
        conversation_id: String,
    },

    /// Sent by tool task through loop_tx when a cancelled subagent is recovered via `/done`.
    ToolCallResolved {
        tool_call_id: String,
        content: Arc<String>,
    },

    /// System-level message (info, warning, error)
    SystemMessage {
        created_at: u64,
        level: SystemMessageLevel,
        message: String,
    },

    /// Signal that permission state has changed. UI should re-query for full state.
    PermissionUpdated {},

    /// Signal that a tool is waiting for user permission approval.
    ToolRequestPermission {
        tool_call_id: String,
    },

    /// Signal that a previously requested permission was approved and the tool is resuming.
    ToolPermissionApproved {
        tool_call_id: String,
    },

    /// A subagent (or one of its descendants) is waiting for user permission.
    SubAgentWaitingPermission {
        conversation_id: String,
    },

    /// A subagent's pending permission was approved.
    SubAgentPermissionApproved {
        conversation_id: String,
    },

    /// A subagent's tool was denied by the user.
    SubAgentPermissionDenied {
        conversation_id: String,
    },

    /// Internal: delta sent via loop_tx from collect_subagent_response to parent event loop.
    SubAgentTokenRollup {
        input_tokens: i32,
        output_tokens: i32,
        cache_creation_tokens: i32,
        cache_read_tokens: i32,
    },

    /// Broadcast: notifies UI of updated aggregate after any token change.
    AggregateTokenUpdate {
        aggregate_input_tokens: i32,
        aggregate_output_tokens: i32,
        aggregate_cache_creation_tokens: i32,
        aggregate_cache_read_tokens: i32,
    },

    /// The LLM has started generating media. Provides a media_id for
    /// correlation with the eventual AssistantMediaOutput.
    AssistantMediaGenerating {
        media_id: String,
    },

    /// The LLM generated media (e.g. via OpenAI's image_generation_call).
    /// `media` is Some if generation succeeded, None if it failed.
    /// `media_id` correlates with AssistantMediaGenerating.
    AssistantMediaOutput {
        media_id: String,
        end_status: MessageEndStatus,
        media: Option<MediaData>,
    },

    /// Broadcast when an LLM request fails and is about to be retried.
    /// Emitted before the backoff sleep, so the UI can show status while waiting.
    LLMRetry {
        /// Which retry attempt this is (1-indexed: 1, 2, 3...)
        attempt: u32,
        /// Total max retry attempts
        max_retries: u32,
        /// Human-readable reason (e.g., "request timed out after 120s",
        /// or the error message from the provider)
        reason: String,
    },
}

/// A broadcast message: a `Message` plus the id assigned to it when it was
/// broadcast. The id is unique per conversation across resumes — it is an
/// epoch-prefixed counter (`(epoch << 32) | counter`), where the epoch
/// strictly increases at every conversation start (see [`UniqueId`] and
/// [`UniqueIdGenerator`]). Id order is NOT guaranteed: ids are minted with a
/// Relaxed atomic, so concurrent `notify_msg` calls can invert the order in
/// which ids appear here, in `msgs`, and in display.jsonl. Only uniqueness is
/// a guarantee; no caller may rely on id order or contiguity (missing/
/// duplicated event detection belongs to the broadcast channel's `Lagged`
/// error and per-connection SSE line numbers).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub id: UniqueId,
    pub msg: Message,
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
    subagent_depth: usize,
    max_subagent_depth: usize,
    state_dir: Option<PathBuf>,
    /// Session directory structure for tool execution (media, logs, previews).
    session_dir: Option<crate::tool::SessionDir>,
    /// Whether the current model supports visual/media input (images, PDFs).
    supports_media: bool,
    /// Permission manager shared across all conversations.
    permission_manager: Arc<crate::permission::PermissionManager>,
    /// Optional container configuration for Docker/Podman sandbox mode.
    container_config: Option<Arc<ContainerConfig>>,
    /// LLM instance for tools that need to call back for review.
    llm: Option<Arc<dyn LLM>>,
    /// Model identifier for the LLM.
    model: String,
}

/// Create the `subagent` tool with a dynamic description listing available models.
pub fn create_subagent_tool(model_descriptions: &[ModelInfo]) -> Tool {
    let models_list: Vec<String> = model_descriptions
        .iter()
        .map(|m| format!("  - `{}`: {}", m.id, m.description))
        .collect();

    let description = format!(
        "Spawn a subagent to handle a self-contained task in its own context window. \
         The subagent has access to the available tools and returns its final answer. \
         Subagents can spawn their own subagents and can be continued via \
         `continue_subagent`.

\
         **Use this when:** the task can be handled independently, may need its own \
         context, or would produce details you do not need to keep in the main \
         conversation.

\
         **Rules:**
\
         - Start the prompt with \"{SUBAGENT_PROMPT_PREFIX}\"\n\
         - Describe the task clearly, including relevant context and acceptance criteria.\n\
         - State what the subagent should return.\n\
         - Spawn in parallel when tasks are independent.

\
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
        "Send a follow-up message to an existing idle subagent for the same \
         delegated task, such as clarification, provenance, correction, or \
         completion. Prefer spawning a new subagent for a distinct phase, \
         deliverable, or independent task. The conversation_id is found in the \
         [subagent_id: ...] prefix of previous subagent results.",
        schema,
    )
}

/// Result of [`prepare_conversation`]: the tool map, the input channel
/// receiver, and the client.
type PreparedConversation = (
    HashMap<String, Arc<Tool>>,
    mpsc::Receiver<Message>,
    Arc<ConversationClient>,
);

/// Prepare tools and channels common to both new and resumed conversations.
/// The caller supplies the shared session id generator (created by
/// [`ConversationManager::ensure_id_generator`], which runs the epoch-file
/// protocol once per session), so ids are globally unique across the root
/// conversation and all subagents.
fn prepare_conversation(
    llm: &mut dyn LLM,
    tools: Vec<Arc<Tool>>,
    ids: Arc<UniqueIdGenerator>,
    summary: ConversationSummary,
) -> Result<PreparedConversation> {
    llm.register_tools(tools.clone());
    let tools_map = tools.into_iter().map(|t| (t.name.clone(), t)).collect();
    let (input_tx, input_rx) = mpsc::channel(100);
    // Temporary mitigation for high-volume streamed tool output (notably bash):
    // event writers subscribe through this broadcast channel and persist tool
    // detail files from it. Broadcast may still drop updates if a receiver lags
    // beyond this capacity, so strict detail-file completeness would require a
    // backpressured persistence path.
    let (notify_tx, _) = broadcast::channel(10_000);
    let client = Arc::new(ConversationClient {
        ids,
        order_lock: parking_lot::Mutex::new(()),
        msgs: parking_lot::RwLock::new(Vec::new()),
        summary: parking_lot::RwLock::new(summary),
        input_channel_tx: input_tx,
        new_msg_notify_tx: notify_tx,
        tool_cancel_tokens: parking_lot::Mutex::new(HashMap::new()),
        cancel_token: parking_lot::Mutex::new(CancellationToken::new()),
        children: parking_lot::Mutex::new(HashMap::new()),
    });
    Ok((tools_map, input_rx, client))
}

// ============================================================================
// ConversationManager
// ============================================================================

pub struct ConversationManager {
    conversations: parking_lot::RwLock<HashMap<String, (Arc<ConversationClient>, AbortHandle)>>,
    /// Maps subagent_conv_id → (parent_conv_id, tool_call_id).
    /// Used by the server to route `/done` recovery to the correct parent.
    subagent_parents: parking_lot::Mutex<HashMap<String, (String, String)>>,
    /// Permission manager shared across all conversations.
    permission_manager: Arc<crate::permission::PermissionManager>,
    /// Optional container configuration for Docker/Podman sandbox mode.
    container_config: Option<Arc<ContainerConfig>>,
    system_prompt_builder: SystemPromptBuilder,
    /// The single shared session id generator. Created lazily by
    /// `ensure_id_generator` from the ROOT session dir (which runs the
    /// epoch-file protocol exactly once per session) and shared by the root
    /// conversation and all subagents. `None` until the first conversation
    /// start (new or resumed) requests one.
    id_generator: parking_lot::Mutex<Option<Arc<UniqueIdGenerator>>>,
}

/// Manages conversations so that any new client can attach to an existing conversation.
impl ConversationManager {
    pub fn new(permissions_path: PathBuf, container_config: Option<ContainerConfig>) -> Arc<Self> {
        Self::new_with_system_prompt_builder(
            permissions_path,
            container_config,
            default_system_prompt_builder(),
        )
    }

    pub fn new_with_system_prompt_builder(
        permissions_path: PathBuf,
        container_config: Option<ContainerConfig>,
        system_prompt_builder: SystemPromptBuilder,
    ) -> Arc<Self> {
        let permission_manager =
            Arc::new(crate::permission::PermissionManager::new(permissions_path));
        Arc::new(Self {
            conversations: parking_lot::RwLock::new(HashMap::new()),
            subagent_parents: parking_lot::Mutex::new(HashMap::new()),
            permission_manager,
            container_config: container_config.map(Arc::new),
            system_prompt_builder,
            id_generator: parking_lot::Mutex::new(None),
        })
    }

    pub(crate) fn build_system_prompt(&self, subagent_depth: usize) -> String {
        (self.system_prompt_builder)(SystemPromptContext { subagent_depth })
    }

    /// Get the permission manager.
    pub fn permission_manager(&self) -> &Arc<crate::permission::PermissionManager> {
        &self.permission_manager
    }

    /// Get (creating once) the session's shared id generator.
    ///
    /// The epoch-file protocol (read `msg-id-epoch`, bump, tmp + fsync +
    /// rename + dir fsync) runs exactly once per session, from the ROOT
    /// session dir (the first caller's `dir`); every later caller reuses the
    /// same generator, so ids are globally unique across the root
    /// conversation and all subagents. `dir` is `None` only for
    /// conversations without a state dir (tests), giving epoch 0, no file.
    fn ensure_id_generator(&self, dir: Option<&Path>) -> anyhow::Result<Arc<UniqueIdGenerator>> {
        let mut guard = self.id_generator.lock();
        if let Some(generator) = guard.as_ref() {
            return Ok(Arc::clone(generator));
        }
        let generator = Arc::new(UniqueIdGenerator::new(dir)?);
        *guard = Some(Arc::clone(&generator));
        Ok(generator)
    }

    /// Create a new conversation. The new conversation will be kept in the manager's
    /// memory until it ends.
    ///
    /// Returns `(conversation_id, client)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_conversation(
        self: &Arc<Self>,
        llm: Box<dyn LLM>,
        model: &str,
        tools: Vec<Arc<Tool>>,
        chat_options: ChatOptions,
        single_turn: bool,
        subagent_depth: usize,
        max_subagent_depth: usize,
        state_dir: Option<PathBuf>,
        supports_media: bool,
    ) -> Result<(String, Arc<ConversationClient>)> {
        let conversation_id = Uuid::new_v4().to_string();
        self.new_conversation_with_id(
            conversation_id,
            llm,
            model,
            tools,
            chat_options,
            single_turn,
            subagent_depth,
            max_subagent_depth,
            state_dir,
            supports_media,
        )
    }

    /// Create the session directory structure from the given state directory.
    ///
    /// Creates the `tool-logs/` subdirectory and grants a `Session`-scoped
    /// `file_read` permission so the LLM can later `read`/`grep`/`glob` log
    /// files produced by tools (e.g., full bash output saved on truncation).
    fn make_session_dir(
        &self,
        state_dir: &Option<PathBuf>,
    ) -> Result<Option<crate::tool::SessionDir>> {
        let dir = match state_dir {
            Some(dir) => dir,
            None => return Ok(None),
        };
        let tool_logs_dir = dir.join("tool-logs");
        if let Err(e) = std::fs::create_dir_all(&tool_logs_dir) {
            tracing::warn!(
                "Failed to create tool logs directory {}: {e}",
                tool_logs_dir.display()
            );
        }
        if let Ok(canonical) = std::fs::canonicalize(&tool_logs_dir) {
            let key = crate::permission::PermissionKey {
                tool: crate::permission::SCOPE_FILE_READ.to_string(),
                key: crate::permission::KEY_PATH.to_string(),
                value: tcode_encoding::path_to_str(&canonical)?.to_string(),
            };
            if let Err(e) = self
                .permission_manager
                .add_permission(key, crate::permission::PermissionScope::Session)
            {
                tracing::warn!("Failed to add permission for tool logs directory: {e}");
            }
        } else {
            tracing::warn!(
                "Failed to canonicalize tool logs directory path: {}",
                tool_logs_dir.display()
            );
        }
        Ok(Some(crate::tool::SessionDir::new(dir.clone())))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_conversation_with_id(
        self: &Arc<Self>,
        conversation_id: String,
        mut llm: Box<dyn LLM>,
        model: &str,
        tools: Vec<Arc<Tool>>,
        chat_options: ChatOptions,
        single_turn: bool,
        subagent_depth: usize,
        max_subagent_depth: usize,
        state_dir: Option<PathBuf>,
        supports_media: bool,
    ) -> Result<(String, Arc<ConversationClient>)> {
        let now = now_millis();
        let llm_clone = Arc::from(llm.clone_box());
        let ids = self.ensure_id_generator(state_dir.as_deref())?;
        let (tools_map, input_rx, client) = prepare_conversation(
            &mut *llm,
            tools,
            ids,
            ConversationSummary {
                description: None,
                created_at: Some(now),
                last_active_at: Some(now),
            },
        )?;
        let system_prompt = self.build_system_prompt(subagent_depth);
        let llm_msgs = vec![LLMMessage::System(system_prompt)];
        let conversation = Conversation {
            id: conversation_id.clone(),
            llm,
            model: model.to_string(),
            llm_msgs,
            input_channel_rx: input_rx,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            aggregate_input_tokens: 0,
            aggregate_output_tokens: 0,
            aggregate_cache_creation_tokens: 0,
            aggregate_cache_read_tokens: 0,
            single_turn,
            pending_tools: HashSet::new(),
            cancelled_tools: HashSet::new(),
            accumulated_tool_content: HashMap::new(),
            description: None,
            created_at: Some(now),
            env: {
                ConversationEnv {
                    conversation_id: conversation_id.clone(),
                    client,
                    conversation_manager: Arc::clone(self),
                    tools: tools_map,
                    chat_options,
                    subagent_depth,
                    max_subagent_depth,
                    state_dir: state_dir.clone(),
                    session_dir: self.make_session_dir(&state_dir)?,
                    supports_media,
                    permission_manager: Arc::clone(&self.permission_manager),
                    container_config: self.container_config.clone(),
                    llm: Some(llm_clone),
                    model: model.to_string(),
                }
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
                log_and_broadcast_system_message(&watcher_client, SystemMessageLevel::Error, msg);
            }
        });

        self.conversations
            .write()
            .insert(conversation_id.clone(), (client.clone(), abort_handle));
        Ok((conversation_id, client))
    }

    /// Get a conversation by its id. It will try to load it from the manager's memory.
    /// If not found, load it from storage and put into the manager's memory.
    pub fn get_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<Arc<ConversationClient>>> {
        Ok(self
            .conversations
            .read()
            .get(conversation_id)
            .map(|x| x.0.clone()))
    }

    /// Remove the conversation from the manager's memory. The conversation should be
    /// cleared if there is no reference to the Arc anymore.
    pub fn end_conversation(&self, _conversation_id: &str) -> Result<()> {
        Ok(()) // placeholder
    }

    /// Register a subagent → parent mapping for `/done` recovery.
    pub fn register_subagent_parent(
        &self,
        subagent_conv_id: &str,
        parent_conv_id: &str,
        tool_call_id: &str,
    ) {
        self.subagent_parents.lock().insert(
            subagent_conv_id.to_string(),
            (parent_conv_id.to_string(), tool_call_id.to_string()),
        );
    }

    /// Look up the parent conversation and tool_call_id for a subagent.
    pub fn get_subagent_parent(&self, subagent_conv_id: &str) -> Option<(String, String)> {
        self.subagent_parents.lock().get(subagent_conv_id).cloned()
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
        max_subagent_depth: usize,
        state_dir: Option<PathBuf>,
        supports_media: bool,
    ) -> Result<(String, Arc<ConversationClient>)> {
        fill_cancelled_tool_results(&mut state.llm_msgs);
        let summary = state.summary();
        let description = summary.description.clone();
        let created_at = summary.created_at;

        let llm_clone = Arc::from(llm.clone_box());
        let ids = self.ensure_id_generator(state_dir.as_deref())?;
        let (tools_map, input_rx, client) = prepare_conversation(&mut *llm, tools, ids, summary)?;
        let conv_id = state.id.clone();
        let conversation = Conversation {
            id: state.id.clone(),
            llm,
            model: state.model.clone(),
            llm_msgs: state.llm_msgs,
            input_channel_rx: input_rx,
            total_input_tokens: state.total_input_tokens,
            total_output_tokens: state.total_output_tokens,
            total_cache_creation_tokens: state.total_cache_creation_tokens,
            total_cache_read_tokens: state.total_cache_read_tokens,
            aggregate_input_tokens: state.aggregate_input_tokens,
            aggregate_output_tokens: state.aggregate_output_tokens,
            aggregate_cache_creation_tokens: state.aggregate_cache_creation_tokens,
            aggregate_cache_read_tokens: state.aggregate_cache_read_tokens,
            single_turn: state.single_turn,
            pending_tools: HashSet::new(),
            cancelled_tools: HashSet::new(),
            accumulated_tool_content: HashMap::new(),
            description,
            created_at,
            env: {
                ConversationEnv {
                    conversation_id: conv_id,
                    client,
                    conversation_manager: Arc::clone(self),
                    tools: tools_map,
                    chat_options: state.chat_options,
                    subagent_depth: state.subagent_depth,
                    max_subagent_depth,
                    state_dir: state_dir.clone(),
                    session_dir: self.make_session_dir(&state_dir)?,
                    supports_media,
                    permission_manager: Arc::clone(&self.permission_manager),
                    container_config: self.container_config.clone(),
                    llm: Some(llm_clone),
                    model: state.model.clone(),
                }
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
        max_subagent_depth: usize,
        state_dir: PathBuf,
        supports_media: bool,
    ) -> Result<(String, Arc<ConversationClient>, Vec<ResumedSubagent>)> {
        // Ordering invariant: run the epoch-file protocol (read/bump/write,
        // tmp + fsync + rename + dir fsync) for the ROOT dir up front, before
        // ANY subagent is resumed, so the new epoch is durable before any
        // event with it can be written (synthetic stale-close appends happen
        // after this whole function returns). The single generator is then
        // shared by the root and every resumed subagent (their resume paths
        // reuse it via ensure_id_generator).
        self.ensure_id_generator(Some(&state_dir))?;

        // Find all subagent states (depth-first: nested before parent)
        let subagent_states = find_subagent_states(&state_dir);

        // Reconstruct parent mapping from all conversation states' llm_msgs
        {
            let mut all_states: Vec<(&str, &[LLMMessage])> = vec![(&state.id, &state.llm_msgs)];
            for (_, sa_state) in &subagent_states {
                all_states.push((&sa_state.id, &sa_state.llm_msgs));
            }
            for (sa_dir, _) in &subagent_states {
                let sa_conv_id = sa_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix("subagent-"));
                if let Some(sa_conv_id) = sa_conv_id {
                    for &(parent_id, llm_msgs) in &all_states {
                        if let Some(tool_call_id) =
                            find_tool_call_for_subagent(llm_msgs, sa_conv_id)
                        {
                            self.register_subagent_parent(sa_conv_id, parent_id, &tool_call_id);
                            break;
                        }
                    }
                }
            }
        }

        let mut resumed_subagents = Vec::new();

        for (sa_dir, sa_state) in subagent_states {
            let mut sa_llm = llm.clone_box();
            sa_llm.set_media_dir(Some(sa_dir.join("media")));
            let sa_tools = tools.clone();
            let (sa_id, sa_client) = self.resume_conversation(
                sa_state,
                sa_llm,
                sa_tools,
                max_subagent_depth,
                Some(sa_dir.clone()),
                supports_media,
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
            max_subagent_depth,
            Some(state_dir),
            supports_media,
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
///
/// INVARIANT: the returned `state_dir` paths are constructed as
/// `dir.join(entry.file_name())` with no canonicalization, byte-identical to
/// the `subagent-*` paths that `tcode-runtime::server::close_stale_in_dir`
/// builds when recursing (server.rs). The resume path keys its id-source map
/// by these exact `PathBuf`s; if either side ever canonicalizes, a resumed
/// subagent would silently fall into the orphaned-dir branch and mint
/// epoch-0 synthetic ids that could collide with its persisted legacy ids.
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
        if let LLMMessage::ToolResult {
            tool_call_id,
            content,
        } = msg
            && content
                .iter()
                .any(|p| matches!(p, ContentPart::Text(t) if t.starts_with(&prefix)))
        {
            return Some(tool_call_id.clone());
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
    cache_creation_input_tokens: i32,
    cache_read_input_tokens: i32,
    end_status: MessageEndStatus,
}

/// Collect a subagent's first-turn response and publish results to the parent via `loop_tx`.
/// `/done` recovery after cancellation is handled by the server's `UserRequestEnd` handler.
async fn collect_subagent_response(
    sub_stream: &mut (
             impl Stream<Item = Result<Arc<BroadcastMessage>, BroadcastStreamRecvError>> + Unpin
         ),
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
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        end_status: MessageEndStatus::Succeeded,
    };
    let mut cancel_sent = false;
    let mut last_rolled_input = 0i32;
    let mut last_rolled_output = 0i32;
    let mut last_rolled_cache_creation = 0i32;
    let mut last_rolled_cache_read = 0i32;

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

        match &msg.msg {
            Message::AssistantMessageChunk { content, .. } => {
                resp.text.push_str(content);
            }
            Message::AssistantMessageEnd {
                end_status: MessageEndStatus::Failed,
                error: Some(err),
                aggregate_input_tokens,
                aggregate_output_tokens,
                aggregate_cache_creation_tokens,
                aggregate_cache_read_tokens,
                ..
            } if resp.text.is_empty() => {
                resp.text = format!("Error: Subagent failed: {}", err);
                resp.end_status = MessageEndStatus::Failed;
                let delta_input = aggregate_input_tokens - last_rolled_input;
                let delta_output = aggregate_output_tokens - last_rolled_output;
                let delta_cache_creation =
                    aggregate_cache_creation_tokens - last_rolled_cache_creation;
                let delta_cache_read = aggregate_cache_read_tokens - last_rolled_cache_read;
                if delta_input != 0
                    || delta_output != 0
                    || delta_cache_creation != 0
                    || delta_cache_read != 0
                {
                    loop_tx
                        .send(Message::SubAgentTokenRollup {
                            input_tokens: delta_input,
                            output_tokens: delta_output,
                            cache_creation_tokens: delta_cache_creation,
                            cache_read_tokens: delta_cache_read,
                        })
                        .await?;
                }
                last_rolled_input = *aggregate_input_tokens;
                last_rolled_output = *aggregate_output_tokens;
                last_rolled_cache_creation = *aggregate_cache_creation_tokens;
                last_rolled_cache_read = *aggregate_cache_read_tokens;
            }
            Message::AssistantMessageEnd {
                end_status: MessageEndStatus::Cancelled,
                aggregate_input_tokens,
                aggregate_output_tokens,
                aggregate_cache_creation_tokens,
                aggregate_cache_read_tokens,
                ..
            } => {
                resp.end_status = MessageEndStatus::Cancelled;
                let delta_input = aggregate_input_tokens - last_rolled_input;
                let delta_output = aggregate_output_tokens - last_rolled_output;
                let delta_cache_creation =
                    aggregate_cache_creation_tokens - last_rolled_cache_creation;
                let delta_cache_read = aggregate_cache_read_tokens - last_rolled_cache_read;
                if delta_input != 0
                    || delta_output != 0
                    || delta_cache_creation != 0
                    || delta_cache_read != 0
                {
                    loop_tx
                        .send(Message::SubAgentTokenRollup {
                            input_tokens: delta_input,
                            output_tokens: delta_output,
                            cache_creation_tokens: delta_cache_creation,
                            cache_read_tokens: delta_cache_read,
                        })
                        .await?;
                }
                last_rolled_input = *aggregate_input_tokens;
                last_rolled_output = *aggregate_output_tokens;
                last_rolled_cache_creation = *aggregate_cache_creation_tokens;
                last_rolled_cache_read = *aggregate_cache_read_tokens;
            }
            Message::AssistantMessageEnd {
                aggregate_input_tokens,
                aggregate_output_tokens,
                aggregate_cache_creation_tokens,
                aggregate_cache_read_tokens,
                ..
            } => {
                // Successful or other status — just roll up aggregates
                let delta_input = aggregate_input_tokens - last_rolled_input;
                let delta_output = aggregate_output_tokens - last_rolled_output;
                let delta_cache_creation =
                    aggregate_cache_creation_tokens - last_rolled_cache_creation;
                let delta_cache_read = aggregate_cache_read_tokens - last_rolled_cache_read;
                if delta_input != 0
                    || delta_output != 0
                    || delta_cache_creation != 0
                    || delta_cache_read != 0
                {
                    loop_tx
                        .send(Message::SubAgentTokenRollup {
                            input_tokens: delta_input,
                            output_tokens: delta_output,
                            cache_creation_tokens: delta_cache_creation,
                            cache_read_tokens: delta_cache_read,
                        })
                        .await?;
                }
                last_rolled_input = *aggregate_input_tokens;
                last_rolled_output = *aggregate_output_tokens;
                last_rolled_cache_creation = *aggregate_cache_creation_tokens;
                last_rolled_cache_read = *aggregate_cache_read_tokens;
            }
            Message::AggregateTokenUpdate {
                aggregate_input_tokens,
                aggregate_output_tokens,
                aggregate_cache_creation_tokens,
                aggregate_cache_read_tokens,
            } => {
                let delta_input = aggregate_input_tokens - last_rolled_input;
                let delta_output = aggregate_output_tokens - last_rolled_output;
                let delta_cache_creation =
                    aggregate_cache_creation_tokens - last_rolled_cache_creation;
                let delta_cache_read = aggregate_cache_read_tokens - last_rolled_cache_read;
                if delta_input != 0
                    || delta_output != 0
                    || delta_cache_creation != 0
                    || delta_cache_read != 0
                {
                    loop_tx
                        .send(Message::SubAgentTokenRollup {
                            input_tokens: delta_input,
                            output_tokens: delta_output,
                            cache_creation_tokens: delta_cache_creation,
                            cache_read_tokens: delta_cache_read,
                        })
                        .await?;
                }
                last_rolled_input = *aggregate_input_tokens;
                last_rolled_output = *aggregate_output_tokens;
                last_rolled_cache_creation = *aggregate_cache_creation_tokens;
                last_rolled_cache_read = *aggregate_cache_read_tokens;
            }
            Message::AssistantRequestEnd {
                total_input_tokens,
                total_output_tokens,
                total_cache_creation_tokens,
                total_cache_read_tokens,
            } => {
                resp.input_tokens = *total_input_tokens;
                resp.output_tokens = *total_output_tokens;
                resp.cache_creation_input_tokens = *total_cache_creation_tokens;
                resp.cache_read_input_tokens = *total_cache_read_tokens;

                // Publish first-turn result to parent
                let text = match Conversation::broadcast_subagent_turn_end(
                    parent_client,
                    subagent_conv_id,
                    &resp,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to broadcast SubAgentTurnEnd");
                        format_subagent_result(subagent_conv_id, &resp.text, &resp.end_status)
                    }
                };
                let cancelled =
                    cancel_token.is_cancelled() || resp.end_status == MessageEndStatus::Cancelled;
                loop_tx
                    .send(Message::ToolOutputChunk {
                        tool_call_id: tool_call_id.to_string(),
                        tool_name: "subagent".to_string(),
                        content: Arc::new(ContentPart::Text(text)),
                    })
                    .await
                    .context("failed to send ToolOutputChunk")?;
                loop_tx
                    .send(Message::ToolMessageEnd {
                        tool_call_id: tool_call_id.to_string(),
                        end_status: if cancelled {
                            MessageEndStatus::Cancelled
                        } else {
                            MessageEndStatus::Succeeded
                        },
                        input_tokens: 0,
                        output_tokens: 0,
                    })
                    .await
                    .context("failed to send ToolMessageEnd")?;

                break; // First turn done — exit regardless of cancel status
            }
            // Tool in subagent requests permission → bubble SubAgent status to parent
            Message::ToolRequestPermission { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentWaitingPermission {
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to send SubAgentWaitingPermission to parent");
                }
            }
            // Tool permission approved → bubble up
            Message::ToolPermissionApproved { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionApproved {
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to send SubAgentPermissionApproved to parent");
                }
            }
            // Tool denied → bubble up
            Message::ToolMessageEnd {
                end_status: MessageEndStatus::UserDenied,
                ..
            } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionDenied {
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to send SubAgentPermissionDenied to parent");
                }
            }
            // Forward permission signals to parent so the UI sees them
            Message::PermissionUpdated { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::PermissionUpdated {}) {
                    tracing::error!(error = %e, "failed to forward PermissionUpdated to parent");
                }
            }
            // Recursive bubble-up from nested subagents: re-emit with THIS subagent's conversation_id
            Message::SubAgentWaitingPermission { .. } => {
                // Also forward PermissionUpdated so the permission UI works at all ancestor levels
                if let Err(e) = parent_client.notify_msg(Message::PermissionUpdated {}) {
                    tracing::error!(error = %e, "failed to forward PermissionUpdated to parent");
                }
                if let Err(e) = parent_client.notify_msg(Message::SubAgentWaitingPermission {
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to re-emit SubAgentWaitingPermission to parent");
                }
            }
            Message::SubAgentPermissionApproved { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionApproved {
                    conversation_id: subagent_conv_id.to_string(),
                }) {
                    tracing::error!(error = %e, "failed to re-emit SubAgentPermissionApproved to parent");
                }
            }
            Message::SubAgentPermissionDenied { .. } => {
                if let Err(e) = parent_client.notify_msg(Message::SubAgentPermissionDenied {
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
pub fn format_subagent_result(
    conversation_id: &str,
    text: &str,
    end_status: &MessageEndStatus,
) -> String {
    if matches!(end_status, MessageEndStatus::Cancelled) {
        format!(
            "[subagent_id: {}]\nSubagent was cancelled by the user. \
             Do not retry or continue this subagent unless the user explicitly asks.",
            conversation_id
        )
    } else if text.is_empty() {
        format!(
            "[subagent_id: {}]\nSubagent completed but produced no output.",
            conversation_id
        )
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

    /// Accumulated token usage for this conversation only.
    ///
    /// Anthropic API token semantics (three non-overlapping input buckets):
    /// - `input_tokens`: tokens NOT involved in any cache (not read from, not written to)
    /// - `cache_creation_tokens`: tokens fully processed AND written to a new cache entry (1.25x cost)
    /// - `cache_read_tokens`: tokens served from an existing cache (0.1x cost, cheapest)
    ///
    /// Total processed input = input_tokens + cache_creation_tokens (all tokens the model computed over)
    /// Total from cache = cache_read_tokens (tokens served from cache without reprocessing)
    total_input_tokens: i32,
    total_output_tokens: i32,
    total_cache_creation_tokens: i32,
    total_cache_read_tokens: i32,

    /// Aggregate token usage (own + all subagent descendants). Same field semantics as above.
    aggregate_input_tokens: i32,
    aggregate_output_tokens: i32,
    aggregate_cache_creation_tokens: i32,
    aggregate_cache_read_tokens: i32,

    /// When true, the conversation exits after one user message + LLM response cycle.
    single_turn: bool,

    /// Outstanding tool_call_ids waiting for completion.
    pending_tools: HashSet<String>,

    /// Tool_call_ids that completed with `Cancelled` status in the current turn.
    /// When all pending tools finish and any were cancelled, the LLM is NOT called
    /// automatically — instead a SystemMessage is broadcast and the turn pauses.
    cancelled_tools: HashSet<String>,

    /// Accumulated tool output per tool_call_id (ContentPart chunks collected).
    accumulated_tool_content: HashMap<String, Vec<ContentPart>>,

    /// Truncated first user input used as session description.
    description: Option<String>,

    /// Timestamp (millis since epoch) when the conversation was created.
    created_at: Option<u64>,

    /// Cloneable environment passed to spawned tool-execution tasks.
    env: ConversationEnv,
}

/// Result of consuming an LLM response stream within a single attempt.
enum StreamResult {
    Success,
    Cancelled,
    Timeout,
    Error(String),
}

/// Multi round LLM conversation. Thread and async safe.
impl Conversation {
    fn broadcast_msg(&self, msg: Message) -> Result<()> {
        self.env.client.notify_msg(msg)
    }

    fn broadcast_cancelled_end(&self) -> Result<()> {
        self.broadcast_msg(Message::AssistantMessageEnd {
            end_status: MessageEndStatus::Cancelled,
            error: None,
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            aggregate_input_tokens: self.aggregate_input_tokens,
            aggregate_output_tokens: self.aggregate_output_tokens,
            aggregate_cache_creation_tokens: self.aggregate_cache_creation_tokens,
            aggregate_cache_read_tokens: self.aggregate_cache_read_tokens,
            tool_call_count: 0,
        })
    }

    fn snapshot_state(&self) -> ConversationState {
        ConversationState {
            id: self.id.clone(),
            model: self.model.clone(),
            llm_msgs: self.llm_msgs.clone(),
            chat_options: self.env.chat_options.clone(),
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            total_cache_creation_tokens: self.total_cache_creation_tokens,
            total_cache_read_tokens: self.total_cache_read_tokens,
            aggregate_input_tokens: self.aggregate_input_tokens,
            aggregate_output_tokens: self.aggregate_output_tokens,
            aggregate_cache_creation_tokens: self.aggregate_cache_creation_tokens,
            aggregate_cache_read_tokens: self.aggregate_cache_read_tokens,
            single_turn: self.single_turn,
            subagent_depth: self.env.subagent_depth,
        }
    }

    fn summary_snapshot(&self, last_active_at: Option<u64>) -> ConversationSummary {
        ConversationSummary {
            description: self.description.clone(),
            created_at: self.created_at,
            last_active_at,
        }
    }

    fn save_state(&self) -> Result<ConversationSummary> {
        let summary = self.summary_snapshot(Some(now_millis()));
        self.env.client.set_conversation_summary(summary.clone());
        if let Some(ref dir) = self.env.state_dir {
            let state = self.snapshot_state();
            let json = serde_json::to_string_pretty(&state)?;
            let tmp = dir.join("conversation-state.json.tmp");
            let target = dir.join("conversation-state.json");
            std::fs::write(&tmp, &json)?;
            std::fs::rename(&tmp, &target)?;
            if let Err(e) = self.broadcast_msg(Message::ConversationSaved {}) {
                tracing::warn!(error = %e, "failed to broadcast conversation saved notification");
            }
        }
        Ok(summary)
    }

    fn push_llm_msg(&mut self, msg: LLMMessage) -> Result<()> {
        if let LLMMessage::Assistant {
            ref content,
            ref tool_calls,
            ..
        } = msg
            && content.is_empty()
            && tool_calls.is_empty()
        {
            bail!("Empty Assistant message (no content, no tool_calls)");
        }
        self.llm_msgs.push(msg);
        self.save_state()?;
        Ok(())
    }

    /// Broadcast SubAgentTurnEnd for a subagent response (without pushing to llm_msgs).
    /// Returns `(tool_call_id, result_text)` for the caller to push.
    fn broadcast_subagent_turn_end(
        client: &ConversationClient,
        conversation_id: &str,
        response: &SubagentResponse,
    ) -> Result<String> {
        let result_text =
            format_subagent_result(conversation_id, &response.text, &response.end_status);

        client.notify_msg(Message::SubAgentTurnEnd {
            conversation_id: conversation_id.to_string(),
            end_status: response.end_status.clone(),
            response: Arc::new(result_text.clone()),
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
            cache_creation_input_tokens: response.cache_creation_input_tokens,
            cache_read_input_tokens: response.cache_read_input_tokens,
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
                    // Clean up any pending permission requests so the permission UI
                    // doesn't keep showing stale entries. Must happen before
                    // AssistantMessageEnd/AssistantRequestEnd so the monitoring loop
                    // (for subagents) can still forward the PermissionUpdated signal.
                    self.env.permission_manager.close_all_pending();
                    self.broadcast_msg(Message::PermissionUpdated {
                                })?;
                    self.broadcast_msg(Message::AssistantMessageEnd {
                                    end_status: MessageEndStatus::Cancelled,
                        error: None,
                        input_tokens: 0,
                        output_tokens: 0,
                        reasoning_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        aggregate_input_tokens: self.aggregate_input_tokens,
                        aggregate_output_tokens: self.aggregate_output_tokens,
                        aggregate_cache_creation_tokens: self.aggregate_cache_creation_tokens,
                        aggregate_cache_read_tokens: self.aggregate_cache_read_tokens,
                        tool_call_count: 0,
                    })?;
                    self.finish_turn()?;
                }
                msg = self.input_channel_rx.recv() => {
                    let Some(msg) = msg else { break };
                    match msg {
                        Message::UserMessage { content, media_filenames, .. } => {
                            // If tools are pending, cancel them and fill synthetic results
                            if !self.pending_tools.is_empty() {
                                self.env.client.cancel_silent();
                                self.fill_remaining_cancelled(true)?;
                                self.env.permission_manager.close_all_pending();
                                self.broadcast_msg(Message::PermissionUpdated {
                                                        })?;
                                self.env.client.reset_cancel_token();
                            }
                            self.cancelled_tools.clear();
                            if self.description.is_none() {
                                self.description = Some(truncate_preview(&content, 80));
                            }
                            let mut parts = vec![ContentPart::Text(content.to_string())];
                            for filename in &media_filenames {
                                let media_type = media_type_from_extension(filename);
                                parts.push(ContentPart::Media(MediaData::new(
                                    filename.clone(),
                                    media_type.to_string(),
                                )));
                            }
                            self.push_llm_msg(LLMMessage::User(parts))?;
                            self.broadcast_msg(Message::UserMessage {
                                                    created_at: now_millis(),
                                content: Arc::clone(&content),
                                media_filenames: media_filenames.clone(),
                            })?;
                            self.call_llm().await?;
                            self.maybe_finish_turn()?;
                        }
                        Message::ToolOutputChunk { tool_call_id, content, .. } => {
                            if self.pending_tools.contains(&tool_call_id) {
                                self.accumulated_tool_content
                                    .entry(tool_call_id).or_default()
                                    .push(content.as_ref().clone());
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
                                let content = build_tool_result_content(&end_status, raw_content);
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
                                if let LLMMessage::ToolResult { tool_call_id: id, content: c } = msg
                                    && *id == tool_call_id
                                {
                                    *c = vec![ContentPart::Text(content.to_string())];
                                    found = true;
                                    break;
                                }
                            }
                            if found {
                                self.save_state()?;
                                self.call_llm().await?;
                                self.maybe_finish_turn()?;
                            }
                            // If not found (e.g. parent moved on), silently ignore
                        }
                        Message::SubAgentTokenRollup { input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens } => {
                            self.aggregate_input_tokens += input_tokens;
                            self.aggregate_output_tokens += output_tokens;
                            self.aggregate_cache_creation_tokens += cache_creation_tokens;
                            self.aggregate_cache_read_tokens += cache_read_tokens;
                            self.broadcast_msg(Message::AggregateTokenUpdate {
                                aggregate_input_tokens: self.aggregate_input_tokens,
                                aggregate_output_tokens: self.aggregate_output_tokens,
                                aggregate_cache_creation_tokens: self.aggregate_cache_creation_tokens,
                                aggregate_cache_read_tokens: self.aggregate_cache_read_tokens,
                            })?;
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

    /// Call the LLM if not cancelled.
    /// Streams the response, handles tool spawning and cancellation internally.
    async fn call_llm(&mut self) -> Result<()> {
        let cancel_token = self.env.client.current_cancel_token();
        if cancel_token.is_cancelled() {
            return Ok(());
        }

        let max_retries = self.env.chat_options.max_retries.unwrap_or(3);
        let read_timeout =
            Duration::from_secs(self.env.chat_options.request_timeout_secs.unwrap_or(120));
        let media_timeout = Duration::from_secs(
            self.env
                .chat_options
                .media_generation_timeout_secs
                .unwrap_or(300),
        );

        self.broadcast_msg(Message::AssistantMessageStart {
            created_at: now_millis(),
        })?;

        for attempt in 0..=max_retries {
            let mut got_content = false;
            let mut accumulated_text = String::new();
            let mut pending_tool_calls = Vec::new();
            let mut tool_call_names: HashMap<usize, String> = HashMap::new();

            let mut response_stream =
                self.llm
                    .chat(self.model.as_str(), &self.llm_msgs, &self.env.chat_options);

            let mut idle_timeout = std::pin::pin!(tokio::time::sleep(read_timeout));
            let inner_result: StreamResult = loop {
                let event = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        self.broadcast_cancelled_end()?;
                        break StreamResult::Cancelled;
                    }
                    event = response_stream.next() => {
                        match event {
                            Some(e) => e,
                            None => { break StreamResult::Success; }
                        }
                    }
                    _ = &mut idle_timeout => {
                        break StreamResult::Timeout;
                    }
                };

                match event {
                    LLMEvent::MessageStart { .. } => {
                        // Token accounting is done in MessageEnd to avoid double-counting.
                    }
                    LLMEvent::TextDelta(text) => {
                        got_content = true;
                        accumulated_text.push_str(&text);
                        self.broadcast_msg(Message::AssistantMessageChunk {
                            content: Arc::new(text),
                        })?;
                    }
                    LLMEvent::ThinkingDelta(text) => {
                        got_content = true;
                        self.broadcast_msg(Message::AssistantThinkingChunk {
                            content: Arc::new(text),
                        })?;
                    }
                    LLMEvent::ToolCall(tool_call) => {
                        got_content = true;
                        pending_tool_calls.push(tool_call);
                    }
                    LLMEvent::ToolCallStart { index, id, name } => {
                        got_content = true;
                        tool_call_names.insert(index, name.clone());
                        if name == "subagent" || name == "continue_subagent" {
                            self.broadcast_msg(Message::SubAgentInputStart {
                                tool_call_index: index,
                                tool_call_id: id,
                                tool_name: name,
                                created_at: now_millis(),
                            })?;
                        } else {
                            self.broadcast_msg(Message::AssistantToolCallStart {
                                tool_call_index: index,
                                tool_call_id: id,
                                tool_name: name,
                                created_at: now_millis(),
                            })?;
                        }
                    }
                    LLMEvent::ToolCallDelta {
                        index,
                        partial_json,
                    } => {
                        let tool_name = tool_call_names.get(&index).cloned().unwrap_or_default();
                        if tool_name == "subagent" || tool_name == "continue_subagent" {
                            self.broadcast_msg(Message::SubAgentInputChunk {
                                tool_call_index: index,
                                tool_name,
                                content: Arc::new(partial_json),
                            })?;
                        } else {
                            self.broadcast_msg(Message::AssistantToolCallArgChunk {
                                tool_call_index: index,
                                tool_name,
                                content: Arc::new(partial_json),
                            })?;
                        }
                    }
                    LLMEvent::MessageEnd {
                        stop_reason,
                        input_tokens,
                        output_tokens,
                        reasoning_tokens,
                        cache_creation_input_tokens,
                        cache_read_input_tokens,
                        mut raw,
                    } => {
                        self.total_input_tokens += input_tokens;
                        self.total_output_tokens += output_tokens;
                        self.total_cache_creation_tokens += cache_creation_input_tokens;
                        self.total_cache_read_tokens += cache_read_input_tokens;
                        self.aggregate_input_tokens += input_tokens;
                        self.aggregate_output_tokens += output_tokens;
                        self.aggregate_cache_creation_tokens += cache_creation_input_tokens;
                        self.aggregate_cache_read_tokens += cache_read_input_tokens;

                        let (end_status, error) = if stop_reason == StopReason::MaxTokens {
                            (
                                MessageEndStatus::Failed,
                                Some("Response truncated: maximum token limit reached".to_string()),
                            )
                        } else {
                            (MessageEndStatus::Succeeded, None)
                        };

                        self.broadcast_msg(Message::AssistantMessageEnd {
                            end_status,
                            error,
                            input_tokens,
                            output_tokens,
                            reasoning_tokens,
                            cache_creation_input_tokens,
                            cache_read_input_tokens,
                            aggregate_input_tokens: self.aggregate_input_tokens,
                            aggregate_output_tokens: self.aggregate_output_tokens,
                            aggregate_cache_creation_tokens: self.aggregate_cache_creation_tokens,
                            aggregate_cache_read_tokens: self.aggregate_cache_read_tokens,
                            tool_call_count: pending_tool_calls.len(),
                        })?;

                        if stop_reason == StopReason::ToolUse && !pending_tool_calls.is_empty() {
                            let tool_calls = std::mem::take(&mut pending_tool_calls);
                            self.push_llm_msg(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: tool_calls.clone(),
                                raw,
                            })?;
                            self.spawn_tool_tasks(tool_calls);
                        } else if raw.is_some() && accumulated_text.is_empty() {
                            // Raw present but no text and no tool calls — only
                            // reasoning.  Inject placeholder content so the message
                            // is valid for the API (both "content" and "tool_calls"
                            // are required by providers like DeepSeek).
                            let placeholder = "[response interrupted]";
                            if let Some(ref mut raw_obj) = raw
                                && raw_obj.is_object()
                            {
                                raw_obj["content"] =
                                    serde_json::Value::String(placeholder.to_string());
                            }
                            self.push_llm_msg(LLMMessage::Assistant {
                                content: placeholder.to_string(),
                                tool_calls: vec![],
                                raw,
                            })?;
                        } else if !accumulated_text.is_empty() {
                            self.push_llm_msg(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: vec![],
                                raw,
                            })?;
                        }
                        break StreamResult::Success;
                    }
                    LLMEvent::Error(error) => {
                        break StreamResult::Error(error);
                    }
                    LLMEvent::MediaGenerationStarted { media_id } => {
                        self.broadcast_msg(Message::AssistantMediaGenerating {
                            media_id: media_id.clone(),
                        })?;
                        idle_timeout
                            .as_mut()
                            .reset(tokio::time::Instant::now() + media_timeout);
                        continue;
                    }
                    LLMEvent::MediaOutput {
                        media_id,
                        relative_path,
                        media_type,
                    } => {
                        let media = MediaData::new(relative_path, media_type);
                        self.broadcast_msg(Message::AssistantMediaOutput {
                            media_id,
                            end_status: MessageEndStatus::Succeeded,
                            media: Some(media),
                        })?;
                    }
                    LLMEvent::MediaGenerationFailed { media_id } => {
                        self.broadcast_msg(Message::AssistantMediaOutput {
                            media_id,
                            end_status: MessageEndStatus::Failed,
                            media: None,
                        })?;
                    }
                }
                idle_timeout
                    .as_mut()
                    .reset(tokio::time::Instant::now() + read_timeout);
            };

            match inner_result {
                StreamResult::Success | StreamResult::Cancelled => {
                    return Ok(());
                }
                StreamResult::Timeout | StreamResult::Error(_)
                    if !got_content && attempt < max_retries =>
                {
                    let reason = match &inner_result {
                        StreamResult::Timeout => {
                            format!("request timed out after {}s", read_timeout.as_secs())
                        }
                        StreamResult::Error(msg) => msg.clone(),
                        _ => unreachable!(),
                    };
                    self.broadcast_msg(Message::LLMRetry {
                        attempt: attempt + 1,
                        max_retries,
                        reason,
                    })?;
                    // Check for cancellation between inner loop returning
                    // and the retry decision. The backoff sleep below also
                    // has its own cancel-aware select.
                    if cancel_token.is_cancelled() {
                        self.broadcast_cancelled_end()?;
                        return Ok(());
                    }
                    let backoff = Duration::from_secs(1 << attempt.min(4));
                    tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => {
                            self.broadcast_cancelled_end()?;
                            return Ok(());
                        }
                        _ = tokio::time::sleep(backoff) => {}
                    }
                }
                StreamResult::Timeout => {
                    self.broadcast_msg(Message::AssistantMessageEnd {
                        end_status: MessageEndStatus::Timeout,
                        error: None,
                        input_tokens: 0,
                        output_tokens: 0,
                        reasoning_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        aggregate_input_tokens: self.aggregate_input_tokens,
                        aggregate_output_tokens: self.aggregate_output_tokens,
                        aggregate_cache_creation_tokens: self.aggregate_cache_creation_tokens,
                        aggregate_cache_read_tokens: self.aggregate_cache_read_tokens,
                        tool_call_count: 0,
                    })?;
                    return Ok(());
                }
                StreamResult::Error(error) => {
                    self.broadcast_msg(Message::AssistantMessageEnd {
                        end_status: MessageEndStatus::Failed,
                        error: Some(error),
                        input_tokens: 0,
                        output_tokens: 0,
                        reasoning_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        aggregate_input_tokens: self.aggregate_input_tokens,
                        aggregate_output_tokens: self.aggregate_output_tokens,
                        aggregate_cache_creation_tokens: self.aggregate_cache_creation_tokens,
                        aggregate_cache_read_tokens: self.aggregate_cache_read_tokens,
                        tool_call_count: 0,
                    })?;
                    return Ok(());
                }
            }
        }

        // Unreachable: the for loop always returns via match arms above.
        Ok(())
    }

    /// Finish a turn if no tools are pending (normal path).
    fn maybe_finish_turn(&mut self) -> Result<()> {
        if !self.pending_tools.is_empty() {
            return Ok(());
        }
        self.finish_turn()
    }

    /// Force finish a turn (called from cancel path).
    fn finish_turn(&mut self) -> Result<()> {
        if self.single_turn {
            self.broadcast_msg(Message::AssistantRequestEnd {
                total_input_tokens: self.total_input_tokens,
                total_output_tokens: self.total_output_tokens,
                total_cache_creation_tokens: self.total_cache_creation_tokens,
                total_cache_read_tokens: self.total_cache_read_tokens,
            })?;
        }
        self.env.client.reset_cancel_token();
        Ok(())
    }

    /// Fill synthetic results for all pending tools. No waiting.
    fn fill_remaining_cancelled(&mut self, user_interrupted: bool) -> Result<()> {
        for id in std::mem::take(&mut self.pending_tools) {
            let raw = self
                .accumulated_tool_content
                .remove(&id)
                .unwrap_or_default();
            let content = if user_interrupted {
                if raw.is_empty() {
                    vec![ContentPart::Text(
                        "Tool execution was interrupted because the user sent a new message."
                            .into(),
                    )]
                } else {
                    let mut parts = vec![ContentPart::Text(
                        "Tool execution was interrupted because the user sent \
                         a new message. Partial result:\n"
                            .to_string(),
                    )];
                    parts.extend(raw);
                    parts
                }
            } else if raw.is_empty() {
                vec![ContentPart::Text(
                    "Tool call was cancelled due to conversation interruption.".into(),
                )]
            } else {
                let mut parts = vec![ContentPart::Text(
                    "Tool call was cancelled. Partial result:\n".into(),
                )];
                parts.extend(raw);
                parts
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
        Self {
            tool_call_id,
            loop_tx,
            defused: false,
        }
    }
    fn defuse(&mut self) {
        self.defused = true;
    }
}

impl Drop for ToolCompleteGuard {
    fn drop(&mut self) {
        if !self.defused {
            // Best-effort send — the channel is bounded so we use try_send.
            if let Err(e) = self.loop_tx.try_send(Message::ToolMessageEnd {
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
        tool_call_id: tool_call.id.clone(),
        created_at: now_millis(),
        tool_name: tool_call.name.clone(),
        tool_args: tool_call.arguments.clone(),
    })?;

    let client_clone = Arc::clone(&env.client);
    let tc_id = tool_call.id.clone();
    let client_clone2 = Arc::clone(&env.client);
    let tc_id2 = tool_call.id.clone();
    let mut scoped_pm = crate::permission::ScopedPermissionManager::new(
        &tool_call.name,
        Arc::clone(&env.permission_manager),
        Arc::new(move || {
            if let Err(e) = client_clone.notify_msg(Message::ToolRequestPermission {
                tool_call_id: tc_id.clone(),
            }) {
                tracing::error!(error = %e, "failed to send ToolRequestPermission");
            }
            if let Err(e) = client_clone.notify_msg(Message::PermissionUpdated {}) {
                tracing::error!(error = %e, "failed to send PermissionUpdated");
            }
        }),
        Arc::new(move || {
            if let Err(e) = client_clone2.notify_msg(Message::ToolPermissionApproved {
                tool_call_id: tc_id2.clone(),
            }) {
                tracing::error!(error = %e, "failed to send ToolPermissionApproved");
            }
        }),
        env.state_dir.clone(),
    );
    scoped_pm.set_cancel_token(cancel_token.clone());
    let scoped_pm_ref = scoped_pm.clone();
    let tool_ctx = ToolContext {
        cancel_token: cancel_token.clone(),
        permission: scoped_pm,
        container_config: env.container_config.clone(),
        session_dir: env.session_dir.clone(),
        supports_media: env.supports_media,
        llm: env.llm.clone(),
        model: Some(env.model.clone()),
    };
    let end_status = if let Some(tool) = tool_arc {
        tracing::debug!(tool_call_id = %tool_call.id, "tool found, starting stream");
        let mut output_stream = tool.execute(tool_ctx, tool_call.arguments.clone());
        let mut chunk_count: usize = 0;
        while let Some(chunk) = output_stream.next().await {
            tracing::debug!(
                tool_call_id = %tool_call.id,
                "tool output chunk"
            );
            let content = Arc::new(chunk.clone());
            env.client.notify_msg(Message::ToolOutputChunk {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                content: Arc::clone(&content),
            })?;
            loop_tx
                .send(Message::ToolOutputChunk {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content,
                })
                .await?;
            chunk_count += 1;
        }
        tracing::info!(
            tool_call_id = %tool_call.id,
            chunk_count,
            "tool stream finished"
        );
        if cancel_token.is_cancelled() {
            MessageEndStatus::Cancelled
        } else if scoped_pm_ref.was_denied() {
            MessageEndStatus::UserDenied
        } else {
            MessageEndStatus::Succeeded
        }
    } else {
        let error_msg = format!("Error: Tool '{}' not found", tool_call.name);
        log_and_broadcast_system_message(
            &env.client,
            SystemMessageLevel::Error,
            format!("Tool '{}' not found", tool_call.name),
        );
        let content = Arc::new(ContentPart::Text(error_msg.clone()));
        env.client.notify_msg(Message::ToolOutputChunk {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            content: Arc::clone(&content),
        })?;
        loop_tx
            .send(Message::ToolOutputChunk {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                content,
            })
            .await?;
        MessageEndStatus::Failed
    };

    env.client.unregister_tool_token(&tool_call.id);

    // If the tool was cancelled (possibly while waiting for permission), notify
    // the permission UI so it refreshes and drops any stale pending entries.
    if end_status == MessageEndStatus::Cancelled
        && let Err(e) = env.client.notify_msg(Message::PermissionUpdated {})
    {
        tracing::error!(error = %e, "failed to send PermissionUpdated on cancel");
    }

    // Broadcast ToolMessageEnd for UI
    env.client.notify_msg(Message::ToolMessageEnd {
        tool_call_id: tool_call.id.clone(),
        end_status: end_status.clone(),
        input_tokens: 0,
        output_tokens: 0,
    })?;

    // Send ToolMessageEnd to event loop
    loop_tx
        .send(Message::ToolMessageEnd {
            tool_call_id: tool_call.id.clone(),
            end_status,
            input_tokens: 0,
            output_tokens: 0,
        })
        .await?;

    guard.defuse();
    Ok(())
}

/// Spawn a background task that monitors the subagent stream: collects the first-turn
/// response, publishes results to the parent, and — if cancelled — keeps watching for
/// `UserRequestEnd` (the user typing `/done`) to recover the subagent result.
fn spawn_subagent_stream_handler(
    mut sub_stream: impl Stream<Item = Result<Arc<BroadcastMessage>, BroadcastStreamRecvError>>
    + Unpin
    + Send
    + 'static,
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
            &mut sub_stream,
            &cancel_token,
            &subagent_client,
            &parent_client,
            &subagent_conv_id,
            &tool_call_id,
            &loop_tx,
        )
        .await
        {
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
    mut llm: Box<dyn LLM>,
    loop_tx: mpsc::Sender<Message>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let params: SubAgentParams = match serde_json::from_str(&tool_call.arguments) {
        Ok(p) => p,
        Err(e) => {
            let error = format!("Error: Failed to parse subagent arguments: {}", e);
            loop_tx
                .send(Message::ToolOutputChunk {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content: Arc::new(ContentPart::Text(error)),
                })
                .await?;
            loop_tx
                .send(Message::ToolMessageEnd {
                    tool_call_id: tool_call.id,
                    end_status: MessageEndStatus::Failed,
                    input_tokens: 0,
                    output_tokens: 0,
                })
                .await?;
            return Ok(());
        }
    };

    // Collect parent's tools; include subagent tools only if depth allows nesting
    let child_depth = env.subagent_depth + 1;
    let allow_nesting = child_depth + 1 < env.max_subagent_depth;
    let subagent_tools: Vec<Arc<Tool>> = env
        .tools
        .values()
        .filter(|t| allow_nesting || t.name != "subagent")
        .cloned()
        .collect();

    // Pre-generate subagent conversation ID so we can create its state_dir
    let subagent_conv_id_pre = Uuid::new_v4().to_string();
    let subagent_state_dir = match env
        .state_dir
        .as_ref()
        .map(|d| {
            let dir = d.join(format!("subagent-{}", subagent_conv_id_pre));
            std::fs::create_dir_all(&dir)?;
            Ok::<_, anyhow::Error>(dir)
        })
        .transpose()
    {
        Ok(d) => d,
        Err(e) => {
            let error = format!("Error: Failed to create subagent state dir: {}", e);
            loop_tx
                .send(Message::ToolOutputChunk {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content: Arc::new(ContentPart::Text(error)),
                })
                .await?;
            loop_tx
                .send(Message::ToolMessageEnd {
                    tool_call_id: tool_call.id,
                    end_status: MessageEndStatus::Failed,
                    input_tokens: 0,
                    output_tokens: 0,
                })
                .await?;
            return Ok(());
        }
    };

    // Create the subagent conversation
    // Set media_dir on the cloned LLM to the subagent's own media subdir
    if let Some(ref sa_dir) = subagent_state_dir {
        llm.set_media_dir(Some(sa_dir.join("media")));
    }
    let (subagent_conv_id, subagent_client) =
        match env.conversation_manager.new_conversation_with_id(
            subagent_conv_id_pre,
            llm,
            &params.model,
            subagent_tools,
            env.chat_options.clone(),
            true, // single_turn
            child_depth,
            env.max_subagent_depth,
            subagent_state_dir,
            env.supports_media,
        ) {
            Ok(result) => result,
            Err(e) => {
                let error = format!("Error: Failed to create subagent conversation: {}", e);
                loop_tx
                    .send(Message::ToolOutputChunk {
                        tool_call_id: tool_call.id.clone(),
                        tool_name: tool_call.name.clone(),
                        content: Arc::new(ContentPart::Text(error)),
                    })
                    .await?;
                loop_tx
                    .send(Message::ToolMessageEnd {
                        tool_call_id: tool_call.id,
                        end_status: MessageEndStatus::Failed,
                        input_tokens: 0,
                        output_tokens: 0,
                    })
                    .await?;
                return Ok(());
            }
        };

    // Register subagent as a child for cascading cancellation
    env.client
        .register_child(subagent_conv_id.clone(), Arc::clone(&subagent_client));

    // Register parent mapping for /done recovery
    env.conversation_manager.register_subagent_parent(
        &subagent_conv_id,
        &env.conversation_id,
        &tool_call.id,
    );

    let task_preview = truncate_preview(
        params
            .task
            .strip_prefix(SUBAGENT_PROMPT_PREFIX)
            .unwrap_or(&params.task)
            .trim_start(),
        100,
    );
    env.client
        .notify_msg(Message::SubAgentStart {
            tool_call_id: tool_call.id.clone(),
            conversation_id: subagent_conv_id.clone(),
            description: task_preview,
        })
        .context("failed to broadcast SubAgentStart")?;

    let sub_stream = subagent_client.subscribe();

    if let Err(e) = subagent_client.send_chat(&params.task).await {
        let error = format!("Error: Failed to send task to subagent: {}", e);
        env.client
            .notify_msg(Message::SubAgentEnd {
                conversation_id: subagent_conv_id.clone(),
                end_status: MessageEndStatus::Failed,
                response: Arc::new(error.clone()),
                input_tokens: 0,
                output_tokens: 0,
            })
            .context("failed to broadcast SubAgentEnd")?;
        loop_tx
            .send(Message::ToolOutputChunk {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                content: Arc::new(ContentPart::Text(error)),
            })
            .await?;
        loop_tx
            .send(Message::ToolMessageEnd {
                tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed,
                input_tokens: 0,
                output_tokens: 0,
            })
            .await?;
        return Ok(());
    }

    spawn_subagent_stream_handler(
        sub_stream,
        cancel_token,
        subagent_client,
        env.client,
        subagent_conv_id,
        tool_call.id,
        loop_tx,
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
            loop_tx
                .send(Message::ToolOutputChunk {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content: Arc::new(ContentPart::Text(error)),
                })
                .await?;
            loop_tx
                .send(Message::ToolMessageEnd {
                    tool_call_id: tool_call.id,
                    end_status: MessageEndStatus::Failed,
                    input_tokens: 0,
                    output_tokens: 0,
                })
                .await?;
            return Ok(());
        }
    };

    let subagent_client = match env
        .conversation_manager
        .get_conversation(&params.conversation_id)
    {
        Ok(Some(client)) => client,
        Ok(None) => {
            let error = format!(
                "Error: Subagent conversation '{}' not found",
                params.conversation_id
            );
            loop_tx
                .send(Message::ToolOutputChunk {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content: Arc::new(ContentPart::Text(error)),
                })
                .await?;
            loop_tx
                .send(Message::ToolMessageEnd {
                    tool_call_id: tool_call.id,
                    end_status: MessageEndStatus::Failed,
                    input_tokens: 0,
                    output_tokens: 0,
                })
                .await?;
            return Ok(());
        }
        Err(e) => {
            let error = format!("Error: Failed to get subagent conversation: {}", e);
            loop_tx
                .send(Message::ToolOutputChunk {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content: Arc::new(ContentPart::Text(error)),
                })
                .await?;
            loop_tx
                .send(Message::ToolMessageEnd {
                    tool_call_id: tool_call.id,
                    end_status: MessageEndStatus::Failed,
                    input_tokens: 0,
                    output_tokens: 0,
                })
                .await?;
            return Ok(());
        }
    };

    // Register subagent as a child for cascading cancellation (idempotent via HashMap)
    env.client
        .register_child(params.conversation_id.clone(), Arc::clone(&subagent_client));

    // Register parent mapping for /done recovery (idempotent)
    env.conversation_manager.register_subagent_parent(
        &params.conversation_id,
        &env.conversation_id,
        &tool_call.id,
    );

    let msg_preview = truncate_preview(&params.message, 100);

    env.client
        .notify_msg(Message::SubAgentContinue {
            tool_call_id: tool_call.id.clone(),
            conversation_id: params.conversation_id.clone(),
            description: msg_preview,
        })
        .context("failed to broadcast SubAgentContinue")?;

    let sub_stream = subagent_client.subscribe_new();

    if let Err(e) = subagent_client.send_chat(&params.message).await {
        let error = format!("Error: Failed to send follow-up to subagent: {}", e);
        env.client
            .notify_msg(Message::SubAgentTurnEnd {
                conversation_id: params.conversation_id,
                end_status: MessageEndStatus::Failed,
                response: Arc::new(error.clone()),
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            })
            .context("failed to broadcast SubAgentTurnEnd")?;
        loop_tx
            .send(Message::ToolOutputChunk {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                content: Arc::new(ContentPart::Text(error)),
            })
            .await?;
        loop_tx
            .send(Message::ToolMessageEnd {
                tool_call_id: tool_call.id,
                end_status: MessageEndStatus::Failed,
                input_tokens: 0,
                output_tokens: 0,
            })
            .await?;
        return Ok(());
    }

    spawn_subagent_stream_handler(
        sub_stream,
        cancel_token,
        subagent_client,
        env.client,
        params.conversation_id,
        tool_call.id,
        loop_tx,
    );
    Ok(())
}

/// Use for the client to send chat messages and subscribe to the conversation's messages.
pub struct ConversationClient {
    /// The session's shared id generator (one per session, see
    /// `ConversationManager::ensure_id_generator`). All conversations in a
    /// session — root and subagents — mint from the same Arc, so ids are
    /// globally unique across the whole session dir.
    ids: Arc<UniqueIdGenerator>,
    /// Serializes notify_msg (push-to-msgs + broadcast) against subscribe()
    /// (history snapshot + channel subscribe) — the replay/live boundary
    /// guarantee (no dropped/duplicated message at the boundary). Id minting
    /// is lock-free and unordered (Relaxed atomic); this mutex is separate and
    /// does NOT impose id order.
    order_lock: parking_lot::Mutex<()>,
    msgs: parking_lot::RwLock<Vec<Arc<BroadcastMessage>>>,
    summary: parking_lot::RwLock<ConversationSummary>,
    input_channel_tx: mpsc::Sender<Message>,
    new_msg_notify_tx: broadcast::Sender<Arc<BroadcastMessage>>,
    tool_cancel_tokens: parking_lot::Mutex<HashMap<String, CancellationToken>>,
    /// Conversation-level cancellation token. Cancelling this cancels all child tool tokens.
    cancel_token: parking_lot::Mutex<CancellationToken>,
    /// Child subagent clients, keyed by conversation_id. Used for cascading cancellation.
    children: parking_lot::Mutex<HashMap<String, Arc<ConversationClient>>>,
}

impl ConversationClient {
    /// Reserve the next unique id without broadcasting. `notify_msg` uses it
    /// internally; the server uses it for synthetic stale-close events.
    pub fn get_unique_id(&self) -> UniqueId {
        self.ids.get_unique_id()
    }

    pub fn conversation_summary(&self) -> ConversationSummary {
        self.summary.read().clone()
    }

    pub(crate) fn set_conversation_summary(&self, summary: ConversationSummary) {
        *self.summary.write() = summary;
    }

    fn update_summary_for_message(&self, msg: &Message) {
        let mut summary = self.summary.write();
        match msg {
            Message::UserMessage {
                created_at,
                content,
                ..
            } => {
                if summary.description.is_none() {
                    summary.description = Some(truncate_preview(content, 80));
                }
                if summary.created_at.is_none() {
                    summary.created_at = Some(*created_at);
                }
                summary.last_active_at = Some(*created_at);
            }
            Message::AssistantMessageStart { created_at, .. }
            | Message::ToolMessageStart { created_at, .. }
            | Message::SubAgentInputStart { created_at, .. }
            | Message::AssistantToolCallStart { created_at, .. }
            | Message::SystemMessage { created_at, .. } => {
                if summary.created_at.is_none() {
                    summary.created_at = Some(*created_at);
                }
                summary.last_active_at = Some(*created_at);
            }
            Message::AssistantMessageEnd { .. }
            | Message::ToolMessageEnd { .. }
            | Message::SubAgentEnd { .. }
            | Message::SubAgentTurnEnd { .. }
            | Message::AssistantRequestEnd { .. }
            | Message::UserRequestEnd { .. }
            | Message::ToolCallResolved { .. } => {
                let now = now_millis();
                if summary.created_at.is_none() {
                    summary.created_at = Some(now);
                }
                summary.last_active_at = Some(now);
            }
            _ => {}
        }
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
        self.cancel_token.lock().cancel();

        // Recursively cancel all child subagent conversations
        let children = self.children.lock();
        for child in children.values() {
            child.cancel_silent();
        }
    }

    /// Register a child subagent client for cascading cancellation.
    pub fn register_child(&self, conversation_id: String, client: Arc<ConversationClient>) {
        self.children.lock().insert(conversation_id, client);
    }

    /// Broadcast a warning-level system message to subscribers.
    pub fn broadcast_system_warning(&self, message: String) {
        if let Err(e) = self.notify_msg(Message::SystemMessage {
            created_at: now_millis(),
            level: SystemMessageLevel::Warning,
            message,
        }) {
            tracing::warn!(error = %e, "failed to broadcast system warning");
        }
    }

    /// Get a clone of the current cancel token for use in `tokio::select!`.
    pub(crate) fn current_cancel_token(&self) -> CancellationToken {
        self.cancel_token.lock().clone()
    }

    /// Replace the cancel token with a fresh one so the conversation can accept new work.
    pub(crate) fn reset_cancel_token(&self) {
        *self.cancel_token.lock() = CancellationToken::new();
    }

    /// Cancel a specific tool call by its ID. Returns true if the tool was found and cancelled.
    pub fn cancel_tool(&self, tool_call_id: &str) -> bool {
        let tokens = self.tool_cancel_tokens.lock();
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
        let token = self.cancel_token.lock().child_token();
        let clone = token.clone();
        self.tool_cancel_tokens
            .lock()
            .insert(tool_call_id.to_string(), token);
        clone
    }

    /// Remove a tool's cancellation token after it completes.
    pub(crate) fn unregister_tool_token(&self, tool_call_id: &str) {
        self.tool_cancel_tokens.lock().remove(tool_call_id);
    }

    /// Send a chat to the conversation. Returns after the message is queued. The message
    /// will be sent to the LLM in the background when the current LLM response finished.
    pub async fn send_chat(&self, content: &str) -> Result<()> {
        self.input_channel_tx
            .send(Message::UserMessage {
                created_at: now_millis(),
                content: Arc::new(content.to_string()),
                media_filenames: vec![],
            })
            .await?;
        Ok(())
    }

    /// Send a chat with attached media to the conversation. Media are relative
    /// filenames from the session's `media/` directory (e.g. `"uuid.png"`).
    pub async fn send_chat_with_media(
        &self,
        content: &str,
        media_filenames: Vec<String>,
    ) -> Result<()> {
        self.input_channel_tx
            .send(Message::UserMessage {
                created_at: now_millis(),
                content: Arc::new(content.to_string()),
                media_filenames,
            })
            .await?;
        Ok(())
    }

    /// Used for conversation to notify a new message if available
    pub fn notify_msg(&self, msg: Message) -> Result<()> {
        self.update_summary_for_message(&msg);
        // Mint lock-free (Relaxed atomic — id order is not guaranteed), then
        // hold order_lock across push-to-msgs + broadcast so subscribe()'s
        // history snapshot + channel subscribe never drops or duplicates a
        // message at the replay/live boundary.
        let id = self.get_unique_id();
        let envelope = Arc::new(BroadcastMessage { id, msg });
        let _guard = self.order_lock.lock();
        self.msgs.write().push(Arc::clone(&envelope));
        self.new_msg_notify_tx.send(envelope).map_err(|e| {
            anyhow::anyhow!("failed to send msg to the notification broadcast: {e}")
        })?;
        Ok(())
    }

    /// Get a snapshot of all messages in the conversation.
    pub fn get_messages(&self) -> Vec<Arc<BroadcastMessage>> {
        self.msgs.read().clone()
    }

    /// Subscribe to the conversation's messages.
    /// This will also send all the historical messages.
    /// If the consumer lagged too far behind, it will receive BroadcastStreamRecvError
    /// then the stream continues with normal messages.
    pub fn subscribe(
        &self,
    ) -> impl Stream<Item = Result<Arc<BroadcastMessage>, BroadcastStreamRecvError>> + use<> {
        // TODO: handle error and return error in stream
        // Hold the order lock while snapshotting history and subscribing to
        // the live channel. notify_msg pushes to msgs and broadcasts under this
        // same lock, so no broadcast can slip between the two steps: every
        // message is delivered exactly once, either in the replay or on the
        // live channel. Without the lock a concurrent notify_msg could be
        // missed entirely (sent before the subscribe) or delivered twice
        // (already in the snapshot and again on the channel). This lock does
        // NOT impose id order — ids are minted lock-free with a Relaxed
        // atomic and may appear out of order.
        let _guard = self.order_lock.lock();
        let msgs = self.msgs.read().clone();
        let tx = self.new_msg_notify_tx.subscribe();
        let stream = BroadcastStream::new(tx);
        tokio_stream::iter(msgs.into_iter().map(Ok)).chain(stream)
    }

    /// Extract the latest assistant response text from this conversation's message history.
    /// Walks backward from the last `AssistantMessageEnd`, collecting `AssistantMessageChunk`s
    /// until `AssistantMessageStart` is found.
    pub fn extract_latest_response(&self) -> Option<String> {
        let msgs = self.msgs.read();
        let mut chunks = Vec::new();
        let mut found_end = false;
        for env in msgs.iter().rev() {
            match &env.msg {
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
    pub async fn send_tool_call_resolved(
        &self,
        tool_call_id: String,
        content: Arc<String>,
    ) -> Result<()> {
        self.input_channel_tx
            .send(Message::ToolCallResolved {
                tool_call_id,
                content,
            })
            .await?;
        Ok(())
    }

    /// Subscribe to only new messages (no history replay).
    /// Useful for continue_subagent to avoid reprocessing old messages.
    pub fn subscribe_new(
        &self,
    ) -> impl Stream<Item = Result<Arc<BroadcastMessage>, BroadcastStreamRecvError>> + use<> {
        let tx = self.new_msg_notify_tx.subscribe();
        BroadcastStream::new(tx)
    }

    /// Create a test-only ConversationClient with dummy channels.
    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        let (input_tx, _input_rx) = mpsc::channel(10);
        // Keep this in sync with the production broadcast capacity above so
        // tests exercise the same lag tolerance for streamed tool output.
        let (notify_tx, _) = broadcast::channel(10_000);
        ConversationClient {
            ids: Arc::new(
                UniqueIdGenerator::new(None)
                    .expect("UniqueIdGenerator::new(None) never fails (no state dir)"),
            ),
            order_lock: parking_lot::Mutex::new(()),
            msgs: parking_lot::RwLock::new(Vec::new()),
            summary: parking_lot::RwLock::new(ConversationSummary::default()),
            input_channel_tx: input_tx,
            new_msg_notify_tx: notify_tx,
            tool_cancel_tokens: parking_lot::Mutex::new(HashMap::new()),
            cancel_token: parking_lot::Mutex::new(CancellationToken::new()),
            children: parking_lot::Mutex::new(HashMap::new()),
        }
    }
}
