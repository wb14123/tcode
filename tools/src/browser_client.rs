use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Result};
use browser_server::{
    ErrorResponse, HealthResponse, SearchResult, WebFetchRequest, WebFetchResponse,
    WebSearchRequest, WebSearchResponse,
};

/// Configuration for auto-restarting the browser-server when it becomes unavailable.
struct RestartConfig {
    socket_path: PathBuf,
    server_exe: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

/// HTTP client for communicating with the browser-server.
pub struct BrowserClient {
    client: reqwest::Client,
    base_url: String,
    token: Option<String>,
    restart_config: Option<RestartConfig>,
}

impl BrowserClient {
    /// Create a client that connects via Unix socket.
    pub fn unix(socket_path: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .unix_socket(socket_path)
            .build()
            .expect("Failed to build reqwest client with unix socket");
        Self {
            client,
            base_url: "http://localhost".to_string(),
            token: None,
            restart_config: None,
        }
    }

    /// Enable auto-restart: if the browser-server becomes unreachable, spawn it again.
    pub fn with_auto_restart(mut self, socket_path: PathBuf, server_exe: PathBuf) -> Self {
        self.restart_config = Some(RestartConfig {
            socket_path,
            server_exe,
            lock: tokio::sync::Mutex::new(()),
        });
        self
    }

    /// Create a client that connects via TCP with bearer token auth.
    pub fn tcp(base_url: String, token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            token: Some(token),
            restart_config: None,
        }
    }

    /// Perform a web search via the browser-server.
    pub async fn web_search(&self, query: &str) -> Result<Vec<SearchResult>> {
        let body = WebSearchRequest {
            query: query.to_string(),
        };
        let resp: WebSearchResponse = self.post("/web_search", &body).await?;
        Ok(resp.results)
    }

    /// Fetch a web page via the browser-server.
    pub async fn web_fetch(&self, url: &str) -> Result<String> {
        let body = WebFetchRequest {
            url: url.to_string(),
        };
        let resp: WebFetchResponse = self.post("/web_fetch", &body).await?;
        Ok(resp.content)
    }

    /// Check if the browser-server is healthy.
    pub async fn health_check(&self) -> bool {
        match self.get::<HealthResponse>("/health").await {
            Ok(resp) => resp.status == "ok",
            Err(_) => false,
        }
    }

    /// Ensure the browser-server is running. Checks health and restarts if needed.
    /// Intended for use at startup; request-time recovery uses the retry logic in `post`.
    pub async fn ensure_server_running(&self) {
        if self.health_check().await {
            return;
        }
        if let Some(ref config) = self.restart_config {
            self.restart_server(config).await;
        }
    }

    async fn post<Req, Resp>(&self, path: &str, body: &Req) -> Result<Resp>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        let build_request = || {
            let url = format!("{}{}", self.base_url, path);
            let mut request = self.client.post(&url).json(body);
            if let Some(ref token) = self.token {
                request = request.bearer_auth(token);
            }
            request
        };

        let response = match build_request().send().await {
            Ok(resp) => resp,
            Err(first_err) => {
                // Connection failed — try restarting the server if configured, then retry once
                if let Some(ref config) = self.restart_config {
                    self.restart_server(config).await;
                    build_request().send().await
                        .map_err(|e| anyhow!("browser-server unreachable after restart attempt: {e}"))?
                } else {
                    return Err(first_err.into());
                }
            }
        };

        let status = response.status();
        if status.is_success() {
            Ok(response.json().await?)
        } else {
            let error: ErrorResponse = response
                .json()
                .await
                .unwrap_or_else(|_| ErrorResponse {
                    error: browser_server::ErrorDetail {
                        message: format!("HTTP {status}"),
                        error_type: "http_error".to_string(),
                    },
                });
            Err(anyhow!("{}", error.error.message))
        }
    }

    async fn get<Resp>(&self, path: &str) -> Result<Resp>
    where
        Resp: serde::de::DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.client.get(&url);

        if let Some(ref token) = self.token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        let status = response.status();

        if status.is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow!("HTTP {status}"))
        }
    }

    /// Restart the browser-server process. Uses a mutex to prevent concurrent restart attempts.
    async fn restart_server(&self, config: &RestartConfig) {
        let _lock = config.lock.lock().await;

        // After acquiring lock, check if another request already restarted the server
        if self.health_check().await {
            return;
        }

        tracing::info!("Browser-server is not responding, attempting restart");

        if config.socket_path.exists() {
            if let Err(e) = std::fs::remove_file(&config.socket_path) {
                tracing::warn!("Failed to remove stale browser-server socket: {e}");
            }
        }

        let log_path = config.socket_path.with_extension("log");
        let stderr_stdio = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(f) => std::process::Stdio::from(f),
            Err(e) => {
                tracing::warn!("Failed to open browser-server log {}: {e}", log_path.display());
                std::process::Stdio::null()
            }
        };

        match std::process::Command::new(&config.server_exe)
            .args([
                "--socket", &config.socket_path.to_string_lossy(),
                "--idle-timeout", "300",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(stderr_stdio)
            .spawn()
        {
            Ok(_) => {
                for _ in 0..50 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if self.health_check().await {
                        tracing::info!("Browser-server restarted successfully");
                        return;
                    }
                }
                tracing::warn!("Browser-server failed to become ready within 5s after restart");
            }
            Err(e) => {
                tracing::warn!("Failed to restart browser-server: {e}");
            }
        }
    }
}

static GLOBAL_CLIENT: OnceLock<BrowserClient> = OnceLock::new();

/// Set the global browser client. Should be called once at startup.
pub fn set_global_client(client: BrowserClient) {
    if GLOBAL_CLIENT.set(client).is_err() {
        tracing::warn!("Global browser client already set");
    }
}

/// Get a reference to the global browser client.
pub fn get_global_client() -> Option<&'static BrowserClient> {
    GLOBAL_CLIENT.get()
}
