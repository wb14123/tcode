//! Bridge from the subscription usage endpoint to the LLM retry loop.
//!
//! `llm-rs` cannot query this endpoint itself: it has no knowledge of OAuth
//! tokens or subscription plans, and it must not depend on this crate. It only
//! defines the [`RateLimitResetSource`] trait that a provider accepts as a
//! fallback when a failing response carries no reset hint.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use llm_rs::llm::RateLimitResetSource;
use parking_lot::Mutex;

use crate::OAuthTokenManager;

#[cfg(test)]
#[path = "rate_limit_source_tests.rs"]
mod rate_limit_source_tests;

/// How long a lookup that produced a reset time is reused.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Lookups that produced no reset are reused only briefly: a stale negative
/// answer would send the next rate-limited request down the short-backoff path
/// even though the endpoint had a reset time by then.
const UNKNOWN_CACHE_TTL: Duration = Duration::from_secs(5);

/// Upper bound on a single lookup, so a hanging request cannot stall a retry
/// loop that is already waiting.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// A lookup result and when it was made. `None` means the lookup failed or
/// found no reset.
struct CachedReset {
    reset_at: Option<SystemTime>,
    looked_up_at: Instant,
}

impl CachedReset {
    /// Whether this answer is still within its time to live.
    fn is_fresh(&self, now: Instant) -> bool {
        let ttl = match self.reset_at {
            Some(_) => CACHE_TTL,
            None => UNKNOWN_CACHE_TTL,
        };
        now.duration_since(self.looked_up_at) < ttl
    }
}

/// The subscription usage endpoint, seen as a rate-limit reset source.
///
/// The provider shares one instance with every subagent clone it spawns, so the
/// cache keeps parallel waiters from querying the endpoint at the same time.
#[derive(Clone)]
struct UsageRateLimitSource {
    token_manager: Arc<dyn OAuthTokenManager>,
    cached: Arc<Mutex<Option<CachedReset>>>,
}

impl UsageRateLimitSource {
    /// The cached answer, if it is still within its time to live.
    fn cached(&self) -> Option<Option<SystemTime>> {
        let cached = self.cached.lock();
        let entry = cached.as_ref()?;
        entry.is_fresh(Instant::now()).then_some(entry.reset_at)
    }

    async fn lookup(&self) -> Option<SystemTime> {
        if let Some(cached) = self.cached() {
            return cached;
        }

        let reset_at = self.query().await;

        // The guard is taken only after the request has finished: holding it
        // across the await would serialize every waiter behind one slow lookup.
        *self.cached.lock() = Some(CachedReset {
            reset_at,
            looked_up_at: Instant::now(),
        });
        reset_at
    }

    async fn query(&self) -> Option<SystemTime> {
        let lookup = self.token_manager.fetch_rate_limit_snapshot();
        match tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await {
            Ok(Ok(snapshot)) => snapshot
                .and_then(|snapshot| snapshot.blocking_reset_at(Utc::now()))
                .and_then(system_time_from_reset),
            Ok(Err(error)) => {
                tracing::warn!(%error, "subscription usage lookup failed");
                None
            }
            Err(_) => {
                tracing::warn!(
                    timeout_secs = LOOKUP_TIMEOUT.as_secs(),
                    "subscription usage lookup timed out"
                );
                None
            }
        }
    }
}

impl RateLimitResetSource for UsageRateLimitSource {
    fn reset_at(&self) -> Pin<Box<dyn Future<Output = Option<SystemTime>> + Send>> {
        let source = self.clone();
        Box::pin(async move { source.lookup().await })
    }
}

/// Convert a snapshot reset time into the `SystemTime` the retry loop expects.
fn system_time_from_reset(reset_at: DateTime<Utc>) -> Option<SystemTime> {
    // Sub-second precision is irrelevant for a wait measured in seconds, and a
    // reset before the Unix epoch is not representable.
    let secs = u64::try_from(reset_at.timestamp()).ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

/// Expose `token_manager`'s subscription usage endpoint as a rate-limit reset
/// source.
pub fn rate_limit_reset_source(
    token_manager: Arc<dyn OAuthTokenManager>,
) -> Arc<dyn RateLimitResetSource> {
    Arc::new(UsageRateLimitSource {
        token_manager,
        cached: Arc::new(Mutex::new(None)),
    })
}
