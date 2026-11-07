//! `render_bundle <bundle.json>... --out <dir> [--scale 3]`: draws workspace
//! bundles (`{workspace, entities, queries}`, as `GET /v1/workspaces/<id>`
//! answers) on the CPU at 390 × 844 and writes `<name>.png`.

use construct_render::Viewport;
use construct_render::interact::Session;
use protocol::{DataBundle, WorkspaceSpec};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| {
        args.iter()
            .position(|a| a == n)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let out = std::path::PathBuf::from(flag("--out").unwrap_or_else(|| ".".into()));
    let scale: f32 = flag("--scale").and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let mut skip = false;
    for a in &args {
        if skip || a.starts_with("--") {
            skip = a.starts_with("--");
            continue;
        }
        let text = std::fs::read_to_string(a).expect("bundle");
        let spec: WorkspaceSpec = serde_json::from_str(&text).expect("a workspace bundle");
        let data = DataBundle {
            entities: spec.entities,
            queries: spec.queries,
        };
        let vp = Viewport {
            width: 390.0,
            height: 844.0,
            scale,
        };
        let mut s = Session::new("bundle", spec.workspace, data, vp);
        let t = std::time::Instant::now();
        let img = s.render_cpu().expect("render");
        let name = std::path::Path::new(a)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .replace(".bundle", "");
        std::fs::write(
            out.join(format!("{name}.png")),
            construct_render::encode::png(&img),
        )
        .unwrap();
        println!("{name}: {:?}", t.elapsed());
    }
}
