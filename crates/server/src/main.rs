//! The Construct server binary: one host serving many pods (D46), each the
//! generic pod (`pod-core`) with the compile-time adapters (`server::adapters`,
//! D40).
//!
//! `server` (or `server serve`) runs the host. `server pods …` manages pods
//! (run it on the server, e.g. `fly ssh console -C "/server pods list"`):
//!
//! - `pods create <name>`: a new pod; prints its token once.
//! - `pods adopt <name> <path/to/server.db>`: make a pre-D46 single-pod
//!   database a pod (copied and checked first; the original is renamed).
//! - `pods remove-adopted [dir]`: delete those renamed originals (default: the
//!   data dir) once the pod works, without a shell on the machine.
//! - `pods rotate <name> [--revoke-mcp]`: a new token; the old one stops working
//!   (with `--revoke-mcp`, every connected app is disconnected too).
//! - `pods delete <name>`: stop a pod now; its files go at the next sweep.
//! - `pods notify-test <name>`: send the pod a test notification (stored in
//!   its inbox and sent by Web Push to its devices).
//! - `pods mcp <name> [label] [--access read|edit|outward]`: a credential
//!   for Claude Code to reach the pod's MCP endpoint (`/v1/mcp` only;
//!   decision `mcp-connector`) with that access (default edit; decision
//!   `mcp-access-levels`); printed once. Claude on the web connects through
//!   OAuth instead.
//! - `pods revoke-mcp <name>`: disconnect Claude (every MCP credential).
//! - `pods monitoring <name> on|off`: whether the pod's apps may read the
//!   server's monitoring data (the `monitor` source; decision `server-monitoring`).
//! - `pods list`: each pod, with whether it is on the construct (a pod the
//!   old layout system still holds is converted when it next opens).
//!
//! Environment:
//! - `CONSTRUCT_ADDR`: listen address (default `127.0.0.1:8080`).
//! - `CONSTRUCT_DATA_DIR`: `host.db` and `pods/` (default `~/.construct-server`).
//! - `CONSTRUCT_OPEN_POD=<name>`: callers without a pod token use this pod
//!   (created if missing). Development only. Unset on a loopback address (the
//!   default `127.0.0.1:8080`): the pod `dev`; `off` turns that off. Refused
//!   on a non-loopback address unless `CONSTRUCT_INSECURE_OPEN_POD_ON_NETWORK=1`,
//!   and always behind Fly (`FLY_APP_NAME`). Only requests whose `Host` is
//!   `localhost` or an IP address use it (no DNS rebinding).
//! - `CONSTRUCT_TEMP_PODS=1`, `CONSTRUCT_TEMP_POD_HOURS`, `CONSTRUCT_TEMP_POD_MAX`:
//!   anyone may create a temporary pod.
//! - `CONSTRUCT_TEST_HOOKS=1`: enables `POST /v1/test/fault` in every pod.
//! - `CONSTRUCT_SHOW_DEV_APPS=1`: MCP also lists the dev core layouts and sources.
//! - `CONSTRUCT_SESSION_TTL_DAYS`: lifetime of sessions other than pod tokens.
//! - `CONSTRUCT_VAPID_PRIVATE_KEY`, `CONSTRUCT_VAPID_SUBJECT`, `CONSTRUCT_PUSH=off`:
//!   Web Push for notifications (D52); without a key the server makes one and
//!   keeps it in `host.db`.
//! - `CONSTRUCT_SECRETS_KEY`: base64 of 32 random bytes (comma-separated:
//!   key rotation, first one seals) that seals upstream credentials in the
//!   pod databases (`pod_core::secrets`). Unset: stored unsealed, with a
//!   warning; a malformed value stops the server.
//! - `CONSTRUCT_PUBLIC_URL`: the base URL clients use (OAuth metadata for
//!   Claude); default: from each request's `Host` off Fly; on Fly `https://$FLY_APP_NAME.fly.dev`.
//! - `CONSTRUCT_INFER_API_KEY`, `CONSTRUCT_INFER_MODEL`, `CONSTRUCT_INFER_URL`:
//!   the model Collections asks for `model`-computed fields (unset: they
//!   keep their default).
//! - `CONSTRUCT_METRICS_ADDR`: serve `GET /metrics` (Prometheus text) on
//!   this address, apart from the public one (decision `server-monitoring`). Unset:
//!   off. On Fly, `[::]:9091`, which only Fly's private network reaches.
//! - `PIXELFED_*`: see `PixelfedConfig::from_env` and docs/server.md.
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

