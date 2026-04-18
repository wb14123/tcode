use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use futures::SinkExt;
use llm_rs::llm::{ChatOptions, Claude, LLM, OpenAI, OpenRouter, ReasoningEffort};
use llm_rs::tool::ContainerConfig;
use tokio::net::UnixStream;
use tokio_stream::StreamExt;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::config::{self, TcodeConfig};
use crate::protocol::{ClientMessage, ServerMessage};
use crate::server::Server;
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
        let session = Session::new(session_id.to_string())?;
        let (llm, model, token_manager) = create_llm(&self.config, self.profile.as_deref())?;
        let server = Server::new(
            session.socket_path(),
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
                Err(e.context("session runtime failed to start"))
            }
            Err(_) => {
                handle.abort();
                Err(anyhow!(
                    "session runtime terminated before signaling readiness"
                ))
            }
        }
    }
}

pub async fn send_socket_message(
    socket_path: PathBuf,
    msg: &ClientMessage,
) -> Result<Option<ServerMessage>> {
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
