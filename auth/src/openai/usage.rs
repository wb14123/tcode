//! Fetch OpenAI Codex subscription usage data.
//!
//! Uses `GET https://chatgpt.com/backend-api/wham/usage` to retrieve rate-limit
//! window utilisation, credit balance, and reset times.

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::{RateLimitSnapshot, RateLimitWindow};

/// Top-level response from `GET /backend-api/wham/usage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionUsage {
    /// Subscription plan type (e.g. `"pro"`).
    pub plan_type: Option<String>,
    /// Rate limit details.
    pub rate_limit: Option<RateLimitStatus>,
    /// Credit balance details.
    pub credits: Option<CreditStatus>,
}

/// Rate limit status across primary and secondary windows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitStatus {
    /// Whether requests are currently allowed.
    pub allowed: Option<bool>,
    /// Whether the rate limit has been reached.
    pub limit_reached: Option<bool>,
    /// Primary rate-limit window.
    pub primary_window: Option<UsageWindow>,
    /// Secondary rate-limit window.
    pub secondary_window: Option<UsageWindow>,
}

/// A single rate-limit window as reported by the usage endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Percentage of the window capacity consumed (0–100).
    pub used_percent: i32,
    /// Total window duration in seconds.
    pub limit_window_seconds: i32,
    /// Seconds until the window resets.
    pub reset_after_seconds: i32,
    /// Unix epoch timestamp at which the window resets.
    pub reset_at: i64,
}

/// Credit balance status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditStatus {
    /// Whether the user has any credits remaining.
    pub has_credits: Option<bool>,
    /// Whether credits are unlimited.
    pub unlimited: Option<bool>,
    /// Credit balance as a string (e.g. `"9.99"`).
    pub balance: Option<String>,
}

/// Fetch OpenAI Codex usage data using an OAuth access token.
///
/// # Errors
/// Returns an error if the request fails or the server responds with a
/// non-success status code.
pub async fn fetch_usage(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
) -> Result<SubscriptionUsage> {
    let mut request = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Accept", "application/json");
    if let Some(id) = account_id {
        request = request.header("ChatGPT-Account-ID", id);
    }
    let response = request
        .send()
        .await
        .context("Failed to send OpenAI usage request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI usage request failed ({}): {}", status, body);
    }

    let payload: SubscriptionUsage = response
        .json()
        .await
        .context("Failed to parse OpenAI usage response")?;

    Ok(payload)
}

/// Convert the wire payload into the provider-neutral rate-limit snapshot.
///
/// `reset_at` epoch timestamps are parsed here, so the rest of the crate works
/// with [`DateTime`] values only.
pub fn to_rate_limit_snapshot(usage: &SubscriptionUsage) -> RateLimitSnapshot {
    let rate_limit = usage.rate_limit.as_ref();

    let windows: Vec<RateLimitWindow> = [
        (
            "primary",
            rate_limit.and_then(|limit| limit.primary_window.as_ref()),
        ),
        (
            "secondary",
            rate_limit.and_then(|limit| limit.secondary_window.as_ref()),
        ),
    ]
    .into_iter()
    .filter_map(|(name, window)| {
        window.map(|window| RateLimitWindow {
            name: name.to_string(),
            utilization: f64::from(window.used_percent),
            reset_at: parse_reset_at(window.reset_at),
        })
    })
    .collect();

    RateLimitSnapshot {
        limit_reached: rate_limit
            .and_then(|limit| limit.limit_reached)
            .unwrap_or(false),
        windows,
    }
}

/// Parse an epoch-seconds reset timestamp as reported by the usage endpoint.
fn parse_reset_at(reset_at: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(reset_at, 0).single()
}

/// Format a [`RateLimitSnapshot`] into a human-readable one-line summary.
///
/// Shows the primary window's usage percentage and time until reset.
/// If the rate limit has been reached, prefixes the output with a warning.
pub fn format_usage(snapshot: &RateLimitSnapshot) -> String {
    let Some(window) = snapshot.window("primary") else {
        return "no usage data".to_string();
    };

    let reset_str = window
        .reset_at
        .map(crate::format_duration_until)
        .unwrap_or_else(|| "unknown".to_string());
    let base = format!(
        "{}% used, resets in {}",
        window.utilization.round() as i64,
        reset_str
    );

    if snapshot.limit_reached {
        format!("⚠ RATE LIMITED — {}", base)
    } else {
        base
    }
}
