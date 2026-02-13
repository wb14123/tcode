mod claude_auth;
mod display;
mod edit;
mod protocol;
mod server;
mod session;
mod tool_call_display;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tokio::process::Child;
use tracing_subscriber::EnvFilter;

/// LLM provider selection
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum Provider {
    #[default]
    Claude,
    OpenAi,
    OpenRouter,
}

impl Provider {
    fn default_model(&self) -> &'static str {
        match self {
            Provider::Claude => "claude-opus-4-6",
            Provider::OpenAi => "gpt-5-nano",
            Provider::OpenRouter => "deepseek/deepseek-r1",
        }
    }

    fn default_base_url(&self) -> &'static str {
        match self {
            Provider::Claude => "https://api.anthropic.com",
            Provider::OpenAi => "https://api.openai.com/v1",
            Provider::OpenRouter => "https://openrouter.ai/api/v1",
        }
    }

    fn env_var_name(&self) -> &'static str {
        match self {
            Provider::Claude => "ANTHROPIC_ACCESS_TOKEN",
            Provider::OpenAi => "OPENAI_API_KEY",
            Provider::OpenRouter => "OPENROUTER_API_KEY",
        }
    }
}

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
use llm_rs::llm::{Claude, GetTokenFn, OpenAI, OpenRouter, LLM};
use server::Server;
use session::Session;
use tool_call_display::ToolCallDisplayClient;

/// Get API key from CLI or environment variable
fn get_api_key(cli: &Cli, provider: Provider) -> Result<String> {
    cli.api_key.clone()
        .or_else(|| std::env::var(provider.env_var_name()).ok())
        .ok_or_else(|| {
            anyhow!("API key required. Set {} env or use --api-key", provider.env_var_name())
        })
}

/// Create an LLM instance from CLI options
fn create_llm(cli: &Cli) -> Result<(Box<dyn LLM>, String)> {
    let provider = cli.provider;
    let model = cli.model.clone().unwrap_or_else(|| provider.default_model().to_string());
    let base_url = cli.base_url.clone().unwrap_or_else(|| provider.default_base_url().to_string());

    let llm: Box<dyn LLM> = match provider {
        Provider::Claude => {
            let manager = claude_auth::load_token_manager().ok_or_else(|| {
                anyhow!("No Claude authentication found. Run 'tcode claude-auth' to authenticate.")
            })?;
            let get_token: GetTokenFn = std::sync::Arc::new(move || {
                let m = manager.clone();
                Box::pin(async move {
                    m.get_access_token().await.map_err(|e| e.to_string())
                })
            });
            Box::new(Claude::with_get_token(get_token, &base_url))
        }
        Provider::OpenAi => {
            let api_key = get_api_key(cli, provider)?;
            Box::new(OpenAI::with_base_url(&api_key, &base_url))
        }
        Provider::OpenRouter => {
            let api_key = get_api_key(cli, provider)?;
            Box::new(OpenRouter::with_base_url(&api_key, &base_url))
        }
    };

    Ok((llm, model))
}

#[derive(Parser)]
#[command(name = "tcode")]
#[command(about = "Terminal-based LLM conversation interface with neovim")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// LLM provider to use
    #[arg(long, value_enum, default_value_t = Provider::Claude)]
    provider: Provider,

    /// API key/token (defaults to provider-specific env var)
    #[arg(long)]
    api_key: Option<String>,

    /// Model to use (defaults based on provider)
    #[arg(long)]
    model: Option<String>,

    /// Base URL for the API (defaults based on provider)
    #[arg(long)]
    base_url: Option<String>,

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
    /// Show details of a specific tool call
    ToolCall {
        /// The tool call ID to display
        tool_call_id: String,
    },
    /// Launch Chrome browser with persistent profile for login setup
    Browser,
    /// Authenticate with Claude via OAuth and get an API key
    ClaudeAuth,
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

    // Initialize tracing to a log file in the session directory.
    let log_dir = PathBuf::from("/tmp/tcode/sessions").join(&session_id);
    fs::create_dir_all(&log_dir).ok();
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("debug.log"))
        .ok();
    if let Some(log_file) = log_file {
        tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug")),
            )
            .with_ansi(false)
            .init();
    }

    match cli.command {
        None => {
            // Unified startup: server + tmux panes
            run_unified(cli, session_id, lua_path).await
        }
        Some(Commands::Serve) => {
            let (llm, model) = create_llm(&cli)?;
            let session = Session::new(session_id.clone())?;
            let server = Server::new(
                session.socket_path(),
                session.display_file(),
                session.status_file(),
                session.session_dir().clone(),
                llm,
                model,
            );
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
            let session = Session::new(session_id.clone())?;
            let client = DisplayClient::new(session, lua_path, session_id);
            client.run().await
        }
        Some(Commands::ToolCall { tool_call_id }) => {
            let session = Session::new(session_id)?;
            let client = ToolCallDisplayClient::new(session, lua_path, tool_call_id);
            client.run().await
        }
        Some(Commands::Browser) => run_browser().await,
        Some(Commands::ClaudeAuth) => claude_auth::run().await,
    }
}

async fn run_browser() -> Result<()> {
    use headless_chrome::{Browser, LaunchOptions};

    let data_dir = tools::browser::chrome_data_dir();
    fs::create_dir_all(&data_dir)?;

    println!("Launching Chrome with persistent profile at: {}", data_dir.display());
    println!("Log in to your accounts, then close the browser window to save the session.");
    println!();

    let launch_options = LaunchOptions {
        headless: false,
        user_data_dir: Some(data_dir),
        ..LaunchOptions::default()
    };

    let browser = Browser::new(launch_options)?;

    // Get the process and wait for it to exit
    let process = browser.get_process_id();
    if let Some(pid) = process {
        // Poll until the process exits
        loop {
            let exists = std::path::Path::new(&format!("/proc/{}", pid)).exists();
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    println!("Browser closed. Your session data has been saved.");
    Ok(())
}

async fn run_unified(cli: Cli, session_id: String, lua_path: PathBuf) -> Result<()> {
    // Check if running inside tmux
    if !is_in_tmux() {
        anyhow::bail!("tcode must be run inside tmux for the unified mode.\nRun `tcode serve` to start the server without tmux.");
    }

    let (llm, model) = create_llm(&cli)?;

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
        session.display_file(),
        session.status_file(),
        session.session_dir().clone(),
        llm,
        model,
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
    let display_session = Session::new(session_id.clone())?;
    let client = DisplayClient::new(display_session, lua_path, session_id);
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
