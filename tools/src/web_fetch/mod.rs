use anyhow::{anyhow, Result};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

use crate::browser_client;

/// Returns true if the Content-Type indicates HTML content.
fn is_html_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.contains("text/html") || ct.contains("application/xhtml+xml")
}

/// Fetch content directly via reqwest (for non-HTML content).
async fn fetch_plain(url: &str) -> Result<String> {
    let response = reqwest::get(url).await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("HTTP request failed with status {status}"));
    }
    Ok(response.text().await?)
}

/// Check Content-Type of a URL via HEAD request. Returns None if the request fails.
async fn probe_content_type(url: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client.head(url).send().await.ok()?;
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Fetch a web page and return cleaned HTML content extracted by Readability.
///
/// Note for LLM agent: When using this tool, you should prefer to create a new sub agent to get
/// useful information instead of keep the whole page content in the main conversation.
/// If the request is blocked, DO NOT try to use sub agent to fetch it again since it will fail as
/// well. If you are given a task just to get info from the URL, do not try to use other ways
/// to get the content if blocked, just say so in the response.
#[tool(timeout_ms = 300000)]
pub fn web_fetch(
    ctx: ToolContext,
    /// The URL to fetch and extract content from
    url: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        // Probe Content-Type to decide whether we need a full browser
        let use_browser = match probe_content_type(&url).await {
            Some(ct) => is_html_content_type(&ct),
            // If HEAD fails or has no Content-Type, fall back to browser
            None => true,
        };

        if use_browser {
            let client = match browser_client::get_global_client() {
                Some(c) => c,
                None => {
                    yield Err(anyhow!("Browser client not initialized. Is the browser-server running?"));
                    return;
                }
            };

            tokio::select! {
                result = client.web_fetch(&url) => {
                    yield result;
                }
                _ = ctx.cancel_token.cancelled() => {
                    yield Err(anyhow!("Cancelled"));
                }
            }
        } else {
            tokio::select! {
                result = fetch_plain(&url) => {
                    yield result;
                }
                _ = ctx.cancel_token.cancelled() => {
                    yield Err(anyhow!("Cancelled"));
                }
            }
        }
    }
}
