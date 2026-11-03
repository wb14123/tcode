//! Tests for rate-limit reset parsing of provider headers and bodies.

use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use super::rate_limit::{
    claude_is_rate_limited_error, claude_legacy_reset_at, claude_reset_at, error_message,
    openai_is_usage_limit_error, openai_reset_at, parse_epoch_header, parse_retry_after,
    truncate_message,
};
use super::sse::HttpError;

const CLAUDE_PREFIX: &str = "anthropic-ratelimit-unified";
const DAY: u64 = 24 * 60 * 60;

/// Header map built from `(name, value)` pairs.
fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_bytes(name.as_bytes()).expect("valid test header name");
        let value = HeaderValue::from_str(value).expect("valid test header value");
        map.insert(name, value);
    }
    map
}

/// HTTP error fixture carrying `body` and no headers.
fn http_error(status: StatusCode, body: &str) -> HttpError {
    HttpError {
        status,
        headers: HeaderMap::new(),
        body: body.to_string(),
    }
}

/// Unified Claude header name for `suffix`.
fn claude(suffix: &str) -> String {
    format!("{CLAUDE_PREFIX}-{suffix}")
}

/// Seconds since the Unix epoch now.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs()
}

/// Epoch seconds `offset` seconds from now.
fn epoch_in(offset: u64) -> String {
    (now_epoch() + offset).to_string()
}

/// Assert a parsed reset lies `offset` seconds from now, allowing for the time
/// spent between building the fixture and parsing it.
fn assert_reset_in(reset: Option<SystemTime>, offset: u64) {
    let reset = reset.expect("expected a reset hint");
    let actual = reset
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("reset is after the Unix epoch")
        .as_secs();
    let expected = now_epoch() + offset;
    assert!(
        actual.abs_diff(expected) <= 3,
        "reset at {actual} is not within 3s of {expected}"
    );
}

// ============================================================================
// retry-after
// ============================================================================

#[test]
fn retry_after_seconds() {
    let map = headers(&[("retry-after", "120")]);
    assert_eq!(parse_retry_after(&map), Some(Duration::from_secs(120)));
}

#[test]
fn retry_after_http_dates() {
    let date = chrono::DateTime::<chrono::Utc>::from(SystemTime::now() + Duration::from_secs(90));
    for value in [date.to_rfc2822(), date.to_rfc3339()] {
        let map = headers(&[("retry-after", &value)]);
        let delay = parse_retry_after(&map).expect("an HTTP date must parse");
        assert!(
            (87..=90).contains(&delay.as_secs()),
            "unexpected delay {delay:?} for {value}"
        );
    }
}

#[test]
fn retry_after_rejects_unusable_values() {
    assert_eq!(parse_retry_after(&HeaderMap::new()), None);
    assert_eq!(
        parse_retry_after(&headers(&[("retry-after", "soon")])),
        None
    );
    assert_eq!(
        parse_retry_after(&headers(&[("retry-after", &epoch_in(31 * DAY))])),
        None
    );
}

// ============================================================================
// Claude unified headers
// ============================================================================

#[test]
fn claude_single_blocked_window() {
    let map = headers(&[
        (&claude("5h-status"), "exceeded"),
        (&claude("5h-reset"), &epoch_in(3600)),
        (&claude("7d-status"), "allowed"),
        (&claude("7d-reset"), &epoch_in(7200)),
    ]);
    assert_reset_in(claude_reset_at(&map), 3600);
}

#[test]
fn claude_waits_for_the_latest_blocked_window() {
    let map = headers(&[
        (&claude("5h-status"), "rate_limited"),
        (&claude("5h-reset"), &epoch_in(600)),
        (&claude("7d-status"), "exceeded"),
        (&claude("7d-reset"), &epoch_in(5400)),
    ]);
    assert_reset_in(claude_reset_at(&map), 5400);
}

#[test]
fn claude_ignores_allowed_windows() {
    let map = headers(&[
        (&claude("5h-status"), "allowed"),
        (&claude("5h-reset"), &epoch_in(600)),
        (&claude("7d-status"), "allowed"),
        (&claude("7d-reset"), &epoch_in(5400)),
    ]);
    assert_eq!(claude_reset_at(&map), None);
}

#[test]
fn claude_uses_representative_claim_reset() {
    // Builds that omit the per-window status headers report the blocked state
    // only in the unified status.
    let map = headers(&[
        (&claude("status"), "exceeded"),
        (&claude("representative-claim"), "five_hour"),
        (&claude("5h-utilization"), "1.0"),
        (&claude("5h-reset"), &epoch_in(1800)),
    ]);
    assert_reset_in(claude_reset_at(&map), 1800);
}

#[test]
fn claude_ignores_claim_without_a_blocking_status() {
    let map = headers(&[
        (&claude("status"), "allowed"),
        (&claude("representative-claim"), "seven_day"),
        (&claude("7d-status"), "allowed"),
        (&claude("7d-reset"), &epoch_in(1800)),
    ]);
    assert_eq!(claude_reset_at(&map), None);
}

