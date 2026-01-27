use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt as FuturesStreamExt};
use llm_rs::conversation::ConversationManager;
use llm_rs::llm::OpenAI;
use tokio::net::{UnixListener, UnixStream};
use tokio_stream::StreamExt as TokioStreamExt;
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
        println!("Server listening on {:?}", self.socket_path);
        println!("Using model: {}", self.model);
        println!("Base URL: {}", self.base_url);

        // Create LLM and conversation manager
        let llm = Box::new(OpenAI::new(&self.api_key, &self.base_url));
        let manager = ConversationManager::new();
        let conversation_client = manager.new_conversation(
            llm,
            "You are a helpful assistant.",
            &self.model,
            vec![], // No tools for PoC
        )?;

        println!("Conversation created. Waiting for connections...\n");

        loop {
            let (stream, _) = listener.accept().await?;
            println!("[Server] Client connected");

            let conv_client = Arc::clone(&conversation_client);
            tokio::spawn(async move {
                if let Err(e) = handle_client(stream, conv_client).await {
                    eprintln!("[Server] Client error: {}", e);
                }
                println!("[Server] Client disconnected");
            });
        }
    }
}

async fn handle_client(
    stream: UnixStream,
    conv_client: Arc<llm_rs::conversation::ConversationClient>,
) -> Result<()> {
    let framed = Framed::new(stream, LengthDelimitedCodec::new());
    let (mut sink, mut stream) = framed.split();

    while let Some(result) = FuturesStreamExt::next(&mut stream).await {
        let bytes = result?;
        let msg: ClientMessage = serde_json::from_slice(&bytes)?;

        match msg {
            ClientMessage::Subscribe => {
                println!("[Server] Client subscribed to events");
                // Subscribe to llm-rs events and forward to client
                let mut events = conv_client.subscribe();
                println!("[Server] Subscription created, waiting for events...");

                while let Some(event_result) = TokioStreamExt::next(&mut events).await {
                    match event_result {
                        Ok(event) => {
                            println!("[Server] Forwarding event: {:?}", std::mem::discriminant(&*event));
                            let resp = ServerMessage::Event((*event).clone());
                            let json = serde_json::to_vec(&resp)?;
                            sink.send(Bytes::from(json)).await?;
                        }
                        Err(e) => {
                            eprintln!("[Server] Event stream error: {:?}", e);
                        }
                    }
                }
                println!("[Server] Event stream ended");
            }
            ClientMessage::SendMessage { content } => {
                println!("[Server] Received message: {}", &content[..content.len().min(50)]);
                println!("[Server] Sending to LLM...");
                match conv_client.send_chat(&content).await {
                    Ok(_) => println!("[Server] Message queued successfully"),
                    Err(e) => eprintln!("[Server] Failed to send message: {}", e),
                }
                let json = serde_json::to_vec(&ServerMessage::Ack)?;
                sink.send(Bytes::from(json)).await?;
            }
        }
    }

    Ok(())
}
