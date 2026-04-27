use std::net::{IpAddr, Ipv4Addr};

use crate::config::{RemoteConfig, RemoteModePolicy};
use tcode_runtime::session::SessionMode;

/// Test D — empty string rejects and the error mentions
/// `TCODE_REMOTE_PASSWORD` so the operator can self-correct.
#[test]
fn try_new_rejects_empty_password() {
    let err = match RemoteConfig::try_new(8765, "".into(), false) {
        Ok(_) => panic!("expected empty password to be rejected"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("TCODE_REMOTE_PASSWORD"),
        "error message did not mention TCODE_REMOTE_PASSWORD: {err}"
    );
}

/// All-whitespace passwords are also rejected (trim-then-check).
#[test]
fn try_new_rejects_whitespace_password() {
    assert!(RemoteConfig::try_new(8765, "   ".into(), false).is_err());
}

/// Short passwords succeed — the warning is a side effect, not an error.
#[test]
fn try_new_accepts_short_password() {
    assert!(RemoteConfig::try_new(8765, "short".into(), false).is_ok());
}

/// Long (>= 16 char) passwords succeed cleanly.
#[test]
fn try_new_accepts_long_password() {
    assert!(RemoteConfig::try_new(8765, "a-long-enough-password-123".into(), false).is_ok());
}

/// Exercise the argv-sourced password branch. The warn! is a side effect
/// we don't assert on.
#[test]
fn try_new_accepts_argv_sourced_password() {
    assert!(RemoteConfig::try_new(8765, "a-long-enough-password-123".into(), true).is_ok());
}

#[test]
fn with_bind_addr_allows_non_loopback_address() -> anyhow::Result<()> {
    let cfg = RemoteConfig::try_new(8765, "valid-password-16chars!".into(), false)?
        .with_bind_addr(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(cfg.bind_addr(), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    Ok(())
}

#[test]
fn remote_mode_policy_defaults_to_all_and_creates_normal_sessions() {
    let cfg = RemoteConfig::try_new(8765, "a-long-enough-password-123".into(), false)
        .expect("valid remote config");
    assert_eq!(cfg.remote_mode_policy, RemoteModePolicy::All);
    assert_eq!(
        cfg.remote_mode_policy.new_session_mode(),
        SessionMode::Normal
    );
    assert!(
        cfg.remote_mode_policy
            .allows_session_mode(SessionMode::Normal)
    );
    assert!(
        cfg.remote_mode_policy
            .allows_session_mode(SessionMode::WebOnly)
    );
}

#[test]
fn remote_mode_policy_web_only_only_creates_and_allows_only_web_only_sessions() {
    let cfg = RemoteConfig::try_new(8765, "a-long-enough-password-123".into(), false)
        .expect("valid remote config")
        .with_remote_mode_policy(RemoteModePolicy::WebOnlyOnly);
    assert_eq!(cfg.remote_mode_policy, RemoteModePolicy::WebOnlyOnly);
    assert_eq!(
        cfg.remote_mode_policy.new_session_mode(),
        SessionMode::WebOnly
    );
    assert!(
        !cfg.remote_mode_policy
            .allows_session_mode(SessionMode::Normal)
    );
    assert!(
        cfg.remote_mode_policy
            .allows_session_mode(SessionMode::WebOnly)
    );
}
