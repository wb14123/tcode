use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::oneshot;

/// A single active progress item from the LSP server.
#[derive(Debug, Clone)]
pub struct ProgressItem {
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
}

/// Tracks active work-done progress notifications from the LSP server.
/// Clone is cheap (inner Arc).
#[derive(Debug, Clone, Default)]
pub struct ProgressTracker {
    items: Arc<Mutex<HashMap<String, ProgressItem>>>,
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all currently active progress items.
    pub fn active_items(&self) -> Vec<ProgressItem> {
        self.items.lock().values().cloned().collect()
    }

    fn begin(
        &self,
        token: String,
        title: String,
        message: Option<String>,
        percentage: Option<u32>,
    ) {
        self.items.lock().insert(
            token,
            ProgressItem {
                title,
                message,
                percentage,
            },
        );
    }

    fn report(&self, token: &str, message: Option<String>, percentage: Option<u32>) {
        if let Some(item) = self.items.lock().get_mut(token) {
            if message.is_some() {
                item.message = message;
            }
            if percentage.is_some() {
                item.percentage = percentage;
            }
        }
    }

    fn end(&self, token: &str) {
        self.items.lock().remove(token);
    }
}

/// JSON-RPC transport over stdio for LSP communication.
pub struct LspTransport {
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    reader_task: Option<tokio::task::JoinHandle<()>>,
    child: Option<Child>,
    progress: ProgressTracker,
}

impl LspTransport {
    /// Create a new transport taking ownership of the child process.
    /// Spawns a background task to read stdout and dispatch responses.
    pub async fn new(mut child: Child) -> Result<Self> {
        let stdin = child
            .stdin
            .take()
            .context("LSP child process has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("LSP child process has no stdout")?;

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
        let progress = ProgressTracker::new();

        let reader_task = {
            let pending = Arc::clone(&pending);
            let stdin = Arc::clone(&stdin);
            let progress = progress.clone();
            tokio::spawn(async move {
                if let Err(e) = reader_loop(stdout, pending, stdin, progress).await {
                    tracing::debug!("LSP reader loop ended: {e}");
                }
            })
        };

        Ok(Self {
            next_id: AtomicI64::new(1),
            pending,
            stdin,
            reader_task: Some(reader_task),
            child: Some(child),
            progress,
        })
    }

    /// Send a JSON-RPC request and await the response (30s timeout).
    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        self.write_message(&msg).await?;

        let result = tokio::time::timeout(std::time::Duration::from_secs(30), rx).await;

        match result {
            Ok(Ok(value)) => {
                // Check for JSON-RPC error in the response
                if let Some(error) = value.get("error") {
                    bail!(
                        "LSP error for {method}: {}",
                        serde_json::to_string(error).unwrap_or_default()
                    );
                }
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(_)) => bail!("LSP response channel closed for request {method} (id={id})"),
            Err(_) => {
                // Clean up the pending entry on timeout
                self.pending.lock().remove(&id);
                bail!("LSP request {method} timed out after 30s (id={id})")
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&msg).await
    }

    /// Shut down the LSP transport: send shutdown request, exit notification, kill child.
    pub async fn shutdown(mut self) -> Result<()> {
        // Send shutdown request (may fail if server already dead)
        let shutdown_result = self.send_request("shutdown", Value::Null).await;
        if let Err(e) = &shutdown_result {
            tracing::warn!("LSP shutdown request failed: {e}");
        }

        // Send exit notification
        if let Err(e) = self.send_notification("exit", Value::Null).await {
            tracing::warn!("LSP exit notification failed: {e}");
        }

        // Kill the child process
        if let Some(mut child) = self.child.take()
            && let Err(e) = child.kill().await
        {
            tracing::debug!("LSP child kill: {e}");
        }

        // Abort the reader task
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }

        Ok(())
    }

    /// Write a framed JSON-RPC message to stdin.
    async fn write_message(&self, msg: &Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(header.as_bytes()).await?;
        stdin.write_all(body.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Get the progress tracker for this transport.
    pub fn progress(&self) -> &ProgressTracker {
        &self.progress
    }
}

impl Drop for LspTransport {
    fn drop(&mut self) {
        // Abort the reader task so it doesn't outlive the transport.
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        // Kill the child process so it doesn't become orphaned.
        if let Some(mut child) = self.child.take()
            && let Err(e) = child.start_kill()
        {
            tracing::debug!("LSP child start_kill in Drop: {e}");
        }
    }
}

fn extract_progress_token(params: &Value) -> Option<String> {
    match params.get("token")? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn handle_progress_notification(msg: &Value, tracker: &ProgressTracker) {
    let Some(params) = msg.get("params") else {
        return;
    };
    let Some(token) = extract_progress_token(params) else {
        return;
    };
    let Some(value) = params.get("value") else {
        return;
    };

    match value.get("kind").and_then(|k| k.as_str()) {
        Some("begin") => {
            let title = value
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let message = value
                .get("message")
                .and_then(|m| m.as_str())
                .map(String::from);
            let percentage = value
                .get("percentage")
                .and_then(|p| p.as_u64())
                .map(|p| p as u32);
            tracker.begin(token, title, message, percentage);
        }
        Some("report") => {
            let message = value
                .get("message")
                .and_then(|m| m.as_str())
                .map(String::from);
            let percentage = value
                .get("percentage")
                .and_then(|p| p.as_u64())
                .map(|p| p as u32);
            tracker.report(&token, message, percentage);
        }
        Some("end") => {
            tracker.end(&token);
        }
        _ => {}
    }
}

/// Background loop: read LSP messages from stdout, dispatch responses/notifications.
async fn reader_loop(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    progress: ProgressTracker,
) -> Result<()> {
    let mut reader = BufReader::new(stdout);

    loop {
        // Read headers until empty line
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                // EOF
                return Ok(());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length:")
                && let Ok(len) = len_str.trim().parse::<usize>()
            {
                content_length = Some(len);
            }
        }

        let Some(content_length) = content_length else {
            tracing::warn!("LSP message missing Content-Length header");
            continue;
        };

        // Read the body
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).await?;

        let msg: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to parse LSP message: {e}");
                continue;
            }
        };

        let has_id = msg.get("id").is_some();
        let has_method = msg.get("method").is_some();

        if has_id && has_method {
            // Server-initiated request — respond with empty success
            let id = msg["id"].clone();
            let method = msg["method"].as_str().unwrap_or("<unknown>");
            tracing::debug!("LSP server request: {method} (id={id})");

            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": Value::Null,
            });
            let resp_body = serde_json::to_string(&response)?;
            let header = format!("Content-Length: {}\r\n\r\n", resp_body.len());

            let mut writer = stdin.lock().await;
            writer.write_all(header.as_bytes()).await?;
            writer.write_all(resp_body.as_bytes()).await?;
            writer.flush().await?;
        } else if has_id {
            // Response to one of our requests — dispatch by id
            let id = match &msg["id"] {
                Value::Number(n) => n.as_i64().unwrap_or(-1),
                _ => -1,
            };
            let sender = pending.lock().remove(&id);
            if let Some(tx) = sender {
                // Send the full message so the caller can check for errors
                if tx.send(msg).is_err() {
                    tracing::debug!("LSP response receiver dropped for id={id}");
                }
            } else {
                tracing::debug!("LSP response for unknown id={id}");
            }
        } else {
            // Server notification
            let method = msg["method"].as_str().unwrap_or("<unknown>");
            tracing::debug!("LSP server notification: {method}");

            if method == "$/progress" {
                handle_progress_notification(&msg, &progress);
            }
        }
    }
}