fn factory() -> Result<server::host::PodFactory, String> {
    let test_hooks = std::env::var("CONSTRUCT_TEST_HOOKS").is_ok_and(|v| v == "1");
    let show_dev_apps = std::env::var("CONSTRUCT_SHOW_DEV_APPS").is_ok_and(|v| v == "1");
    if test_hooks {
        tracing::warn!(
            "CONSTRUCT_TEST_HOOKS=1: POST /v1/test/fault is enabled and Web Push may reach local addresses"
        );
    }
    let push = server::push_from_env(&data_dir(), test_hooks)?;
    match &push {
        Some(p) => tracing::info!(public_key = %p.public_key(), "Web Push enabled"),
        None => tracing::info!("Web Push off (CONSTRUCT_PUSH=off)"),
    }
    Ok(server::pod_factory_with_push(
        server::PixelfedConfig::from_env(),
        test_hooks,
        server::session::session_ttl_from_env(),
        show_dev_apps,
        push,
    ))
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
    if let Err(e) = init_secrets() {
        eprintln!("server: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => match serve(LocalPod::default()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("server: {e}");
                std::process::ExitCode::FAILURE
            }
        },
        ["serve", flags @ ..] => match LocalPod::parse(flags).and_then(serve) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("server: {e}");
                std::process::ExitCode::FAILURE
            }
        },
        ["backup", dest] => match backup(dest) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
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
                "usage: server [serve]\n       \
                 server serve --pod-code <cpod_…> [--mcp-key <cmcp_…>] [--pod-name <name>]   (local testing)\n       \
                 server pods create <name>\n       \
                 server pods adopt <name> <path/to/server.db>\n       server pods remove-adopted [dir]\n       \
                 server pods rotate <name> [--revoke-mcp]\n       \
                 server pods delete <name>\n       server pods notify-test <name>\n       \
                 server pods mcp <name> [label] [--access read|edit|outward]\n       server pods revoke-mcp <name>\n       \
                 server pods monitoring <name> on|off\n       server pods list\n       \
                 server backup <dir>   (a consistent copy of the data; docs/server.md)"
            );
            std::process::ExitCode::from(2)
        }
    }
}

