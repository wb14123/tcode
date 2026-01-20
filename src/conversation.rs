use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, RwLock};
use std::sync::atomic::AtomicI32;
use std::time::Instant;

use crate::llm::{LLM, LLMEvent, LLMRole, StopReason};
use crate::tool::Tool;
use anyhow::Result;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use tokio_stream::{Stream, StreamExt};

type MessageID = i32;

pub enum MessageEndStatus {
    SUCCEEDED,
    FAILED,
    CANCELLED,
    TIMEOUT,
}

pub enum Message {
    UserMessage {
        msg_id: MessageID,
        created_at: Instant,
        content: Arc<String>,
    },

    AssistantMessageStart {
        msg_id: MessageID,
        created_at: Instant,
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
        created_at: Instant,
        tool_name: String,
        tool_args: Box<dyn Display + Send + Sync>,
    },

    ToolOutputChunk {
        msg_id: MessageID,
        tool_name: String,
        content: Arc<String>,
    },

    ToolMessageEnd {
        msg_id: MessageID,
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

pub struct ConversationManager {}

/// Manages conversations so that any new client can attach to an existing conversation.
impl ConversationManager {
    /// Create a new conversation. The new conversation will be kept in the manager's
    /// memory until it ends.
    pub fn new_conversation(
        &self,
        llm: Box<dyn LLM>,
        system_prompt: &str,
        model: &str,
        tools: HashMap<String, Arc<Tool>>,
    ) -> Result<Arc<Conversation>> {
        let (input_tx, input_rx) = mpsc::channel(10);
        let (notify_tx, _) = broadcast::channel(100);
        let llm_msgs = if system_prompt.is_empty() {
            vec![]
        } else {
            vec![(LLMRole::System, system_prompt.to_string())]
        };
        Ok(Arc::new(Conversation {
            llm,
            model: model.to_string(),
            tools,
            llm_msgs,
            input_channel_tx: input_tx,
            input_channel_rx: input_rx,
            msgs: RwLock::new(vec![]),
            new_msg_notify_tx: notify_tx,
            msg_id_counter: AtomicI32::new(0),
            total_input_tokens: 0,
            total_output_tokens: 0,
        }))
    }

    /// Get a conversation by its id. It will try to load it from the manager's memory.
    /// If not found, load it from storage and put into the manager's memory.
    pub fn get_conversation(&self, conversation_id: &str) -> Option<Arc<Conversation>> {
        None // placeholder
    }

    /// Remove the conversation from the manager's memory. The conversation should be
    /// cleared if there is no reference to the Arc anymore.
    pub fn end_conversation(&self, conversation_id: &str) -> Result<()> {
        Ok(()) // placeholder
    }
}

pub struct Conversation {
    /// LLM API.
    llm: Box<dyn LLM>,

    model: String,

    /// Tools available for the conversation, keyed by tool name for O(1) lookup.
    tools: HashMap<String, Arc<Tool>>,

    /// LLM messages so far. Used to keep tracking the current messages and send the next message
    /// to LLM.
    llm_msgs: Vec<(LLMRole, String)>,

    /// Chat input queue sender. Used for client to send a chat message.
    input_channel_tx: mpsc::Sender<String>,

    /// Chat input receiver. Used for Conversation to poll the messages.
    input_channel_rx: mpsc::Receiver<String>,

    /// Chat messages history.
    msgs: RwLock<Vec<Arc<Message>>>,

    new_msg_notify_tx: broadcast::Sender<Arc<Message>>,

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
        let msg = Arc::new(msg);
        self.msgs.write().unwrap().push(Arc::clone(&msg));
        // Ignore send errors - happens when no subscribers
        let _ = self.new_msg_notify_tx.send(msg);
    }

    /// Start the worker loop for the conversation. Should only be called once.
    async fn start(&mut self) {
        while let Some(user_input) = self.input_channel_rx.recv().await {
            // Create and broadcast user message
            let user_msg_id = self.next_msg_id();
            self.broadcast_msg(Message::UserMessage {
                msg_id: user_msg_id,
                created_at: Instant::now(),
                content: Arc::new(user_input.clone()),
            });

            self.llm_msgs.push((LLMRole::User, user_input));
            self.call_llm().await;
        }
    }

    /// Call the LLM and handle the response.
    /// It handles the tool call and continues the loop when there is no tool call request anymore.
    async fn call_llm(&mut self) {
        loop {
            let mut response_stream =
                self.llm
                    .chat(self.model.as_str(), &self.tools, &self.llm_msgs);
            let mut accumulated_text = String::new();
            let mut pending_tool_calls = Vec::new();
            let mut should_continue = false;

            // Broadcast assistant message start
            self.broadcast_msg(Message::AssistantMessageStart {
                msg_id: self.next_msg_id(),
                created_at: Instant::now(),
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
                                MessageEndStatus::FAILED,
                                Some("Response truncated: maximum token limit reached".to_string()),
                            )
                        } else {
                            (MessageEndStatus::SUCCEEDED, None)
                        };

                        self.broadcast_msg(Message::AssistantMessageEnd {
                            msg_id: self.next_msg_id(),
                            end_status,
                            error,
                            input_tokens,
                            output_tokens,
                        });

                        // Add assistant response to llm_msgs for context
                        if !accumulated_text.is_empty() {
                            self.llm_msgs
                                .push((LLMRole::Assistant, accumulated_text.clone()));
                        }

                        // Handle tool calls if any
                        if stop_reason == StopReason::ToolUse && !pending_tool_calls.is_empty() {
                            self.execute_tool_calls(&pending_tool_calls).await;
                            should_continue = true;
                        }
                    }
                    LLMEvent::Error(error) => {
                        self.broadcast_msg(Message::AssistantMessageEnd {
                            msg_id: self.next_msg_id(),
                            end_status: MessageEndStatus::FAILED,
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

    async fn execute_tool_calls(&mut self, tool_calls: &[crate::llm::ToolCall]) {
        for tool_call in tool_calls {
            let tool_msg_id = self.next_msg_id();

            // Broadcast tool start
            self.broadcast_msg(Message::ToolMessageStart {
                msg_id: tool_msg_id,
                created_at: Instant::now(),
                tool_name: tool_call.name.clone(),
                tool_args: Box::new(tool_call.arguments.clone()),
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
                        tool_name: tool_call.name.clone(),
                        content: Arc::new(chunk.clone()),
                    });
                    result_parts.push(chunk);
                }
                (MessageEndStatus::SUCCEEDED, result_parts.join(""))
            } else {
                let error_msg = format!("Error: Tool '{}' not found", tool_call.name);
                // Broadcast the error as a chunk too
                self.broadcast_msg(Message::ToolOutputChunk {
                    msg_id: self.next_msg_id(),
                    tool_name: tool_call.name.clone(),
                    content: Arc::new(error_msg.clone()),
                });
                (MessageEndStatus::FAILED, error_msg)
            };

            self.llm_msgs.push((LLMRole::Tool, tool_result));

            // Broadcast tool end
            self.broadcast_msg(Message::ToolMessageEnd {
                msg_id: self.next_msg_id(),
                end_status,
                input_tokens: 0,
                output_tokens: 0,
            });
        }
    }

    /// Send a chat to the conversation. Returns after the message is queued. The message
    /// will be sent to the LLM in the background when the current LLM response finished.
    pub async fn send_chat(&self, content: &str) -> Result<()> {
        self.input_channel_tx.send(content.to_string()).await?;
        Ok(())
    }

    /// Stop the current LLM response and all the messages in the queue.
    pub async fn break_conversation(&self) -> Result<()> {
        Ok(()) // placeholder
    }

    /// Subscribe to the conversation's messages.
    /// This will also send all the historical messages.
    /// If the consumer lagged too far behind, it will receive BroadcastStreamRecvError
    /// then the stream continues with normal messages.
    pub fn subscribe(&self) -> impl Stream<Item = Result<Arc<Message>, BroadcastStreamRecvError>> {
        let msgs = self.msgs.read().unwrap();
        let tx = self.new_msg_notify_tx.subscribe();
        let stream = BroadcastStream::new(tx);
        tokio_stream::iter(msgs.clone().into_iter().map(Ok)).chain(stream)
    }
}
