use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};

use crate::{OAuthTokenManager, RateLimitSnapshot, RateLimitWindow};

use super::{CachedReset, rate_limit_reset_source};

/// Token manager whose usage endpoint answers from a script and counts calls.
struct FakeTokenManager {
    client: reqwest::Client,
    answer: Result<Option<RateLimitSnapshot>, String>,
    lookups: AtomicUsize,
}

impl FakeTokenManager {
    fn new(answer: Result<Option<RateLimitSnapshot>, String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            answer,
            lookups: AtomicUsize::new(0),
        })
    }

    fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }
}

impl llm_rs::llm::TokenProvider for FakeTokenManager {
    fn get_access_token(
        &self,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send>> {
        Box::pin(async { Ok("access-token".to_string()) })
    }
}

#[async_trait::async_trait]
impl OAuthTokenManager for FakeTokenManager {
    fn client(&self) -> &reqwest::Client {
        &self.client
    }

    async fn fetch_rate_limit_snapshot(&self) -> Result<Option<RateLimitSnapshot>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        match &self.answer {
            Ok(snapshot) => Ok(snapshot.clone()),
            Err(error) => Err(anyhow::anyhow!(error.clone())),
        }
    }

    async fn fetch_formatted_usage(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

/// A snapshot whose only window is exhausted and resets at `reset_at`.
fn exhausted_snapshot(reset_at: DateTime<Utc>) -> RateLimitSnapshot {
    RateLimitSnapshot {
        windows: vec![RateLimitWindow {
            name: "5h".to_string(),
            utilization: 100.0,
            reset_at: Some(reset_at),
        }],
        limit_reached: true,
    }
}

#[tokio::test]
async fn waiters_share_one_usage_lookup() -> Result<()> {
    let reset_at = Utc::now() + ChronoDuration::hours(1);
    let manager = FakeTokenManager::new(Ok(Some(exhausted_snapshot(reset_at))));
    let source = rate_limit_reset_source(Arc::clone(&manager) as Arc<dyn OAuthTokenManager>);

    let first = source.reset_at().await;
    let second = source.reset_at().await;

    assert_eq!(first, second);
    assert_eq!(
        first,
        Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(reset_at.timestamp())?)),
        "the reset of the exhausted window reaches the retry loop"
    );
    assert_eq!(
        manager.lookups(),
        1,
        "a second waiter reuses the cached answer"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_lookup_reports_no_reset_and_is_cached() -> Result<()> {
    let manager = FakeTokenManager::new(Err("usage endpoint unavailable".to_string()));
    let source = rate_limit_reset_source(Arc::clone(&manager) as Arc<dyn OAuthTokenManager>);

    assert_eq!(source.reset_at().await, None);
    assert_eq!(source.reset_at().await, None);
    assert_eq!(
        manager.lookups(),
        1,
        "a negative answer is reused for the shorter time to live"
    );
    Ok(())
}

#[tokio::test]
async fn a_snapshot_without_a_blocking_reset_reports_no_reset() -> Result<()> {
    let stale_reset = Utc::now() - ChronoDuration::minutes(1);
    let manager = FakeTokenManager::new(Ok(Some(exhausted_snapshot(stale_reset))));
    let source = rate_limit_reset_source(Arc::clone(&manager) as Arc<dyn OAuthTokenManager>);

    assert_eq!(
        source.reset_at().await,
        None,
        "a reset that already passed cannot be waited for"
    );
    Ok(())
}

#[test]
fn a_negative_answer_is_cached_for_a_shorter_time() {
    let now = Instant::now();
    let negative = CachedReset {
        reset_at: None,
        looked_up_at: now,
    };
    assert!(negative.is_fresh(now + Duration::from_secs(4)));
    assert!(!negative.is_fresh(now + Duration::from_secs(5)));

    let positive = CachedReset {
        reset_at: Some(UNIX_EPOCH),
        looked_up_at: now,
    };
    assert!(positive.is_fresh(now + Duration::from_secs(29)));
    assert!(!positive.is_fresh(now + Duration::from_secs(30)));
}
