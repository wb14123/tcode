use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use futures::SinkExt;
use llm_rs::llm::{ChatOptions, Claude, LLM, OpenAI, OpenRouter, ReasoningEffort};
use llm_rs::tool::ContainerConfig;
use tokio::net::UnixStream;
use tokio_stream::StreamExt;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::config::{self, TcodeConfig};
use crate::protocol::{ClientMessage, RuntimeOwnerKind, ServerMessage, SessionRuntimeInfo};
use crate::server::{Server, ServerRuntimeOptions};
use crate::session::Session;

#[derive(Clone)]
pub struct RuntimeSettings {
    profile: Option<String>,
    config: TcodeConfig,
    container_config: Option<ContainerConfig>,
}

impl RuntimeSettings {
    pub fn load(
        profile: Option<String>,
        container_config: Option<ContainerConfig>,
    ) -> Result<Self> {
        let config = config::load_config(profile.as_deref())?;
        Ok(Self {
            profile,
            config,
            container_config,
        })
    }

    pub async fn init_globals(&self) -> Result<()> {
        init_browser_client(
            self.config.browser_server_url.clone(),
            self.config.browser_server_token.clone(),
        )
        .await?;
        let search_engine = parse_search_engine(self.config.search_engine_str())?;
        tools::set_search_engine(search_engine);
        Ok(())
    }

    pub async fn start_runtime(&self, session_id: &str) -> Result<tokio::task::JoinHandle<()>> {
        let owner_kind = RuntimeOwnerKind::Web;
        let session = Session::new(session_id.to_string())?;
        let socket_path = session.socket_path();

        match probe_runtime_status(&socket_path).await? {
            RuntimeProbeStatus::Active(info) => {
                tracing::info!(
                    session_id,
                    owner_kind = ?info.owner_kind,
                    "session runtime already active; attaching instead of starting duplicate runtime"
                );
                return Ok(monitor_existing_runtime(socket_path));
            }
            RuntimeProbeStatus::Unresponsive => {
                return Err(anyhow!(
                    "session runtime socket at {:?} is unresponsive; another runtime may still be starting",
                    socket_path
                ));
            }
            RuntimeProbeStatus::NoSocket | RuntimeProbeStatus::NoListener => {}
        }

        let (llm, model, token_manager) = create_llm(&self.config, self.profile.as_deref())?;
        let server = Server::new_with_runtime_options(
            socket_path.clone(),
            session.display_file(),
            session.status_file(),
            session.usage_file(),
            session.session_dir().clone(),
            session.conversation_state_file(),
            llm,
            model,
            build_chat_options(),
            self.config.max_subagent_depth.unwrap_or(10),
            self.config.subagent_model_selection.unwrap_or(false),
            token_manager,
            self.container_config.clone(),
            ServerRuntimeOptions {
                owner_kind,
                ..ServerRuntimeOptions::default()
            },
        );

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let session_id_owned = session_id.to_string();
        let handle = tokio::spawn(async move {
            if let Err(e) = server.run(Some(ready_tx)).await {
                tracing::error!(session_id = %session_id_owned, error = %e, "session runtime exited with error");
            }
        });

        match ready_rx.await {
            Ok(Ok(())) => Ok(handle),
            Ok(Err(e)) => {
                handle.abort();
                if matches!(
                    probe_runtime_status(&socket_path).await?,
                    RuntimeProbeStatus::Active(_)
                ) {
                    tracing::info!(session_id, "another runtime won startup race; attaching");
                    return Ok(monitor_existing_runtime(socket_path));
                }
                Err(e.context("session runtime failed to start"))
            }
            Err(_) => {
                handle.abort();
                if matches!(
                    probe_runtime_status(&socket_path).await?,
                    RuntimeProbeStatus::Active(_)
                ) {
                    tracing::info!(
                        session_id,
                        "runtime became active after startup task ended; attaching"
                    );
                    return Ok(monitor_existing_runtime(socket_path));
                }
                Err(anyhow!(
                    "session runtime terminated before signaling readiness"
                ))
            }
        }
    }
}

