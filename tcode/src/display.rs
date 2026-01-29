use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use llm_rs::conversation::Message;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::{ClientMessage, ServerMessage};
use crate::session::Session;

pub struct DisplayClient {
    session: Session,
    lua_path: PathBuf,
}

impl DisplayClient {
    pub fn new(session: Session, lua_path: PathBuf) -> Self {
        Self { session, lua_path }
    }

    pub async fn run(&self) -> Result<()> {
        let display_file = self.session.display_file();
        let status_file = self.session.status_file();

        tokio::fs::write(&display_file, "").await?;
        tokio::fs::write(&status_file, "Connecting...").await?;

        let stream = UnixStream::connect(self.session.socket_path())
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to connect to socket {:?}: {}. Is the server running?",
                    self.session.socket_path(),
                    e
                )
            })?;

        let framed = Framed::new(stream, LengthDelimitedCodec::new());
        let (mut sink, mut stream) = framed.split();

        // Subscribe to events
        let json = serde_json::to_vec(&ClientMessage::Subscribe)?;
        sink.send(Bytes::from(json)).await?;

        // Spawn neovim
        let mut nvim = spawn_nvim(&self.lua_path, &display_file, &status_file)?;

        tokio::select! {
            biased;
            _ = nvim.wait() => {
                let json = serde_json::to_vec(&ClientMessage::Shutdown)?;
                let _ = sink.send(Bytes::from(json)).await;
            }
            _ = process_server_messages(&mut stream, &display_file, &status_file) => {
                let _ = nvim.kill().await;
            }
        }

        Ok(())
    }
}

fn spawn_nvim(lua_path: &PathBuf, display_file: &PathBuf, status_file: &PathBuf) -> Result<Child> {
    let lua_cmd = format!(
        "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_display('{}', '{}')",
        lua_path.display(),
        display_file.display(),
        status_file.display()
    );

    let child = Command::new("nvim")
        .args(["-c", &lua_cmd])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    Ok(child)
}

async fn process_server_messages<S>(stream: &mut S, display_file: &PathBuf, status_file: &PathBuf)
where
    S: StreamExt<Item = Result<bytes::BytesMut, std::io::Error>> + Unpin,
{
    while let Some(Ok(bytes)) = stream.next().await {
        let Ok(msg) = serde_json::from_slice::<ServerMessage>(&bytes) else {
            continue;
        };

        match msg {
            ServerMessage::Event(event) => {
                let _ = append_event(display_file, &event).await;
            }
            ServerMessage::Status { message } => {
                let _ = tokio::fs::write(status_file, &message).await;
            }
            ServerMessage::Ack | ServerMessage::Error { .. } => {}
        }
    }
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
