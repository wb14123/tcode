#[cfg(test)]
mod web_fetch_tests;

use anyhow::{Context, Result, anyhow};
use llm_rs::fetch_budget::WebFetchBudget;
use llm_rs::media::ContentPart;
use llm_rs::permission::SCOPE_WEB_FETCH;
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

use crate::browser_client;
use crate::media_util;

static MEDIA_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.stop();
            }
            match browser_server::web_fetch::validate_url(attempt.url().as_str()) {
                Ok(()) => attempt.follow(),
                Err(_) => attempt.stop(),
            }
        }))
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::ACCEPT,
                reqwest::header::HeaderValue::from_static("image/*, application/pdf, */*"),
            );
            headers.insert(
                reqwest::header::ACCEPT_LANGUAGE,
                reqwest::header::HeaderValue::from_static("en-US,en;q=0.9"),
            );
            headers
        })
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build shared media client")
});

/// Returns true if the Content-Type indicates HTML content.
fn is_html_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.contains("text/html") || ct.contains("application/xhtml+xml")
}

/// Fetch content directly via reqwest (for non-HTML content).
async fn fetch_plain(url: &str) -> Result<String> {
    let response = MEDIA_CLIENT.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("HTTP request failed with status {status}"));
    }
    Ok(response.text().await?)
}