#[test]
fn claude_seven_day_sonnet_window() {
    let map = headers(&[
        (&claude("7d_sonnet-status"), "exceeded"),
        (&claude("7d_sonnet-reset"), &epoch_in(900)),
    ]);
    assert_reset_in(claude_reset_at(&map), 900);
}

#[test]
fn claude_ignores_missing_or_malformed_reset() {
    let missing = headers(&[(&claude("5h-status"), "exceeded")]);
    assert_eq!(claude_reset_at(&missing), None);

    for value in ["soon", "", "1762244400.0"] {
        let map = headers(&[
            (&claude("5h-status"), "exceeded"),
            (&claude("5h-reset"), value),
        ]);
        assert_eq!(claude_reset_at(&map), None, "reset value {value:?}");
    }
}

#[test]
fn claude_falls_back_to_retry_after() {
    let map = headers(&[(&claude("5h-status"), "exceeded"), ("retry-after", "42")]);
    assert_reset_in(claude_reset_at(&map), 42);
}

#[test]
fn claude_clamps_a_reset_in_the_past() {
    let past = (now_epoch() - 600).to_string();
    let map = headers(&[
        (&claude("5h-status"), "exceeded"),
        (&claude("5h-reset"), &past),
    ]);
    assert_reset_in(claude_reset_at(&map), 0);
}

#[test]
fn claude_error_type_detection() {
    assert!(claude_is_rate_limited_error("rate_limit_error"));
    assert!(!claude_is_rate_limited_error("overloaded_error"));
    assert!(!claude_is_rate_limited_error(""));
}

#[test]
fn claude_legacy_message_epoch() {
    let epoch = now_epoch() + 600;
    assert_reset_in(
        claude_legacy_reset_at(&format!("rate limit reached | {epoch}")),
        600,
    );
}

#[test]
fn claude_legacy_message_without_epoch() {
    assert_eq!(claude_legacy_reset_at("rate limit reached"), None);
    assert_eq!(claude_legacy_reset_at("rate limit reached | soon"), None);
}

// ============================================================================
// OpenAI Codex headers and usage-limit body
// ============================================================================

#[test]
fn openai_usage_limit_body_reset() {
    let body = format!(
        r#"{{"error":{{"type":"usage_limit_reached","resets_at":{},"resets_in_seconds":1800}}}}"#,
        now_epoch() + 1800
    );
    assert!(openai_is_usage_limit_error(&body));
    assert_reset_in(openai_reset_at(&HeaderMap::new(), &body), 1800);
}

#[test]
fn openai_usage_limit_body_detection() {
    assert!(openai_is_usage_limit_error(
        r#"{"error":{"code":"usage_limit_reached"}}"#
    ));
    assert!(!openai_is_usage_limit_error(
        r#"{"error":{"type":"server_error"}}"#
    ));
    assert!(!openai_is_usage_limit_error("not json"));
}

#[test]
fn openai_body_of_another_error_type_is_ignored() {
    let body = r#"{"error":{"type":"server_error","resets_at":1762244400}}"#;
    assert_eq!(openai_reset_at(&HeaderMap::new(), body), None);
}

#[test]
fn openai_body_without_positive_resets_at_falls_back_to_headers() {
    let body = r#"{"error":{"type":"usage_limit_reached","resets_at":0}}"#;
    let map = headers(&[
        ("x-codex-primary-used-percent", "100"),
        ("x-codex-primary-reset-at", &epoch_in(240)),
    ]);
    assert_reset_in(openai_reset_at(&map, body), 240);
}

#[test]
fn openai_codex_header_reset() {
    let map = headers(&[
        ("x-codex-primary-used-percent", "100"),
        ("x-codex-primary-reset-at", &epoch_in(120)),
        ("x-codex-secondary-used-percent", "42"),
        ("x-codex-secondary-reset-at", &epoch_in(9000)),
    ]);
    assert_reset_in(openai_reset_at(&map, ""), 120);
}

#[test]
fn openai_secondary_only_limit() {
    let map = headers(&[
        ("x-codex-primary-used-percent", "12"),
        ("x-codex-primary-reset-at", &epoch_in(60)),
        ("x-codex-secondary-used-percent", "100"),
        ("x-codex-secondary-reset-at", &epoch_in(600)),
    ]);
    assert_reset_in(openai_reset_at(&map, ""), 600);
}

#[test]
fn openai_takes_the_latest_reset_across_header_families() {
    let map = headers(&[
        ("x-codex-active-limit", "premium"),
        ("x-codex-primary-used-percent", "100"),
        ("x-codex-primary-reset-at", &epoch_in(300)),
        ("x-premium-primary-used-percent", "100"),
        ("x-premium-primary-reset-at", &epoch_in(960)),
    ]);
    assert_reset_in(openai_reset_at(&map, ""), 960);
}

