use chrono::{DateTime, TimeZone, Utc};

use crate::claude::usage as claude_usage;
use crate::openai::usage as openai_usage;
use crate::{RateLimitSnapshot, RateLimitWindow};

fn utc(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0)
        .single()
        .expect("timestamp should be in range")
}

fn claude_window(utilization: f64, resets_at: Option<DateTime<Utc>>) -> claude_usage::UsageWindow {
    claude_usage::UsageWindow {
        utilization,
        resets_at: resets_at.map(|reset_at| reset_at.to_rfc3339()),
    }
}

fn claude_payload(
    five_hour: Option<claude_usage::UsageWindow>,
    seven_day: Option<claude_usage::UsageWindow>,
) -> claude_usage::SubscriptionUsage {
    claude_usage::SubscriptionUsage {
        five_hour,
        seven_day,
        seven_day_sonnet: None,
        seven_day_opus: None,
    }
}

fn openai_window(used_percent: i32, reset_at: i64) -> openai_usage::UsageWindow {
    openai_usage::UsageWindow {
        used_percent,
        limit_window_seconds: 18_000,
        reset_after_seconds: 0,
        reset_at,
    }
}

fn openai_payload(
    limit_reached: bool,
    primary_window: Option<openai_usage::UsageWindow>,
    secondary_window: Option<openai_usage::UsageWindow>,
) -> openai_usage::SubscriptionUsage {
    openai_usage::SubscriptionUsage {
        plan_type: Some("pro".to_string()),
        rate_limit: Some(openai_usage::RateLimitStatus {
            allowed: Some(!limit_reached),
            limit_reached: Some(limit_reached),
            primary_window,
            secondary_window,
        }),
        credits: None,
    }
}

#[test]
fn claude_snapshot_names_and_parses_windows() {
    let payload = claude_payload(
        Some(claude_window(42.5, Some(utc(1_800)))),
        Some(claude_window(10.0, None)),
    );

    let snapshot = claude_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(
        snapshot,
        RateLimitSnapshot {
            windows: vec![
                RateLimitWindow {
                    name: "5h".to_string(),
                    utilization: 42.5,
                    reset_at: Some(utc(1_800)),
                },
                RateLimitWindow {
                    name: "7d".to_string(),
                    utilization: 10.0,
                    reset_at: None,
                },
            ],
            limit_reached: false,
        }
    );
}

#[test]
fn claude_blocking_reset_uses_latest_exhausted_window() {
    let payload = claude_payload(
        Some(claude_window(100.0, Some(utc(1_000)))),
        Some(claude_window(100.0, Some(utc(2_000)))),
    );

    let snapshot = claude_usage::to_rate_limit_snapshot(&payload);

    assert!(snapshot.limit_reached);
    assert_eq!(snapshot.blocking_reset_at(utc(0)), Some(utc(2_000)));
}

#[test]
fn claude_blocking_reset_ignores_windows_under_the_limit() {
    let payload = claude_payload(
        Some(claude_window(99.4, Some(utc(1_000)))),
        Some(claude_window(10.0, Some(utc(2_000)))),
    );

    let snapshot = claude_usage::to_rate_limit_snapshot(&payload);

    assert!(!snapshot.limit_reached);
    assert_eq!(snapshot.blocking_reset_at(utc(0)), None);
}

#[test]
fn claude_blocking_reset_does_not_use_an_unrelated_window() {
    // The exhausted 5h window reports no reset. The reset of the mostly unused
    // 7d window says nothing about when the 5h limit lifts.
    let payload = claude_payload(
        Some(claude_usage::UsageWindow {
            utilization: 100.0,
            resets_at: None,
        }),
        Some(claude_window(10.0, Some(utc(2_000)))),
    );

    let snapshot = claude_usage::to_rate_limit_snapshot(&payload);

    assert!(snapshot.limit_reached);
    assert_eq!(snapshot.blocking_reset_at(utc(0)), None);
}

#[test]
fn claude_snapshot_drops_unparseable_reset() {
    let payload = claude_payload(
        Some(claude_usage::UsageWindow {
            utilization: 100.0,
            resets_at: Some("not-a-timestamp".to_string()),
        }),
        None,
    );

    let snapshot = claude_usage::to_rate_limit_snapshot(&payload);

    assert!(snapshot.limit_reached);
    assert_eq!(snapshot.windows[0].reset_at, None);
    assert_eq!(snapshot.blocking_reset_at(utc(0)), None);
}

#[test]
fn claude_format_usage_reports_5h_window() {
    let payload = claude_payload(Some(claude_window(42.4, Some(utc(1)))), None);

    let snapshot = claude_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(
        claude_usage::format_usage(&snapshot),
        "42% used, resets in now"
    );
}

#[test]
fn claude_format_usage_without_5h_window_reports_no_data() {
    let payload = claude_payload(None, Some(claude_window(10.0, Some(utc(2_000)))));

    let snapshot = claude_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(claude_usage::format_usage(&snapshot), "no usage data");
}

#[test]
fn openai_snapshot_names_and_parses_windows() {
    let payload = openai_payload(
        false,
        Some(openai_window(42, 1_800)),
        Some(openai_window(10, 3_600)),
    );

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(snapshot.windows[0].name, "primary");
    assert_eq!(snapshot.windows[0].utilization, 42.0);
    assert_eq!(snapshot.windows[0].reset_at, Some(utc(1_800)));
    assert_eq!(snapshot.windows[1].name, "secondary");
    assert_eq!(snapshot.windows[1].reset_at, Some(utc(3_600)));
}

#[test]
fn openai_blocking_reset_uses_latest_exhausted_window() {
    let payload = openai_payload(
        true,
        Some(openai_window(100, 1_000)),
        Some(openai_window(100, 2_000)),
    );

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(snapshot.blocking_reset_at(utc(0)), Some(utc(2_000)));
}

#[test]
fn openai_blocking_reset_falls_back_to_latest_window() {
    let payload = openai_payload(
        true,
        Some(openai_window(80, 1_000)),
        Some(openai_window(40, 2_000)),
    );

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(snapshot.blocking_reset_at(utc(0)), Some(utc(2_000)));
}

#[test]
fn openai_blocking_reset_is_none_when_limit_not_reached() {
    let payload = openai_payload(false, Some(openai_window(80, 1_000)), None);

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(snapshot.blocking_reset_at(utc(0)), None);
}

#[test]
fn openai_blocking_reset_ignores_a_reset_that_already_passed() {
    let payload = openai_payload(true, Some(openai_window(100, 1_000)), None);

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(snapshot.blocking_reset_at(utc(500)), Some(utc(1_000)));
    assert_eq!(snapshot.blocking_reset_at(utc(2_000)), None);
}

#[test]
fn openai_snapshot_drops_out_of_range_reset() {
    let payload = openai_payload(true, Some(openai_window(100, i64::MAX)), None);

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(snapshot.windows[0].reset_at, None);
    assert_eq!(snapshot.blocking_reset_at(utc(0)), None);
}

#[test]
fn openai_format_usage_prefixes_when_limit_reached() {
    let payload = openai_payload(true, Some(openai_window(100, 1)), None);

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(
        openai_usage::format_usage(&snapshot),
        "⚠ RATE LIMITED — 100% used, resets in now"
    );
}

#[test]
fn openai_format_usage_without_primary_window_reports_no_data() {
    let payload = openai_payload(false, None, Some(openai_window(10, 3_600)));

    let snapshot = openai_usage::to_rate_limit_snapshot(&payload);

    assert_eq!(openai_usage::format_usage(&snapshot), "no usage data");
}