/// Check Content-Type of a URL via HEAD request. Returns None if the request fails.
async fn probe_content_type(url: &str) -> Option<String> {
    let resp = MEDIA_CLIENT
        .head(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?;
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

/// Why the sent content is shorter than the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TruncationReason {
    /// The whole page (from the requested offset) was sent.
    None,
    /// The fetch itself cut the page (`skip_chars` or `max_length`).
    MaxLength,
    /// The remaining web_fetch budget cut what was sent.
    BudgetLimited { shown: usize, skip: usize },
}

/// Budget figures printed above a result (`None` in the main conversation).
struct BudgetSummary {
    /// Effective remaining chars of the tree, after this call's charge.
    remaining: u32,
    /// Chars this call was granted.
    used: u32,
}

/// What fetched text became after being charged against the tree budget.
struct ChargeOutcome {
    /// Chars the fetched text had before the charge.
    cost: u32,
    /// Chars actually granted (exactly what may be sent).
    granted: u32,
    /// Effective remaining chars of the tree after the charge.
    remaining: u32,
    /// The text to send: the fetched text cut to `granted` chars.
    text: String,
}

impl ChargeOutcome {
    /// True when a non-empty page could not be charged at all, so nothing may
    /// be sent.
    fn is_blocked(&self) -> bool {
        self.cost > 0 && self.granted == 0
    }

    fn summary(&self) -> BudgetSummary {
        BudgetSummary {
            remaining: self.remaining,
            used: self.granted,
        }
    }
}

/// Budget line printed above a content result.
fn budget_line(remaining: u32, cost: u32) -> String {
    format!(
        "[web_fetch] Budget (web_fetch only): {remaining} chars remaining for this subagent tree \
         (this call used {cost})."
    )
}

/// Budget line printed above an image/PDF result, which are never charged.
fn media_budget_line(remaining: u32) -> String {
    format!(
        "[web_fetch] Budget (web_fetch only): {remaining} chars remaining for this subagent tree \
         (images and PDFs are not charged)."
    )
}

/// Notice returned instead of content once the tree budget is exhausted.
fn blocked_notice() -> String {
    "[web_fetch] Blocked: the web_fetch character budget for this subagent tree is exhausted \
     (0 chars remaining). Nothing was sent.\n\
     Do not retry web_fetch. Report what you already have; this budget only limits web_fetch, \
     so keep using the other tools as needed."
        .to_string()
}

/// Line explaining that the budget, not the page, limited the output.
fn budget_limited_line(shown: usize, skip: usize) -> String {
    format!(
        "[web_fetch] Output limited by the remaining web_fetch budget: showing {shown} chars \
         starting at {skip}. The next fetch will be blocked; use the other tools normally and \
         report what you have unless more of this page is essential."
    )
}

/// Charges `text` against `budget` and cuts it to what was granted.
fn charge_text(budget: &WebFetchBudget, text: &str) -> ChargeOutcome {
    let cost = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
    let granted = budget.charge(cost);
    let text = if granted < cost {
        apply_truncation(text, granted as usize, 0).0
    } else {
        text.to_string()
    };
    ChargeOutcome {
        cost,
        granted,
        remaining: budget.get(),
        text,
    }
}

/// The `max_length` this call may ask for, or `None` when the tree budget has
/// no chars left and nothing may be fetched.
fn budgeted_max_length(requested: u32, budget: &WebFetchBudget) -> Option<u32> {
    match budget.get() {
        0 => None,
        remaining => Some(requested.min(remaining)),
    }
}

/// Picks the wording of a budgeted result; a budget cut wins over the fetch's
/// own truncation.
fn result_reason(
    requested: u32,
    effective: u32,
    skip_chars: usize,
    fetch_truncated: bool,
    charged: &ChargeOutcome,
) -> TruncationReason {
    let budget_cut = charged.granted < charged.cost
        || (fetch_truncated && effective < requested && effective > 0 && charged.cost == effective);
    // `fetch_truncated` also covers a non-zero `skip_chars`, so a page that ends
    // exactly at the clamp with an offset is reported as budget-limited even
    // though its whole remainder was sent. Harmless here: the pool is at 0, so
    // the "next fetch will be blocked" advice is true either way.
    if budget_cut {
        return TruncationReason::BudgetLimited {
            shown: charged.granted as usize,
            skip: skip_chars,
        };
    }
    if fetch_truncated {
        return TruncationReason::MaxLength;
    }
    TruncationReason::None
}

/// Format the web_fetch result with metadata header. `budget` is `None` in the
/// main conversation, which prints no budget lines.
fn format_result(
    content: &str,
    total_length: u32,
    reason: TruncationReason,
    skip_chars: usize,
    budget: Option<BudgetSummary>,
) -> String {
    let header = match reason {
        TruncationReason::None => {
            format!("[web_fetch] Showing full content ({total_length} chars).")
        }
        TruncationReason::BudgetLimited { shown, skip } => budget_limited_line(shown, skip),
        TruncationReason::MaxLength => {
            let start = skip_chars;
            let end = start + content.chars().count();
            let remaining = (total_length as usize).saturating_sub(end);
            match budget {
                Some(_) => format!(
                    "[web_fetch] Content truncated. Showing chars {start}-{end} of {total_length}.\n\
                     There are {remaining} chars remaining. To read the rest, call web_fetch again with \
                     skip_chars={end}; further fetches cost the same tree budget."
                ),
                None => format!(
                    "[web_fetch] Content truncated. Showing chars {start}-{end} of {total_length}.\n\
                     There are {remaining} chars remaining. To read the full page, create separate sub agents \
                     each fetching a different portion using skip_chars (e.g. skip_chars={end}). \
                     Each sub agent should summarize its own chunk to maintain a smaller context window."
                ),
            }
        }
    };

    match budget {
        Some(summary) => format!(
            "{}\n{header}\n{content}",
            budget_line(summary.remaining, summary.used)
        ),
        None => format!("{header}\n{content}"),
    }
}

/// Check if a URL's path suggests an image or PDF, based on file extension.
/// Returns "image", "pdf", or None.
fn url_media_hint(url: &str) -> Option<&'static str> {
    let path = reqwest::Url::parse(url).ok()?.path().to_lowercase();
    if path.ends_with(".png")
        || path.ends_with(".jpg")
        || path.ends_with(".jpeg")
        || path.ends_with(".gif")
        || path.ends_with(".webp")
        || path.ends_with(".bmp")
    {
        Some("image")
    } else if path.ends_with(".pdf") {
        Some("pdf")
    } else {
        None
    }
}

/// Returns true if the Content-Type indicates an image.
fn is_image_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    let base = ct.split(';').next().unwrap_or(&ct).trim();
    matches!(
        base,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp"
    )
}

/// Returns true if the Content-Type indicates a PDF.
fn is_pdf_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    let base = ct.split(';').next().unwrap_or(&ct).trim();
    base == "application/pdf"
}

