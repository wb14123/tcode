use std::sync::Arc;

use anyhow::{Result, anyhow};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use llm_rs::llm::{Claude, LLM, OpenAI, OpenRouter};
use llm_rs_server::auth::{default_token_file, load_tokens};
use llm_rs_server::claude_auth;
use llm_rs_server::config::Provider;
use llm_rs_server::handler::{AppState, create_router};

#[derive(Parser)]
#[command(name = "llm-rs-server")]
#[command(about = "OpenAI-compatible API proxy for llm-rs providers")]
struct Cli {
    /// Upstream LLM provider
    #[arg(long, value_enum, default_value_t = Provider::OpenRouter)]
    provider: Provider,

    /// API key for the upstream provider (or set provider-specific env var)
    #[arg(long, env)]
    api_key: Option<String>,

    /// Base URL override for the upstream API
    #[arg(long)]
    base_url: Option<String>,

    /// Address to bind the server to
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,

    /// Path to the token file (JSON array of allowed bearer tokens)
    #[arg(long)]
    token_file: Option<String>,
}

fn get_api_key(cli: &Cli, provider: Provider) -> Option<String> {
    cli.api_key
        .clone()
        .or_else(|| std::env::var(provider.env_var_name()).ok())
}

fn create_llm(cli: &Cli) -> Result<Box<dyn LLM>> {
    let provider = cli.provider;
    let base_url = cli
        .base_url
        .clone()
        .unwrap_or_else(|| provider.default_base_url().to_string());

    match provider {
        Provider::Claude => {
            if let Some(api_key) = get_api_key(cli, provider) {
                Ok(Box::new(Claude::with_base_url(&api_key, &base_url)))
            } else if let Some(manager) = claude_auth::load_token_manager() {
                tracing::info!("Using Claude OAuth tokens from ~/.tcode/auth/claude_tokens.json");
                Ok(Box::new(Claude::with_token_provider(manager, &base_url)))
            } else {
                Err(anyhow!(
                    "Claude requires an API key or OAuth tokens.\n\
                     Set ANTHROPIC_API_KEY, use --api-key, or run 'tcode claude-auth' first."
                ))
            }
        }
        Provider::OpenAi => {
            let api_key = get_api_key(cli, provider)
                .ok_or_else(|| anyhow!("API key required. Set OPENAI_API_KEY or use --api-key"))?;
            Ok(Box::new(OpenAI::with_base_url(&api_key, &base_url)))
        }
        Provider::OpenRouter => {
            let api_key = get_api_key(cli, provider).ok_or_else(|| {
                anyhow!("API key required. Set OPENROUTER_API_KEY or use --api-key")
            })?;
            Ok(Box::new(OpenRouter::with_base_url(&api_key, &base_url)))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let token_path = cli
        .token_file
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_token_file);
    let auth_tokens = load_tokens(&token_path)?;
    tracing::info!(
        "Loaded {} auth token(s) from {}",
        auth_tokens.len(),
        token_path.display()
    );

    let llm = create_llm(&cli)?;

    let state = Arc::new(AppState { llm, auth_tokens });

    let app = create_router(state);
    let listener = tokio::net::TcpListener::bind(&cli.bind).await?;
    tracing::info!("Listening on {}", cli.bind);
    axum::serve(listener, app).await?;

    Ok(())
}
