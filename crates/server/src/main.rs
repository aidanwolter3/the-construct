//! The Construct server binary: the generic pod (`pod-core`) serving the
//! compile-time adapters (`server::adapters`, D40).
//!
//! Environment:
//! - `CONSTRUCT_ADDR`: listen address (default `127.0.0.1:8080`).
//! - `CONSTRUCT_DATA_DIR`: directory for `server.db` (default `~/.construct-server`,
//!   created if missing).
//! - `CONSTRUCT_TEST_HOOKS=1`: enables `POST /v1/test/fault`.
//! - `CONSTRUCT_SHOW_DEV_APPS=1`: `GET /v1/apps` also lists the dev apps
//!   (`pixelfed_demo`).
//! - `CONSTRUCT_SESSION_TTL_DAYS`: pod session lifetime (default 90).
//! - `PIXELFED_INSTANCE_URL`: Pixelfed instance for pod->upstream calls
//!   (default `http://127.0.0.1:8081`, the local fake; `http://localhost:8090` for the
//!   local real Pixelfed, whose web/OAuth routes only answer on Host `localhost`).
//! - `PIXELFED_PUBLIC_URL`: base for browser-facing links such as the authorize
//!   endpoint (default = instance URL; e.g. `http://10.0.2.2:8081` for the emulator).
//!
//! On start, new layout files (`layouts/<app>/NNNN.json`, embedded) are appended
//! as new app versions before the server listens. A changed, out-of-order or
//! invalid file stops the start (exit code 1) with an error naming it and how to
//! recover; a deployed file missing from this build only logs a warning.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CONSTRUCT_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".construct-server")
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // No ANSI escapes when logging to a file / pipe.
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    server::validate_fixtures();

    let dir = data_dir();
    let test_hooks = std::env::var("CONSTRUCT_TEST_HOOKS").is_ok_and(|v| v == "1");
    let show_dev_apps = std::env::var("CONSTRUCT_SHOW_DEV_APPS").is_ok_and(|v| v == "1");
    let pixelfed = server::PixelfedConfig::from_env();
    tracing::info!(
        instance = %pixelfed.instance_url,
        public = %pixelfed.public_url,
        others = ?pixelfed.allowed_instances,
        any_instance = pixelfed.any_instance,
        "Pixelfed adapter configured"
    );
    let state = match server::AppState::open_dir(&dir, server::adapters(pixelfed)) {
        Ok(s) => s,
        Err(e) => {
            // E.g. a layout file changed after it was deployed: say which, and stop.
            tracing::error!("cannot start: {e} (data dir {})", dir.display());
            std::process::exit(1);
        }
    }
    .with_test_hooks(test_hooks)
    .with_session_ttl_secs(server::session::session_ttl_from_env())
    .with_dev_apps(show_dev_apps);
    tracing::info!(db = %dir.join("server.db").display(), "server database opened");
    if test_hooks {
        tracing::warn!("CONSTRUCT_TEST_HOOKS=1: POST /v1/test/fault is enabled");
    }
    for app in state.catalog().apps() {
        if let Ok(Some(v)) = state.store().app_latest_meta(app.id) {
            tracing::info!(app = app.id, version = v.version, "serving layout");
        }
    }

    let addr = std::env::var("CONSTRUCT_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| std::io::Error::new(e.kind(), format!("failed to bind {addr}: {e}")))?;
    tracing::info!(
        "construct server listening on http://{}",
        listener.local_addr()?
    );

    let state = Arc::new(state);
    let app = server::app(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            state.shutdown(); // end SSE streams so shutdown can complete
        })
        .await
}

/// Ctrl-C / SIGINT (Fly's default kill signal) or SIGTERM (`kill`, `docker stop`):
/// either one ends SSE streams cleanly, so clients see the stream end instead of a
/// cut chunked body ("error decoding response body").
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                return;
            }
            Err(e) => tracing::warn!("cannot listen for SIGTERM: {e}"),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
