//! A desktop window drawing The Construct with the Rust engine, no JVM.
//!
//! ```text
//! construct-viewer                       # your pod, as the phone app opens it
//! construct-viewer layout.json           # a file: {"layout": <plumbing>, "data": {entities, queries}}
//! construct-viewer --demo                # the bundled demo
//! ```
//!
//! Your pod: the one opened last (`pod.json`, in
//! `~/Library/Application Support/The Construct` on macOS, else
//! `$XDG_CONFIG_HOME/the-construct` or `~/.config/the-construct`), run by
//! the engine with its cache there (`engine/`), so it opens offline as last
//! seen; its session and last pod code in the Keychain on macOS, else in
//! `secure.json` there (readable by you only).
//! Without a session, the pod's sign-in screen (a pod code, or a temporary
//! pod), staging unless `CONSTRUCT_POD_URL` says otherwise. Leave pod in
//! Settings goes back to it, with the last code filled in. Signing in to
//! Pixelfed from Settings opens the browser and comes back on a loopback
//! redirect (`http://127.0.0.1:<port>/callback`).

use construct_render::interact::Session;
use construct_render::pods::PodHome;
use construct_render::viewer::Controller;
use construct_render::window::{Platform, Reopen};
use construct_render::{Viewport, demo};
use protocol::{DataBundle, UiNode};
use serde_json::Value;

fn from_file(path: &str) -> Result<(String, UiNode, DataBundle), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    demo::parse(&v).map_err(|e| format!("{path}: {e}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fail = |e: String| -> ! {
        eprintln!("construct-viewer: {e}");
        std::process::exit(1);
    };
    let file = args.iter().find(|a| !a.starts_with("--"));
    let home = pod_home();
    let session = if let Some(path) = file {
        let (id, root, data) = from_file(path).unwrap_or_else(|e| fail(e));
        Session::new(&id, root, data, Viewport::PHONE)
    } else if args.iter().any(|a| a == "--demo") {
        let (id, root, data) = demo::welcome().unwrap_or_else(|e| fail(e.to_string()));
        Session::new(&id, root, data, Viewport::PHONE)
    } else {
        home.start(|| None)
    };
    let event_loop = winit::event_loop::EventLoop::new().unwrap_or_else(|e| {
        eprintln!("construct-viewer: no display: {e}");
        std::process::exit(1);
    });
    let app = construct_render::window::App::new(Controller::new(session), "The Construct")
        .with_platform(Box::new(Desktop(home)));
    if let Err(e) = construct_render::window::run_app(event_loop, app) {
        eprintln!("construct-viewer: {e}");
        std::process::exit(1);
    }
}

/// Where the desktop keeps its pod (`pod.json`).
fn state_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/The Construct"));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")))
        .map(|d| d.join("the-construct"))
}

fn log(m: &str) {
    eprintln!("construct-viewer: {m}");
}

fn pod_home() -> PodHome {
    let dir = state_dir();
    let server = std::env::var("CONSTRUCT_POD_URL")
        .map(|u| u.trim_end_matches('/').to_owned())
        .unwrap_or_else(|_| construct_render::pods::DEFAULT_SERVER.to_owned());
    // As the desktop app's picker: staging, a local server, and
    // `CONSTRUCT_POD_URL` when it is another.
    let servers = construct_render::pods::server_options(
        &[("local", "Local", "http://127.0.0.1:8080")],
        &server,
    );
    PodHome::new(
        dir.as_ref().map(|d| d.join("pod.json")),
        dir.as_ref().map(|d| d.join("engine")),
        server,
        construct_render::engine::desktop_store(dir.as_deref()),
        log,
    )
    .with_sign_in(construct_render::signin::SignIn::desktop())
    .with_servers(servers)
}

/// The desktop: no on-screen keyboard or insets; signing in and leaving the
/// pod keep its state as the phone app does.
struct Desktop(PodHome);

impl Platform for Desktop {
    fn show_keyboard(&self, _show: bool) {}
    fn insets_px(&self, _window_px: (u32, u32)) -> [f32; 4] {
        [0.0; 4]
    }
    fn reopen(&self, how: Reopen) -> Option<Session> {
        Some(self.0.reopen(how))
    }
}
