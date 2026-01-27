mod display;
mod edit;
mod protocol;
mod server;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use display::DisplayClient;
use edit::EditClient;
use server::Server;

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

    /// Socket path
    #[arg(long)]
    socket: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the server (default behavior)
    Serve,
    /// Open edit window to compose messages
    Edit,
    /// Open display window to view conversation
    Display,
}

fn get_socket_path(socket: Option<PathBuf>) -> PathBuf {
    socket.unwrap_or_else(|| {
        // Try to get tmux session name for per-session isolation
        let session_id = std::process::Command::new("tmux")
            .args(["display-message", "-p", "#S"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".to_string());

        PathBuf::from(format!("/tmp/tcode-{}.sock", session_id))
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = get_socket_path(cli.socket);
    let lua_path = get_lua_path();

    match cli.command {
        None | Some(Commands::Serve) => {
            let api_key = cli.api_key.ok_or_else(|| {
                anyhow::anyhow!("API key required. Set OPENAI_API_KEY env or use --api-key")
            })?;
            let server = Server::new(socket_path, api_key, cli.model, cli.base_url);
            server.run().await
        }
        Some(Commands::Edit) => {
            let client = EditClient::new(socket_path, lua_path);
            client.run().await
        }
        Some(Commands::Display) => {
            let client = DisplayClient::new(socket_path, lua_path);
            client.run().await
        }
    }
}
