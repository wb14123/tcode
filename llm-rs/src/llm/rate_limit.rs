//! Rate-limit reset detection from provider responses.
//!
//! The parsers operate on the headers and body of a failing response only, so
//! they can be tested without a network. A reset hint is discarded when it is
//! implausible and clamped to "now" when it lies in the past.

use std::time::{Duration, SystemTime};

use reqwest::header::HeaderMap;

use super::sse::HttpError;

/// Bound on a plausible reset hint, protecting against unit and parse bugs.
///
/// This is a sanity check on provider data, never a cap on how long the
/// conversation is willing to wait.
const MAX_RESET_HORIZON: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Maximum number of characters of provider text kept for display and logs.
const MAX_DISPLAY_CHARS: usize = 500;

/// Error type and code OpenAI uses for an exhausted usage limit.
pub(crate) const USAGE_LIMIT_REACHED: &str = "usage_limit_reached";

const RETRY_AFTER: &str = "retry-after";

// ============================================================================
// Shared parsing
// ============================================================================

/// Header value as a trimmed string; values that are not UTF-8 are ignored.
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
}

/// Keep a reset hint only when it is plausible, clamping past values to now.
///
/// A hint beyond [`MAX_RESET_HORIZON`] comes from a bug rather than a real
/// window. A hint in the past means the window may already have reopened, so it
/// is clamped rather than dropped: the retry loop still keeps its minimum pause.
fn validate_reset(reset: SystemTime) -> Option<SystemTime> {
    let now = SystemTime::now();
    if reset > now + MAX_RESET_HORIZON {
        return None;
    }
    Some(reset.max(now))
}

/// A delay in seconds, or `None` when it is implausible.
fn clamp_delay(seconds: u64) -> Option<Duration> {
    (seconds <= MAX_RESET_HORIZON.as_secs()).then(|| Duration::from_secs(seconds))
}

/// Reset `seconds` from now, or `None` when the delay is implausible.
fn reset_after_seconds(seconds: u64) -> Option<SystemTime> {
    clamp_delay(seconds).map(|delay| SystemTime::now() + delay)
}

/// Reset at `seconds` since the Unix epoch.
fn epoch_reset(seconds: u64) -> Option<SystemTime> {
    let reset = SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))?;
    validate_reset(reset)
}

/// Parse an HTTP date (`retry-after` carries one instead of a delay).
fn http_date(value: &str) -> Option<SystemTime> {
    let parsed = chrono::DateTime::parse_from_rfc2822(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(value));
    parsed
        .ok()
        .map(|date| SystemTime::from(date.with_timezone(&chrono::Utc)))
}

/// Reset carried by a `retry-after` header: a delay in seconds or an HTTP date.
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = header_str(headers, RETRY_AFTER)?;
    if let Ok(seconds) = value.parse::<u64>() {
        return clamp_delay(seconds);
    }
    let now = SystemTime::now();
    let reset = validate_reset(http_date(value)?)?;
    Some(reset.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Reset carried by the header `name` as epoch seconds.
pub(crate) fn parse_epoch_header(headers: &HeaderMap, name: &str) -> Option<SystemTime> {
    epoch_reset(header_str(headers, name)?.parse::<u64>().ok()?)
}

/// Reset implied by a `retry-after` header.
fn retry_after_reset(headers: &HeaderMap) -> Option<SystemTime> {
    let delay = parse_retry_after(headers)?;
    Some(SystemTime::now() + delay)
}

// ============================================================================
// Claude (unified rate-limit headers)
// ============================================================================

/// Prefix of the unified rate-limit headers, e.g. `anthropic-ratelimit-unified-5h-reset`.
const CLAUDE_UNIFIED: &str = "anthropic-ratelimit-unified";

/// Usage windows reported by the unified rate-limit headers.
const CLAUDE_WINDOWS: [&str; 3] = ["5h", "7d", "7d_sonnet"];

/// Window suffix of a unified representative claim.
fn claude_claim_window(claim: &str) -> Option<&'static str> {
    match claim {
        "five_hour" => Some("5h"),
        "seven_day" => Some("7d"),
        "seven_day_sonnet" => Some("7d_sonnet"),
        _ => None,
    }
}