#[test]
fn openai_active_limit_value_is_case_insensitive() {
    let map = headers(&[
        ("x-codex-active-limit", "Premium"),
        ("x-premium-primary-used-percent", "100"),
        ("x-premium-primary-reset-at", &epoch_in(420)),
    ]);
    assert_reset_in(openai_reset_at(&map, ""), 420);
}

#[test]
fn openai_active_limit_codex_keeps_the_default_family() {
    let map = headers(&[
        ("x-codex-active-limit", "codex"),
        ("x-codex-primary-used-percent", "100"),
        ("x-codex-primary-reset-at", &epoch_in(180)),
    ]);
    assert_reset_in(openai_reset_at(&map, ""), 180);
}

#[test]
fn openai_in_stream_failure_event_reset() {
    let body = format!(
        r#"{{"type":"response.failed","response":{{"error":{{"code":"usage_limit_reached","resets_at":{}}}}}}}"#,
        now_epoch() + 1500
    );
    assert!(openai_is_usage_limit_error(&body));
    assert_reset_in(openai_reset_at(&HeaderMap::new(), &body), 1500);
}

#[test]
fn openai_in_stream_failure_of_another_error_is_ignored() {
    let body = format!(
        r#"{{"type":"response.failed","response":{{"error":{{"code":"server_error","resets_at":{}}}}}}}"#,
        now_epoch() + 1500
    );
    assert!(!openai_is_usage_limit_error(&body));
    assert_eq!(openai_reset_at(&HeaderMap::new(), &body), None);
}

#[test]
fn openai_reset_after_seconds_header() {
    let map = headers(&[
        ("x-codex-secondary-used-percent", "100"),
        ("x-codex-secondary-reset-after-seconds", "300"),
    ]);
    assert_reset_in(openai_reset_at(&map, ""), 300);
}

#[test]
fn openai_ignores_malformed_and_absurd_resets() {
    let map = headers(&[
        ("x-codex-primary-used-percent", "100"),
        ("x-codex-primary-reset-at", "soon"),
        ("x-codex-secondary-used-percent", "100"),
        ("x-codex-secondary-reset-at", &epoch_in(31 * DAY)),
    ]);
    assert_eq!(openai_reset_at(&map, ""), None);
}

#[test]
fn openai_falls_back_to_retry_after() {
    let map = headers(&[("retry-after", "30")]);
    assert_reset_in(openai_reset_at(&map, ""), 30);
}

// ============================================================================
// Shared validation
// ============================================================================

#[test]
fn parse_epoch_header_reads_seconds() {
    let map = headers(&[("x-reset", &epoch_in(77))]);
    assert_reset_in(parse_epoch_header(&map, "x-reset"), 77);
    assert_eq!(parse_epoch_header(&map, "x-missing"), None);
    assert_eq!(
        parse_epoch_header(&headers(&[("x-reset", "-5")]), "x-reset"),
        None
    );
}

#[test]
fn parse_epoch_header_rejects_reset_beyond_the_horizon() {
    let map = headers(&[("x-reset", &epoch_in(31 * DAY))]);
    assert_eq!(parse_epoch_header(&map, "x-reset"), None);
}

// ============================================================================
// Reporting a rate-limited response
// ============================================================================

#[test]
fn error_message_prefers_the_provider_message() {
    let error = http_error(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"type":"error","error":{"type":"rate_limit_error","message":"5-hour limit reached"}}"#,
    );
    assert_eq!(error_message(&error), "5-hour limit reached");
}

#[test]
fn error_message_falls_back_to_the_http_text() {
    let error = http_error(StatusCode::TOO_MANY_REQUESTS, "<html>slow down</html>");
    assert_eq!(
        error_message(&error),
        "API error 429 Too Many Requests: <html>slow down</html>"
    );
}

#[test]
fn error_message_clips_an_oversized_provider_message() {
    let long = "x".repeat(2000);
    let body = format!(r#"{{"error":{{"message":"{long}"}}}}"#);
    let message = error_message(&http_error(StatusCode::TOO_MANY_REQUESTS, &body));
    assert!(message.ends_with("..."), "unexpected end: {message:?}");
    assert!(message.chars().count() <= 503, "message not clipped");

    let message = error_message(&http_error(StatusCode::TOO_MANY_REQUESTS, &long));
    assert!(message.ends_with("..."), "unexpected end: {message:?}");
    assert!(message.chars().count() <= 503, "message not clipped");
}

#[test]
fn truncate_message_leaves_short_text_untouched() {
    assert_eq!(
        truncate_message("5-hour limit reached"),
        "5-hour limit reached"
    );

    let at_limit = "y".repeat(500);
    assert_eq!(truncate_message(&at_limit), at_limit);

    let over_limit = "y".repeat(501);
    assert_eq!(
        truncate_message(&over_limit),
        format!("{}...", "y".repeat(500))
    );
}