/// Download URL bytes with a size limit, streaming chunks to bound memory.
/// Returns an error if the response exceeds max_size bytes.
async fn download_with_limit(
    client: &reqwest::Client,
    url: &str,
    max_size: usize,
) -> Result<Vec<u8>> {
    let response = client.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("HTTP request failed with status {status}"));
    }

    // Check Content-Length if available (early rejection, best-effort)
    if let Some(content_length) = response.content_length()
        && content_length > max_size as u64
    {
        return Err(anyhow!(
            "Response too large: {} bytes (max {})",
            content_length,
            max_size
        ));
    }

    // Stream response body, enforcing the size limit as we go
    let mut data = Vec::new();
    let mut stream = response.bytes_stream();
    use tokio_stream::StreamExt;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Failed to read response body")?;
        if data.len() + chunk.len() > max_size {
            return Err(anyhow!(
                "Response too large: exceeds {} bytes limit",
                max_size
            ));
        }
        data.extend_from_slice(&chunk);
    }

    Ok(data)
}

enum MediaKind {
    Image,
    Pdf,
}

/// Fetch a web page and return cleaned content extracted by Chrome's accessibility tree.
/// Default max_length is 20000 chars. Use skip_chars for pagination.
/// For large pages, paginate with skip_chars; chunks fetched by subagents all draw on the same tree budget.
/// Prefer targeted pages and partial reads.
/// If the site blocks the request, do NOT retry via a subagent - it will also fail; say so.
/// In a subagent tree, calls draw on a shared web_fetch budget (400000 chars by
/// default). Only web_fetch content counts. `max_length` shrinks when the budget
/// is short, calls return a notice when it is 0, and results show what remains.
#[tool(timeout_ms = 300000)]
pub fn web_fetch(
    ctx: ToolContext,
    /// The URL to fetch and extract content from
    url: String,
    /// Max chars to return (default: 20000). Use with skip_chars to read large pages in chunks via parallel sub agents. Lowered automatically if the remaining web_fetch budget is smaller.
    #[serde(default)]
    max_length: Option<u32>,
    /// Chars to skip from content start (default: 0). For pagination: if first call returned 20000 chars of a 50000-char page, set skip_chars=20000 to read the next chunk.
    #[serde(default)]
    skip_chars: Option<u32>,
) -> impl tokio_stream::Stream<Item = Result<ContentPart>> {
    async_stream::try_stream! {
        // Fail fast: an exhausted tree budget fetches nothing, prompts for
        // nothing, and costs nothing.
        let budget = ctx.web_fetch_budget.clone();
        if budget.as_ref().is_some_and(|node| node.get() == 0) {
            yield ContentPart::Text(blocked_notice());
            return;
        }

        // Extract hostname for permission check
        let hostname = reqwest::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());

        // NOTE: `hostname` must be a real host string, never the literal "*".
        // "*" is reserved as a wildcard in the permission store and only
        // enters storage via the add-permission UI.
        ctx.permission.ask_permission_for(
            SCOPE_WEB_FETCH,
            &format!("Allow web_fetch to access {}?", url),
            "hostname",
            &hostname,
        ).await?;

        // SSRF check: validate URL before any HTTP I/O
        browser_server::web_fetch::validate_url(&url)
            .map_err(|e| anyhow!("SSRF check failed for {}: {}", url, e))?;

        // --- Image/PDF detection ---
        let media_hint = url_media_hint(&url);
        let content_type = probe_content_type(&url).await;

        let media_kind = match (&content_type, media_hint) {
            (Some(ct), _) if is_image_content_type(ct) => Some(MediaKind::Image),
            (Some(ct), _) if is_pdf_content_type(ct) => Some(MediaKind::Pdf),
            (Some(ct), Some("image")) if !is_image_content_type(ct) => {
                tracing::warn!(
                    "URL {} has image extension but Content-Type is {:?}; using Content-Type",
                    url, ct
                );
                None
            }
            (Some(ct), Some("pdf")) if !is_pdf_content_type(ct) => {
                tracing::warn!(
                    "URL {} has .pdf extension but Content-Type is {:?}; using Content-Type",
                    url, ct
                );
                None
            }
            (None, Some("image")) => Some(MediaKind::Image),
            (None, Some("pdf")) => Some(MediaKind::Pdf),
            _ => None,
        };

        match media_kind {
            Some(MediaKind::Image) => {
                let media_dir = media_util::require_media_dir(&ctx, "fetch image URL")?;

                if max_length.is_some() || skip_chars.is_some() {
                    tracing::warn!(
                        "max_length and skip_chars are ignored for image/PDF URLs: {}",
                        url
                    );
                }

                let data = download_with_limit(
                    &MEDIA_CLIENT,
                    &url,
                    media_util::MAX_IMAGE_SIZE as usize,
                ).await?;

                let (media_data, annotation) =
                    media_util::save_image_to_media(data, &url, &media_dir)
                        .await
                        .map_err(|e| anyhow!("Failed to process image from {}: {}", url, e))?;
                if let Some(node) = &budget {
                    yield ContentPart::Text(media_budget_line(node.get()));
                }
                yield ContentPart::Media(media_data);
                yield ContentPart::Text(annotation);
                return;
            }
            Some(MediaKind::Pdf) => {
                let media_dir = media_util::require_media_dir(&ctx, "fetch PDF URL")?;

                if max_length.is_some() || skip_chars.is_some() {
                    tracing::warn!(
                        "max_length and skip_chars are ignored for image/PDF URLs: {}",
                        url
                    );
                }

                let data = download_with_limit(
                    &MEDIA_CLIENT,
                    &url,
                    media_util::MAX_PDF_SIZE as usize,
                ).await?;

                let (media_data, annotation) =
                    media_util::save_pdf_to_media(data, &url, &media_dir)
                        .await
                        .map_err(|e| anyhow!("Failed to process PDF from {}: {}", url, e))?;
                if let Some(node) = &budget {
                    yield ContentPart::Text(media_budget_line(node.get()));
                }
                yield ContentPart::Media(media_data);
                yield ContentPart::Text(annotation);
                return;
            }
            None => { /* fall through to HTML/TEXT path */ }
        }

        // --- Continue with existing HTML/TEXT path ---
        let requested = max_length.unwrap_or(20_000);
        let skip = skip_chars.unwrap_or(0) as usize;

        // A budgeted call asks the fetch for no more than the tree can pay for.
        // The budget is re-read here: another call may have drained it since the
        // fail-fast check, in which case nothing may be sent.
        let (effective, browser_max_length) = match budget.as_deref() {
            Some(node) => match budgeted_max_length(requested, node) {
                Some(effective) => (effective, Some(effective)),
                None => {
                    yield ContentPart::Text(blocked_notice());
                    return;
                }
            },
            None => (requested, max_length),
        };

        // Reuse the Content-Type already probed above
        let use_browser = match &content_type {
            Some(ct) => is_html_content_type(ct),
            None => true,
        };

        let (content, total_length, is_truncated) = if use_browser {
            let client = browser_client::get_global_client()
                .ok_or_else(|| anyhow!("Browser client not initialized. Is the browser-server running?"))?;

            let result = tokio::select! {
                result = client.web_fetch(&url, browser_max_length, skip_chars) => result,
                _ = ctx.cancel_token.cancelled() => Err(anyhow!("Cancelled")),
            };
            result?
        } else {
            let result = tokio::select! {
                result = fetch_plain(&url) => result,
                _ = ctx.cancel_token.cancelled() => Err(anyhow!("Cancelled")),
            };
            let full_content = result?;
            apply_truncation(&full_content, effective as usize, skip)
        };

        match budget {
            None => {
                let reason = if is_truncated {
                    TruncationReason::MaxLength
                } else {
                    TruncationReason::None
                };
                yield ContentPart::Text(format_result(
                    &content,
                    total_length,
                    reason,
                    skip,
                    None,
                ));
            }
            Some(node) => {
                let charged = charge_text(node.as_ref(), &content);
                if charged.is_blocked() {
                    yield ContentPart::Text(blocked_notice());
                } else {
                    let reason =
                        result_reason(requested, effective, skip, is_truncated, &charged);
                    yield ContentPart::Text(format_result(
                        &charged.text,
                        total_length,
                        reason,
                        skip,
                        Some(charged.summary()),
                    ));
                }
            }
        }
    }
}