/// Whether a unified window status still blocks requests.
fn claude_status_blocks(status: &str) -> bool {
    matches!(status, "exceeded" | "rate_limited")
}

fn claude_window_blocks(headers: &HeaderMap, window: &str) -> bool {
    header_str(headers, &format!("{CLAUDE_UNIFIED}-{window}-status"))
        .is_some_and(claude_status_blocks)
}

fn claude_window_reset(headers: &HeaderMap, window: &str) -> Option<SystemTime> {
    parse_epoch_header(headers, &format!("{CLAUDE_UNIFIED}-{window}-reset"))
}

/// Reset hinted by the representative claim when no blocking window reports one.
fn claude_claim_reset(headers: &HeaderMap) -> Option<SystemTime> {
    let claim = header_str(headers, &format!("{CLAUDE_UNIFIED}-representative-claim"))?;
    let window = claude_claim_window(claim)?;
    // Some builds omit the per-window status headers and report the blocked
    // state only in the unified status.
    let blocks = claude_window_blocks(headers, window)
        || header_str(headers, &format!("{CLAUDE_UNIFIED}-status"))
            .is_some_and(claude_status_blocks);
    if !blocks {
        return None;
    }
    claude_window_reset(headers, window)
}

/// Moment the Claude rate limit lifts, taken from the unified headers.
pub(crate) fn claude_reset_at(headers: &HeaderMap) -> Option<SystemTime> {
    // Every blocking window has to reset before a retry can succeed, so the
    // latest of them is the earliest moment worth retrying.
    let latest = CLAUDE_WINDOWS
        .into_iter()
        .filter(|&window| claude_window_blocks(headers, window))
        .filter_map(|window| claude_window_reset(headers, window))
        .max();
    if latest.is_some() {
        return latest;
    }
    claude_claim_reset(headers).or_else(|| retry_after_reset(headers))
}

/// Whether an in-stream error type reports a rate limit.
pub(crate) fn claude_is_rate_limited_error(error_type: &str) -> bool {
    error_type == "rate_limit_error"
}

/// Reset epoch appended to a rate-limit error message after a `|`.
pub(crate) fn claude_legacy_reset_at(message: &str) -> Option<SystemTime> {
    let (_, epoch) = message.rsplit_once('|')?;
    epoch_reset(epoch.trim().parse::<u64>().ok()?)
}

// ============================================================================
// OpenAI Codex (per-window headers and usage-limit body)
// ============================================================================

/// Header family of the per-window Codex headers.
const CODEX_FAMILY: &str = "x-codex";

/// Windows reported per header family, e.g. `x-codex-primary-used-percent`.
const CODEX_WINDOWS: [&str; 2] = ["primary", "secondary"];

/// Header prefixes of the per-window headers.
///
/// Besides the fixed `x-codex` family, some builds use the active limit name as
/// prefix (e.g. `x-premium-primary-reset-at`).
fn openai_families(headers: &HeaderMap) -> Vec<String> {
    let mut families = vec![CODEX_FAMILY.to_string()];
    if let Some(limit) = header_str(headers, "x-codex-active-limit") {
        let family = format!("x-{}", limit.to_ascii_lowercase());
        if family != CODEX_FAMILY {
            families.push(family);
        }
    }
    families
}

/// Per-window header prefixes, e.g. `x-codex-primary`.
fn openai_windows(headers: &HeaderMap) -> Vec<String> {
    let mut windows = Vec::new();
    for family in openai_families(headers) {
        for window in CODEX_WINDOWS {
            windows.push(format!("{family}-{window}"));
        }
    }
    windows
}

