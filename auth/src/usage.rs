//! Fetch Claude subscription usage data from the Anthropic API.
//!
//! Uses the undocumented `GET https://api.anthropic.com/api/oauth/usage` endpoint
//! to retrieve rate-limit window utilisation and reset times.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Top-level response from `GET /api/oauth/usage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionUsage {
    /// 5-hour rolling usage window (applies to all models).
    pub five_hour: Option<UsageWindow>,
    /// 7-day rolling usage window (applies to all models).
    pub seven_day: Option<UsageWindow>,
    /// 7-day rolling usage window specific to Sonnet models.
    pub seven_day_sonnet: Option<UsageWindow>,
    /// 7-day rolling usage window specific to Opus models.
    pub seven_day_opus: Option<UsageWindow>,
}

/// A single rate-limit window with utilisation and reset information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Percentage of the window capacity consumed (0–100).
    pub utilization: f64,
    /// ISO 8601 timestamp at which the window resets, if known.
    pub resets_at: Option<String>,
}

/// Fetch Claude subscription usage data using an OAuth access token.
///
/// # Errors
/// Returns an error if the request fails or the server responds with a
/// non-success status code.
pub async fn fetch_usage(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<SubscriptionUsage> {
    let response = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "claude-cli/2.1.2 (external, cli)")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .send()
        .await
        .context("Failed to send usage request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Usage request failed ({}): {}", status, body);
    }

    let usage: SubscriptionUsage = response
        .json()
        .await
        .context("Failed to parse usage response")?;

    Ok(usage)
}

/// Format the time remaining until a usage window resets as a human-readable string.
///
/// Accepts ISO 8601 / RFC 3339 timestamps (with or without fractional seconds).
///
/// Returns strings like `"2h 13m"`, `"45m"`, `"3h 0m"`, or `"now"` when the
/// reset time is already in the past.  Returns `None` if the timestamp cannot
/// be parsed.
pub fn format_resets_in(resets_at: &str) -> Option<String> {
    let reset_time: DateTime<Utc> = DateTime::parse_from_rfc3339(resets_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))?;

    let now = Utc::now();
    let duration = reset_time.signed_duration_since(now);

    if duration.num_seconds() <= 0 {
        return Some("now".to_string());
    }

    let total_minutes = duration.num_minutes();
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours > 0 {
        Some(format!("{}h {}m", hours, minutes))
    } else {
        Some(format!("{}m", minutes))
    }
}