/// Read `CONSTRUCT_SECRETS_KEY` once, before any pod opens, and say whether
/// upstream credentials are sealed. A deploy without the key keeps working
/// (credentials stay or become plaintext) but logs an error on Fly.
fn init_secrets() -> Result<(), String> {
    let secrets = server::secrets::Secrets::init_process()?;
    if secrets.enabled() {
        tracing::info!(key_ids = ?secrets.key_ids(), "upstream credentials are sealed at rest");
    } else if std::env::var_os("FLY_APP_NAME").is_some() {
        tracing::error!(
            "CONSTRUCT_SECRETS_KEY is not set: upstream sign-in tokens are stored UNENCRYPTED. \
             Set it with `flyctl secrets set CONSTRUCT_SECRETS_KEY=$(openssl rand -base64 32)`"
        );
    } else {
        tracing::warn!(
            "CONSTRUCT_SECRETS_KEY is not set: upstream sign-in tokens are stored unencrypted \
             (fine for development)"
        );
    }
    Ok(())
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

/// `server backup <dir>`: a consistent copy of the host's data (host.db and
/// every pod's database), safe while the server runs. To restore it, put
/// the folder at `<data dir>/restore` and restart (docs/server.md).
fn backup(dest: &str) -> Result<(), String> {
    let written = server::host::backup::backup(&data_dir(), std::path::Path::new(dest))?;
    for f in &written {
        println!("{}", f.display());
    }
    println!("{} databases copied", written.len());
    Ok(())
}

/// The admin commands. They write `host.db` and the pod files directly; a
/// running server sees new tokens at once (it reads them per request).
fn pods(args: &[&str]) -> Result<(), String> {
    let dir = data_dir();
    let host = Host::open(&dir, factory()?, HostConfig::default())?;
    match args {
        ["create", name] => {
            let pod = host.create_pod(name, PodKind::Admin, None, &[])?;
            print_token("Created", &pod.name, &pod.token);
        }
        ["adopt", name, path] => {
            let pod = host.adopt(name, &PathBuf::from(path))?;
            print_token("Adopted the database as", &pod.name, &pod.token);
            println!(
                "The original file was renamed to {path}.adopted-{}.",
                pod.id
            );
            println!(
                "It still holds the old sessions and app tokens: once the pod works, delete it \
                 with `server pods remove-adopted`."
            );
        }
        ["rotate", name, flags @ ..] => {
            let revoke = match flags {
                [] => false,
                ["--revoke-mcp"] => true,
                _ => return Err("usage: server pods rotate <name> [--revoke-mcp]".into()),
            };
            let connected = host.mcp_credentials(name)?.len();
            let token = host.rotate(name, revoke)?;
            print_token("New code for", name, &token);
            println!("The old code no longer works; sign in to your apps again.");
            if revoke {
                println!("Disconnected {connected} connected app(s) (MCP credentials).");
            } else if connected > 0 {
                println!(
                    "{connected} connected app(s) (MCP credentials) stay connected. If the old code \
                     leaked, disconnect them in Settings > Connected apps, or run \
                     `server pods rotate {name} --revoke-mcp` (or `server pods revoke-mcp {name}`)."
                );
            }
        }
        ["delete", name] => {
            host.delete(name)?;
            println!(
                "Pod '{name}' is stopped; the server deletes its files within {} minutes.",
                server::host::SWEEP_EVERY.as_secs() / 60
            );
        }
        ["remove-adopted", rest @ ..] => {
            let dir = match rest {
                [] => dir,
                [path] => PathBuf::from(path),
                _ => return Err("usage: server pods remove-adopted [dir]".into()),
            };
            let removed = host.remove_adopted(&dir)?;
            if removed.is_empty() {
                println!("No adopted originals in {}.", dir.display());
            }
            for p in removed {
                println!("Deleted {}", p.display());
            }
        }
        ["notify-test", name] => {
            let title = host.notify_test(name)?;
            println!("Sent \"{title}\" to pod '{name}'.");
        }
        ["mcp", name, rest @ ..] => {
            let usage = || {
                "usage: server pods mcp <name> [label] [--access read|edit|outward] [--grant <file.json>]"
                    .to_owned()
            };
            // Decision `mcp-access-levels`: edit unless asked.
            let (mut label, mut access) = (None, pod_core::mcp::Access::Edit);
            let mut grant = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                if *a == "--access" {
                    access = it
                        .next()
                        .and_then(|v| pod_core::mcp::Access::from_id(v))
                        .ok_or_else(usage)?;
                } else if *a == "--grant" {
                    // Decision `mcp-grants`: a narrower scope for an
                    // unattended job, from a JSON file.
                    let path = it.next().ok_or_else(usage)?;
                    let text = std::fs::read_to_string(path)
                        .map_err(|e| format!("can't read {path}: {e}"))?;
                    grant = Some(
                        pod_core::grant::Grant::from_text(&text)
                            .map_err(|e| format!("{path}: {e}"))?,
                    );
                } else if label.is_none() {
                    label = Some(*a);
                } else {
                    return Err(usage());
                }
            }
            let label = label.unwrap_or("Claude Code");
            let token = host.issue_mcp_scoped(name, label, access, grant.as_ref())?;
            println!(
                "An MCP credential for pod '{name}' ({label}, {} access{}), shown only now:",
                access.id(),
                grant
                    .as_ref()
                    .map(|g| format!(", scoped: {}", g.summary()))
                    .unwrap_or_default()
            );
            println!();
            println!("    {token}");
            println!();
            println!(
                "It reaches only the pod's MCP endpoint, e.g.\n  claude mcp add --transport http construct \
                 <server>/v1/mcp --header \"Authorization: Bearer {token}\"\nRevoke it with \
                 `server pods revoke-mcp {name}`."
            );
        }
        ["monitoring", name, setting @ ("on" | "off")] => {
            let on = *setting == "on";
            host.set_admin_pod_setting(
                name,
                server::monitor::ENABLED_SETTING,
                if on { "1" } else { "0" },
            )?;
            if on {
                println!(
                    "Pod '{name}' can now read the server's monitoring data (the \"Server \
                     monitoring\" source); build an app on it, e.g. with Claude."
                );
            } else {
                println!("Pod '{name}' can no longer read the server's monitoring data.");
            }
        }
        ["gaps", rest @ ..] => {
            let name = match rest {
                [] => None,
                [n] => Some(*n),
                _ => return Err("usage: server pods gaps [name]".into()),
            };
            let all = host.gaps(name)?;
            if all.iter().all(|(_, g)| g.is_empty()) {
                println!("No agent has noted anything missing.");
            }
            for (pod, gaps) in all {
                for g in gaps {
                    println!(
                        "{pod}\t{}\t{}",
                        g["at"].as_i64().unwrap_or(0),
                        g["what"].as_str().unwrap_or("")
                    );
                }
            }
        }
        ["revoke-mcp", name] => {
            let n = host.revoke_mcp(name)?;
            println!("Revoked {n} MCP credential(s) of pod '{name}'; Claude is disconnected.");
        }
        ["list"] => {
            let now = server::util::now_secs();
            for p in host.store().pods().map_err(|e| e.to_string())? {
                // A deleted pod stops at once (no tokens, no slot) and the
                // server's sweeper removes it within 5 minutes.
                let expires = match p.expires_at {
                    None => "-".to_owned(),
                    Some(t) if t <= now => "deleted (files go within 5 min)".to_owned(),
                    Some(t) => t.to_string(),
                };
                let mcp = host.store().mcp_credentials(&p.id).map_or(0, |c| c.len());
                // Whether it is on the construct (a pod the old layout
                // system holds converts when it next opens).
                let construct = match host.pod_on_construct(&p.id) {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "?",
                };
                println!(
                    "{:<24} {:<9} id {}  created {}  expires {expires}  mcp {mcp}  construct {construct}",
                    p.name,
                    format!("{:?}", p.kind).to_lowercase(),
                    p.id,
                    p.created_at
                );
                if let Ok(Some(by)) = host.store().created_by(&p.id) {
                    println!("{:<24} created by {by}", "");
                }
            }
        }
        _ => {
            return Err(
                "unknown pods command (create, adopt, remove-adopted, rotate, delete, notify-test, mcp, revoke-mcp, monitoring, list)".into(),
            );
        }
    }
    Ok(())
}

