use std::collections::HashMap;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::llm::{ChatOptions, LLMEvent, LLMMessage, ModelInfo, StopReason, LLM};
use crate::tool::Tool;
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

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
struct GetSubAgentLogsParams {
    /// The conversation ID of the subagent (returned from the subagent tool)
    conversation_id: String,
}

/// Create the `subagent` tool with a dynamic description listing available models.
pub fn create_subagent_tool(model_descriptions: &[ModelInfo]) -> Tool {
    let models_list: Vec<String> = model_descriptions
        .iter()
        .map(|m| format!("  - `{}`: {}", m.id, m.description))
        .collect();

    let description = format!(
        "Spawn a subagent to handle a task in its own context window. \
         The subagent has access to all tools (except subagent and get_subagent_logs) \
         and will return its final answer. Use this for tasks that produce large outputs \
         (web fetches, research, multi-step tool use) so the results are summarized \
         in the subagent's context rather than consuming your context window.\n\n\
         Available models:\n{}",
        models_list.join("\n")
    );

    let schema = schemars::schema_for!(SubAgentParams);
    Tool::new_sentinel("subagent", description, schema)
}

/// Create the `get_subagent_logs` tool.
pub fn create_get_subagent_logs_tool() -> Tool {
    let schema = schemars::schema_for!(GetSubAgentLogsParams);
    Tool::new_sentinel(
        "get_subagent_logs",
        "Retrieve the raw conversation log of a completed subagent for debugging. \
         Returns the full message history of the subagent conversation.",
        schema,
    )
}

// ============================================================================
// ConversationManager
// ============================================================================

pub struct ConversationManager {
    conversations: RwLock<HashMap<String, (Arc<ConversationClient>, JoinHandle<()>)>>,
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
        mut llm: Box<dyn LLM>,
        system_prompt: &str,
        model: &str,
        tools: Vec<Arc<Tool>>,
        chat_options: ChatOptions,
        single_turn: bool,
        subagent_max_iterations: usize,
    ) -> Result<(String, Arc<ConversationClient>)> {
        // Register tools with the LLM for caching
        llm.register_tools(tools.clone());

        // Convert tools list to HashMap for O(1) lookup during execution
        let tools_map: HashMap<String, Arc<Tool>> = tools
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect();

        let (input_tx, input_rx) = mpsc::channel(10);
        let (notify_tx, _) = broadcast::channel(100);
        let llm_msgs = if system_prompt.is_empty() {
            vec![]
        } else {
            vec![LLMMessage::System(system_prompt.to_string())]
        };
        let conversation_id = Uuid::new_v4().to_string();
        let conversation = Conversation {
            id: conversation_id.clone(),
            llm,
            model: model.to_string(),
            tools: tools_map,
            llm_msgs,
            input_channel_rx: input_rx,
            msg_id_counter: AtomicI32::new(0),
            total_input_tokens: 0,
            total_output_tokens: 0,
            chat_options,
            conversation_client: {
                Arc::new(ConversationClient {
                    msgs: RwLock::new(Vec::new()),
                    input_channel_tx: input_tx,
                    new_msg_notify_tx: notify_tx,
                })
            },
            conversation_manager: Arc::clone(self),
            single_turn,
            subagent_max_iterations,
        };
        let client = conversation.conversation_client.clone();
        let task = tokio::spawn(async move {
            let mut conv = conversation;
            conv.start().await;
        });
        self.conversations
            .write()
            .map_err(|e| anyhow::anyhow!("failed to acquire conversations write lock: {e}"))?
            .insert(conversation_id.clone(), (client.clone(), task));
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
}

// ============================================================================
// Conversation
// ============================================================================

pub struct Conversation {
    pub id: String,

    /// LLM API.
    llm: Box<dyn LLM>,

    model: String,

    /// Tools available for the conversation, keyed by tool name for O(1) lookup.
    tools: HashMap<String, Arc<Tool>>,

    /// LLM messages so far. Used to keep tracking the current messages and send the next message
    /// to LLM.
    llm_msgs: Vec<LLMMessage>,

    conversation_client: Arc<ConversationClient>,

    /// Chat input receiver. Used for Conversation to poll the messages.
    input_channel_rx: mpsc::Receiver<String>,

    /// Message ID counter for generating unique message IDs.
    msg_id_counter: AtomicI32,

    /// Accumulated token usage.
    total_input_tokens: i32,
    total_output_tokens: i32,

    /// Chat options (includes reasoning config) for this conversation.
    chat_options: ChatOptions,

    /// Reference to the ConversationManager for creating subagent conversations.
    conversation_manager: Arc<ConversationManager>,

    /// When true, the conversation exits after one user message + LLM response cycle.
    single_turn: bool,

    /// Maximum number of LLM call iterations for subagent conversations.
    subagent_max_iterations: usize,
}