/// Whether a window reports its usage at or over the limit.
fn openai_window_blocks(headers: &HeaderMap, window: &str) -> bool {
    header_str(headers, &format!("{window}-used-percent"))
        .and_then(|value| value.parse::<f64>().ok())
        .is_some_and(|used| used >= 100.0)
}

/// Error object of a provider payload.
///
/// HTTP error bodies keep it at the root; in-stream failure events nest it under
/// `response`.
fn payload_error(value: &serde_json::Value) -> Option<&serde_json::Value> {
    value
        .get("error")
        .or_else(|| value.get("response")?.get("error"))
}

/// Whether an error object's `type` or `code` reports an exhausted usage limit.
fn reports_usage_limit(error: &serde_json::Value) -> bool {
    ["type", "code"]
        .into_iter()
        .any(|field| error.get(field).and_then(|value| value.as_str()) == Some(USAGE_LIMIT_REACHED))
}

/// Whether a payload reports an exhausted usage limit.
pub(crate) fn openai_is_usage_limit_error(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .is_ok_and(|value| payload_error(&value).is_some_and(reports_usage_limit))
}

/// `resets_at` of a usage-limit body.
fn payload_reset_at(body: &str) -> Option<SystemTime> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let seconds = payload_error(&value)?.get("resets_at")?.as_u64()?;
    if seconds == 0 {
        return None;
    }
    epoch_reset(seconds)
}

/// Moment the OpenAI rate limit lifts, taken from the body or the Codex headers.
pub(crate) fn openai_reset_at(headers: &HeaderMap, body: &str) -> Option<SystemTime> {
    if openai_is_usage_limit_error(body)
        && let Some(reset) = payload_reset_at(body)
    {
        return Some(reset);
    }
    let windows = openai_windows(headers);
    let mut resets = Vec::new();
    let mut delays = Vec::new();
    for window in &windows {
        if !openai_window_blocks(headers, window) {
            continue;
        }
        if let Some(reset) = parse_epoch_header(headers, &format!("{window}-reset-at")) {
            resets.push(reset);
        }
        if let Some(seconds) = header_str(headers, &format!("{window}-reset-after-seconds"))
            .and_then(|value| value.parse::<u64>().ok())
            && let Some(reset) = reset_after_seconds(seconds)
        {
            delays.push(reset);
        }
    }
    // Every exhausted window has to reset before a retry can succeed.
    if let Some(latest) = resets.into_iter().max() {
        return Some(latest);
    }
    if let Some(latest) = delays.into_iter().max() {
        return Some(latest);
    }
    retry_after_reset(headers)
}

// ============================================================================
// Reporting a rate-limited response
// ============================================================================

/// `error.message` of a provider error body.
fn json_error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let message = payload_error(&value)?.get("message")?.as_str()?;
    Some(message.to_string())
}

/// Truncate `text` to the length kept for display and logs.
///
/// Rate-limit text ends up in the transcript, in the status bar and in
/// `display.jsonl`, so a provider that answers with a huge body must not be able
/// to write it all out.
pub(crate) fn truncate_message(text: &str) -> String {
    match text.char_indices().nth(MAX_DISPLAY_CHARS) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_string(),
    }
}

/// Message describing a rate-limited response.
///
/// The provider's own error message is preferred; an unparseable body falls back
/// to the full error text.
pub(crate) fn error_message(error: &HttpError) -> String {
    match json_error_message(&error.body) {
        Some(message) => truncate_message(&message),
        None => truncate_message(&error.to_string()),
    }
}

/// Log a rate-limited response that carried no reset hint.
///
/// Header values are omitted because they may hold credentials; the truncated
/// body lets real captures refine the parsers.
pub(crate) fn log_missing_reset(provider: &str, error: &HttpError) {
    let headers = error
        .headers
        .keys()
        .map(reqwest::header::HeaderName::as_str)
        .collect::<Vec<_>>()
        .join(",");
    tracing::warn!(
        provider,
        status = %error.status,
        headers = %headers,
        body = %truncate_message(&error.body),
        "Rate limited without a reset hint"
    );
}
