use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use parking_lot::Mutex;
use tcode_runtime::bootstrap::send_socket_message;
use tcode_runtime::protocol::{ClientKind, ClientLeaseInfo, ClientMessage, ServerMessage};

use crate::session::Session;

const DEFAULT_CLI_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const CLI_LEASE_REGISTER_ATTEMPTS: usize = 3;
const CLI_LEASE_REGISTER_RETRY_DELAY: Duration = Duration::from_millis(200);
const CLI_LEASE_HEARTBEAT_ATTEMPTS: usize = 3;
const CLI_LEASE_HEARTBEAT_RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(crate) struct CliClientLease {
    socket_path: PathBuf,
    current_client_id: Arc<Mutex<String>>,
    heartbeat_task: tokio::task::JoinHandle<()>,
}

impl CliClientLease {
    pub(crate) async fn detach(self) {
        self.heartbeat_task.abort();
        let client_id = self.current_client_id.lock().clone();
        detach_cli_lease_id(&self.socket_path, client_id).await;
    }
}

impl Drop for CliClientLease {
    fn drop(&mut self) {
        self.heartbeat_task.abort();
    }
}

pub(crate) async fn register_cli_lease(
    socket_path: PathBuf,
    client_label: impl Into<String>,
) -> Result<CliClientLease> {
    let client_label = client_label.into();
    let info = register_cli_lease_info(&socket_path, &client_label).await?;
    let interval = heartbeat_interval(Duration::from_secs(info.lease_timeout_seconds));
    let current_client_id = Arc::new(Mutex::new(info.client_id));
    let heartbeat_socket_path = socket_path.clone();
    let heartbeat_client_label = client_label.clone();
    let heartbeat_client_id = Arc::clone(&current_client_id);
    let heartbeat_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;

            let mut renewed = false;
            let mut rejection_message: Option<String> = None;
            for attempt in 1..=CLI_LEASE_HEARTBEAT_ATTEMPTS {
                let client_id = heartbeat_client_id.lock().clone();
                match heartbeat_cli_lease_once(&heartbeat_socket_path, &client_id).await {
                    Ok(HeartbeatResult::Renewed) => {
                        renewed = true;
                        break;
                    }
                    Ok(HeartbeatResult::Rejected(message)) => {
                        tracing::warn!(
                            attempt,
                            attempts = CLI_LEASE_HEARTBEAT_ATTEMPTS,
                            message,
                            "CLI lease heartbeat rejected; attempting to re-register"
                        );
                        rejection_message = Some(message);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            attempt,
                            attempts = CLI_LEASE_HEARTBEAT_ATTEMPTS,
                            error = %e,
                            "CLI lease heartbeat failed"
                        );
                        if attempt < CLI_LEASE_HEARTBEAT_ATTEMPTS {
                            tokio::time::sleep(heartbeat_retry_delay(attempt)).await;
                        }
                    }
                }
            }

            if renewed {
                continue;
            }

            match register_cli_lease_info(&heartbeat_socket_path, &heartbeat_client_label).await {
                Ok(info) => {
                    let previous_client_id = heartbeat_client_id.lock().clone();
                    if previous_client_id != info.client_id {
                        detach_cli_lease_id(&heartbeat_socket_path, previous_client_id).await;
                    }
                    *heartbeat_client_id.lock() = info.client_id;
                    tracing::warn!("re-registered CLI client lease after heartbeat failure");
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        rejection = rejection_message.as_deref().unwrap_or(""),
                        "CLI lease liveness could not be restored after bounded retries; will retry registration"
                    );
                    tokio::time::sleep(CLI_LEASE_REGISTER_RETRY_DELAY).await;
                }
            }
        }
    });

    Ok(CliClientLease {
        socket_path,
        current_client_id,
        heartbeat_task,
    })
}

