use anyhow::{anyhow, Result};
use llm_rs_macros::tool;

use crate::browser;

const READABILITY_JS: &str = include_str!("vendor/readability-0.6.0.js");
const EXTRACT_CONTENT_JS: &str = include_str!("extract-content.js");

/// Fetch a web page using headless Chrome and extract clean HTML using Readability.js.
fn fetch_and_extract(url: &str) -> Result<String> {
    let (_browser, tab) = browser::navigate_and_wait(url)?;

    tab.evaluate(READABILITY_JS, false)?;
    let result = tab.evaluate(EXTRACT_CONTENT_JS, false)?;

    match result.value {
        Some(serde_json::Value::String(content)) => Ok(content),
        Some(serde_json::Value::Null) | None => {
            Err(anyhow!("Readability could not extract content from this page"))
        }
        Some(other) => Err(anyhow!("Unexpected result type: {:?}", other)),
    }
}

/// Fetch a web page and return cleaned HTML content extracted by Readability.
/// Note for LLM agent: You should prefer to use a sub agent (if available) to get useful
/// information from the web page instead of keep the whole page content in the main conversation.
#[tool(timeout_ms = 300000)]
pub fn web_fetch(
    /// The URL to fetch and extract content from
    url: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        yield tokio::task::spawn_blocking(move || fetch_and_extract(&url))
            .await
            .map_err(anyhow::Error::from)
            .flatten()
    }
}

#[cfg(test)]
mod tests;
