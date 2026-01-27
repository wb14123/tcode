use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use llm_rs::conversation::Message;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::{ClientMessage, ServerMessage};

pub struct DisplayClient {
    socket_path: PathBuf,
    lua_path: PathBuf,
}

impl DisplayClient {
    pub fn new(socket_path: PathBuf, lua_path: PathBuf) -> Self {
        Self {
            socket_path,
            lua_path,
        }
    }

    pub async fn run(&self) -> Result<()> {
        // File for communication with neovim
        let display_file = PathBuf::from("/tmp/tcode-display.txt");
        // Clear the file
        tokio::fs::write(&display_file, "").await?;

        // Connect to server
        let stream = UnixStream::connect(&self.socket_path).await
            .map_err(|e| anyhow::anyhow!(
                "Failed to connect to socket {:?}: {}. Is the server running?",
                self.socket_path, e
            ))?;

        let framed = Framed::new(stream, LengthDelimitedCodec::new());
        let (mut sink, mut stream) = framed.split();

        // Send subscribe request
        let json = serde_json::to_vec(&ClientMessage::Subscribe)?;
        sink.send(Bytes::from(json)).await?;

        // Spawn neovim with display plugin
        let lua_cmd = format!(
            "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_display('{}')",
            self.lua_path.display(),
            display_file.display()
        );

        let mut nvim = Command::new("nvim")
            .args(["-c", &lua_cmd])
            .spawn()?;

        // Stream events and append to file
        while let Some(result) = stream.next().await {
            // Check if neovim exited
            if let Ok(Some(_)) = nvim.try_wait() {
                break;
            }

            let bytes = result?;
            let msg: ServerMessage = serde_json::from_slice(&bytes)?;

            if let ServerMessage::Event(event) = msg {
                let formatted = format_event(&event);
                if !formatted.is_empty() {
                    // Append to display file
                    let mut file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&display_file)
                        .await?;
                    file.write_all(formatted.as_bytes()).await?;
                    file.flush().await?;
                }
            }
        }

        // Wait for neovim to exit
        let _ = nvim.wait();
        Ok(())
    }
}

/// Format a conversation event for display
fn format_event(event: &Message) -> String {
    match event {
        Message::UserMessage { content, .. } => {
            format!("\n>>> USER:\n{}\n", content)
        }
        Message::AssistantMessageStart { .. } => {
            "\n>>> ASSISTANT:\n".to_string()
        }
        Message::AssistantMessageChunk { content, .. } => {
            content.to_string()
        }
        Message::AssistantMessageEnd { .. } => {
            "\n".to_string()
        }
        Message::ToolMessageStart { tool_name, tool_args, .. } => {
            format!("\n>>> TOOL: {} ({})\n", tool_name, tool_args)
        }
        Message::ToolOutputChunk { content, .. } => {
            content.to_string()
        }
        Message::ToolMessageEnd { .. } => {
            "\n".to_string()
        }
        _ => String::new(),
    }
}
