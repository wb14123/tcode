mod approve_ui;
mod claude_auth;
mod config_wizard;
mod container;
mod display;
mod edit;
mod openai_auth;
mod permission_ui;
mod session_picker;
mod tool_call_display;
mod tree;
mod tree_nav;
mod tty_stdio;

pub(crate) use tcode_runtime::bootstrap::{auth_command_for_profile, create_llm};
pub(crate) use tcode_runtime::{config, protocol, server, session};

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod config_wizard_tests;

#[cfg(test)]
mod oauth_profile_tests;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use tokio::process::Child;
use tracing_subscriber::EnvFilter;

/// Escape a string for use inside a Lua single-quoted string literal.
/// Replaces `\` with `\\`, `'` with `\'`, and newlines/carriage returns
/// with their escape sequences to prevent injection and syntax errors.
pub(crate) fn lua_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
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
use llm_rs::llm::{ChatOptions, LLM};
use server::Server;
use session::Session;
use tcode_runtime::bootstrap::{
    browser_server_socket_path, build_chat_options, init_browser_client, parse_search_engine,
};
use tool_call_display::ToolCallDisplayClient;

#[derive(Parser)]
#[command(name = "tcode")]
#[command(version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_HASH"), ")"))]
#[command(about = "Terminal-based LLM conversation interface with neovim")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Session ID (defaults to tmux session name or "default")
    #[arg(long)]
    session: Option<String>,

    /// Config profile to use (loads ~/.tcode/config-<profile>.toml)
    #[arg(short = 'p', long)]
    profile: Option<String>,

    /// Name or ID of a running container to exec bash commands into
    #[arg(short = 'c', long = "container")]
    container: Option<String>,

    /// Container runtime CLI to use (default: docker)
    #[arg(long = "container-runtime", default_value = "docker", value_parser = ["docker", "podman"], requires = "container")]
    container_runtime: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the server only (for standalone mode)
    Serve,
    /// Open edit window to compose messages
    Edit {
        /// Target a specific subagent conversation (for interactive recovery)
        #[arg(long)]
        conversation_id: Option<String>,
    },
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
    /// Authenticate with OpenAI via OAuth (Codex subscription)
    OpenaiAuth,
    /// Interactively create a new config file
    Config,
    /// Attach to an existing session and resume the conversation
    Attach,
    /// Cancel a running tool call
    CancelTool {
        /// The tool call ID to cancel
        tool_call_id: String,
    },
    /// Cancel an entire conversation (cascades to all tools and child subagents)
    CancelConversation {
        /// The conversation ID to cancel
        conversation_id: String,
    },
    /// List active sessions
    Sessions,
    /// Show tree view of subagents and tool calls
    Tree,
    /// Open a tool-call detail view in a new tmux window
    OpenToolCall {
        /// The tool call ID to display
        tool_call_id: String,
    },
    /// Open a subagent display+edit split in a new tmux window
    OpenSubagent {
        /// The conversation ID of the subagent
        conversation_id: String,
    },
    /// Show the permission tree view
    Permission,
    /// Open pending tool approvals one by one (used by Ctrl-P shortcut)
    ApproveNext,
    /// Approve or manage a permission (used in tmux display-popup)
    Approve {
        /// Tool name
        #[arg(long)]
        tool: String,
        /// Permission key
        #[arg(long)]
        key: String,
        /// Permission value
        #[arg(long, required_unless_present = "add")]
        value: Option<String>,
        /// Manage (revoke) mode instead of approve mode
        #[arg(long)]
        manage: bool,
        /// Add-permission mode (interactive value input)
        #[arg(long, conflicts_with = "manage")]
        add: bool,
        /// Human-readable prompt describing what is being approved
        #[arg(long, default_value = "")]
        prompt: String,
        /// Per-invocation request ID (UUID) for AllowOnce targeting
        #[arg(long)]
        request_id: Option<String>,
        /// Path to a preview file (for "[v] View in nvim" support)
        #[arg(long)]
        preview_file_path: Option<String>,
        /// Only offer "Allow once" and "Deny" (no session/project caching)
        #[arg(long)]
        once_only: bool,
    },
    /// Start the web backend for browser access (PoC, localhost only).
    ///
    /// Binds to 127.0.0.1 only — connect via http://127.0.0.1:<port>,
    /// not http://localhost:<port> (IPv6 dual-stack may misroute the latter).
    ///
    /// `-p <profile>` is accepted at the top level but not used yet;
    /// reserved for a future login/session ticket.
    ///
    /// Prefer `TCODE_REMOTE_PASSWORD=<secret>` over `--password` to avoid
    /// leaking the secret into `ps` / shell history. The password is stored
    /// as-received (no trimming), so leading/trailing whitespace in the
    /// secret is significant at login time.
    Remote {
        /// TCP port to bind on 127.0.0.1. Required — no default, so the
        /// operator always chooses a port and avoids silent collisions.
        /// `--port 0` is rejected at parse time; pick a concrete port.
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,

        /// Shared secret used for the single-user login flow.
        /// Prefer passing via env (TCODE_REMOTE_PASSWORD).
        // NOTE: if `Cli`/`Commands` ever derives `Debug`/`Display`, wrap `password` in a redacted newtype — Secret in tcode-web does not extend to this CLI-layer struct.
        #[arg(long, env = "TCODE_REMOTE_PASSWORD")]
        password: String,
    },
}

