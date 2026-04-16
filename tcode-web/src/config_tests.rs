use crate::config::RemoteConfig;

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
/// we don't assert on (see §8 of plan.md — intentional coverage gap).
#[test]
fn try_new_accepts_argv_sourced_password() {
    assert!(RemoteConfig::try_new(8765, "a-long-enough-password-123".into(), true).is_ok());
}
