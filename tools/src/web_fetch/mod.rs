use anyhow::{Context, Result, anyhow};
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

/// Format the web_fetch result with metadata header.
fn format_result(
    content: &str,
    total_length: u32,
    is_truncated: bool,
    skip_chars: usize,
) -> String {
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
/// For large pages, spawn parallel sub agents each with a different skip_chars value; have each summarize its chunk.
/// Prefer using a sub agent to avoid keeping the full page in the main context.
/// If blocked, do NOT retry via sub agent — it will also fail. Just say so.
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
) -> impl tokio_stream::Stream<Item = Result<ContentPart>> {
    async_stream::try_stream! {
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
                yield ContentPart::Media(media_data);
                yield ContentPart::Text(annotation);
                return;
            }
            None => { /* fall through to HTML/TEXT path */ }
        }

        // --- Continue with existing HTML/TEXT path ---
        let max_len = max_length.unwrap_or(20_000) as usize;
        let skip = skip_chars.unwrap_or(0) as usize;

        // Reuse the Content-Type already probed above
        let use_browser = match &content_type {
            Some(ct) => is_html_content_type(ct),
            None => true,
        };

        if use_browser {
            let client = browser_client::get_global_client()
                .ok_or_else(|| anyhow!("Browser client not initialized. Is the browser-server running?"))?;

            let result = tokio::select! {
                result = client.web_fetch(&url, max_length, skip_chars) => result,
                _ = ctx.cancel_token.cancelled() => Err(anyhow!("Cancelled")),
            };
            let (content, total_length, is_truncated) = result?;
            yield ContentPart::Text(format_result(&content, total_length, is_truncated, skip));
        } else {
            let result = tokio::select! {
                result = fetch_plain(&url) => result,
                _ = ctx.cancel_token.cancelled() => Err(anyhow!("Cancelled")),
            };
            let full_content = result?;
            let (content, total_length, is_truncated) = apply_truncation(&full_content, max_len, skip);
            yield ContentPart::Text(format_result(&content, total_length, is_truncated, skip));
        }
    }
}
