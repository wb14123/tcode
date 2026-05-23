use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;

use crate::config::{self, RemoteConfig};
use crate::routes::build_router;
use crate::state::AppState;

/// Bind a TCP listener using the address configured in `RemoteConfig`.
pub(crate) async fn bind_listener(config: &RemoteConfig) -> anyhow::Result<TcpListener> {
    let addr = SocketAddr::new(config.bind_addr(), config.port);
    TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind tcode remote server on {addr}"))
}

/// Run the axum router on an already-bound listener until the shutdown
/// future resolves.
pub(crate) async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let router = build_router(state);
    let server = axum::serve(listener, router);

    tokio::select! {
        result = server => result.context("axum server error"),
        () = shutdown => {
            tracing::info!("shutdown requested; stopping server without waiting for clients");
            Ok(())
        }
    }
}

/// Public entry point used by `tcode remote`.
///
/// 1. Load users from `web-users.toml`.
/// 2. Load runtime settings.
/// 3. Bind a TCP listener via [`bind_listener`].
/// 4. Construct `AppState` with users and runtime.
/// 5. Log the startup URL and any exposure warning.
/// 6. Call [`serve`] with a Ctrl-C-driven shutdown future.
pub async fn run(config: RemoteConfig) -> anyhow::Result<()> {
    let users = config::load_web_users()?;

    let runtime_settings = tcode_runtime::bootstrap::RuntimeSettings::load(
        config.profile.clone(),
        config.container_config.clone(),
    )?;
    runtime_settings.init_globals().await?;

    let listener = bind_listener(&config).await?;
    let local = listener
        .local_addr()
        .context("failed to read local_addr after bind")?;

    let secure_session_cookie = !config.allow_insecure_http;
    let state = Arc::new(AppState::from_users_and_runtime(
        users,
        runtime_settings,
        secure_session_cookie,
    ));

    tracing::info!("tcode remote listening on http://{local}");
    tracing::info!(
        "all web users share a single Chrome browser profile at ~/.tcode/chrome/; per-user browser profiles are not yet implemented"
    );
    if !local.ip().is_loopback() {
        tracing::warn!(
            "tcode remote is listening on a non-loopback address over HTTP; use a strong password and prefer HTTPS/tunnel exposure"
        );
    }
    if !secure_session_cookie {
        tracing::warn!(
            "--allow-insecure-http is enabled; login passwords and session cookies may be sent over the network in cleartext"
        );
    }

    let shutdown = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => tracing::info!("received Ctrl-C; stopping server"),
            Err(e) => {
                tracing::error!(error = ?e, "ctrl_c handler failed; stopping server")
            }
        }
    };

    serve(listener, state, shutdown).await
}