/// Embedded Lua source, compiled into the binary.
const TCODE_LUA: &str = include_str!("../lua/tcode.lua");

/// Embedded tree-sitter query files for the tcode filetype.
const INJECTIONS_SCM: &str = include_str!("../../tree-sitter-tcode/queries/injections.scm");
const HIGHLIGHTS_SCM: &str = include_str!("../../tree-sitter-tcode/queries/highlights.scm");

/// Write the embedded Lua source and tree-sitter query files to a cache directory
/// and return the directory path for the Lua files.
/// Uses `<session_dir>/lua/` so each session gets its own copy (avoids conflicts
/// between concurrent sessions running different binary versions).
/// Also writes `queries/tcode/{injections,highlights}.scm` under `session_dir`.
fn ensure_lua_files(session_dir: &Path, shortcuts: &HashMap<String, String>) -> Result<PathBuf> {
    let lua_dir = session_dir.join("lua");
    std::fs::create_dir_all(&lua_dir)
        .with_context(|| format!("Failed to create lua cache directory {:?}", lua_dir))?;
    let lua_file = lua_dir.join("tcode.lua");

    // Build shortcuts preamble (must come before the module code since it ends with `return M`)
    let content = if shortcuts.is_empty() {
        TCODE_LUA.to_string()
    } else {
        use std::fmt::Write;
        let mut preamble = String::from("_G.tcode_shortcuts = {\n");
        for (name, template) in shortcuts {
            writeln!(
                preamble,
                "  ['{}'] = '{}',",
                lua_escape(name),
                lua_escape(template)
            )
            .expect("writing to String cannot fail");
        }
        preamble.push_str("}\n\n");
        preamble.push_str(TCODE_LUA);
        preamble
    };

    std::fs::write(&lua_file, content)
        .with_context(|| format!("Failed to write tcode.lua to {:?}", lua_file))?;

    // Write tree-sitter query files for the tcode filetype
    let queries_dir = session_dir.join("queries").join("tcode");
    std::fs::create_dir_all(&queries_dir)
        .with_context(|| format!("Failed to create queries directory {:?}", queries_dir))?;
    std::fs::write(queries_dir.join("injections.scm"), INJECTIONS_SCM)
        .with_context(|| format!("Failed to write injections.scm to {:?}", queries_dir))?;
    std::fs::write(queries_dir.join("highlights.scm"), HIGHLIGHTS_SCM)
        .with_context(|| format!("Failed to write highlights.scm to {:?}", queries_dir))?;

    Ok(lua_dir)
}

fn is_in_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// Extract root session ID by stripping any /subagent-* suffix.
fn root_session_id(session_id: &str) -> String {
    session_id
        .split("/subagent-")
        .next()
        .unwrap_or(session_id)
        .to_string()
}

