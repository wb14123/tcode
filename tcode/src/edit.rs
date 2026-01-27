use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
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
        // Connect to server
        let stream = UnixStream::connect(&self.socket_path).await
            .map_err(|e| anyhow::anyhow!(
                "Failed to connect to socket {:?}: {}. Is the server running?",
                self.socket_path, e
            ))?;
        let framed = Framed::new(stream, LengthDelimitedCodec::new());
        let (mut sink, _stream) = framed.split();

        // Use a temp file for communication with neovim
        let msg_file = PathBuf::from("/tmp/tcode-edit-msg.txt");
        // Clean up any existing file
        let _ = fs::remove_file(&msg_file).await;

        // Spawn neovim with edit hooks
        let lua_cmd = format!(
            "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_edit('{}')",
            self.lua_path.display(),
            msg_file.display()
        );

        let mut nvim = Command::new("nvim")
            .args(["-c", &lua_cmd])
            .spawn()?;

        // Poll for messages in the temp file
        loop {
            // Check if neovim exited
            match nvim.try_wait()? {
                Some(_) => break, // Neovim exited
                None => {}
            }

            // Check for message file
            if msg_file.exists() {
                let mut content = String::new();
                if let Ok(mut file) = fs::File::open(&msg_file).await {
                    if file.read_to_string(&mut content).await.is_ok() && !content.trim().is_empty() {
                        // Remove the file first to avoid re-reading
                        let _ = fs::remove_file(&msg_file).await;

                        let content = content.trim().to_string();
                        let json = serde_json::to_vec(&ClientMessage::SendMessage { content })?;
                        sink.send(Bytes::from(json)).await?;
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }

        // Cleanup
        let _ = fs::remove_file(&msg_file).await;
        Ok(())
    }
}
