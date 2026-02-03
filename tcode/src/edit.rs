use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::ClientMessage;
use crate::session::Session;

pub struct EditClient {
    session: Session,
    lua_path: PathBuf,
}

impl EditClient {
    pub fn new(session: Session, lua_path: PathBuf) -> Self {
        Self { session, lua_path }
    }

    pub async fn run(&self) -> Result<()> {
        let msg_file = self.session.msg_file();
        let _ = fs::remove_file(&msg_file).await;

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
        let (mut sink, mut server_stream) = framed.split();

        let mut nvim = spawn_nvim(&self.lua_path, &msg_file)?;

        // Set up file watcher using inotify
        let (tx, rx) = mpsc::channel();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        })?;

        // Watch the session directory for file creation/modification
        watcher.watch(self.session.session_dir(), RecursiveMode::NonRecursive)
            .with_context(|| format!("Failed to watch session directory {:?}", self.session.session_dir()))?;

        // Convert sync channel to async stream
        let (async_tx, mut file_events) = tokio::sync::mpsc::unbounded_channel::<Event>();
        std::thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if async_tx.send(event).is_err() {
                    break;
                }
            }
        });

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
                Some(event) = file_events.recv() => {
                    if is_msg_file_event(&event, &msg_file) {
                        if let Some(content) = read_message_file(&msg_file).await {
                            let json = serde_json::to_vec(&ClientMessage::SendMessage { content })?;
                            sink.send(Bytes::from(json)).await?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

fn is_msg_file_event(event: &Event, msg_file: &PathBuf) -> bool {
    // Check if this is a create or modify event for our message file
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_)
    ) && event.paths.iter().any(|p| p == msg_file)
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
        .spawn()
        .context("Failed to spawn 'nvim' for edit - is neovim installed and in PATH?")?;

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