/// Resolve session ID from CLI option, falling back to interactive picker.
/// Returns None if the user cancels the picker.
fn session_id_or_pick(opt: Option<String>) -> Result<Option<String>> {
    match opt {
        Some(id) => Ok(Some(id)),
        None => session_picker::pick_session(),
    }
}

/// Send a message to the server and print the response.
async fn send_server_message(
    session: &Session,
    msg: protocol::ClientMessage,
    success_msg: &str,
) -> Result<()> {
    let response = tcode_runtime::bootstrap::send_socket_message(session.socket_path(), &msg)
        .await
        .context("Failed to connect to server socket. Is the server running?")?;
    if let Some(resp) = response {
        match resp {
            protocol::ServerMessage::Ack => println!("{}", success_msg),
            protocol::ServerMessage::Error { message } => eprintln!("Error: {}", message),
            _ => {}
        }
    }
    Ok(())
}

/// Run a shell command and bail on failure.
fn run_shell_cmd(cmd: &str, context_msg: &str) -> Result<()> {
    let output = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| anyhow!("{}: {}", context_msg, e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}: {}", context_msg, stderr);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let Cli {
        command,
        session,
        profile,
        container,
        container_runtime,
    } = cli;

    // Helper to require --session flag for subcommands
    let require_session = |opt: Option<String>| -> Result<String> {
        opt.ok_or_else(|| anyhow!("--session=<id> is required for this subcommand"))
    };

    // Helper: load config lazily (only called by branches that need it)
    let load_cfg = || config::load_config(profile.as_deref());

    // Validate container and build ContainerConfig lazily — only called by
    // branches that actually exec into the container (None, Serve, Attach).
    // The Permission branch only needs the container name for display.
    async fn resolve_container_config(
        container: &Option<String>,
        runtime: &str,
    ) -> Result<Option<llm_rs::tool::ContainerConfig>> {
        if let Some(ref name) = *container {
            container::validate_container(name, runtime).await?;
            Ok(Some(container::build_container_config(name, runtime)))
        } else {
            Ok(None)
        }
    }

    match command {
        None => {
            if profile.is_none() && !config::config_file_exists(None) {
                use std::io::IsTerminal;
                if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                    config_wizard::run(None, true)?;
                    return Ok(());
                }
                // Non-TTY: fall through; load_cfg() will surface the hint.
            }
            let config = load_cfg()?;
            let container_config = resolve_container_config(&container, &container_runtime).await?;
            run_unified(config, profile.as_deref(), container_config).await
        }
        Some(Commands::Serve) => {
            let config = load_cfg()?;
            let session_id = require_session(session)?;
            init_tracing(&session_id);
            init_browser_client(
                config.browser_server_url.clone(),
                config.browser_server_token.clone(),
            )
            .await?;
            let search_engine = parse_search_engine(config.search_engine_str())?;
            tools::set_search_engine(search_engine);
            let (llm, model, token_manager) = create_llm(&config, profile.as_deref())?;
            let chat_options = build_chat_options();
            let container_config = resolve_container_config(&container, &container_runtime).await?;
            let sess = Session::new(session_id)?;
            let server = Server::new(
                sess.socket_path(),
                sess.display_file(),
                sess.status_file(),
                sess.usage_file(),
                sess.session_dir().clone(),
                sess.conversation_state_file(),
                llm,
                model,
                chat_options,
                config.max_subagent_depth.unwrap_or(10),
                config.subagent_model_selection.unwrap_or(false),
                token_manager,
                container_config,
            );
            server.run(None).await
        }
        Some(Commands::Edit { conversation_id }) => {
            let session_id = require_session(session)?;
            init_tracing(&session_id);
            let session = Session::new(session_id)?;
            let config = load_cfg()?;
            let lua_dir = ensure_lua_files(session.session_dir(), &config.shortcuts)?;
            let client = EditClient::new(session, lua_dir, conversation_id);
            client.run().await
        }
        Some(Commands::Display) => {
            let session_id = require_session(session)?;
            init_tracing(&session_id);
            let session = Session::new(session_id.clone())?;
            let config = load_cfg()?;
            let lua_dir = ensure_lua_files(session.session_dir(), &config.shortcuts)?;
            let runtime_dir = session.session_dir().clone();
            let client = DisplayClient::new(session, lua_dir, session_id, runtime_dir);
            client.run().await
        }
        Some(Commands::ToolCall { tool_call_id }) => {
            let session_id = require_session(session)?;
            init_tracing(&session_id);
            let session = Session::new(session_id)?;
            let config = load_cfg()?;
            let lua_dir = ensure_lua_files(session.session_dir(), &config.shortcuts)?;
            let client = ToolCallDisplayClient::new(session, lua_dir, tool_call_id);
            client.run().await
        }
        Some(Commands::CancelTool { tool_call_id }) => {
            let session_id = require_session(session)?;
            let session = Session::new(root_session_id(&session_id))?;
            let msg = protocol::ClientMessage::CancelTool { tool_call_id };
            send_server_message(&session, msg, "Tool cancelled").await
        }
        Some(Commands::CancelConversation { conversation_id }) => {
            let session_id = require_session(session)?;
            let session = Session::new(root_session_id(&session_id))?;
            let msg = protocol::ClientMessage::CancelConversation { conversation_id };
            send_server_message(&session, msg, "Conversation cancelled").await
        }
        Some(Commands::Attach) => {
            let config = load_cfg()?;
            let session_id = match session_id_or_pick(session)? {
                Some(id) => id,
                None => return Ok(()),
            };
            if !is_in_tmux() {
                anyhow::bail!(
                    "tcode attach must be run inside tmux.\nRun `tcode serve` to start the server without tmux."
                );
            }
            let sess = Session::new(session_id.clone())?;
            if !sess.conversation_state_file().exists() {
                anyhow::bail!(
                    "No conversation state found for session '{}'. Nothing to resume.",
                    session_id
                );
            }
            let (llm, model, token_manager) = create_llm(&config, profile.as_deref())?;
            let chat_options = build_chat_options();
            let container_config = resolve_container_config(&container, &container_runtime).await?;
            run_unified_with_session(
                sess,
                session_id,
                llm,
                model,
                chat_options,
                &config,
                token_manager,
                "Attaching to session",
                container_config,
            )
            .await
        }
        Some(Commands::Sessions) => {
            use llm_rs::conversation::SessionMeta;
            use std::os::unix::net::UnixStream;
            let sessions = session::list_sessions()?;
            if sessions.is_empty() {
                println!("No sessions in ~/.tcode/sessions/");
            } else {
                let mut entries: Vec<(String, String, Option<String>, u64)> = sessions
                    .into_iter()
                    .map(|id| {
                        let session = Session::new(id.clone())?;
                        let status = if UnixStream::connect(session.socket_path()).is_ok() {
                            "active"
                        } else {
                            "inactive"
                        };
                        let meta = fs::read_to_string(session.session_meta_file())
                            .ok()
                            .and_then(|json| serde_json::from_str::<SessionMeta>(&json).ok());
                        let last_active = meta.as_ref().and_then(|m| m.last_active_at).unwrap_or(0);
                        let description = meta.and_then(|m| m.description);
                        Ok((id, status.to_string(), description, last_active))
                    })
                    .collect::<Result<Vec<_>>>()?;

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
            let session_id = match session_id_or_pick(session)? {
                Some(id) => id,
                None => return Ok(()),
            };
            let session = Session::new(session_id)?;
            tree::run_tree(session)
        }
        Some(Commands::OpenToolCall { tool_call_id }) => {
            let session_id = require_session(session)?;
            let exe = std::env::current_exe().context("Failed to determine current executable")?;
            let exe_str = exe.to_string_lossy();
            let inner_cmd = format!(
                "{} --session={} tool-call {}",
                exe_str, session_id, tool_call_id
            );
            let tmux_cmd = format!("tmux new-window -n \"tool-detail\" \"{}\"", inner_cmd);
            run_shell_cmd(&tmux_cmd, "Failed to open tool-call detail window")
        }
        Some(Commands::OpenSubagent { conversation_id }) => {
            let session_id = require_session(session)?;
            let exe = std::env::current_exe().context("Failed to determine current executable")?;
            let exe_str = exe.to_string_lossy();
            let sa_session = format!("{}/subagent-{}", session_id, conversation_id);
            let display_cmd = format!(
                "{} --session={} display; tmux kill-window -t \\$TMUX_PANE",
                exe_str, sa_session
            );
            let edit_cmd = format!(
                "{} --session={} edit --conversation-id={}",
                exe_str, sa_session, conversation_id
            );
            let tmux_cmd = format!(
                "tmux new-window -n \"subagent\" \"{}\" \\; split-window -v -p 30 \"{}\"",
                display_cmd, edit_cmd
            );
            run_shell_cmd(&tmux_cmd, "Failed to open subagent window")
        }
        Some(Commands::Permission) => {
            let session_id = match session_id_or_pick(session)? {
                Some(id) => id,
                None => return Ok(()),
            };
            let session = Session::new(session_id)?;
            permission_ui::run_permission_ui(session, container.clone())
        }
        Some(Commands::ApproveNext) => {
            let session_id = require_session(session)?;
            let session = Session::new(root_session_id(&session_id))?;
            if let Some(0) = permission_ui::approve_all_pending(&session_id, &session.socket_path())
            {
                println!("No pending approvals");
            }
            Ok(())
        }
        Some(Commands::Approve {
            tool,
            key,
            value,
            manage,
            add,
            prompt,
            request_id,
            preview_file_path,
            once_only,
        }) => {
            let session_id = require_session(session)?;
            let session = Session::new(root_session_id(&session_id))?;
            let args = approve_ui::ApproveArgs {
                socket_path: session.socket_path(),
                tool,
                key,
                value,
                manage,
                add,
                prompt,
                request_id,
                preview_file_path: preview_file_path.map(PathBuf::from),
                once_only,
            };
            let result = approve_ui::run_approve(args)?;
            match result {
                approve_ui::ApproveResult::Done => Ok(()),
                approve_ui::ApproveResult::Cancelled => std::process::exit(1),
                approve_ui::ApproveResult::ViewPopup => std::process::exit(10),
            }
        }
        Some(Commands::Browser) => run_browser().await,
        Some(Commands::ClaudeAuth) => claude_auth::run(profile.as_deref()).await,
        Some(Commands::OpenaiAuth) => openai_auth::run(profile.as_deref()).await,
        Some(Commands::Config) => config_wizard::run(profile.as_deref(), false),
        Some(Commands::Remote { port, password }) => {
            // Ordering is load-bearing: init_remote_tracing() MUST run before
            // try_new() so the warnings/advisories emitted inside try_new()
            // are actually delivered.
            init_remote_tracing();

            let container_config = resolve_container_config(&container, &container_runtime).await?;
            let password_on_argv = password_on_argv();
            let cfg = tcode_web::RemoteConfig::try_new(port, password, password_on_argv)?
                .with_runtime_options(profile.clone(), container_config);
            tcode_web::run(cfg).await
        }
    }
}

