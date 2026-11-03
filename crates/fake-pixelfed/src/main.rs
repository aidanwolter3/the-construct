//! Fake Pixelfed upstream binary.
//!
//! Environment:
//! - `FAKE_PIXELFED_ADDR`: listen address (default `127.0.0.1:8081`; with only `PORT` set,
//!   as on most PaaS, `0.0.0.0:$PORT`).
//! - `FAKE_PIXELFED_PUBLIC_URL`: base URL for every link it emits (default `http://<bound addr>`,
//!   with an unspecified bind address such as `0.0.0.0` shown as `127.0.0.1`).
//! - `FAKE_PIXELFED_AUTO_APPROVE=1`: `GET /oauth/authorize` redirects immediately.
//! - `FAKE_PIXELFED_TOKEN`: a pre-seeded valid access token.
//! - `FAKE_PIXELFED_FIXTURES`: fixture directory (default: the copies embedded in the binary).
//! - `FAKE_PIXELFED_ADMIN`: `1` serves the `/_admin` test controls on any address, `0` never;
//!   unset, only when bound to a loopback address.

use std::io::IsTerminal;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// `http://<addr>`, with an unspecified IP (0.0.0.0 / ::) replaced by 127.0.0.1.
fn default_public_url(local: SocketAddr) -> String {
    let ip = if local.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        local.ip()
    };
    format!("http://{}", SocketAddr::new(ip, local.port()))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let addr = env_nonempty("FAKE_PIXELFED_ADDR")
        .or_else(|| env_nonempty("PORT").map(|p| format!("0.0.0.0:{p}")))
        .unwrap_or_else(|| "127.0.0.1:8081".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| std::io::Error::new(e.kind(), format!("failed to bind {addr}: {e}")))?;
    let local = listener.local_addr()?;
    let public_url =
        env_nonempty("FAKE_PIXELFED_PUBLIC_URL").unwrap_or_else(|| default_public_url(local));
    let fixtures_dir = env_nonempty("FAKE_PIXELFED_FIXTURES").map(PathBuf::from);
    let auto_approve = env_nonempty("FAKE_PIXELFED_AUTO_APPROVE").is_some_and(|v| v == "1");
    let seed_token = env_nonempty("FAKE_PIXELFED_TOKEN");
    let admin = match env_nonempty("FAKE_PIXELFED_ADMIN").as_deref() {
        Some("1" | "true") => true,
        Some(_) => false,
        None => local.ip().is_loopback(),
    };

    let mastodon_threads = match env_nonempty("FAKE_PIXELFED_THREADS").as_deref() {
        None | Some("pixelfed") => false,
        Some("mastodon") => true,
        Some(other) => {
            return Err(std::io::Error::other(format!(
                "FAKE_PIXELFED_THREADS={other:?}: expected pixelfed or mastodon"
            )));
        }
    };
    let has_seed = seed_token.is_some();
    let state = fake_pixelfed::AppState::new(fake_pixelfed::Config {
        fixtures_dir: fixtures_dir.clone().unwrap_or_default(),
        public_url: public_url.clone(),
        auto_approve,
        seed_token,
    })?
    .with_admin(admin)
    .with_mastodon_threads(mastodon_threads);
    match &fixtures_dir {
        Some(dir) => tracing::info!(fixtures = %dir.display(), public_url, "fixtures loaded"),
        None => tracing::info!(public_url, "embedded fixtures loaded"),
    }
    if auto_approve {
        tracing::warn!("FAKE_PIXELFED_AUTO_APPROVE=1: /oauth/authorize approves without a click");
    }
    if has_seed {
        tracing::warn!("FAKE_PIXELFED_TOKEN set: a pre-seeded access token is valid");
    }
    if admin {
        tracing::warn!("admin endpoints on: /_admin/state, /_admin/reset, /_admin/faults");
    }
    tracing::info!("fake pixelfed listening on http://{local}");

    axum::serve(listener, fake_pixelfed::app(Arc::new(state)))
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
