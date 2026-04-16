use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;

use crate::config::RemoteConfig;
use crate::routes::build_router;
use crate::state::AppState;

/// Bind a TCP listener using the address configured in `RemoteConfig`.
///
/// This is the single code path that turns a `RemoteConfig` into a bound
/// TCP socket; both [`run`] and tests go through it so a regression in the
/// bind address is caught by Test B.
pub(crate) async fn bind_listener(config: &RemoteConfig) -> anyhow::Result<TcpListener> {
    let addr = SocketAddr::new(config.bind_addr(), config.port);
    TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind tcode remote server on {addr}"))
}

/// Run the axum router on an already-bound listener until the shutdown
/// future resolves. Tests pass a `oneshot::Receiver`-backed future so they
/// can stop the server deterministically.
pub(crate) async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let router = build_router(state);
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .context("axum server error")
}

/// Public entry point used by `tcode remote`.
///
/// 1. Bind a TCP listener via [`bind_listener`].
/// 2. Capture the local address before moving the listener.
/// 3. Move the plaintext password into an `AppState`.
/// 4. Log the startup URL and the localhost-only warning.
/// 5. Call [`serve`] with a Ctrl-C-driven shutdown future.
pub async fn run(config: RemoteConfig) -> anyhow::Result<()> {
    let listener = bind_listener(&config).await?;
    let local = listener
        .local_addr()
        .context("failed to read local_addr after bind")?;

    // Move the plaintext out of `config` exactly once; subsequent access
    // to the password goes through `AppState::password`.
    let RemoteConfig { password, .. } = config;
    let state = Arc::new(AppState::new(password));

    tracing::info!("tcode remote listening on http://{local}");
    tracing::warn!("localhost-only HTTP PoC; use HTTPS/tunnel for remote access");

    let shutdown = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => tracing::info!("received Ctrl-C; initiating graceful shutdown"),
            Err(e) => {
                tracing::error!(error = ?e, "ctrl_c handler failed; initiating graceful shutdown")
            }
        }
    };

    serve(listener, state, shutdown).await
}
