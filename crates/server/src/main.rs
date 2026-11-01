//! Project Canvas server binary. Listens on `CANVAS_ADDR` (default
//! `127.0.0.1:8080`).

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    server::validate_fixtures();

    let addr = std::env::var("CANVAS_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(
        "canvas server listening on http://{}",
        listener.local_addr()?
    );

    axum::serve(listener, server::app())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