/// `server serve --pod-code … [--mcp-key …] [--pod-name …]`: a pod whose
/// codes stay the same across restarts, for testing on this machine
/// ([`server::host::Host::ensure_local_pod`]). Refused on Fly and on a
/// non-loopback address.
#[derive(Default)]
struct LocalPod {
    name: Option<String>,
    code: Option<String>,
    mcp_key: Option<String>,
}

impl LocalPod {
    fn parse(flags: &[&str]) -> Result<Self, String> {
        let mut out = LocalPod::default();
        let mut it = flags.iter();
        while let Some(flag) = it.next() {
            let mut value = || {
                it.next()
                    .map(|v| (*v).to_owned())
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match *flag {
                "--pod-code" => out.code = Some(value()?),
                "--mcp-key" => out.mcp_key = Some(value()?),
                "--pod-name" => out.name = Some(value()?),
                other => return Err(format!("unknown flag {other}")),
            }
        }
        if out.code.is_none() && (out.mcp_key.is_some() || out.name.is_some()) {
            return Err("--mcp-key and --pod-name go with --pod-code".to_owned());
        }
        Ok(out)
    }
}

/// The local pod's name when `--pod-name` isn't given.
const LOCAL_POD: &str = "local";

/// The stack of the runtime's threads. Reading, expanding and checking a
/// layout recurse once per nested node, and components nest boxes in boxes:
/// the default 2 MiB is not far above what a deep page needs, and a stack
/// overflow ends the whole server (decision `element-roles-and-reuse`).
/// Address space, not memory: pages are touched only as far as used.
const THREAD_STACK: usize = 16 * 1024 * 1024;

fn serve(local: LocalPod) -> Result<(), String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(THREAD_STACK)
        .build()
        .map_err(|e| format!("starting the runtime: {e}"))?
        .block_on(serve_on(local))
}

