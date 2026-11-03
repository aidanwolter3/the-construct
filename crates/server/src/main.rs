//! The Construct server binary: one host serving many pods (D46), each the
//! generic pod (`pod-core`) with the compile-time adapters (`server::adapters`,
//! D40).
//!
//! `server` (or `server serve`) runs the host. `server pods …` manages pods
//! (run it on the server, e.g. `fly ssh console -C "/server pods list"`):
//!
//! - `pods create <name> [--import-recipes]`: a new pod; prints its token once.
//! - `pods adopt <name> <path/to/server.db>`: make a pre-D46 single-pod
//!   database a pod (copied and checked first; the original is renamed).
//! - `pods rotate <name>`: a new token; the old one stops working.
//! - `pods delete <name>`: stop a pod now; its files go at the next sweep.
//! - `pods list`.
//!
//! Environment:
//! - `CONSTRUCT_ADDR`: listen address (default `127.0.0.1:8080`).
//! - `CONSTRUCT_DATA_DIR`: `host.db` and `pods/` (default `~/.construct-server`).
//! - `CONSTRUCT_OPEN_POD=<name>`: callers without a pod token use this pod
//!   (created if missing). Development only.
//! - `CONSTRUCT_TEMP_PODS=1`, `CONSTRUCT_TEMP_POD_HOURS`, `CONSTRUCT_TEMP_POD_MAX`:
//!   anyone may create a temporary pod.
//! - `CONSTRUCT_TEST_HOOKS=1`: enables `POST /v1/test/fault` in every pod.
//! - `CONSTRUCT_SHOW_DEV_APPS=1`: `GET /v1/apps` also lists the dev apps.
//! - `CONSTRUCT_SESSION_TTL_DAYS`: lifetime of sessions other than pod tokens.
//! - `PIXELFED_*`: see `PixelfedConfig::from_env` and README.
//!
//! A pod's layout files are synced into its database when the pod is first
//! opened; a changed or invalid file makes that pod answer 503 (logged).

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use server::host::{Host, HostConfig, PodKind};
use tracing_subscriber::EnvFilter;

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CONSTRUCT_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".construct-server")
}

fn factory() -> server::host::PodFactory {
    let test_hooks = std::env::var("CONSTRUCT_TEST_HOOKS").is_ok_and(|v| v == "1");
    let show_dev_apps = std::env::var("CONSTRUCT_SHOW_DEV_APPS").is_ok_and(|v| v == "1");
    if test_hooks {
        tracing::warn!("CONSTRUCT_TEST_HOOKS=1: POST /v1/test/fault is enabled");
    }
    server::pod_factory(
        server::PixelfedConfig::from_env(),
        test_hooks,
        server::session::session_ttl_from_env(),
        show_dev_apps,
    )
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();
    server::validate_fixtures();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => match serve() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("server: {e}");
                std::process::ExitCode::FAILURE
            }
        },
        ["pods", rest @ ..] => match pods(rest) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: server [serve]\n       server pods create <name> [--import-recipes]\n       \
                 server pods adopt <name> <path/to/server.db>\n       server pods rotate <name>\n       \
                 server pods delete <name>\n       server pods list"
            );
            std::process::ExitCode::from(2)
        }
    }
}

fn print_token(what: &str, name: &str, token: &str) {
    println!("{what} pod '{name}'. Its pod code (shown only now; keep it in a password manager):");
    println!();
    println!("    {token}");
    println!();
    println!(
        "Enter it in the app's \"Open your pod\" screen, or send it as `Authorization: Bearer`."
    );
}

/// The admin commands. They write `host.db` and the pod files directly; a
/// running server sees new tokens at once (it reads them per request).
fn pods(args: &[&str]) -> Result<(), String> {
    let dir = data_dir();
    let config = HostConfig {
        new_pod_settings: server::new_pod_settings(false),
        ..HostConfig::default()
    };
    let host = Host::open(&dir, factory(), config)?;
    match args {
        ["create", name, flags @ ..] => {
            let import = match flags {
                [] => false,
                ["--import-recipes"] => true,
                _ => return Err(format!("unknown flags {flags:?}")),
            };
            let pod = host.create_pod(
                name,
                PodKind::Admin,
                None,
                &server::new_pod_settings(import),
            )?;
            print_token("Created", &pod.name, &pod.token);
        }
        ["adopt", name, path] => {
            let pod = host.adopt(name, &PathBuf::from(path))?;
            print_token("Adopted the database as", &pod.name, &pod.token);
            println!(
                "The original file was renamed to {path}.adopted-{}.",
                pod.id
            );
        }
        ["rotate", name] => {
            let token = host.rotate(name)?;
            print_token("New code for", name, &token);
            println!("The old code no longer works; sign in to your apps again.");
        }
        ["delete", name] => {
            host.delete(name)?;
            println!(
                "Pod '{name}' is stopped; the server deletes its files within {} minutes.",
                server::host::SWEEP_EVERY.as_secs() / 60
            );
        }
        ["list"] => {
            for p in host.store().pods().map_err(|e| e.to_string())? {
                let expires = p
                    .expires_at
                    .map_or_else(|| "-".to_owned(), |t| t.to_string());
                println!(
                    "{:<24} {:<9} id {}  created {}  expires {expires}",
                    p.name,
                    format!("{:?}", p.kind).to_lowercase(),
                    p.id,
                    p.created_at
                );
            }
        }
        _ => return Err("unknown pods command (create, adopt, rotate, delete, list)".into()),
    }
    Ok(())
}

#[tokio::main]
async fn serve() -> Result<(), String> {
    let dir = data_dir();
    let pixelfed = server::PixelfedConfig::from_env();
    tracing::info!(
        instance = %pixelfed.instance_url,
        public = %pixelfed.public_url,
        others = ?pixelfed.allowed_instances,
        any_instance = pixelfed.any_instance,
        "Pixelfed adapter configured"
    );
    let config = HostConfig {
        new_pod_settings: server::new_pod_settings(false),
        ..HostConfig::from_env()
    };
    if let Some(t) = config.temporary {
        tracing::info!(hours = t.hours, max = t.max_live, "temporary pods enabled");
    }
    let open_pod = config.open_pod.is_some();
    let host = tokio::task::spawn_blocking(move || Host::open(&dir, factory(), config))
        .await
        .map_err(|e| e.to_string())??;
    let host = Arc::new(host);
    {
        let h = host.clone();
        let swept = tokio::task::spawn_blocking(move || h.sweep())
            .await
            .unwrap_or(0);
        if swept > 0 {
            tracing::info!(swept, "expired pods deleted at startup");
        }
    }
    host.spawn_sweeper();
    if !open_pod {
        tracing::info!("callers need a pod token (see `server pods`)");
    }

    let addr = std::env::var("CONSTRUCT_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;
    tracing::info!(
        "construct server listening on http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let app = server::host::app(host.clone());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        host.shutdown(); // end SSE streams so shutdown can complete
    })
    .await
    .map_err(|e| e.to_string())
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
