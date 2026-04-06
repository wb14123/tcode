use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use browser_server::SearchEngineKind;
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

use crate::browser_client;

static SEARCH_ENGINE: OnceLock<SearchEngineKind> = OnceLock::new();

pub fn set_search_engine(engine: SearchEngineKind) {
    if SEARCH_ENGINE.set(engine).is_err() {
        tracing::warn!("Search engine already set");
    }
}

fn get_search_engine() -> SearchEngineKind {
    SEARCH_ENGINE.get().copied().unwrap_or_default()
}

/// Search the web and return search results
#[tool(timeout_ms = 300000)]
pub fn web_search(
    ctx: ToolContext,
    /// The search query
    query: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let client = match browser_client::get_global_client() {
            Some(c) => c,
            None => {
                yield Err(anyhow!("Browser client not initialized. Is the browser-server running?"));
                return;
            }
        };

        let engine = get_search_engine();

        tokio::select! {
            result = client.web_search(&query, engine) => {
                match result {
                    Ok(content) if content.is_empty() => {
                        yield Ok("No search results found.".to_string());
                    }
                    Ok(content) => yield Ok(content),
                    Err(e) => yield Err(e),
                }
            }
            _ = ctx.cancel_token.cancelled() => {
                yield Err(anyhow!("Cancelled"));
            }
        }
    }
}
