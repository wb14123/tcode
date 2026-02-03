mod display;
mod edit;
mod protocol;
mod server;
mod session;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::process::Child;

/// Gracefully stop a neovim child: SIGTERM with timeout, then SIGKILL.
pub(crate) async fn terminate_child(child: &mut Child) -> Result<()> {
    let graceful = child.id().is_some_and(|pid| {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .is_ok()
    });
    if !graceful
        || tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
    {
        child.kill().await.context("Failed to kill neovim")?;
    }
    Ok(())
}

use display::DisplayClient;
use edit::EditClient;
use server::Server;
use session::Session;

#[derive(Parser)]
#[command(name = "tcode")]
#[command(about = "Terminal-based LLM conversation interface with neovim")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// API key for the LLM provider
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// Model to use
    #[arg(long, default_value = "gpt-4o")]
    model: String,

    /// Base URL for the API
    #[arg(long, default_value = "https://api.openai.com/v1")]
    base_url: String,

    /// Session ID (defaults to tmux session name or "default")
    #[arg(long)]
    session: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the server only (for standalone mode)
    Serve,
    /// Open edit window to compose messages
    Edit,
    /// Open display window to view conversation
    Display,
}

fn get_session_id(session: Option<String>) -> String {
    session.unwrap_or_else(|| {
        // Try to get tmux session name for per-session isolation
        Command::new("tmux")
            .args(["display-message", "-p", "#S"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".to_string())
    })
}

fn get_lua_path() -> PathBuf {
    // Try multiple locations for the lua directory
    let candidates = [
        // Next to executable (for installed builds)
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("lua"))),
        // In tcode directory (for development)
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")),
        // Current directory fallback
        std::env::current_dir().ok().map(|p| p.join("lua")),
        std::env::current_dir().ok().map(|p| p.join("tcode/lua")),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.join("tcode.lua").exists() {
            return candidate;
        }
    }

    // Final fallback
    PathBuf::from("lua")
}

fn is_in_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let session_id = get_session_id(cli.session.clone());
    let lua_path = get_lua_path();

    match cli.command {
        None => {
            // Unified startup: server + tmux panes
            run_unified(cli, session_id, lua_path).await
        }
        Some(Commands::Serve) => {
            let api_key = cli.api_key.ok_or_else(|| {
                anyhow::anyhow!("API key required. Set OPENAI_API_KEY env or use --api-key")
            })?;
            let session = Session::new(session_id)?;
            let server = Server::new(session.socket_path(), api_key, cli.model, cli.base_url);
            let result = server.run().await;
            session.cleanup();
            result
        }
        Some(Commands::Edit) => {
            let session = Session::new(session_id)?;
            let client = EditClient::new(session, lua_path);
            client.run().await
        }
        Some(Commands::Display) => {
            let session = Session::new(session_id)?;
            let client = DisplayClient::new(session, lua_path);
            client.run().await
        }
    }
}

async fn run_unified(cli: Cli, session_id: String, lua_path: PathBuf) -> Result<()> {
    // Check if running inside tmux
    if !is_in_tmux() {
        anyhow::bail!("tcode must be run inside tmux for the unified mode.\nRun `tcode serve` to start the server without tmux.");
    }

    let api_key = cli.api_key.ok_or_else(|| {
        anyhow::anyhow!("API key required. Set OPENAI_API_KEY env or use --api-key")
    })?;

    // Create session directory
    let session = Session::new(session_id.clone())?;
    let socket_path = session.socket_path();

    // Get the path to the current executable
    let exe_path = std::env::current_exe().context("Failed to determine current executable path")?;
    let exe_str = exe_path.to_string_lossy();
    let session_arg = format!("--session={}", session_id);

    // Start server as a background task
    let server = Server::new(
        socket_path,
        api_key,
        cli.model.clone(),
        cli.base_url.clone(),
    );

    // Spawn server in background
    let server_handle = tokio::spawn(async move {
        if let Err(e) = server.run().await {
            eprintln!("[Server] Error: {}", e);
        }
    });

    // Wait for server to start and create socket
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Create edit pane (bottom 30%) and capture the pane ID
    let edit_cmd = format!("{} {} edit", exe_str, session_arg);

    let output = Command::new("tmux")
        .args(["split-window", "-v", "-p", "30", "-P", "-F", "#{pane_id}", &edit_cmd])
        .output()
        .context("Failed to run 'tmux' - is tmux installed and in PATH?");

    let edit_pane_id = match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => {
            server_handle.abort();
            session.cleanup();
            return Err(e);
        }
    };

    // Run display client in current pane (create a new session that shares the directory)
    let display_session = Session::new(session_id)?;
    let client = DisplayClient::new(display_session, lua_path);
    let result = client.run().await;

    // Kill the edit pane to ensure it exits
    let _ = Command::new("tmux")
        .args(["kill-pane", "-t", &edit_pane_id])
        .output();

    // Abort server task (it should already be shutting down)
    server_handle.abort();

    // Clean up session files
    session.cleanup();

    result
}