const RUNTIME_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
pub const SOCKET_RPC_TIMEOUT: Duration = Duration::from_secs(5);
pub const RUNTIME_MONITOR_INTERVAL: Duration = Duration::from_secs(2);
pub const RUNTIME_UNRESPONSIVE_GRACE_PROBES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMonitorAction {
    Continue,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeProbeStatus {
    Active(SessionRuntimeInfo),
    NoSocket,
    NoListener,
    Unresponsive,
}

pub fn runtime_monitor_action(
    status: &RuntimeProbeStatus,
    consecutive_unresponsive: &mut usize,
    max_consecutive_unresponsive: usize,
) -> RuntimeMonitorAction {
    match status {
        RuntimeProbeStatus::Active(_) => {
            *consecutive_unresponsive = 0;
            RuntimeMonitorAction::Continue
        }
        RuntimeProbeStatus::Unresponsive => {
            *consecutive_unresponsive = (*consecutive_unresponsive).saturating_add(1);
            if *consecutive_unresponsive >= max_consecutive_unresponsive {
                RuntimeMonitorAction::Stop
            } else {
                RuntimeMonitorAction::Continue
            }
        }
        RuntimeProbeStatus::NoSocket | RuntimeProbeStatus::NoListener => RuntimeMonitorAction::Stop,
    }
}

pub async fn probe_runtime_status(socket_path: &Path) -> Result<RuntimeProbeStatus> {
    if !socket_path.exists() {
        return Ok(RuntimeProbeStatus::NoSocket);
    }

    let stream = match tokio::time::timeout(RUNTIME_PROBE_TIMEOUT, UnixStream::connect(socket_path))
        .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            tracing::debug!(socket = %socket_path.display(), error = %e, "runtime probe found no listener");
            return Ok(RuntimeProbeStatus::NoListener);
        }
        Err(_) => {
            tracing::debug!(socket = %socket_path.display(), "runtime probe connect timed out");
            return Ok(RuntimeProbeStatus::Unresponsive);
        }
    };

    let probe = async move {
        let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
        let json = serde_json::to_vec(&ClientMessage::GetSessionRuntimeInfo)?;
        framed.send(Bytes::from(json)).await?;
        let Some(resp) = framed.next().await else {
            return Ok::<_, anyhow::Error>(None);
        };
        let resp = resp?;
        Ok(Some(serde_json::from_slice::<ServerMessage>(&resp)?))
    };

    match tokio::time::timeout(RUNTIME_PROBE_TIMEOUT, probe).await {
        Ok(Ok(Some(ServerMessage::SessionRuntimeInfo(info)))) if info.active => {
            Ok(RuntimeProbeStatus::Active(info))
        }
        Ok(Ok(_)) => Ok(RuntimeProbeStatus::Unresponsive),
        Ok(Err(e)) => {
            tracing::debug!(socket = %socket_path.display(), error = %e, "runtime probe failed after connect");
            Ok(RuntimeProbeStatus::Unresponsive)
        }
        Err(_) => {
            tracing::debug!(socket = %socket_path.display(), "runtime probe response timed out");
            Ok(RuntimeProbeStatus::Unresponsive)
        }
    }
}

fn monitor_existing_runtime(socket_path: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut consecutive_unresponsive = 0;
        loop {
            tokio::time::sleep(RUNTIME_MONITOR_INTERVAL).await;
            match probe_runtime_status(&socket_path).await {
                Ok(status) => match runtime_monitor_action(
                    &status,
                    &mut consecutive_unresponsive,
                    RUNTIME_UNRESPONSIVE_GRACE_PROBES,
                ) {
                    RuntimeMonitorAction::Continue => {}
                    RuntimeMonitorAction::Stop => {
                        if matches!(status, RuntimeProbeStatus::Unresponsive) {
                            tracing::warn!(
                                socket = %socket_path.display(),
                                probes = consecutive_unresponsive,
                                "runtime monitor stopped after consecutive unresponsive probes"
                            );
                        }
                        break;
                    }
                },
                Err(e) => {
                    tracing::debug!(socket = %socket_path.display(), error = %e, "runtime monitor probe failed");
                    break;
                }
            }
        }
    })
}

