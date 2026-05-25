use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::config::LspConfig;
use crate::server::LspServer;

/// Manages multiple LSP server instances, one per server type.
pub struct LspManager {
    config: LspConfig,
    servers: tokio::sync::Mutex<HashMap<String, Arc<LspServer>>>,
    root_dir: PathBuf,
}

impl LspManager {
    /// Create a new manager with the given config and project root.
    pub fn new(config: LspConfig, root_dir: PathBuf) -> Self {
        Self {
            config,
            servers: tokio::sync::Mutex::new(HashMap::new()),
            root_dir,
        }
    }

    /// Whether any LSP servers are configured.
    pub fn has_servers(&self) -> bool {
        self.config.has_servers()
    }

    /// Look up the filetype for a file extension (e.g. ".rs" → "rust").
    pub fn filetype_for_extension(&self, ext: &str) -> Option<&str> {
        self.config
            .extension_to_filetype
            .get(ext)
            .map(String::as_str)
    }

    /// Get the underlying LSP config.
    pub fn config(&self) -> &LspConfig {
        &self.config
    }

    /// Get or start the LSP server for the given filetype.
    pub async fn get_or_start_server(&self, filetype: &str) -> Result<Arc<LspServer>> {
        // Find which server config handles this filetype
        let server_config = self
            .config
            .servers
            .iter()
            .find(|s| s.filetypes.contains(&filetype.to_string()));

        let Some(server_config) = server_config else {
            bail!("no LSP server configured for {filetype} files");
        };

        // Fast path: check if already running under lock
        {
            let servers = self.servers.lock().await;
            if let Some(server) = servers.get(&server_config.name) {
                return Ok(Arc::clone(server));
            }
        }

        // Slow path: start server outside lock
        let root_dir = find_root_dir(&self.root_dir, &server_config.root_markers);

        tracing::info!(
            "Starting LSP server '{}' in {}",
            server_config.name,
            root_dir.display()
        );

        let server = LspServer::start(server_config, &root_dir).await?;
        server.wait_until_ready(Duration::from_secs(10)).await;

        // Re-acquire lock for insert-or-dedup
        let mut servers = self.servers.lock().await;
        if let Some(existing) = servers.get(&server_config.name) {
            // Lost the race — shutdown our duplicate, return the one that won
            if let Err(e) = server.shutdown().await {
                tracing::warn!(
                    "Failed to shutdown duplicate LSP server '{}': {e}",
                    server_config.name
                );
            }
            return Ok(Arc::clone(existing));
        }

        let server = Arc::new(server);
        servers.insert(server_config.name.clone(), Arc::clone(&server));
        Ok(server)
    }

    /// Shut down all running LSP servers.
    pub async fn shutdown_all(&self) {
        let servers = {
            let mut guard = self.servers.lock().await;
            std::mem::take(&mut *guard)
        };

        for (name, server) in servers {
            match Arc::try_unwrap(server) {
                Ok(server) => {
                    if let Err(e) = server.shutdown().await {
                        tracing::warn!("Failed to shut down LSP server '{name}': {e}");
                    }
                }
                Err(_arc) => {
                    tracing::warn!(
                        "LSP server '{name}' still has references, cannot shut down cleanly"
                    );
                }
            }
        }
    }
}

/// Walk up from `start_dir` looking for any of the given root marker files.
/// Returns the directory containing the first found marker, or `start_dir` as fallback.
fn find_root_dir(start_dir: &Path, root_markers: &[String]) -> PathBuf {
    let mut dir = start_dir.to_path_buf();
    loop {
        for marker in root_markers {
            if dir.join(marker).exists() {
                return dir;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    start_dir.to_path_buf()
}
