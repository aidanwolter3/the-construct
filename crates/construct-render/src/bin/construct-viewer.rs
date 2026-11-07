//! A desktop window drawing a layout with the Rust engine, no JVM.
//!
//! ```text
//! construct-viewer                       # the bundled demo
//! construct-viewer layout.json           # {"layout": <plumbing>, "data": {entities, queries}}
//! construct-viewer --pod https://pod.example [--layout home]   # live; CONSTRUCT_POD_TOKEN: pod code or session
//! ```

use construct_render::interact::Session;
use construct_render::viewer::Controller;
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
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let fail = |e: String| -> ! {
        eprintln!("construct-viewer: {e}");
        std::process::exit(1);
    };
    let session = if let Some(pod) = flag("--pod") {
        // Live: a session token or the pod code (exchanged for a session).
        let token = std::env::var("CONSTRUCT_POD_TOKEN").unwrap_or_else(|_| {
            fail("--pod needs CONSTRUCT_POD_TOKEN (a pod code or session token)".into())
        });
        construct_render::remote::PodClient::connect(&pod, &token)
            .and_then(|c| {
                c.session(
                    &flag("--layout").unwrap_or_else(|| "home".into()),
                    Viewport::PHONE,
                )
            })
            .unwrap_or_else(|e| fail(e))
    } else {
        let loaded = match args.iter().find(|a| !a.starts_with("--")) {
            Some(path) => from_file(path),
            None => demo::welcome().map_err(|e| e.to_string()),
        };
        let (id, root, data) = loaded.unwrap_or_else(|e| fail(e));
        Session::new(&id, root, data, Viewport::PHONE)
    };
    let event_loop = winit::event_loop::EventLoop::new().unwrap_or_else(|e| {
        eprintln!("construct-viewer: no display: {e}");
        std::process::exit(1);
    });
    if let Err(e) =
        construct_render::window::run(event_loop, Controller::new(session), "The Construct")
    {
        eprintln!("construct-viewer: {e}");
        std::process::exit(1);
    }
}
