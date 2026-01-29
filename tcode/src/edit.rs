use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::ClientMessage;

pub struct EditClient {
    socket_path: PathBuf,
    lua_path: PathBuf,
}

impl EditClient {
    pub fn new(socket_path: PathBuf, lua_path: PathBuf) -> Self {
        Self {
            socket_path,
            lua_path,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let msg_file = PathBuf::from("/tmp/tcode-edit-msg.txt");
        let _ = fs::remove_file(&msg_file).await;

        let stream = UnixStream::connect(&self.socket_path).await
            .map_err(|e| anyhow::anyhow!(
                "Failed to connect to socket {:?}: {}. Is the server running?",
                self.socket_path, e
            ))?;

        let framed = Framed::new(stream, LengthDelimitedCodec::new());
        let (mut sink, mut server_stream) = framed.split();

        let mut nvim = spawn_nvim(&self.lua_path, &msg_file)?;
        let mut poll_interval = tokio::time::interval(tokio::time::Duration::from_millis(200));

        loop {
            tokio::select! {
                biased;
                _ = nvim.wait() => {
                    let json = serde_json::to_vec(&ClientMessage::Shutdown)?;
                    let _ = sink.send(Bytes::from(json)).await;
                    break;
                }
                msg = server_stream.next() => {
                    match msg {
                        None | Some(Err(_)) => {
                            let _ = nvim.kill().await;
                            break;
                        }
                        Some(Ok(_)) => {}
                    }
                }
                _ = poll_interval.tick() => {
                    if let Some(content) = read_message_file(&msg_file).await {
                        let json = serde_json::to_vec(&ClientMessage::SendMessage { content })?;
                        sink.send(Bytes::from(json)).await?;
                    }
                }
            }
        }

        let _ = fs::remove_file(&msg_file).await;
        Ok(())
    }
}

fn spawn_nvim(lua_path: &PathBuf, msg_file: &PathBuf) -> Result<Child> {
    let lua_cmd = format!(
        "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_edit('{}')",
        lua_path.display(),
        msg_file.display()
    );

    let child = Command::new("nvim")
        .args(["-c", &lua_cmd])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    Ok(child)
}

async fn read_message_file(msg_file: &PathBuf) -> Option<String> {
    if !msg_file.exists() {
        return None;
    }

    let mut content = String::new();
    let mut file = fs::File::open(msg_file).await.ok()?;
    file.read_to_string(&mut content).await.ok()?;

    let content = content.trim();
    if content.is_empty() {
        return None;
    }

    // Remove file to avoid re-reading
    let _ = fs::remove_file(msg_file).await;
    Some(content.to_string())
}
