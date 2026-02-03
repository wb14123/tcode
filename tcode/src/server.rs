use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use llm_rs::conversation::{ConversationManager, Message};
use llm_rs::llm::OpenAI;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::{ClientMessage, ServerMessage};

pub struct Server {
    socket_path: PathBuf,
    display_file: PathBuf,
    status_file: PathBuf,
    api_key: String,
    model: String,
    base_url: String,
}

impl Server {
    pub fn new(
        socket_path: PathBuf,
        display_file: PathBuf,
        status_file: PathBuf,
        api_key: String,
        model: String,
        base_url: String,
    ) -> Self {
        Self {
            socket_path,
            display_file,
            status_file,
            api_key,
            model,
            base_url,
        }
    }

    pub async fn run(&self) -> Result<()> {
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

        // Create LLM and conversation manager
        let llm = Box::new(OpenAI::new(&self.api_key, &self.base_url));
        let manager = ConversationManager::new();
        let conversation_client = manager.new_conversation(
            llm,
            "You are a helpful assistant.",
            &self.model,
            vec![],
        )?;

        // Spawn background task to write conversation events to display files
        let mut events = conversation_client.subscribe();
        let display_file = self.display_file.clone();
        let status_file = self.status_file.clone();
        let mut event_writer = tokio::spawn(async move {
            while let Some(Ok(event)) = events.next().await {
                if matches!(&*event, Message::AssistantMessageStart { .. }) {
                    tokio::fs::write(&status_file, "Streaming...").await
                        .context("Failed to write status file")?;
                }
                if matches!(&*event, Message::AssistantMessageEnd { .. }) {
                    tokio::fs::write(&status_file, "Ready").await
                        .context("Failed to write status file")?;
                }
                append_event(&display_file, &event).await
                    .context("Failed to append display event")?;
            }
            Ok::<(), anyhow::Error>(())
        });

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

async fn append_event(display_file: &PathBuf, event: &Message) -> Result<()> {
    let formatted = format_event(event);
    if formatted.is_empty() {
        return Ok(());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(display_file)
        .await?;
    file.write_all(formatted.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

fn format_event(event: &Message) -> String {
    match event {
        Message::UserMessage { content, .. } => format!("\n>>> USER:\n{}\n", content),
        Message::AssistantMessageStart { .. } => "\n>>> ASSISTANT:\n".to_string(),
        Message::AssistantMessageChunk { content, .. } => content.to_string(),
        Message::AssistantMessageEnd { .. } => "\n".to_string(),
        Message::ToolMessageStart { tool_name, tool_args, .. } => {
            format!("\n>>> TOOL: {} ({})\n", tool_name, tool_args)
        }
        Message::ToolOutputChunk { content, .. } => content.to_string(),
        Message::ToolMessageEnd { .. } => "\n".to_string(),
        _ => String::new(),
    }
}
