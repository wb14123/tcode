use std::time::{Duration, Instant};

use crate::state::{AppState, SESSION_TTL};

const B64URL_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn is_b64url(s: &str) -> bool {
    s.bytes().all(|b| B64URL_ALPHABET.contains(&b))
}

#[test]
fn mint_session_emits_43char_base64url() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    let token = state
        .mint_session("test-user".into())
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    assert_eq!(token.len(), 43, "token was {token:?}");
    assert!(
        is_b64url(&token),
        "token contains non-base64url bytes: {token:?}"
    );
    Ok(())
}

#[test]
fn mint_session_emits_distinct_tokens() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    let a = state
        .mint_session("test-user".into())
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    let b = state
        .mint_session("test-user".into())
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    assert_ne!(a, b, "two fresh 256-bit tokens must differ");
    Ok(())
}

#[test]
fn verify_session_accepts_minted_token() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    let token = state
        .mint_session("test-user".into())
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    assert_eq!(state.verify_session(&token), Some("test-user".into()));
    Ok(())
}

#[test]
fn verify_session_rejects_wrong_length() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    assert_eq!(state.verify_session(""), None);
    assert_eq!(state.verify_session("short"), None);
    // 44 chars: one longer than valid.
    assert_eq!(state.verify_session(&"A".repeat(44)), None);
    Ok(())
}

#[test]
fn verify_session_rejects_unminted_token() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    // 43 'A' chars is a valid base64url string but the state never minted it.
    let forged = "A".repeat(43);
    assert_eq!(state.verify_session(&forged), None);
    Ok(())
}

#[test]
fn revoke_session_makes_verify_return_none() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    let token = state
        .mint_session("test-user".into())
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    assert_eq!(state.verify_session(&token), Some("test-user".into()));
    state.revoke_session(&token);
    assert_eq!(state.verify_session(&token), None);
    // Idempotent — revoking again is a no-op.
    state.revoke_session(&token);
    assert_eq!(state.verify_session(&token), None);
    Ok(())
}

#[test]
fn session_ttl_is_seven_days() {
    // Pin the policy choice so a future "let's tweak the TTL" change
    // shows up as a deliberate test edit, not a silent shift.
    assert_eq!(SESSION_TTL, Duration::from_secs(7 * 24 * 60 * 60));
}

#[test]
fn verify_session_rejects_expired_token() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    // One-second-in-the-past expiry: synthesizes a token that is past
    // its TTL the moment `verify_session` looks at it.
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or_else(|| anyhow::anyhow!("Instant::now is too close to epoch to subtract 1s"))?;
    let token = state
        .insert_session_with_expiry(past)
        .map_err(|e| anyhow::anyhow!("insert_session_with_expiry failed: {e}"))?;
    assert_eq!(state.verify_session(&token), None);
    Ok(())
}

#[test]
fn verify_session_evicts_expired_token() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or_else(|| anyhow::anyhow!("Instant::now is too close to epoch to subtract 1s"))?;
    let token = state
        .insert_session_with_expiry(past)
        .map_err(|e| anyhow::anyhow!("insert_session_with_expiry failed: {e}"))?;
    assert_eq!(state.sessions_len_for_test(), 1);
    // First verify both rejects AND lazily evicts.
    assert_eq!(state.verify_session(&token), None);
    assert_eq!(
        state.sessions_len_for_test(),
        0,
        "expired token should be evicted on the rejecting verify_session call"
    );
    Ok(())
}

#[test]
fn verify_session_accepts_token_before_expiry() -> anyhow::Result<()> {
    let state = AppState::new_with_test_user();
    // Far enough in the future that test scheduling jitter cannot
    // push us past the deadline.
    let future = Instant::now() + Duration::from_secs(60);
    let token = state
        .insert_session_with_expiry(future)
        .map_err(|e| anyhow::anyhow!("insert_session_with_expiry failed: {e}"))?;
    assert_eq!(state.verify_session(&token), Some("test-user".into()));
    // Still present (no eviction on the happy path).
    assert_eq!(state.sessions_len_for_test(), 1);
    Ok(())
}
