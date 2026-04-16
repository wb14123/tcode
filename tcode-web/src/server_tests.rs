use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::RemoteConfig;
use crate::state::AppState;

/// Test B — real TCP bind via `bind_listener` + reachability smoke test.
#[tokio::test]
async fn bind_listener_binds_loopback_and_serves() -> anyhow::Result<()> {
    let cfg = RemoteConfig::for_test(0, "valid-password-16chars!".into());
    let listener = crate::server::bind_listener(&cfg).await?;
    let addr = listener.local_addr()?;
    assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let state = Arc::new(AppState::new("valid-password-16chars!".into()));
    let handle = tokio::spawn(async move {
        crate::server::serve(listener, state, async move {
            if let Err(e) = rx.await {
                tracing::warn!(?e, "shutdown channel dropped");
            }
        })
        .await
    });

    // Drive the client interaction under a 5s timeout so a dead server
    // fails fast rather than hanging CI.
    let client_result: anyhow::Result<()> = tokio::time::timeout(
        Duration::from_secs(5),
        async move {
            let mut stream = TcpStream::connect(addr).await?;
            stream
                .write_all(
                    b"GET /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                )
                .await?;

            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).await?;

            let head = std::str::from_utf8(&buf).context("non-utf8 response body")?;
            let first_line = head.lines().next().unwrap_or("");
            anyhow::ensure!(
                first_line.starts_with("HTTP/1.0 200") || first_line.starts_with("HTTP/1.1 200"),
                "unexpected status line: {first_line}"
            );
            Ok(())
        },
    )
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("client interaction timed out after 5s")));

    // Signal shutdown. Ignore a closed receiver — that means the server
    // already exited (e.g. due to a serve-level error surfaced below).
    if let Err(e) = tx.send(()) {
        tracing::debug!(?e, "server already shut down before test signaled");
    }

    // Bound the server's shutdown too — if `serve` is wedged, fail fast.
    let serve_result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .context("serve did not shut down within 2s")?;
    // Join error → propagate; inner `Result` from `serve` → propagate.
    let serve_inner = serve_result.context("serve task join error")?;
    serve_inner?;

    client_result?;
    Ok(())
}

/// Test C — `RemoteConfig::try_new` default-address guard.
#[test]
fn remote_config_binds_to_loopback() -> anyhow::Result<()> {
    let cfg = RemoteConfig::try_new(8765, "valid-password-16chars!".into(), false)?;
    assert_eq!(cfg.bind_addr(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    Ok(())
}
