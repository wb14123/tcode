use std::fmt::Display;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::llm::{LLMRole, Tool, LLM};
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

    MessageChunk {
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
    }
}

pub struct ConversationManager {
}

/// Manages conversations so that any new client can attach to an existing conversation.
impl ConversationManager {
    /// Create a new conversation. The new conversation will be kept in the manager's
    /// memory until it ends.
    pub fn new_conversation(&self, llm: Box<dyn LLM>, system_prompt: &str, model: &str, tools: Vec<Arc<Tool<>>>) -> Result<Arc<Conversation>> {
        Ok(Arc::new(Conversation {
            llm,
            model: model.to_string(),
            tools,
            llm_msgs: vec![],
            input_channel_tx: mpsc::channel(10).0,
            input_channel_rx: mpsc::channel(10).1,
            msgs: RwLock::new(vec![]),
            new_msg_notify_tx: broadcast::channel(10).0,
        })) // placeholder
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

    tools: Vec<Arc<Tool>>,

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
}

/// Multi round LLM conversation. Thread and async safe.
impl Conversation {

    async fn start(&mut self) {
        while let Some(user_input) = self.input_channel_rx.recv().await {
            self.llm_msgs.push((LLMRole::User, user_input));
            self.llm.chat(self.model.as_str(), &self.tools, &self.llm_msgs);
            // TODO: consume the responses and send to msgs
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