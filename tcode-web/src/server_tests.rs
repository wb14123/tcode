use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::RemoteConfig;
use crate::routes::SESSION_COOKIE_NAME;
use crate::routes::test_support::{HomeGuard, VALID_PASSWORD};
use crate::state::AppState;

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/tcode-web-server")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

async fn read_http_headers(stream: &mut TcpStream) -> anyhow::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 128];
    loop {
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut chunk))
            .await
            .context("timed out waiting for response headers")??;
        anyhow::ensure!(read != 0, "connection closed before response headers");
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(String::from_utf8(buf).context("non-utf8 response headers")?);
        }
    }
}

fn assert_ok_response_head(head: &str) -> anyhow::Result<()> {
    let first_line = head.lines().next().unwrap_or("");
    anyhow::ensure!(
        first_line.starts_with("HTTP/1.0 200") || first_line.starts_with("HTTP/1.1 200"),
        "unexpected status line: {first_line}"
    );
    Ok(())
}

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

#[tokio::test]
async fn serve_shutdown_does_not_wait_for_sse_client() -> anyhow::Result<()> {
    let home_dir = temp_dir();
    let _home_guard = HomeGuard::set(&home_dir);

    let session_id = "abc123xy";
    let session_dir = tcode_runtime::session::base_path()?.join(session_id);
    tokio::fs::create_dir_all(&session_dir).await?;

    let cfg = RemoteConfig::for_test(0, VALID_PASSWORD.into());
    let listener = crate::server::bind_listener(&cfg).await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let state = Arc::new(AppState::new(VALID_PASSWORD.into()));
    let token = state.mint_session()?;
    let handle = tokio::spawn(async move {
        crate::server::serve(listener, state, async move {
            if let Err(e) = rx.await {
                tracing::warn!(?e, "shutdown channel dropped");
            }
        })
        .await
    });

    let mut stream = TcpStream::connect(addr).await?;
    let request = format!(
        "GET /api/sessions/{session_id}/display.jsonl HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Accept: text/event-stream\r\n\
         Cookie: {SESSION_COOKIE_NAME}={token}\r\n\
         \r\n"
    );
    stream.write_all(request.as_bytes()).await?;

    let head = read_http_headers(&mut stream).await?;
    assert_ok_response_head(&head)?;
    anyhow::ensure!(
        head.to_ascii_lowercase().contains("text/event-stream"),
        "SSE response missing text/event-stream content type: {head}"
    );

    if let Err(e) = tx.send(()) {
        tracing::debug!(?e, "server already shut down before test signaled");
    }

    let serve_result = tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .context("serve waited for an open SSE client during shutdown")?;
    let serve_inner = serve_result.context("serve task join error")?;
    serve_inner?;

    // Keep the client stream alive until after `serve` exits. With axum's
    // unbounded graceful shutdown this open SSE response would block here.
    drop(stream);
    Ok(())
}

/// Test C — `RemoteConfig::try_new` default-address guard.
#[test]
fn remote_config_binds_to_loopback() -> anyhow::Result<()> {
    let cfg = RemoteConfig::try_new(8765, "valid-password-16chars!".into(), false)?;
    assert_eq!(cfg.bind_addr(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    Ok(())
}
