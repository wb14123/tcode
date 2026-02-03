use std::collections::HashMap;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::llm::{LLMEvent, LLMMessage, StopReason, LLM};
use crate::tool::Tool;
use anyhow::Result;
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

    AssistantMessageEnd {
        msg_id: MessageID,
        end_status: MessageEndStatus,
        error: Option<String>,
        input_tokens: i32,
        output_tokens: i32,
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
        input_tokens: i32,
        output_tokens: i32,
    },

    // assistant request to end the conversation, useful for sub agents
    AssistantRequestEnd {
        total_input_tokens: i32,
        total_output_tokens: i32,
    },
}

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
    pub fn new() -> Self {
        Self::default()
    }
    /// Create a new conversation. The new conversation will be kept in the manager's
    /// memory until it ends.
    pub fn new_conversation(
        &self,
        mut llm: Box<dyn LLM>,
        system_prompt: &str,
        model: &str,
        tools: Vec<Arc<Tool>>,
    ) -> Result<Arc<ConversationClient>> {
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
        let mut conversation = Conversation {
            id: conversation_id.clone(),
            llm,
            model: model.to_string(),
            tools: tools_map,
            llm_msgs,
            input_channel_rx: input_rx,
            msg_id_counter: AtomicI32::new(0),
            total_input_tokens: 0,
            total_output_tokens: 0,
            conversation_client: {
                Arc::new(ConversationClient {
                    msgs: RwLock::new(Vec::new()),
                    input_channel_tx: input_tx,
                    new_msg_notify_tx: notify_tx,
                })
            }
        };
        let client = &(&conversation).conversation_client.clone();
        let task = tokio::spawn(async move {
            conversation.start().await;
        });
        self.conversations
            .write()
            .map_err(|e| anyhow::anyhow!("failed to acquire conversations write lock: {e}"))?
            .insert(conversation_id, (client.clone(), task));
        Ok(client.clone())
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
        }
    }

    /// Call the LLM and handle the response.
    /// It handles the tool call and continues the loop when there is no tool call request anymore.
    async fn call_llm(&mut self) {
        loop {
            let mut response_stream =
                self.llm
                    .chat(self.model.as_str(), &self.llm_msgs);
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
                    LLMEvent::ToolCall(tool_call) => {
                        pending_tool_calls.push(tool_call);
                    }
                    LLMEvent::MessageEnd {
                        stop_reason,
                        input_tokens,
                        output_tokens,
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
                        });

                        // Handle tool calls if any
                        if stop_reason == StopReason::ToolUse && !pending_tool_calls.is_empty() {
                            // Add assistant message with tool calls to llm_msgs
                            let tool_calls = std::mem::take(&mut pending_tool_calls);
                            self.llm_msgs.push(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: tool_calls.clone(),
                            });
                            self.execute_tool_calls(tool_calls).await;
                            should_continue = true;
                        } else if !accumulated_text.is_empty() {
                            // Add assistant response to llm_msgs for context (no tool calls)
                            self.llm_msgs.push(LLMMessage::Assistant {
                                content: accumulated_text.clone(),
                                tool_calls: vec![],
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
            let tool_msg_id = self.next_msg_id();

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
                // Execute the tool and stream output chunks
                let mut output_stream = tool.execute(tool_call.arguments.clone());
                let mut result_parts = Vec::new();
                while let Some(chunk) = output_stream.next().await {
                    // Broadcast each chunk immediately with its own msg_id
                    self.broadcast_msg(Message::ToolOutputChunk {
                        msg_id: self.next_msg_id(),
                        tool_call_id: tool_call.id.clone(),
                        tool_name: tool_call.name.clone(),
                        content: Arc::new(chunk.clone()),
                    });
                    result_parts.push(chunk);
                }
                (MessageEndStatus::Succeeded, result_parts.join(""))
            } else {
                let error_msg = format!("Error: Tool '{}' not found", tool_call.name);
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
