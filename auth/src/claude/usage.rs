//! Fetch Claude subscription usage data from the Anthropic API.
//!
//! Uses the undocumented `GET https://api.anthropic.com/api/oauth/usage` endpoint
//! to retrieve rate-limit window utilisation and reset times.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use llm_rs::llm::CLAUDE_CLI_USER_AGENT;
use serde::{Deserialize, Serialize};

use crate::{RateLimitSnapshot, RateLimitWindow};

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
        .header("User-Agent", CLAUDE_CLI_USER_AGENT)
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

/// Convert the wire payload into the provider-neutral rate-limit snapshot.
///
/// `resets_at` timestamps are parsed here, so the rest of the crate works with
/// [`DateTime`] values only.
pub fn to_rate_limit_snapshot(usage: &SubscriptionUsage) -> RateLimitSnapshot {
    let windows: Vec<RateLimitWindow> = [
        ("5h", usage.five_hour.as_ref()),
        ("7d", usage.seven_day.as_ref()),
        ("7d sonnet", usage.seven_day_sonnet.as_ref()),
        ("7d opus", usage.seven_day_opus.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, window)| {
        window.map(|window| RateLimitWindow {
            name: name.to_string(),
            utilization: window.utilization,
            reset_at: window.resets_at.as_deref().and_then(parse_resets_at),
        })
    })
    .collect();

    RateLimitSnapshot {
        limit_reached: windows
            .iter()
            .any(|window| window.utilization >= crate::EXHAUSTED_UTILIZATION),
        windows,
    }
}

/// Parse an RFC 3339 reset timestamp as reported by the usage endpoint.
fn parse_resets_at(resets_at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(resets_at)
        .ok()
        .map(|reset_at| reset_at.with_timezone(&Utc))
}

/// Format a [`RateLimitSnapshot`] into a human-readable one-line summary.
///
/// Shows the 5-hour window utilisation percentage and time until reset.
pub fn format_usage(snapshot: &RateLimitSnapshot) -> String {
    let Some(window) = snapshot.window("5h") else {
        return "no usage data".to_string();
    };

    let reset_str = window
        .reset_at
        .map(crate::format_duration_until)
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "{}% used, resets in {}",
        window.utilization.round() as i64,
        reset_str
    )
}