/// Scan argv for `--password` / `--password=...` without parsing. Used to
/// emit an argv-leak warning; the actual password value is resolved by clap
/// (including `TCODE_REMOTE_PASSWORD`).
fn password_on_argv() -> bool {
    std::env::args_os().any(|arg| {
        let bytes = arg.as_encoded_bytes();
        bytes == b"--password" || bytes.starts_with(b"--password=")
    })
}

/// Initialize a stderr-writing tracing subscriber for the `tcode remote`
/// entry point. The existing `init_tracing(session_id)` writes to a
/// session-specific log file; the web backend has no session dir, so we
/// install a fresh subscriber here.
///
/// Uses `.init()` (not `try_init()`) — `tcode remote` is a fresh process
/// that never had a subscriber installed before this point, so a double-
/// install would be a bug we want to surface loudly.
fn init_remote_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn init_tracing(session_id: &str) {
    let log_dir = match session::base_path() {
        Ok(p) => p.join(session_id),
        Err(e) => {
            eprintln!("Failed to determine session base path: {e}");
            return;
        }
    };
    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("Failed to create log directory {:?}: {}", log_dir, e);
    }
    let log_file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("debug.log"))
    {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("Failed to open debug.log in {:?}: {}", log_dir, e);
            None
        }
    };
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
    use tools::browser_client::BrowserClient;

    let socket_path = browser_server_socket_path();
    if socket_path.exists() {
        let client = BrowserClient::unix(socket_path.clone())?;
        if client.health_check().await {
            println!("Detected running browser-server; stopping it to free the Chrome profile...");
            if let Err(e) = client.shutdown_server().await {
                eprintln!("Warning: failed to request browser-server shutdown: {e}");
            }
            // Poll until the server stops responding (up to ~5s).
            let mut stopped = false;
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if !client.health_check().await {
                    stopped = true;
                    break;
                }
            }
            if !stopped {
                anyhow::bail!(
                    "browser-server did not stop within 5s after /shutdown request. \
                     Stop it manually (e.g. `pkill -f browser-server`) and retry."
                );
            }
            println!("Browser-server stopped.");
            // Give Chrome a brief moment to release the SingletonLock file.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    // If the profile lock is still held, another browser-server (possibly on a
    // non-default --socket path) or a stale lock file is blocking us. Produce
    // a helpful error instead of letting `launch_interactive` emit a generic one.
    let lock_file = browser_server::browser::chrome_data_dir().join("SingletonLock");
    if lock_file.exists() {
        anyhow::bail!(
            "Chrome profile is locked ({}).\n\
             If a browser-server is running on a custom --socket path, stop it manually \
             (e.g. `pkill -f browser-server`).\n\
             Otherwise, remove the stale lock file: `rm {}`",
            lock_file.display(),
            lock_file.display()
        );
    }

    browser_server::browser::launch_interactive().await
}

