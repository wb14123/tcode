mod claude_auth;
mod display;
mod edit;
mod protocol;
mod server;
mod session;
mod session_picker;
mod tool_call_display;
mod tree;
mod tty_stdio;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use clap::{Parser, Subcommand, ValueEnum};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio::process::Child;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
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
            Provider::Claude => "ANTHROPIC_API_KEY",
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
use llm_rs::llm::{ChatOptions, Claude, GetTokenFn, OpenAI, OpenRouter, ReasoningEffort, LLM};
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

/// Build ChatOptions from CLI args
fn build_chat_options(_cli: &Cli) -> ChatOptions {
    ChatOptions {
        reasoning_effort: Some(ReasoningEffort::Medium),
        ..Default::default()
    }
}

/// Create an LLM instance from CLI options
fn create_llm(cli: &Cli) -> Result<(Box<dyn LLM>, String)> {
    let provider = cli.provider;
    let model = cli.model.clone().unwrap_or_else(|| provider.default_model().to_string());
    let base_url = cli.base_url.clone().unwrap_or_else(|| provider.default_base_url().to_string());

    let llm: Box<dyn LLM> = match provider {
        Provider::Claude => {
            // Try API key first, fall back to OAuth
            if let Ok(api_key) = get_api_key(cli, provider) {
                Box::new(Claude::with_base_url(&api_key, &base_url))
            } else {
                let manager = claude_auth::load_token_manager().ok_or_else(|| {
                    anyhow!("No Claude authentication found. Set ANTHROPIC_API_KEY env, use --api-key, or run 'tcode claude-auth' to authenticate via OAuth.")
                })?;
                let get_token: GetTokenFn = std::sync::Arc::new(move || {
                    let m = manager.clone();
                    Box::pin(async move {
                        m.get_access_token().await.map_err(|e| e.to_string())
                    })
                });
                Box::new(Claude::with_get_token(get_token, &base_url))
            }
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

    /// Maximum number of LLM call iterations for subagent conversations
    #[arg(long, default_value_t = 50)]
    subagent_max_iterations: usize,

    /// Maximum nesting depth for subagents (0 = no subagents, 1 = one level, etc.)
    #[arg(long, default_value_t = 10)]
    max_subagent_depth: usize,
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
    /// Attach to an existing session and resume the conversation
    Attach,
    /// Cancel a running tool call
    CancelTool {
        /// The tool call ID to cancel
        tool_call_id: String,
    },
    /// List active sessions
    Sessions,
    /// Show tree view of subagents and tool calls
    Tree,
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
    let lua_path = get_lua_path();

    // Helper to require --session flag for subcommands
    let require_session = |opt: Option<String>| -> Result<String> {
        opt.ok_or_else(|| anyhow!("--session=<id> is required for this subcommand"))
    };

    match cli.command {
        None => {
            // Unified startup: server + tmux panes (generates new session ID)
            run_unified(cli, lua_path).await
        }
        Some(Commands::Serve) => {
            let session_id = require_session(cli.session.clone())?;
            init_tracing(&session_id);
            let (llm, model) = create_llm(&cli)?;
            let chat_options = build_chat_options(&cli);
            let session = Session::new(session_id)?;
            let server = Server::new(
                session.socket_path(),
                session.display_file(),
                session.status_file(),
                session.session_dir().clone(),
                session.conversation_state_file(),
                llm,
                model,
                chat_options,
                cli.subagent_max_iterations,
                cli.max_subagent_depth,
            );
            server.run().await
        }
        Some(Commands::Edit) => {
            let session_id = require_session(cli.session)?;
            init_tracing(&session_id);
            let session = Session::new(session_id)?;
            let client = EditClient::new(session, lua_path);
            client.run().await
        }
        Some(Commands::Display) => {
            let session_id = require_session(cli.session)?;
            init_tracing(&session_id);
            let session = Session::new(session_id.clone())?;
            let client = DisplayClient::new(session, lua_path, session_id);
            client.run().await
        }
        Some(Commands::ToolCall { tool_call_id }) => {
            let session_id = require_session(cli.session)?;
            init_tracing(&session_id);
            let session = Session::new(session_id)?;
            let client = ToolCallDisplayClient::new(session, lua_path, tool_call_id);
            client.run().await
        }
        Some(Commands::CancelTool { tool_call_id }) => {
            let session_id = require_session(cli.session)?;
            // Extract root session ID (strip /subagent-* suffix) since the socket
            // is only in the root session directory
            let root_session_id = session_id.split("/subagent-").next().unwrap_or(&session_id).to_string();
            let session = Session::new(root_session_id)?;
            let stream = UnixStream::connect(session.socket_path()).await
                .context("Failed to connect to server socket. Is the server running?")?;
            let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
            let msg = protocol::ClientMessage::CancelTool { tool_call_id };
            let json = serde_json::to_vec(&msg)?;
            framed.send(Bytes::from(json)).await?;
            if let Some(Ok(resp)) = framed.next().await {
                let resp: protocol::ServerMessage = serde_json::from_slice(&resp)?;
                match resp {
                    protocol::ServerMessage::Ack => println!("Tool cancelled"),
                    protocol::ServerMessage::Error { message } => eprintln!("Error: {}", message),
                }
            }
            Ok(())
        }
        Some(Commands::Attach) => {
            let session_id = match cli.session.clone() {
                Some(id) => id,
                None => match session_picker::pick_session()? {
                    Some(id) => id,
                    None => return Ok(()),
                },
            };
            if !is_in_tmux() {
                anyhow::bail!("tcode attach must be run inside tmux.\nRun `tcode serve` to start the server without tmux.");
            }
            let session = Session::new(session_id.clone())?;
            if !session.conversation_state_file().exists() {
                anyhow::bail!("No conversation state found for session '{}'. Nothing to resume.", session_id);
            }
            let (llm, model) = create_llm(&cli)?;
            let chat_options = build_chat_options(&cli);
            run_unified_with_session(
                session, session_id,
                llm, model, chat_options,
                cli.subagent_max_iterations, cli.max_subagent_depth,
                "Attaching to session",
            ).await
        }
        Some(Commands::Sessions) => {
            use std::os::unix::net::UnixStream;
            use llm_rs::conversation::SessionMeta;
            let sessions = session::list_sessions()?;
            if sessions.is_empty() {
                println!("No sessions in ~/.tcode/sessions/");
            } else {
                // Collect session info with metadata for sorting
                let mut entries: Vec<(String, String, Option<String>, u64)> = sessions
                    .into_iter()
                    .map(|id| {
                        let session = Session::new(id.clone())?;
                        let status = if UnixStream::connect(session.socket_path()).is_ok() {
                            "active"
                        } else {
                            "inactive"
                        };
                        let meta = std::fs::read_to_string(session.session_meta_file())
                            .ok()
                            .and_then(|json| serde_json::from_str::<SessionMeta>(&json).ok());
                        let last_active = meta.as_ref().and_then(|m| m.last_active_at).unwrap_or(0);
                        let description = meta.and_then(|m| m.description);
                        Ok((id, status.to_string(), description, last_active))
                    })
                    .collect::<Result<Vec<_>>>()?;

                // Sort by last_active_at descending (most recent first)
                entries.sort_by(|a, b| b.3.cmp(&a.3));

                println!("Sessions:");
                for (id, status, description, _) in entries {
                    if let Some(desc) = description {
                        println!("  {} ({}) {}", id, status, desc);
                    } else {
                        println!("  {} ({})", id, status);
                    }
                }
            }
            Ok(())
        }
        Some(Commands::Tree) => {
            let session_id = match cli.session.clone() {
                Some(id) => id,
                None => match session_picker::pick_session()? {
                    Some(id) => id,
                    None => return Ok(()),
                },
            };
            let session = Session::new(session_id)?;
            tree::run_tree(session)
        }
        Some(Commands::Browser) => run_browser().await,
        Some(Commands::ClaudeAuth) => claude_auth::run().await,
    }
}

fn init_tracing(session_id: &str) {
    let log_dir = session::base_path().join(session_id);
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

async fn run_unified(cli: Cli, _lua_path: PathBuf) -> Result<()> {
    if !is_in_tmux() {
        anyhow::bail!("tcode must be run inside tmux for the unified mode.\nRun `tcode serve` to start the server without tmux.");
    }

    let session_id = session::generate_session_id();
    let session = Session::new(session_id.clone())?;
    let (llm, model) = create_llm(&cli)?;
    let chat_options = build_chat_options(&cli);

    run_unified_with_session(
        session, session_id,
        llm, model, chat_options,
        cli.subagent_max_iterations, cli.max_subagent_depth,
        "Session",
    ).await
}

/// Shared entry point for unified mode: redirects stdio, initializes tracing,
/// starts the server, creates tmux panes, and waits for the display to exit.
async fn run_unified_with_session(
    session: Session,
    session_id: String,
    llm: Box<dyn LLM>,
    model: String,
    chat_options: ChatOptions,
    subagent_max_iterations: usize,
    max_subagent_depth: usize,
    label: &str,
) -> Result<()> {
    let original_stdout = tty_stdio::redirect_output_to_files(
        &session.stdout_log(),
        &session.stderr_log(),
    );
    tty_stdio::write_to_terminal(original_stdout, &format!("{}: {}\n", label, session_id));

    init_tracing(&session_id);

    let socket_path = session.socket_path();

    let exe_path = std::env::current_exe().context("Failed to determine current executable path")?;
    let exe_str = exe_path.to_string_lossy();
    let session_arg = format!("--session={}", session_id);

    let server = Server::new(
        socket_path,
        session.display_file(),
        session.status_file(),
        session.session_dir().clone(),
        session.conversation_state_file(),
        llm,
        model,
        chat_options,
        subagent_max_iterations,
        max_subagent_depth,
    );

    let server_handle = tokio::spawn(async move {
        if let Err(e) = server.run().await {
            eprintln!("[Server] Error: {}", e);
        }
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Capture current pane ID before splitting (for tree pane placement)
    let current_pane_id = Command::new("tmux")
        .args(["display-message", "-p", "#{pane_id}"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let edit_cmd = format!("{} {} edit", exe_str, session_arg);

    let output = Command::new("tmux")
        .args(["split-window", "-v", "-p", "30", "-P", "-F", "#{pane_id}", &edit_cmd])
        .output()
        .context("Failed to run 'tmux' - is tmux installed and in PATH?");

    let edit_pane_id = match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => {
            server_handle.abort();
            return Err(e);
        }
    };

    // Spawn tree pane to the right of the display pane (without stealing focus)
    let tree_cmd = format!("{} {} tree", exe_str, session_arg);
    let tree_pane_id = if !current_pane_id.is_empty() {
        Command::new("tmux")
            .args([
                "split-window", "-h", "-d", "-p", "25",
                "-t", &current_pane_id,
                "-P", "-F", "#{pane_id}",
                &tree_cmd,
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        None
    };

    // Focus the edit pane so the user starts typing there
    let _ = Command::new("tmux")
        .args(["select-pane", "-t", &edit_pane_id])
        .output();

    let display_cmd = format!("{} {} display", exe_str, session_arg);
    let (stdin, stdout, stderr) = tty_stdio::get_original_stdio()
        .context("Failed to get original stdio fds")?;

    let mut display_child = std::process::Command::new("sh")
        .args(["-c", &display_cmd])
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("Failed to spawn display process")?;

    let display_pid = display_child.id();
    let result = {
        let wait_handle = tokio::task::spawn_blocking(move || display_child.wait());
        tokio::select! {
            result = wait_handle => {
                result.unwrap_or_else(|e| Err(std::io::Error::other(e)))
            }
            _ = tokio::signal::ctrl_c() => {
                // Ctrl+C received — terminate display child so we can proceed to cleanup
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(display_pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                ).ok();
                Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "interrupted by Ctrl+C"))
            }
        }
    };

    // Always clean up: kill Chrome, tmux panes, and server — regardless of how we exited
    tools::browser::shutdown_browser();

    let _ = Command::new("tmux")
        .args(["kill-pane", "-t", &edit_pane_id])
        .output();

    if let Some(ref tree_pane) = tree_pane_id {
        let _ = Command::new("tmux")
            .args(["kill-pane", "-t", tree_pane])
            .output();
    }

    server_handle.abort();

    match result {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(()),
        Err(e) => {
            tty_stdio::write_error_to_terminal(&format!("Error: {:?}", e));
            Err(anyhow::anyhow!(e).context("Display process failed"))
        }
    }
}
