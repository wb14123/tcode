use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use clap::Parser;
use parking_lot::Mutex;
use tcode_runtime::protocol::{RuntimeOwnerKind, ServerMessage, SessionRuntimeInfo};
use tcode_runtime::session::Session;
use tcode_runtime::session::{SessionMode, read_session_mode};

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/tcode-cli-runtime")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

fn home_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct HomeGuard {
    _guard: parking_lot::MutexGuard<'static, ()>,
    previous_home: Option<OsString>,
}

impl HomeGuard {
    fn set(home_dir: &Path) -> Self {
        let guard = home_env_lock().lock();
        let previous_home = std::env::var_os("HOME");
        // SAFETY: this test holds a process-wide lock while mutating HOME.
        unsafe { std::env::set_var("HOME", home_dir) };
        Self {
            _guard: guard,
            previous_home,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(previous_home) => {
                // SAFETY: restoration happens while the same process-wide lock is held.
                unsafe { std::env::set_var("HOME", previous_home) };
            }
            None => {
                // SAFETY: restoration happens while the same process-wide lock is held.
                unsafe { std::env::remove_var("HOME") };
            }
        }
    }
}

#[test]
fn web_only_global_flag_is_accepted_after_remote_subcommand() {
    let cli = crate::Cli::try_parse_from([
        "tcode",
        "remote",
        "--port",
        "1234",
        "--password",
        "super-secret-password",
        "--web-only",
    ])
    .expect("remote --web-only should parse as global flag");

    assert!(cli.web_only);
    match cli.command {
        Some(crate::Commands::Remote { port, password }) => {
            assert_eq!(port, 1234);
            assert_eq!(password, "super-secret-password");
        }
        _ => panic!("expected remote command"),
    }
}

#[test]
fn requested_session_mode_follows_web_only_flag() {
    assert_eq!(crate::requested_session_mode(false), SessionMode::Normal);
    assert_eq!(crate::requested_session_mode(true), SessionMode::WebOnly);
}

#[test]
fn serve_initializes_requested_mode_when_only_stale_socket_exists() -> anyhow::Result<()> {
    let home_dir = temp_dir();
    let _home_guard = HomeGuard::set(&home_dir);
    let session = Session::new("stales01".to_string())?;
    std::fs::write(session.socket_path(), b"stale socket placeholder")?;

    let mode = crate::session_mode_for_serve(&session, SessionMode::WebOnly)?;

    assert_eq!(mode, SessionMode::WebOnly);
    assert_eq!(
        read_session_mode(session.session_dir())?,
        SessionMode::WebOnly
    );
    Ok(())
}

#[test]
fn active_runtime_mode_validation_rejects_mismatch() {
    let info = SessionRuntimeInfo {
        active: true,
        owner_kind: RuntimeOwnerKind::Cli,
        session_mode: SessionMode::Normal,
        runtime_id: "runtime".to_string(),
        active_lease_count: 0,
        lease_timeout_seconds: 60,
    };

    let error = crate::validate_active_runtime_mode("session-id", &info, SessionMode::WebOnly)
        .expect_err("mode mismatch should be rejected");

    assert!(
        error.to_string().contains("session runtime mode mismatch"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn active_runtime_mode_validation_accepts_match() {
    let info = SessionRuntimeInfo {
        active: true,
        owner_kind: RuntimeOwnerKind::Cli,
        session_mode: SessionMode::WebOnly,
        runtime_id: "runtime".to_string(),
        active_lease_count: 0,
        lease_timeout_seconds: 60,
    };

    crate::validate_active_runtime_mode("session-id", &info, SessionMode::WebOnly)
        .expect("matching mode should be accepted");
}

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
        session_mode: SessionMode::Normal,
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
