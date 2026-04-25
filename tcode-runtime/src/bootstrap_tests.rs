use std::path::{Path, PathBuf};

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::UnixListener;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::bootstrap::{
    RuntimeMonitorAction, RuntimeProbeStatus, probe_runtime_status, runtime_monitor_action,
};
use crate::protocol::{
    ClientMessage, DEFAULT_LEASE_TIMEOUT_SECONDS, RuntimeOwnerKind, ServerMessage,
    SessionRuntimeInfo,
};

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory should have repository parent")
        .join("target/test-tmp/bootstrap")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

fn spawn_dummy_runtime(socket_path: PathBuf) -> Result<tokio::task::JoinHandle<()>> {
    let listener = UnixListener::bind(socket_path)?;
    Ok(tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
                let Some(Ok(bytes)) = framed.next().await else {
                    return;
                };
                let Ok(ClientMessage::GetSessionRuntimeInfo) = serde_json::from_slice(&bytes)
                else {
                    return;
                };
                let response = ServerMessage::SessionRuntimeInfo(SessionRuntimeInfo {
                    active: true,
                    owner_kind: RuntimeOwnerKind::Cli,
                    active_lease_count: 1,
                    lease_timeout_seconds: DEFAULT_LEASE_TIMEOUT_SECONDS,
                    runtime_id: "dummy-runtime".to_string(),
                });
                let Ok(json) = serde_json::to_vec(&response) else {
                    return;
                };
                if let Err(e) = framed.send(Bytes::from(json)).await {
                    tracing::debug!(error = %e, "dummy runtime failed to send probe response");
                }
            });
        }
    }))
}

#[tokio::test]
async fn probe_runtime_detects_live_runtime() -> Result<()> {
    let socket_path = temp_dir().join("s");
    let handle = spawn_dummy_runtime(socket_path.clone())?;

    let status = probe_runtime_status(&socket_path).await?;

    assert!(
        matches!(status, RuntimeProbeStatus::Active(info) if info.active && info.owner_kind == RuntimeOwnerKind::Cli && info.runtime_id == "dummy-runtime")
    );

    handle.abort();
    Ok(())
}

#[test]
fn runtime_monitor_stops_after_bounded_consecutive_unresponsive_probes() {
    let mut consecutive_unresponsive = 0;

    assert_eq!(
        runtime_monitor_action(
            &RuntimeProbeStatus::Unresponsive,
            &mut consecutive_unresponsive,
            3,
        ),
        RuntimeMonitorAction::Continue
    );
    assert_eq!(consecutive_unresponsive, 1);
    assert_eq!(
        runtime_monitor_action(
            &RuntimeProbeStatus::Unresponsive,
            &mut consecutive_unresponsive,
            3,
        ),
        RuntimeMonitorAction::Continue
    );
    assert_eq!(consecutive_unresponsive, 2);
    assert_eq!(
        runtime_monitor_action(
            &RuntimeProbeStatus::Unresponsive,
            &mut consecutive_unresponsive,
            3,
        ),
        RuntimeMonitorAction::Stop
    );
    assert_eq!(consecutive_unresponsive, 3);
}

#[test]
fn runtime_monitor_resets_unresponsive_count_after_active_probe() {
    let mut consecutive_unresponsive = 2;
    let active = RuntimeProbeStatus::Active(SessionRuntimeInfo {
        active: true,
        owner_kind: RuntimeOwnerKind::Cli,
        active_lease_count: 1,
        lease_timeout_seconds: DEFAULT_LEASE_TIMEOUT_SECONDS,
        runtime_id: "runtime".to_string(),
    });

    assert_eq!(
        runtime_monitor_action(&active, &mut consecutive_unresponsive, 3),
        RuntimeMonitorAction::Continue
    );
    assert_eq!(consecutive_unresponsive, 0);
}

#[test]
fn runtime_monitor_stops_for_missing_listener() {
    let mut consecutive_unresponsive = 0;

    assert_eq!(
        runtime_monitor_action(
            &RuntimeProbeStatus::NoSocket,
            &mut consecutive_unresponsive,
            3
        ),
        RuntimeMonitorAction::Stop
    );
    assert_eq!(
        runtime_monitor_action(
            &RuntimeProbeStatus::NoListener,
            &mut consecutive_unresponsive,
            3,
        ),
        RuntimeMonitorAction::Stop
    );
}
