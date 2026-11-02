//! Fake Pixelfed upstream binary.
//!
//! Environment:
//! - `FAKE_PIXELFED_ADDR`: listen address (default `127.0.0.1:8081`).
//! - `FAKE_PIXELFED_PUBLIC_URL`: base URL for every link it emits (default `http://<bound addr>`).
//! - `FAKE_PIXELFED_AUTO_APPROVE=1`: `GET /oauth/authorize` redirects immediately.
//! - `FAKE_PIXELFED_TOKEN`: a pre-seeded valid access token.
//! - `FAKE_PIXELFED_FIXTURES`: fixture directory (default `fixtures/pixelfed` in this repo).

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let addr = env_nonempty("FAKE_PIXELFED_ADDR").unwrap_or_else(|| "127.0.0.1:8081".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| std::io::Error::new(e.kind(), format!("failed to bind {addr}: {e}")))?;
    let local = listener.local_addr()?;
    let public_url =
        env_nonempty("FAKE_PIXELFED_PUBLIC_URL").unwrap_or_else(|| format!("http://{local}"));
    let fixtures_dir = env_nonempty("FAKE_PIXELFED_FIXTURES")
        .map_or_else(fake_pixelfed::default_fixtures_dir, PathBuf::from);
    let auto_approve = env_nonempty("FAKE_PIXELFED_AUTO_APPROVE").is_some_and(|v| v == "1");
    let seed_token = env_nonempty("FAKE_PIXELFED_TOKEN");

    let has_seed = seed_token.is_some();
    let state = fake_pixelfed::AppState::new(fake_pixelfed::Config {
        fixtures_dir: fixtures_dir.clone(),
        public_url: public_url.clone(),
        auto_approve,
        seed_token,
    })?;
    tracing::info!(fixtures = %fixtures_dir.display(), public_url, "fixtures loaded");
    if auto_approve {
        tracing::warn!("FAKE_PIXELFED_AUTO_APPROVE=1: /oauth/authorize approves without a click");
    }
    if has_seed {
        tracing::warn!("FAKE_PIXELFED_TOKEN set: a pre-seeded access token is valid");
    }
    tracing::info!("fake pixelfed listening on http://{local}");

    axum::serve(listener, fake_pixelfed::app(Arc::new(state)))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
