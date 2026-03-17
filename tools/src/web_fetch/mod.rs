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

/// Apply skip_chars and max_length truncation to content.
/// Returns `(truncated_content, total_length, is_truncated)`.
fn apply_truncation(content: &str, max_length: usize, skip_chars: usize) -> (String, u32, bool) {
    let total_length = content.len() as u32;

    let skip_end = content
        .char_indices()
        .nth(skip_chars)
        .map(|(i, _)| i)
        .unwrap_or(content.len());
    let after_skip = &content[skip_end..];

    let truncate_end = after_skip
        .char_indices()
        .nth(max_length)
        .map(|(i, _)| i)
        .unwrap_or(after_skip.len());
    let result = after_skip[..truncate_end].to_string();

    let is_truncated = skip_chars > 0 || truncate_end < after_skip.len();
    (result, total_length, is_truncated)
}

/// Format the web_fetch result with metadata header.
fn format_result(content: &str, total_length: u32, is_truncated: bool, skip_chars: usize) -> String {
    if is_truncated {
        let start = skip_chars;
        let end = start + content.chars().count();
        let remaining = (total_length as usize).saturating_sub(end);
        format!(
            "[web_fetch] Content truncated. Showing chars {start}-{end} of {total_length}.\n\
             There are {remaining} chars remaining. To read the full page, create separate sub agents \
             each fetching a different portion using skip_chars (e.g. skip_chars={end}). \
             Each sub agent should summarize its own chunk to maintain a smaller context window.\n\
             {content}"
        )
    } else {
        format!("[web_fetch] Showing full content ({total_length} chars).\n{content}")
    }
}

/// Fetch a web page and return cleaned HTML content extracted by Readability.
///
/// Default max_length is 20000 chars. Use skip_chars for pagination.
/// For large pages: create separate sub agents each with a different skip_chars
/// value to read different portions in parallel, and have each sub agent summarize
/// its own chunk for better context management.
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
    /// Max chars to return (default: 20000). Use with skip_chars to read large pages in chunks via parallel sub agents.
    #[serde(default)]
    max_length: Option<u32>,
    /// Chars to skip from content start (default: 0). For pagination: if first call returned 20000 chars of a 50000-char page, set skip_chars=20000 to read the next chunk.
    #[serde(default)]
    skip_chars: Option<u32>,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        // Extract hostname for permission check
        let hostname = reqwest::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());

        if !ctx.permission.ask_permission(
            &format!("Allow web_fetch to access {}?", hostname),
            "hostname",
            &hostname,
        ).await {
            yield Err(anyhow!("Permission denied: web_fetch access to {} was not allowed", hostname));
            return;
        }

        let max_len = max_length.unwrap_or(20_000) as usize;
        let skip = skip_chars.unwrap_or(0) as usize;

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
                result = client.web_fetch(&url, max_length, skip_chars) => {
                    match result {
                        Ok((content, total_length, is_truncated)) => {
                            yield Ok(format_result(&content, total_length, is_truncated, skip));
                        }
                        Err(e) => yield Err(e),
                    }
                }
                _ = ctx.cancel_token.cancelled() => {
                    yield Err(anyhow!("Cancelled"));
                }
            }
        } else {
            tokio::select! {
                result = fetch_plain(&url) => {
                    match result {
                        Ok(full_content) => {
                            let (content, total_length, is_truncated) = apply_truncation(&full_content, max_len, skip);
                            yield Ok(format_result(&content, total_length, is_truncated, skip));
                        }
                        Err(e) => yield Err(e),
                    }
                }
                _ = ctx.cancel_token.cancelled() => {
                    yield Err(anyhow!("Cancelled"));
                }
            }
        }
    }
}