pub async fn send_socket_message(
    socket_path: PathBuf,
    msg: &ClientMessage,
) -> Result<Option<ServerMessage>> {
    send_socket_message_with_timeout(socket_path, msg, SOCKET_RPC_TIMEOUT).await
}

pub async fn send_socket_message_with_timeout(
    socket_path: PathBuf,
    msg: &ClientMessage,
    timeout_duration: Duration,
) -> Result<Option<ServerMessage>> {
    let rpc = async {
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect to server socket {:?}", socket_path))?;
        let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
        let json = serde_json::to_vec(msg)?;
        framed.send(Bytes::from(json)).await?;
        if let Some(resp) = framed.next().await {
            let resp = resp?;
            return Ok(Some(serde_json::from_slice(&resp)?));
        }
        Ok(None)
    };

    tokio::time::timeout(timeout_duration, rpc)
        .await
        .with_context(|| format!("timed out waiting for runtime socket {:?}", socket_path))?
}

pub fn auth_command_for_profile(profile: Option<&str>, command: &str) -> String {
    match profile {
        Some(profile) => format!("tcode -p {profile} {command}"),
        None => format!("tcode {command}"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Provider {
    Claude,
    ClaudeOauth,
    OpenAi,
    OpenAiOauth,
    OpenRouter,
}

impl Provider {
    fn default_model(&self) -> &'static str {
        match self {
            Provider::Claude | Provider::ClaudeOauth => "claude-opus-4-6",
            Provider::OpenAi => "gpt-5-nano",
            Provider::OpenAiOauth => "gpt-5.4",
            Provider::OpenRouter => "deepseek/deepseek-r1",
        }
    }

    fn default_base_url(&self) -> &'static str {
        match self {
            Provider::Claude | Provider::ClaudeOauth => "https://api.anthropic.com",
            Provider::OpenAi => "https://api.openai.com/v1",
            Provider::OpenAiOauth => "https://chatgpt.com/backend-api/codex",
            Provider::OpenRouter => "https://openrouter.ai/api/v1",
        }
    }

    fn env_var_name(&self) -> &'static str {
        match self {
            Provider::Claude => "ANTHROPIC_API_KEY",
            Provider::OpenAi => "OPENAI_API_KEY",
            Provider::OpenRouter => "OPENROUTER_API_KEY",
            Provider::ClaudeOauth | Provider::OpenAiOauth => {
                unreachable!("env_var_name called on an OAuth provider variant")
            }
        }
    }
}

fn get_api_key(config: &TcodeConfig, provider: Provider) -> String {
    if let Some(k) = config.api_key.as_ref()
        && !k.is_empty()
    {
        return k.clone();
    }
    if let Ok(k) = std::env::var(provider.env_var_name())
        && !k.is_empty()
    {
        return k;
    }
    String::new()
}

pub fn build_chat_options() -> ChatOptions {
    ChatOptions {
        reasoning_effort: Some(ReasoningEffort::High),
        ..Default::default()
    }
}

type CreateLlmResult = (
    Box<dyn LLM>,
    String,
    Option<Arc<dyn auth::OAuthTokenManager>>,
);

