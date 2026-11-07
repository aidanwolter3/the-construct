//! A desktop window drawing a layout with the Rust engine, no JVM.
//!
//! ```text
//! construct-viewer                       # the bundled demo
//! construct-viewer layout.json           # {"layout": <plumbing>, "data": {entities, queries}}
//! construct-viewer --pod https://pod.example [--layout home]   # token in CONSTRUCT_POD_TOKEN
//! ```

use construct_render::interact::Session;
use construct_render::viewer::Controller;
use construct_render::{Viewport, demo};
use protocol::{DataBundle, UiNode, WorkspaceSpec};
use serde_json::Value;

fn from_file(path: &str) -> Result<(String, UiNode, DataBundle), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    demo::parse(&v).map_err(|e| format!("{path}: {e}"))
}

fn from_pod(base: &str, layout: &str) -> Result<(String, UiNode, DataBundle), String> {
    let mut req = reqwest::blocking::Client::new()
        .get(format!(
            "{}/v1/workspaces/{layout}",
            base.trim_end_matches('/')
        ))
        .header(protocol::FEATURES_HEADER, protocol::FEATURES.join(","));
    if let Ok(token) = std::env::var("CONSTRUCT_POD_TOKEN") {
        req = req.bearer_auth(token);
    }
    let spec: WorkspaceSpec = req
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| format!("{base}: {e}"))?;
    let data = DataBundle {
        entities: spec.entities,
        queries: spec.queries,
    };
    Ok((layout.to_owned(), spec.workspace, data))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let loaded = if let Some(pod) = flag("--pod") {
        from_pod(&pod, &flag("--layout").unwrap_or_else(|| "home".into()))
    } else if let Some(path) = args.iter().find(|a| !a.starts_with("--")) {
        from_file(path)
    } else {
        demo::welcome().map_err(|e| e.to_string())
    };
    let (id, root, data) = loaded.unwrap_or_else(|e| {
        eprintln!("construct-viewer: {e}");
        std::process::exit(1);
    });
    let session = Session::new(&id, root, data, Viewport::PHONE);
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
