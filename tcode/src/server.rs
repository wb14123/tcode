use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use llm_rs::conversation::ConversationManager;
use llm_rs::llm::OpenAI;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::{ClientMessage, ServerMessage};

pub struct Server {
    socket_path: PathBuf,
    api_key: String,
    model: String,
    base_url: String,
}

impl Server {
    pub fn new(socket_path: PathBuf, api_key: String, model: String, base_url: String) -> Self {
        Self {
            socket_path,
            api_key,
            model,
            base_url,
        }
    }

    pub async fn run(&self) -> Result<()> {
        // Clean up existing socket file
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        // Create LLM and conversation manager
        let llm = Box::new(OpenAI::new(&self.api_key, &self.base_url));
        let manager = ConversationManager::new();
        let conversation_client = manager.new_conversation(
            llm,
            "You are a helpful assistant.",
            &self.model,
            vec![],
        )?;

        // Shutdown signal
        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        let shutdown_tx = Arc::new(shutdown_tx);
        let socket_path = self.socket_path.clone();

        // Accept loop
        let mut shutdown_rx = shutdown_tx.subscribe();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => break,
                result = listener.accept() => {
                    let (stream, _) = result?;
                    let conv_client = Arc::clone(&conversation_client);
                    let shutdown_tx = Arc::clone(&shutdown_tx);
                    tokio::spawn(handle_client(stream, conv_client, shutdown_tx));
                }
            }
        }

        let _ = std::fs::remove_file(&socket_path);
        Ok(())
    }
}

async fn handle_client(
    stream: UnixStream,
    conv_client: Arc<llm_rs::conversation::ConversationClient>,
    shutdown_tx: Arc<broadcast::Sender<()>>,
) {
    let shutdown_rx = shutdown_tx.subscribe();
    let _ = handle_client_inner(stream, conv_client, shutdown_tx, shutdown_rx).await;
}

async fn handle_client_inner(
    stream: UnixStream,
    conv_client: Arc<llm_rs::conversation::ConversationClient>,
    shutdown_tx: Arc<broadcast::Sender<()>>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let framed = Framed::new(stream, LengthDelimitedCodec::new());
    let (mut sink, mut stream) = framed.split();

    send_msg(&mut sink, &ServerMessage::Status { message: "Connected".into() }).await?;

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.recv() => break,
            result = stream.next() => {
                let Some(Ok(bytes)) = result else { break };
                let Ok(msg) = serde_json::from_slice::<ClientMessage>(&bytes) else { continue };

                match msg {
                    ClientMessage::Subscribe => {
                        send_msg(&mut sink, &ServerMessage::Status { message: "Ready".into() }).await?;

                        let mut events = conv_client.subscribe();

                        loop {
                            tokio::select! {
                                biased;
                                _ = shutdown_rx.recv() => return Ok(()),
                                event_result = events.next() => {
                                    let Some(Ok(event)) = event_result else { break };

                                    if matches!(&*event, llm_rs::conversation::Message::AssistantMessageStart { .. }) {
                                        send_msg(&mut sink, &ServerMessage::Status { message: "Streaming...".into() }).await?;
                                    }
                                    if matches!(&*event, llm_rs::conversation::Message::AssistantMessageEnd { .. }) {
                                        send_msg(&mut sink, &ServerMessage::Status { message: "Ready".into() }).await?;
                                    }

                                    send_msg(&mut sink, &ServerMessage::Event((*event).clone())).await?;
                                }
                            }
                        }
                    }
                    ClientMessage::SendMessage { content } => {
                        let _ = conv_client.send_chat(&content).await;
                        send_msg(&mut sink, &ServerMessage::Ack).await?;
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