async fn register_cli_lease_info(
    socket_path: &Path,
    client_label: &str,
) -> Result<ClientLeaseInfo> {
    let mut last_error = None;
    let mut response = None;
    for attempt in 1..=CLI_LEASE_REGISTER_ATTEMPTS {
        match send_socket_message(
            socket_path.to_path_buf(),
            &ClientMessage::RegisterClientLease {
                client_kind: ClientKind::Cli,
                client_label: Some(client_label.to_string()),
            },
        )
        .await
        {
            Ok(next_response) => {
                response = Some(next_response);
                break;
            }
            Err(e) => {
                tracing::warn!(
                    attempt,
                    attempts = CLI_LEASE_REGISTER_ATTEMPTS,
                    error = %e,
                    "failed to register CLI lease"
                );
                last_error = Some(e);
                if attempt < CLI_LEASE_REGISTER_ATTEMPTS {
                    tokio::time::sleep(CLI_LEASE_REGISTER_RETRY_DELAY).await;
                }
            }
        }
    }

    let response = match response {
        Some(response) => response,
        None => {
            if let Some(error) = last_error {
                return Err(error).with_context(|| {
                    format!("failed to register CLI lease with {:?}", socket_path)
                });
            }
            return Err(anyhow!(
                "failed to register CLI lease with {:?}",
                socket_path
            ));
        }
    };

    match response {
        Some(ServerMessage::ClientLeaseRegistered(info)) => Ok(info),
        Some(ServerMessage::Error { message }) => {
            Err(anyhow!("failed to register CLI lease: {message}"))
        }
        Some(other) => Err(anyhow!("unexpected lease registration response: {other:?}")),
        None => Err(anyhow!(
            "runtime closed connection before registering CLI lease"
        )),
    }
}

async fn detach_cli_lease_id(socket_path: &Path, client_id: String) {
    match send_socket_message(
        socket_path.to_path_buf(),
        &ClientMessage::DetachClientLease { client_id },
    )
    .await
    {
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "failed to detach CLI client lease"),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeartbeatResult {
    Renewed,
    Rejected(String),
}

async fn heartbeat_cli_lease_once(socket_path: &Path, client_id: &str) -> Result<HeartbeatResult> {
    heartbeat_response_result(
        send_socket_message(
            socket_path.to_path_buf(),
            &ClientMessage::HeartbeatClientLease {
                client_id: client_id.to_string(),
            },
        )
        .await?,
    )
}

pub(crate) fn heartbeat_response_result(
    response: Option<ServerMessage>,
) -> Result<HeartbeatResult> {
    match response {
        Some(ServerMessage::SessionRuntimeInfo(info)) if info.active => {
            Ok(HeartbeatResult::Renewed)
        }
        Some(ServerMessage::Error { message }) => Ok(HeartbeatResult::Rejected(message)),
        Some(other) => Err(anyhow!("unexpected lease heartbeat response: {other:?}")),
        None => Err(anyhow!("runtime closed connection before lease heartbeat")),
    }
}

pub(crate) fn heartbeat_interval(lease_timeout: Duration) -> Duration {
    let third = lease_timeout / 3;
    if third.is_zero() {
        Duration::from_secs(1)
    } else {
        third.min(DEFAULT_CLI_HEARTBEAT_INTERVAL)
    }
}

pub(crate) fn heartbeat_retry_delay(attempt: usize) -> Duration {
    let factor = u32::try_from(attempt).unwrap_or(u32::MAX);
    CLI_LEASE_HEARTBEAT_RETRY_DELAY.saturating_mul(factor)
}

pub(crate) fn root_socket_path_for_session(session: &Session) -> PathBuf {
    let mut dir = session.session_dir().clone();
    while dir
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("subagent-"))
    {
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        } else {
            break;
        }
    }
    dir.join("server.sock")
}

pub(crate) async fn wait_for_runtime_end(socket_path: PathBuf) {
    let mut consecutive_unresponsive = 0;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(tcode_runtime::bootstrap::RUNTIME_MONITOR_INTERVAL) => {
                match tcode_runtime::bootstrap::probe_runtime_status(&socket_path).await {
                    Ok(status) => match tcode_runtime::bootstrap::runtime_monitor_action(
                        &status,
                        &mut consecutive_unresponsive,
                        tcode_runtime::bootstrap::RUNTIME_UNRESPONSIVE_GRACE_PROBES,
                    ) {
                        tcode_runtime::bootstrap::RuntimeMonitorAction::Continue => {}
                        tcode_runtime::bootstrap::RuntimeMonitorAction::Stop => {
                            if matches!(status, tcode_runtime::bootstrap::RuntimeProbeStatus::Unresponsive) {
                                tracing::warn!(
                                    socket = %socket_path.display(),
                                    probes = consecutive_unresponsive,
                                    "runtime wait stopped after consecutive unresponsive probes"
                                );
                            }
                            break;
                        }
                    },
                    Err(e) => {
                        tracing::debug!(socket = %socket_path.display(), error = %e, "runtime monitor probe failed");
                        break;
                    }
                }
            }
        }
    }
}