async fn run_unified(
    config: config::TcodeConfig,
    profile: Option<&str>,
    container_config: Option<llm_rs::tool::ContainerConfig>,
) -> Result<()> {
    if !is_in_tmux() {
        anyhow::bail!(
            "tcode must be run inside tmux for the unified mode.\nRun `tcode serve` to start the server without tmux."
        );
    }

    let session_id = session::generate_session_id();
    let session = Session::new(session_id.clone())?;
    let (llm, model, token_manager) = create_llm(&config, profile)?;
    let chat_options = build_chat_options();

    run_unified_with_session(
        session,
        session_id,
        llm,
        model,
        chat_options,
        &config,
        token_manager,
        "Session",
        container_config,
    )
    .await
}

struct PaneInfo {
    pane_id: String,
    command: config::PanelCommand,
    focus: bool,
}

/// Recursively create tmux panes for the layout tree.
/// `current_pane` is the tmux pane ID that this node occupies.
/// `spawned_pane_ids` collects IDs of newly created panes so the caller can clean
/// them up if a later step fails.
fn create_layout_panes(
    node: &config::LayoutNode,
    current_pane: &str,
    spawned_pane_ids: &mut Vec<String>,
) -> Result<Vec<PaneInfo>> {
    match node {
        config::LayoutNode::Leaf { command, focus, .. } => Ok(vec![PaneInfo {
            pane_id: current_pane.to_string(),
            command: *command,
            focus: focus.unwrap_or(false),
        }]),
        config::LayoutNode::Split { split, a, b, .. } => {
            let b_size = b
                .size()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "50".to_string());

            let split_flag = match split {
                config::SplitDirection::Horizontal => "-h",
                config::SplitDirection::Vertical => "-v",
            };

            let output = Command::new("tmux")
                .args([
                    "split-window",
                    split_flag,
                    "-d",
                    "-p",
                    &b_size,
                    "-t",
                    current_pane,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    // NOTE: `sleep infinity` is a GNU coreutils extension and
                    // fails immediately on macOS's BSD `sleep`, causing the
                    // freshly-created pane to die before the next split-window
                    // can target it ("can't find pane: %N"). Use a large finite
                    // value (~68 years) which is portable across Linux + macOS.
                    "sleep 2147483647",
                ])
                .output()
                .context("Failed to run tmux split-window")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("tmux split-window failed: {}", stderr.trim());
            }
            let b_pane_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if b_pane_id.is_empty() {
                bail!("tmux split-window did not return a pane ID");
            }
            spawned_pane_ids.push(b_pane_id.clone());

            let mut panes = create_layout_panes(a, current_pane, spawned_pane_ids)?;
            panes.extend(create_layout_panes(b, &b_pane_id, spawned_pane_ids)?);
            Ok(panes)
        }
    }
}