/// Multi round LLM conversation. Thread and async safe.
impl Conversation {
    fn next_msg_id(&self) -> MessageID {
        self.msg_id_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    fn broadcast_msg(&self, msg: Message) {
        // TODO: also broadcast error
        let _ = self.conversation_client.notify_msg(msg);
    }

    /// Start the worker loop for the conversation. Should only be called once.
    async fn start(&mut self) {
        while let Some(user_input) = self.input_channel_rx.recv().await {
            let user_msg_id = self.next_msg_id();
            self.broadcast_msg(Message::UserMessage {
                msg_id: user_msg_id,
                created_at: now_millis(),
                content: Arc::new(user_input.clone()),
            });

            self.llm_msgs.push(LLMMessage::User(user_input));
            self.call_llm().await;

            // Single-turn conversations exit after one cycle
            if self.single_turn {
                self.broadcast_msg(Message::AssistantRequestEnd {
                    total_input_tokens: self.total_input_tokens,
                    total_output_tokens: self.total_output_tokens,
                });
                break;
            }
        }
    }

    /// Call the LLM and handle the response.
    /// It handles the tool call and continues the loop when there is no tool call request anymore.
    async fn call_llm(&mut self) {
        let max_iterations = if self.single_turn { self.subagent_max_iterations } else { usize::MAX };
        let mut iteration = 0;

        loop {
            iteration += 1;
            if iteration > max_iterations {
                tracing::warn!(
                    conversation_id = %self.id,
                    max_iterations,
                    "subagent hit max iterations limit"
                );
                self.broadcast_msg(Message::SystemMessage {
                    msg_id: self.next_msg_id(),
                    created_at: now_millis(),
                    level: SystemMessageLevel::Warning,
                    message: format!("Subagent reached maximum iterations limit ({})", max_iterations),
                });
                break;
            }

            let mut response_stream =
                self.llm
                    .chat(self.model.as_str(), &self.llm_msgs, &self.chat_options);
            let mut accumulated_text = String::new();
            let mut pending_tool_calls = Vec::new();
            let mut should_continue = false;

            // Broadcast assistant message start
            self.broadcast_msg(Message::AssistantMessageStart {
                msg_id: self.next_msg_id(),
                created_at: now_millis(),
            });

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
                        });
                    }
                    LLMEvent::ThinkingDelta(text) => {
                        // Reasoning is streamed for display; raw field handles round-tripping
                        self.broadcast_msg(Message::AssistantThinkingChunk {
                            msg_id: self.next_msg_id(),
                            content: Arc::new(text),
                        });
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
                        });

                        // Handle tool calls if any
                        if stop_reason == StopReason::ToolUse && !pending_tool_calls.is_empty() {
                            // Add assistant message with tool calls to llm_msgs
                            let tool_calls = std::mem::take(&mut pending_tool_calls);
                            self.llm_msgs.push(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: tool_calls.clone(),
                                raw: raw.clone(),
                            });
                            self.execute_tool_calls(tool_calls).await;
                            should_continue = true;
                        } else if !accumulated_text.is_empty() || raw.is_some() {
                            // Add assistant response to llm_msgs for context
                            self.llm_msgs.push(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: vec![],
                                raw: raw.clone(),
                            });
                        }
                    }
                    LLMEvent::Error(error) => {
                        self.broadcast_msg(Message::AssistantMessageEnd {
                            msg_id: self.next_msg_id(),
                            end_status: MessageEndStatus::Failed,
                            error: Some(error),
                            input_tokens: 0,
                            output_tokens: 0,
                            reasoning_tokens: 0,
                        });
                        return;
                    }
                }
            }

            if !should_continue {
                break;
            }
        }
    }

    async fn execute_tool_calls(&mut self, tool_calls: Vec<crate::llm::ToolCall>) {
        for tool_call in tool_calls {
            // Intercept subagent and get_subagent_logs tool calls
            match tool_call.name.as_str() {
                "subagent" => {
                    self.execute_subagent(tool_call).await;
                    continue;
                }
                "get_subagent_logs" => {
                    self.execute_get_subagent_logs(tool_call).await;
                    continue;
                }
                _ => {}
            }

            let tool_msg_id = self.next_msg_id();

            tracing::info!(
                tool_call_id = %tool_call.id,
                tool_name = %tool_call.name,
                args = %tool_call.arguments,
                "executing tool call"
            );

            // Broadcast tool start
            self.broadcast_msg(Message::ToolMessageStart {
                msg_id: tool_msg_id,
                tool_call_id: tool_call.id.clone(),
                created_at: now_millis(),
                tool_name: tool_call.name.clone(),
                tool_args: tool_call.arguments.clone(),
            });

            // Look up and execute the tool
            let (end_status, tool_result) = if let Some(tool) = self.tools.get(&tool_call.name) {
                tracing::debug!(tool_call_id = %tool_call.id, "tool found, starting stream");
                // Execute the tool and stream output chunks
                let mut output_stream = tool.execute(tool_call.arguments.clone());
                let mut result_parts = Vec::new();
                while let Some(chunk) = output_stream.next().await {
                    tracing::debug!(
                        tool_call_id = %tool_call.id,
                        chunk_len = chunk.len(),
                        "tool output chunk"
                    );
                    // Broadcast each chunk immediately with its own msg_id
                    self.broadcast_msg(Message::ToolOutputChunk {
                        msg_id: self.next_msg_id(),
                        tool_call_id: tool_call.id.clone(),
                        tool_name: tool_call.name.clone(),
                        content: Arc::new(chunk.clone()),
                    });
                    result_parts.push(chunk);
                }
                tracing::info!(
                    tool_call_id = %tool_call.id,
                    result_len = result_parts.iter().map(|s| s.len()).sum::<usize>(),
                    "tool stream finished"
                );
                (MessageEndStatus::Succeeded, result_parts.join(""))
            } else {
                let error_msg = format!("Error: Tool '{}' not found", tool_call.name);
                tracing::error!(
                    tool_call_id = %tool_call.id,
                    tool_name = %tool_call.name,
                    "tool not found"
                );
                // Broadcast the error as a chunk too
                self.broadcast_msg(Message::ToolOutputChunk {
                    msg_id: self.next_msg_id(),
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content: Arc::new(error_msg.clone()),
                });
                (MessageEndStatus::Failed, error_msg)
            };

            self.broadcast_msg(Message::ToolMessageEnd {
                msg_id: self.next_msg_id(),
                tool_call_id: tool_call.id.clone(),
                end_status,
                input_tokens: 0,
                output_tokens: 0,
            });

            // Push tool result with tool_call_id for proper API format
            self.llm_msgs.push(LLMMessage::ToolResult {
                tool_call_id: tool_call.id,
                content: tool_result,
            });
        }
    }

    /// Execute a subagent tool call. Creates a new conversation via ConversationManager,
    /// sends the task, collects the final answer, and returns it as a tool result.
    async fn execute_subagent(&mut self, tool_call: crate::llm::ToolCall) {
        let params: SubAgentParams = match serde_json::from_str(&tool_call.arguments) {
            Ok(p) => p,
            Err(e) => {
                let error = format!("Error: Failed to parse subagent arguments: {}", e);
                self.llm_msgs.push(LLMMessage::ToolResult {
                    tool_call_id: tool_call.id,
                    content: error,
                });
                return;
            }
        };

        // Collect parent's tools, excluding subagent and get_subagent_logs
        let subagent_tools: Vec<Arc<Tool>> = self.tools.values()
            .filter(|t| t.name != "subagent" && t.name != "get_subagent_logs")
            .cloned()
            .collect();

        // Create a new LLM instance for the subagent
        let subagent_llm = self.llm.clone_box();

        // Create the subagent conversation
        let (subagent_conv_id, subagent_client) = match self.conversation_manager.new_conversation(
            subagent_llm,
            "You are a helpful assistant performing a specific task. Complete the task and provide a clear, concise answer.",
            &params.model,
            subagent_tools,
            self.chat_options.clone(),
            true, // single_turn
            self.subagent_max_iterations,
        ) {
            Ok(result) => result,
            Err(e) => {
                let error = format!("Error: Failed to create subagent conversation: {}", e);
                self.llm_msgs.push(LLMMessage::ToolResult {
                    tool_call_id: tool_call.id,
                    content: error,
                });
                return;
            }
        };

        // Broadcast SubAgentStart to parent
        let task_preview = if params.task.len() > 100 {
            format!("{}...", &params.task[..100])
        } else {
            params.task.clone()
        };
        self.broadcast_msg(Message::SubAgentStart {
            msg_id: self.next_msg_id(),
            conversation_id: subagent_conv_id.clone(),
            description: task_preview,
        });

        // Subscribe to subagent messages before sending the task
        let mut sub_stream = subagent_client.subscribe();

        // Send the task to the subagent
        if let Err(e) = subagent_client.send_chat(&params.task).await {
            let error = format!("Error: Failed to send task to subagent: {}", e);
            self.broadcast_msg(Message::SubAgentEnd {
                msg_id: self.next_msg_id(),
                conversation_id: subagent_conv_id,
                end_status: MessageEndStatus::Failed,
                response: Arc::new(error.clone()),
                input_tokens: 0,
                output_tokens: 0,
            });
            self.llm_msgs.push(LLMMessage::ToolResult {
                tool_call_id: tool_call.id,
                content: error,
            });
            return;
        }

        // Collect AssistantMessageChunk text until AssistantRequestEnd
        let mut accumulated_text = String::new();
        let mut sub_input_tokens = 0i32;
        let mut sub_output_tokens = 0i32;
        let mut end_status = MessageEndStatus::Succeeded;

        while let Some(result) = sub_stream.next().await {
            let msg = match result {
                Ok(msg) => msg,
                Err(_) => continue, // skip lagged messages
            };

            match &*msg {
                Message::AssistantMessageChunk { content, .. } => {
                    accumulated_text.push_str(content);
                }
                Message::AssistantMessageEnd { end_status: status, error, .. } => {
                    if matches!(status, MessageEndStatus::Failed) {
                        if let Some(err) = error {
                            // If the subagent had an error, include it
                            if accumulated_text.is_empty() {
                                accumulated_text = format!("Error: Subagent failed: {}", err);
                                end_status = MessageEndStatus::Failed;
                            }
                        }
                    }
                }
                Message::AssistantRequestEnd { total_input_tokens, total_output_tokens } => {
                    sub_input_tokens = *total_input_tokens;
                    sub_output_tokens = *total_output_tokens;
                    break;
                }
                _ => {} // ignore other messages
            }
        }

        // Use the accumulated text as the tool result
        let result_text = if accumulated_text.is_empty() {
            format!("Subagent completed but produced no output. Conversation ID: {}", subagent_conv_id)
        } else {
            accumulated_text
        };

        // Broadcast SubAgentEnd to parent (includes the response)
        self.broadcast_msg(Message::SubAgentEnd {
            msg_id: self.next_msg_id(),
            conversation_id: subagent_conv_id.clone(),
            end_status,
            response: Arc::new(result_text.clone()),
            input_tokens: sub_input_tokens,
            output_tokens: sub_output_tokens,
        });

        self.llm_msgs.push(LLMMessage::ToolResult {
            tool_call_id: tool_call.id,
            content: result_text,
        });
    }

    /// Execute get_subagent_logs tool call. Reads the subagent's conversation messages.
    async fn execute_get_subagent_logs(&mut self, tool_call: crate::llm::ToolCall) {
        let params: GetSubAgentLogsParams = match serde_json::from_str(&tool_call.arguments) {
            Ok(p) => p,
            Err(e) => {
                let error = format!("Error: Failed to parse get_subagent_logs arguments: {}", e);
                self.llm_msgs.push(LLMMessage::ToolResult {
                    tool_call_id: tool_call.id,
                    content: error,
                });
                return;
            }
        };

        let result = match self.conversation_manager.get_conversation(&params.conversation_id) {
            Ok(Some(client)) => {
                let msgs = client.get_messages();
                // Format messages into a readable log
                let mut log = String::new();
                for msg in &msgs {
                    match &**msg {
                        Message::UserMessage { content, .. } => {
                            log.push_str(&format!("[User] {}\n", content));
                        }
                        Message::AssistantMessageChunk { content, .. } => {
                            log.push_str(content);
                        }
                        Message::AssistantMessageStart { .. } => {
                            log.push_str("[Assistant] ");
                        }
                        Message::AssistantMessageEnd { input_tokens, output_tokens, .. } => {
                            log.push_str(&format!("\n[tokens: {} in, {} out]\n", input_tokens, output_tokens));
                        }
                        Message::ToolMessageStart { tool_name, tool_args, .. } => {
                            log.push_str(&format!("[Tool: {} args: {}]\n", tool_name, tool_args));
                        }
                        Message::ToolOutputChunk { content, .. } => {
                            log.push_str(&format!("[Tool output] {}\n", content));
                        }
                        Message::ToolMessageEnd { .. } => {
                            log.push_str("[Tool end]\n");
                        }
                        Message::SystemMessage { message, .. } => {
                            log.push_str(&format!("[System] {}\n", message));
                        }
                        _ => {}
                    }
                }
                if log.is_empty() {
                    "No messages found for this conversation.".to_string()
                } else {
                    log
                }
            }
            Ok(None) => {
                format!("Error: Conversation '{}' not found", params.conversation_id)
            }
            Err(e) => {
                format!("Error: Failed to get conversation: {}", e)
            }
        };

        self.llm_msgs.push(LLMMessage::ToolResult {
            tool_call_id: tool_call.id,
            content: result,
        });
    }

    /// Stop the current LLM response and all the messages in the queue.
    pub async fn break_conversation(&self) -> Result<()> {
        Ok(()) // placeholder
    }
}


/// Use for the client to send chat messages and subscribe to the conversation's messages.
pub struct ConversationClient {
    msgs: RwLock<Vec<Arc<Message>>>,
    input_channel_tx: mpsc::Sender<String>,
    new_msg_notify_tx: broadcast::Sender<Arc<Message>>,
}

impl ConversationClient {
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
}
