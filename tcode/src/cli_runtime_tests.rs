use std::time::Duration;

use tcode_runtime::protocol::{RuntimeOwnerKind, ServerMessage, SessionRuntimeInfo};

#[test]
fn heartbeat_interval_caps_at_default_for_normal_timeout() {
    assert_eq!(
        crate::cli_runtime::heartbeat_interval(Duration::from_secs(60)),
        Duration::from_secs(10)
    );
}

#[test]
fn heartbeat_interval_scales_down_for_short_timeout() {
    assert_eq!(
        crate::cli_runtime::heartbeat_interval(Duration::from_secs(9)),
        Duration::from_secs(3)
    );
}

#[test]
fn heartbeat_interval_never_returns_zero() {
    assert_eq!(
        crate::cli_runtime::heartbeat_interval(Duration::ZERO),
        Duration::from_secs(1)
    );
}

#[test]
fn heartbeat_retry_delay_backs_off_between_attempts() {
    assert_eq!(
        crate::cli_runtime::heartbeat_retry_delay(1),
        Duration::from_millis(500)
    );
    assert_eq!(
        crate::cli_runtime::heartbeat_retry_delay(2),
        Duration::from_secs(1)
    );
}

#[test]
fn heartbeat_response_treats_unexpected_messages_as_errors() {
    let ack_error = crate::cli_runtime::heartbeat_response_result(Some(ServerMessage::Ack))
        .expect_err("ack should be unexpected for heartbeat");
    assert!(
        ack_error
            .to_string()
            .contains("unexpected lease heartbeat response"),
        "unexpected error: {ack_error:#}"
    );

    let inactive_runtime = ServerMessage::SessionRuntimeInfo(SessionRuntimeInfo {
        active: false,
        owner_kind: RuntimeOwnerKind::Cli,
        runtime_id: "runtime".to_string(),
        active_lease_count: 0,
        lease_timeout_seconds: 60,
    });

    let error = crate::cli_runtime::heartbeat_response_result(Some(inactive_runtime))
        .expect_err("inactive runtime info should be unexpected");

    assert!(
        error
            .to_string()
            .contains("unexpected lease heartbeat response"),
        "unexpected error: {error:#}"
    );
}