/// Kill all spawned (non-original) tmux panes. Best-effort; errors are ignored.
fn kill_spawned_panes(pane_ids: &[String]) {
    for pane_id in pane_ids {
        Command::new("tmux")
            .args(["kill-pane", "-t", pane_id])
            .output()
            .ok();
    }
}

/// Create layout panes, swap display into the original pane, start commands,
/// and set focus. On any failure the already-created panes are killed before
/// returning the error.
fn setup_layout_panes(
    layout: &config::LayoutNode,
    current_pane_id: &str,
    exe_str: &str,
    session_arg: &str,
) -> Result<Vec<PaneInfo>> {
    let mut spawned_pane_ids: Vec<String> = Vec::new();

    let mut panes = match create_layout_panes(layout, current_pane_id, &mut spawned_pane_ids) {
        Ok(p) => p,
        Err(e) => {
            kill_spawned_panes(&spawned_pane_ids);
            return Err(e);
        }
    };

    // Ensure display pane is in the original pane (where we have saved stdio FDs)
    let display_idx = panes
        .iter()
        .position(|p| p.command == config::PanelCommand::Display)
        .ok_or_else(|| anyhow!("no display panel in layout"))?;

    if panes[display_idx].pane_id != current_pane_id {
        let orig_idx = panes
            .iter()
            .position(|p| p.pane_id == current_pane_id)
            .ok_or_else(|| anyhow!("original pane not found in layout"))?;

        let output = Command::new("tmux")
            .args([
                "swap-pane",
                "-d",
                "-s",
                &panes[display_idx].pane_id,
                "-t",
                current_pane_id,
            ])
            .output()
            .context("Failed to run tmux swap-pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            kill_spawned_panes(&spawned_pane_ids);
            bail!("tmux swap-pane failed: {}", stderr);
        }

        // Swap pane IDs in our records (commands stay, pane positions swapped)
        let display_pane_id = panes[display_idx].pane_id.clone();
        panes[display_idx].pane_id = current_pane_id.to_string();
        panes[orig_idx].pane_id = display_pane_id;
    }

    // Start real commands in non-display panes
    for pane in &panes {
        if pane.command == config::PanelCommand::Display {
            continue; // display runs in-process in the caller
        }
        let cmd = match pane.command {
            config::PanelCommand::Edit => format!("{} {} edit", exe_str, session_arg),
            config::PanelCommand::Tree => format!("{} {} tree", exe_str, session_arg),
            config::PanelCommand::Permission => {
                let inner = format!("{} {} permission", exe_str, session_arg);
                format!(
                    "bash -c '{} 2>&1; ret=$?; if [ $ret -ne 0 ]; then echo \"[permission pane exited with code $ret — press Enter to close]\"; read; fi'",
                    inner.replace('\'', "'\\''")
                )
            }
            config::PanelCommand::Display => unreachable!(),
        };
        let output = Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", &pane.pane_id, &cmd])
            .output();
        match output {
            Ok(o) if !o.status.success() => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!(
                    "failed to start {} pane in {}: {}",
                    pane.command,
                    pane.pane_id,
                    stderr
                );
            }
            Err(e) => {
                tracing::warn!("failed to start {} pane: {e}", pane.command);
            }
            _ => {}
        }
    }

    // Set focus
    let focus_pane = panes.iter().find(|p| p.focus).or_else(|| {
        panes
            .iter()
            .find(|p| p.command == config::PanelCommand::Edit)
    });
    if let Some(fp) = focus_pane
        && let Err(e) = Command::new("tmux")
            .args(["select-pane", "-t", &fp.pane_id])
            .output()
    {
        tracing::warn!("failed to focus pane: {e}");
    }

    Ok(panes)
}

