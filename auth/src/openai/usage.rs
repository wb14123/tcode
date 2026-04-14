//! Fetch OpenAI Codex subscription usage data.
//!
//! Uses `GET https://chatgpt.com/backend-api/wham/usage` to retrieve rate-limit
//! window utilisation, credit balance, and reset times.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level response from `GET /api/codex/usage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitStatusPayload {
    /// Subscription plan type (e.g. `"pro"`).
    pub plan_type: Option<String>,
    /// Rate limit details.
    pub rate_limit: Option<RateLimitStatusDetails>,
    /// Credit balance details.
    pub credits: Option<CreditStatusDetails>,
}

/// Rate limit status across primary and secondary windows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitStatusDetails {
    /// Whether requests are currently allowed.
    pub allowed: Option<bool>,
    /// Whether the rate limit has been reached.
    pub limit_reached: Option<bool>,
    /// Primary rate-limit window snapshot.
    pub primary_window: Option<RateLimitWindowSnapshot>,
    /// Secondary rate-limit window snapshot.
    pub secondary_window: Option<RateLimitWindowSnapshot>,
}

/// Snapshot of a single rate-limit window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitWindowSnapshot {
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
pub struct CreditStatusDetails {
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
) -> Result<RateLimitStatusPayload> {
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

    let payload: RateLimitStatusPayload = response
        .json()
        .await
        .context("Failed to parse OpenAI usage response")?;

    Ok(payload)
}

/// Format a [`RateLimitStatusPayload`] into a human-readable one-line summary.
///
/// Shows the primary window's usage percentage and time until reset.
/// If the rate limit has been reached, prefixes the output with a warning.
pub fn format_usage(payload: &RateLimitStatusPayload) -> String {
    let limit_reached = payload
        .rate_limit
        .as_ref()
        .and_then(|rl| rl.limit_reached)
        .unwrap_or(false);

    let window = payload
        .rate_limit
        .as_ref()
        .and_then(|rl| rl.primary_window.as_ref());

    match window {
        Some(w) => {
            let reset_str =
                crate::format_resets_in_epoch(w.reset_at).unwrap_or_else(|| "unknown".to_string());
            let base = format!("{}% used, resets in {}", w.used_percent, reset_str);
            if limit_reached {
                format!("⚠ RATE LIMITED — {}", base)
            } else {
                base
            }
        }
        None => "no usage data".to_string(),
    }
}
