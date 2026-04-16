use crate::state::{AppState, Secret};

const B64URL_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn is_b64url(s: &str) -> bool {
    s.bytes().all(|b| B64URL_ALPHABET.contains(&b))
}

#[test]
fn secret_verify_accepts_matching_bytes() -> anyhow::Result<()> {
    let s = Secret::new("valid-password-16chars!".into());
    assert!(s.verify(b"valid-password-16chars!"));
    Ok(())
}

#[test]
fn secret_verify_rejects_wrong_bytes() -> anyhow::Result<()> {
    let s = Secret::new("valid-password-16chars!".into());
    assert!(!s.verify(b"Valid-password-16chars!"));
    Ok(())
}

#[test]
fn secret_verify_rejects_different_length() -> anyhow::Result<()> {
    let s = Secret::new("valid-password-16chars!".into());
    assert!(!s.verify(b"valid-password-16chars!x"));
    assert!(!s.verify(b"valid-password-16char"));
    assert!(!s.verify(b""));
    Ok(())
}

#[test]
fn secret_debug_redacts() -> anyhow::Result<()> {
    let s = Secret::new("valid-password-16chars!".into());
    assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
    Ok(())
}

#[test]
fn mint_session_emits_43char_base64url() -> anyhow::Result<()> {
    let state = AppState::new("valid-password-16chars!".into());
    let token = state
        .mint_session()
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
    let state = AppState::new("valid-password-16chars!".into());
    let a = state
        .mint_session()
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    let b = state
        .mint_session()
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    assert_ne!(a, b, "two fresh 256-bit tokens must differ");
    Ok(())
}

#[test]
fn verify_session_accepts_minted_token() -> anyhow::Result<()> {
    let state = AppState::new("valid-password-16chars!".into());
    let token = state
        .mint_session()
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    assert!(state.verify_session(&token));
    Ok(())
}

#[test]
fn verify_session_rejects_wrong_length() -> anyhow::Result<()> {
    let state = AppState::new("valid-password-16chars!".into());
    assert!(!state.verify_session(""));
    assert!(!state.verify_session("short"));
    // 44 chars: one longer than valid.
    assert!(!state.verify_session(&"A".repeat(44)));
    Ok(())
}

#[test]
fn verify_session_rejects_unminted_token() -> anyhow::Result<()> {
    let state = AppState::new("valid-password-16chars!".into());
    // 43 'A' chars is a valid base64url string but the state never minted it.
    let forged = "A".repeat(43);
    assert!(!state.verify_session(&forged));
    Ok(())
}

#[test]
fn revoke_session_makes_verify_return_false() -> anyhow::Result<()> {
    let state = AppState::new("valid-password-16chars!".into());
    let token = state
        .mint_session()
        .map_err(|e| anyhow::anyhow!("mint_session failed: {e}"))?;
    assert!(state.verify_session(&token));
    state.revoke_session(&token);
    assert!(!state.verify_session(&token));
    // Idempotent — revoking again is a no-op.
    state.revoke_session(&token);
    assert!(!state.verify_session(&token));
    Ok(())
}
