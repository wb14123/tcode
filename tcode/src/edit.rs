use std::path::{Path, PathBuf};
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

use crate::lua_escape;
use crate::protocol::ClientMessage;
use crate::session::Session;
use crate::tty_stdio;

pub struct EditClient {
    session: Session,
    lua_dir: PathBuf,
    /// When set, messages are routed to a specific subagent conversation.
    /// `/done` sends `UserRequestEnd` instead of `SendMessage`.
    conversation_id: Option<String>,
}

impl EditClient {
    pub fn new(session: Session, lua_dir: PathBuf, conversation_id: Option<String>) -> Self {
        Self {
            session,
            lua_dir,
            conversation_id,
        }
    }

    /// Determine the socket path. When targeting a subagent, walk up to the root
    /// session directory to find `server.sock`.
    fn socket_path(&self) -> PathBuf {
        if self.conversation_id.is_some() {
            // Walk up past subagent-* components to find root session dir
            let mut dir = self.session.session_dir().clone();
            while dir
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("subagent-"))
            {
                if let Some(parent) = dir.parent() {
                    dir = parent.to_path_buf();
                } else {
                    break;
                }
            }
            dir.join("server.sock")
        } else {
            self.session.socket_path()
        }
    }

    pub async fn run(&self) -> Result<()> {
        let msg_file = self.session.msg_file();
        if let Err(e) = fs::remove_file(&msg_file).await {
            // File may not exist yet; only warn on unexpected errors
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, path = %msg_file.display(), "failed to remove msg file");
            }
        }

        let socket_path = self.socket_path();
        let stream = UnixStream::connect(&socket_path).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to connect to socket {:?}: {}. Is the server running?",
                socket_path,
                e
            )
        })?;

        let framed = Framed::new(stream, LengthDelimitedCodec::new());
        let (mut sink, mut server_stream) = framed.split();

        let is_subagent = self.conversation_id.is_some();
        let exe_path =
            std::env::current_exe().context("Failed to determine current executable path")?;
        let session_id = self
            .session
            .session_dir()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let mut nvim = spawn_nvim(
            &self.lua_dir,
            &msg_file,
            is_subagent,
            &session_id,
            &exe_path,
        )?;

        // Set up file watcher using inotify
        let (tx, rx) = mpsc::channel();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
            if let Ok(event) = res
                && tx.send(event).is_err()
            {
                tracing::debug!("edit file watcher channel closed");
            }
        })?;

        // Watch the session directory for file creation/modification
        watcher
            .watch(self.session.session_dir(), RecursiveMode::NonRecursive)
            .with_context(|| {
                format!(
                    "Failed to watch session directory {:?}",
                    self.session.session_dir()
                )
            })?;

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
                    if self.conversation_id.is_none() {
                        let json = serde_json::to_vec(&ClientMessage::Shutdown)?;
                        if let Err(e) = sink.send(Bytes::from(json)).await {
                            tracing::warn!(error = %e, "failed to send shutdown message to server");
                        }
                    }
                    break;
                }
                msg = server_stream.next() => {
                    match msg {
                        None | Some(Err(_)) => {
                            crate::terminate_child(&mut nvim).await?;
                            break;
                        }
                        Some(Ok(_)) => {}
                    }
                }
                Some(event) = file_events.recv() => {
                    if is_msg_file_event(&event, &msg_file) && let Some(content) = read_message_file(&msg_file).await {
                        let msg = self.build_client_message(&content);
                        let json = serde_json::to_vec(&msg)?;
                        sink.send(Bytes::from(json)).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Build the appropriate `ClientMessage` based on conversation_id and content.
    fn build_client_message(&self, content: &str) -> ClientMessage {
        match &self.conversation_id {
            Some(conv_id) if content.trim() == "/done" => ClientMessage::UserRequestEnd {
                conversation_id: conv_id.clone(),
            },
            Some(conv_id) => ClientMessage::SendMessage {
                conversation_id: Some(conv_id.clone()),
                content: content.to_string(),
            },
            None => ClientMessage::SendMessage {
                conversation_id: None,
                content: content.to_string(),
            },
        }
    }
}

fn is_msg_file_event(event: &Event, msg_file: &PathBuf) -> bool {
    // Check if this is a create or modify event for our message file
    matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
        && event.paths.iter().any(|p| p == msg_file)
}

fn spawn_nvim(
    lua_dir: &Path,
    msg_file: &Path,
    is_subagent: bool,
    session_id: &str,
    exe_path: &Path,
) -> Result<Child> {
    let lua_cmd = format!(
        "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_edit('{}', {}, '{}', '{}')",
        lua_escape(&lua_dir.display().to_string()),
        lua_escape(&msg_file.display().to_string()),
        is_subagent,
        lua_escape(session_id),
        lua_escape(&exe_path.display().to_string()),
    );

    let (stdin, stdout, stderr) = tty_stdio::get_tty_stdio();

    let child = Command::new("nvim")
        .args(["-c", &lua_cmd])
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
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
    if let Err(e) = fs::remove_file(msg_file).await {
        tracing::warn!(error = %e, "failed to remove msg file after reading");
    }
    Some(content.to_string())
}
