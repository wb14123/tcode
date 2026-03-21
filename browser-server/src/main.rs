use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::middleware;
use clap::{Parser, Subcommand};
use tokio::net::{TcpListener, UnixListener};
use tracing_subscriber::EnvFilter;

use browser_server::auth::{self, TokenSet};
use browser_server::browser;
use browser_server::handler::{AppState, build_app};

#[derive(Parser)]
#[command(name = "browser-server")]
#[command(about = "Headless Chrome browser server exposing web_search and web_fetch as REST APIs")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Unix socket path (default: ~/.tcode/browser-server.sock)
    #[arg(long)]
    socket: Option<PathBuf>,

    /// TCP address for remote access (e.g., 0.0.0.0:8090). Enables bearer token auth.
    #[arg(long)]
    bind: Option<String>,

    /// Path to token file for TCP auth (default: ~/.config/browser-server/tokens.json)
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// Exit after N seconds with no requests. Used by tcode for auto-started instances.
    #[arg(long)]
    idle_timeout: Option<u64>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch Chrome with persistent profile for login setup
    Browser,
}

fn default_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tcode")
        .join("browser-server.sock")
}

fn default_token_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("browser-server")
        .join("tokens.json")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    if let Some(Commands::Browser) = cli.command {
        return run_browser().await;
    }

    let state = Arc::new(AppState::new());

    // Set up idle timeout shutdown task
    let idle_timeout = cli.idle_timeout;
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());

    if let Some(timeout_secs) = idle_timeout {
        let state_clone = Arc::clone(&state);
        let notify = Arc::clone(&shutdown_notify);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                let last = state_clone.last_activity.load(Ordering::Relaxed);
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if now.saturating_sub(last) >= timeout_secs {
                    tracing::info!("Idle timeout reached ({timeout_secs}s), shutting down");
                    notify.notify_one();
                    return;
                }
            }
        });
    }

    // Determine which mode to run in
    if let Some(ref bind_addr) = cli.bind {
        // TCP mode with bearer token auth
        let token_file = cli.token_file.unwrap_or_else(default_token_file);
        let token_set = Arc::new(
            TokenSet::from_file(&token_file)
                .with_context(|| format!("Failed to load token file: {}", token_file.display()))?,
        );

        let app = build_app(Arc::clone(&state))
            .layer(middleware::from_fn(auth::bearer_auth))
            .layer(axum::Extension(token_set));

        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("Failed to bind to {bind_addr}"))?;
        tracing::info!("Listening on TCP {bind_addr} (auth enabled)");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(Arc::clone(&shutdown_notify)))
            .await?;
    } else {
        // Unix socket mode (no auth)
        let socket_path = cli.socket.unwrap_or_else(default_socket_path);

        // Clean up stale socket
        if socket_path.exists() {
            std::fs::remove_file(&socket_path).with_context(|| {
                format!("Failed to remove stale socket: {}", socket_path.display())
            })?;
        }

        // Ensure parent directory exists
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("Failed to bind Unix socket at {}", socket_path.display()))?;
        tracing::info!("Listening on Unix socket {}", socket_path.display());

        let app = build_app(Arc::clone(&state));

        axum::serve(tokio_listener_from_unix(listener), app)
            .with_graceful_shutdown(shutdown_signal(Arc::clone(&shutdown_notify)))
            .await?;

        // Clean up socket file
        if socket_path.exists() {
            if let Err(e) = std::fs::remove_file(&socket_path) {
                tracing::warn!("Failed to remove socket file on shutdown: {e}");
            }
        }
    }

    // Shut down Chrome
    browser::shutdown_browser();
    tracing::info!("Browser-server shut down");

    Ok(())
}

async fn shutdown_signal(notify: Arc<tokio::sync::Notify>) {
    let ctrl_c = tokio::signal::ctrl_c();
    let idle = notify.notified();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received Ctrl+C, shutting down");
        }
        _ = idle => {
            tracing::info!("Idle timeout triggered shutdown");
        }
    }
}

/// Convert a `tokio::net::UnixListener` into a type that implements the
/// `axum::serve` listener trait via `tokio_util`.
fn tokio_listener_from_unix(
    listener: UnixListener,
) -> impl axum::serve::Listener<Addr = std::sync::Arc<tokio::net::unix::SocketAddr>> {
    TokioUnixListener(listener)
}

struct TokioUnixListener(UnixListener);

impl axum::serve::Listener for TokioUnixListener {
    type Io = tokio::net::UnixStream;
    type Addr = std::sync::Arc<tokio::net::unix::SocketAddr>;

    fn accept(&mut self) -> impl std::future::Future<Output = (Self::Io, Self::Addr)> + Send {
        async {
            loop {
                match self.0.accept().await {
                    Ok((stream, addr)) => return (stream, std::sync::Arc::new(addr)),
                    Err(e) => {
                        tracing::error!("Accept error: {e}");
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr().map(std::sync::Arc::new)
    }
}

async fn run_browser() -> Result<()> {
    browser::launch_interactive().await
}