/// Shared entry point for unified mode: redirects stdio, initializes tracing,
/// starts the server, creates tmux panes, and waits for the display to exit.
#[allow(clippy::too_many_arguments)]
async fn run_unified_with_session(
    session: Session,
    session_id: String,
    llm: Box<dyn LLM>,
    model: String,
    chat_options: ChatOptions,
    config: &config::TcodeConfig,
    token_manager: Option<Arc<dyn auth::OAuthTokenManager>>,
    label: &str,
    container_config: Option<llm_rs::tool::ContainerConfig>,
) -> Result<()> {
    let max_subagent_depth = config.max_subagent_depth.unwrap_or(10);
    let subagent_model_selection = config.subagent_model_selection.unwrap_or(false);
    let browser_server_url = config.browser_server_url.clone();
    let browser_server_token = config.browser_server_token.clone();
    let search_engine = parse_search_engine(config.search_engine_str())?;
    let layout = config
        .layout
        .clone()
        .unwrap_or_else(config::LayoutNode::default_layout);

    let original_stdout =
        tty_stdio::redirect_output_to_files(&session.stdout_log(), &session.stderr_log());
    tty_stdio::write_to_terminal(original_stdout, &format!("{}: {}\n", label, session_id));

    init_tracing(&session_id);

    // Initialize browser client (before tool registration)
    init_browser_client(browser_server_url, browser_server_token).await?;
    tools::set_search_engine(search_engine);

    let socket_path = session.socket_path();

    let exe_path =
        std::env::current_exe().context("Failed to determine current executable path")?;
    let exe_str = exe_path.to_string_lossy();
    let session_arg = format!("--session={}", session_id);

    let server = Server::new(
        socket_path,
        session.display_file(),
        session.status_file(),
        session.usage_file(),
        session.session_dir().clone(),
        session.conversation_state_file(),
        llm,
        model,
        chat_options,
        max_subagent_depth,
        subagent_model_selection,
        token_manager,
        container_config,
    );

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = server.run(Some(ready_tx)).await {
            eprintln!("[Server] Error: {}", e);
        }
    });

    match ready_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e.context("Server failed to start")),
        Err(_) => return Err(anyhow::anyhow!("Server task terminated unexpectedly")),
    }

    // Capture current pane ID before splitting (for layout placement).
    let current_pane_id = std::env::var("TMUX_PANE")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("TMUX_PANE not set — cannot determine current tmux pane"))?;

    let panes = match setup_layout_panes(&layout, &current_pane_id, &exe_str, &session_arg) {
        Ok(p) => p,
        Err(e) => {
            server_handle.abort();
            return Err(e.context("Failed to set up layout"));
        }
    };

    // Display runs as child process with saved original stdio FDs
    let display_cmd = format!("{} {} display", exe_str, session_arg);
    let (stdin, stdout, stderr) =
        tty_stdio::get_original_stdio().context("Failed to get original stdio fds")?;

    let mut display_child = Command::new("sh")
        .args(["-c", &display_cmd])
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("Failed to spawn display process")?;

    let display_pid: i32 = display_child.id().try_into().unwrap_or(-1);
    let result = {
        let wait_handle = tokio::task::spawn_blocking(move || display_child.wait());
        tokio::select! {
            result = wait_handle => {
                result.unwrap_or_else(|e| Err(std::io::Error::other(e)))
            }
            _ = tokio::signal::ctrl_c() => {
                if display_pid > 0 {
                    nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(display_pid),
                        nix::sys::signal::Signal::SIGTERM,
                    ).ok();
                }
                Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "interrupted by Ctrl+C"))
            }
        }
    };

    // Clean up: kill all non-display panes, then abort server
    for pane in &panes {
        if pane.command == config::PanelCommand::Display {
            continue;
        }
        if let Err(e) = Command::new("tmux")
            .args(["kill-pane", "-t", &pane.pane_id])
            .output()
        {
            tracing::debug!("failed to kill {} pane {}: {e}", pane.command, pane.pane_id);
        }
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
