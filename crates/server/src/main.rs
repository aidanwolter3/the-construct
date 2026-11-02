//! Project Canvas server binary.
//!
//! Environment:
//! - `CANVAS_ADDR`: listen address (default `127.0.0.1:8080`).
//! - `CANVAS_DATA_DIR`: directory for `server.db` (default `~/.canvas-server`,
//!   created if missing).
//! - `CANVAS_TEST_HOOKS=1`: enables `POST /v1/test/fault`.
//! - `CANVAS_AGENT_OPEN=1`: layout versions / rollback / agent endpoints accept
//!   requests without a pod session (dev and demo only).
//! - `CANVAS_LLM_PROVIDER`: layout agent provider, `auto` (default: `anthropic`
//!   if `ANTHROPIC_API_KEY` is set, else `rules`) | `anthropic` | `rules`.
//! - `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL` (default `https://api.anthropic.com`),
//!   `CANVAS_LLM_MODEL` (model id), `CANVAS_LLM_FALLBACKS=0` (no server-side refusal fallback).
//! - `PIXELFED_INSTANCE_URL`: Pixelfed instance for pod->upstream calls
//!   (default `http://127.0.0.1:8081`, the local fake; `http://localhost:8090` for the
//!   local real Pixelfed, whose web/OAuth routes only answer on Host `localhost`).
//! - `PIXELFED_PUBLIC_URL`: base for browser-facing links such as the authorize
//!   endpoint (default = instance URL; e.g. `http://10.0.2.2:8081` for the emulator).

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CANVAS_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".canvas-server")
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
    let test_hooks = std::env::var("CANVAS_TEST_HOOKS").is_ok_and(|v| v == "1");
    let agent_open = std::env::var("CANVAS_AGENT_OPEN").is_ok_and(|v| v == "1");
    let state = server::AppState::open_dir(&dir)
        .map_err(|e| std::io::Error::other(format!("failed to open {}: {e}", dir.display())))?
        .with_test_hooks(test_hooks)
        .with_pixelfed(server::PixelfedConfig::from_env())
        .with_agent_open(agent_open)
        .with_agent(server::agent::Selection::from_env());
    let pixelfed = state.pixelfed_config();
    tracing::info!(
        instance = %pixelfed.instance_url,
        public = %pixelfed.public_url,
        "Pixelfed adapter configured"
    );
    tracing::info!(db = %dir.join("server.db").display(), "server database opened");
    if test_hooks {
        tracing::warn!("CANVAS_TEST_HOOKS=1: POST /v1/test/fault is enabled");
    }
    match state.agent() {
        server::agent::Selection::Ready(p) => tracing::info!(
            provider = p.name(),
            model = p.model().unwrap_or("-"),
            "layout agent provider selected"
        ),
        server::agent::Selection::NotConfigured { provider, detail } => tracing::warn!(
            provider = %provider,
            %detail,
            "layout agent provider NOT configured: POST /v1/agent/modify_layout answers 503"
        ),
    }
    if agent_open {
        tracing::warn!(
            "CANVAS_AGENT_OPEN=1: layout versions/rollback/agent endpoints need no session (dev only)"
        );
    }

    let addr = std::env::var("CANVAS_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| std::io::Error::new(e.kind(), format!("failed to bind {addr}: {e}")))?;
    tracing::info!(
        "canvas server listening on http://{}",
        listener.local_addr()?
    );

    let state = Arc::new(state);
    let app = server::app(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            state.shutdown(); // end SSE streams so shutdown can complete
        })
        .await
}