pub fn create_llm(config: &TcodeConfig, profile: Option<&str>) -> Result<CreateLlmResult> {
    let provider_str = config.provider.as_deref().ok_or_else(|| {
        let path = config::config_path_for(profile)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        anyhow!(
            "provider is required in {}. Expected one of: claude | claude-oauth | open-ai | open-ai-oauth | open-router.",
            path
        )
    })?;
    let provider = parse_provider(provider_str)?;
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| provider.default_model().to_string());
    let base_url = config
        .base_url
        .clone()
        .unwrap_or_else(|| provider.default_base_url().to_string());

    let (llm, token_manager): (Box<dyn LLM>, Option<Arc<dyn auth::OAuthTokenManager>>) =
        match provider {
            Provider::Claude => {
                let api_key = get_api_key(config, provider);
                (Box::new(Claude::with_base_url(&api_key, &base_url)), None)
            }
            Provider::ClaudeOauth => {
                let manager = auth::claude::TokenManager::load(profile).ok_or_else(|| {
                    let auth_command = auth_command_for_profile(profile, "claude-auth");
                    let storage_path = auth::claude::TokenManager::storage_path(profile);
                    anyhow!(
                        "No Claude OAuth tokens found at {}. Run `{}` to authenticate.",
                        storage_path.display(),
                        auth_command
                    )
                })?;
                let llm = Box::new(Claude::with_token_provider(manager.clone(), &base_url));
                (
                    llm,
                    Some(Arc::new(manager) as Arc<dyn auth::OAuthTokenManager>),
                )
            }
            Provider::OpenAi => {
                let api_key = get_api_key(config, provider);
                (Box::new(OpenAI::with_base_url(&api_key, &base_url)), None)
            }
            Provider::OpenAiOauth => {
                let manager = auth::openai::TokenManager::load(profile).ok_or_else(|| {
                    let auth_command = auth_command_for_profile(profile, "openai-auth");
                    let storage_path = auth::openai::TokenManager::storage_path(profile);
                    anyhow!(
                        "No OpenAI OAuth tokens found at {}. Run `{}` to authenticate.",
                        storage_path.display(),
                        auth_command
                    )
                })?;
                let account_id = manager
                    .tokens()
                    .try_read()
                    .ok()
                    .and_then(|t| t.account_id.clone());
                let llm = Box::new(
                    OpenAI::with_token_provider(manager.clone(), &base_url)
                        .with_account_id(account_id),
                );
                (
                    llm,
                    Some(Arc::new(manager) as Arc<dyn auth::OAuthTokenManager>),
                )
            }
            Provider::OpenRouter => {
                let api_key = get_api_key(config, provider);
                (
                    Box::new(OpenRouter::with_base_url(&api_key, &base_url)),
                    None,
                )
            }
        };

    Ok((llm, model, token_manager))
}

fn parse_provider(s: &str) -> Result<Provider> {
    match s {
        "claude" => Ok(Provider::Claude),
        "claude-oauth" => Ok(Provider::ClaudeOauth),
        "open-ai" | "openai" => Ok(Provider::OpenAi),
        "open-ai-oauth" | "openai-oauth" => Ok(Provider::OpenAiOauth),
        "open-router" | "openrouter" => Ok(Provider::OpenRouter),
        other => bail!(
            "unknown provider \"{other}\" in config file, expected: claude, claude-oauth, open-ai, open-ai-oauth, open-router"
        ),
    }
}

pub fn parse_search_engine(s: &str) -> Result<browser_server::SearchEngineKind> {
    match s {
        "kagi" => Ok(browser_server::SearchEngineKind::Kagi),
        "google" => Ok(browser_server::SearchEngineKind::Google),
        other => bail!("unknown search_engine \"{other}\" in config file, expected: kagi, google"),
    }
}

pub fn browser_server_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tcode")
        .join("browser-server.sock")
}

pub async fn init_browser_client(
    browser_server_url: Option<String>,
    browser_server_token: Option<String>,
) -> Result<()> {
    use tools::browser_client::{BrowserClient, set_global_client};

    if let Some(url) = browser_server_url {
        let token = browser_server_token.unwrap_or_default();
        set_global_client(BrowserClient::tcp(url, token));
        return Ok(());
    }

    let socket_path = browser_server_socket_path();
    let browser_server_exe = std::env::current_exe()
        .context("Failed to determine current executable")?
        .parent()
        .ok_or_else(|| anyhow!("No parent directory for executable"))?
        .join("browser-server");

    let client = BrowserClient::unix(socket_path.clone())?
        .with_auto_restart(socket_path, browser_server_exe);
    client.ensure_server_running().await;
    set_global_client(client);
    Ok(())
}