async fn serve_on(local: LocalPod) -> Result<(), String> {
    let dir = data_dir();
    // A backup waiting in <data dir>/restore goes in place before anything
    // opens a database (docs/server.md "Backups and restore").
    if let Some(aside) = server::host::backup::restore_pending(&dir)? {
        tracing::warn!(replaced = %aside.display(), "restored the data from a backup");
    }
    // Before any render: fence the headless browser's network, so its user
    // reaches only its proxy (decision `browser-network-fence`); logs and
    // serves on if it can't.
    if web_extract::render::Browser::from_env(false).is_some() {
        web_extract::render::block_private_network().await;
    }
    let pixelfed = server::PixelfedConfig::from_env();
    tracing::info!(
        instance = %pixelfed.instance_url,
        public = %pixelfed.public_url,
        others = ?pixelfed.allowed_instances,
        any_instance = pixelfed.any_instance,
        "Pixelfed adapter configured"
    );
    let addr = std::env::var("CONSTRUCT_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let mut config = HostConfig {
        temporary_pod_settings: server::temporary_pod_settings(),
        ..HostConfig::from_env()
    };
    let on_fly = std::env::var_os("FLY_APP_NAME").is_some_and(|v| !v.is_empty());
    // Only this machine can reach a loopback server: keep `cargo run -p
    // server` working without a pod token, as before D46.
    if config.open_pod.is_none()
        && !on_fly
        && server::is_loopback_addr(&addr)
        && std::env::var("CONSTRUCT_OPEN_POD").is_err()
    {
        config.open_pod = Some("dev".to_owned());
    }
    if config.open_pod.is_some() {
        let opt_in = std::env::var(server::INSECURE_OPEN_POD_VAR).is_ok_and(|v| v == "1");
        server::check_open_pod(&addr, on_fly, opt_in)?;
        config.open_pod_any_ip = opt_in;
        if opt_in && !server::is_loopback_addr(&addr) {
            tracing::warn!(
                %addr,
                "{}=1: anyone on this network can use the open pod",
                server::INSECURE_OPEN_POD_VAR
            );
        }
    }
    if let Some(t) = config.temporary {
        tracing::info!(hours = t.hours, max = t.max_live, "temporary pods enabled");
    }
    if !config.android.cert_sha256.is_empty() {
        tracing::info!(
            package = %config.android.package,
            certs = config.android.cert_sha256.len(),
            "Android App Links: serving /.well-known/assetlinks.json"
        );
    }
    let open_pod = config.open_pod.is_some();
    if local.code.is_some() && (on_fly || !server::is_loopback_addr(&addr)) {
        return Err(
            "--pod-code is for testing on this machine: not on Fly or a non-loopback address"
                .into(),
        );
    }
    let host = tokio::task::spawn_blocking(move || -> Result<Host, String> {
        let host = Host::open(&dir, factory()?, config)?;
        if let Some(code) = &local.code {
            let name = local.name.as_deref().unwrap_or(LOCAL_POD);
            host.ensure_local_pod(name, code, local.mcp_key.as_deref())?;
            tracing::warn!(
                pod = name,
                mcp = local.mcp_key.is_some(),
                "local testing: pod opens with the --pod-code given on the command line"
            );
        }
        Ok(host)
    })
    .await
    .map_err(|e| e.to_string())??;
    let host = Arc::new(host);
    {
        let h = host.clone();
        server::metrics::set_gauge_source(move || h.monitoring_gauges());
    }
    pod_core::paid_use::set_server_ledger(host.paid_use_ledger());
    serve_metrics().await;
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
    host.spawn_scheduler();
    if !open_pod {
        tracing::info!("callers need a pod token (see `server pods`)");
    }

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

/// `GET /metrics` on `CONSTRUCT_METRICS_ADDR` (decision `server-monitoring`): a
/// listener of its own, so the public address never serves it. Also keeps
/// the copies of the counters that the last hour and day are measured from.
/// A listener that can't start is logged, not fatal: the pod still serves.
async fn serve_metrics() {
    server::metrics::started_at();
    tokio::spawn(async {
        let every = std::time::Duration::from_secs(server::metrics::SNAPSHOT_EVERY_SECS as u64);
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            server::metrics::snapshot();
        }
    });
    let Some(addr) = std::env::var("CONSTRUCT_METRICS_ADDR")
        .ok()
        .filter(|a| !a.trim().is_empty())
    else {
        return;
    };
    let listener = match tokio::net::TcpListener::bind(addr.trim()).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "CONSTRUCT_METRICS_ADDR: cannot listen; /metrics is off");
            return;
        }
    };
    tracing::info!(addr = %addr.trim(), "metrics on /metrics");
    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(|| async {
            let text = tokio::task::spawn_blocking(server::metrics::render)
                .await
                .unwrap_or_default();
            (
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; version=0.0.4; charset=utf-8",
                )],
                text,
            )
        }),
    );
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "metrics listener stopped");
        }
    });
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
