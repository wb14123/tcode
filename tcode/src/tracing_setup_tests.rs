use crate::{DEFAULT_SESSION_LOG_FILTER, SessionLogFilter};

#[test]
fn rust_log_unset_disables_session_logging() {
    assert!(matches!(
        SessionLogFilter::resolve(None),
        SessionLogFilter::Disabled
    ));
}

#[test]
fn empty_rust_log_disables_session_logging() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("")),
        SessionLogFilter::Disabled
    ));
}

#[test]
fn whitespace_only_rust_log_disables_session_logging() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("   \t\n ")),
        SessionLogFilter::Disabled
    ));
}

#[test]
fn off_rust_log_disables_session_logging() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("off")),
        SessionLogFilter::Disabled
    ));
}

#[test]
fn uppercase_off_with_surrounding_whitespace_disables_session_logging() {
    assert!(matches!(
        SessionLogFilter::resolve(Some(" OFF ")),
        SessionLogFilter::Disabled
    ));
}

#[test]
fn level_rust_log_uses_user_filter() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("info")),
        SessionLogFilter::User(_)
    ));
}

#[test]
fn module_directive_rust_log_uses_user_filter() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("tcode_runtime=debug")),
        SessionLogFilter::User(_)
    ));
}

#[test]
fn surrounding_whitespace_is_trimmed_before_parsing() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("  debug  ")),
        SessionLogFilter::User(_)
    ));
}

#[test]
fn invalid_level_rust_log_falls_back_to_default_filter() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("debug=bogus")),
        SessionLogFilter::Invalid
    ));
}

#[test]
fn malformed_rust_log_falls_back_to_default_filter() {
    assert!(matches!(
        SessionLogFilter::resolve(Some("info,=debug")),
        SessionLogFilter::Invalid
    ));
}

#[test]
fn default_session_log_filter_is_a_valid_filter() {
    assert!(
        tracing_subscriber::EnvFilter::try_new(DEFAULT_SESSION_LOG_FILTER).is_ok(),
        "DEFAULT_SESSION_LOG_FILTER must parse as an EnvFilter"
    );
}
