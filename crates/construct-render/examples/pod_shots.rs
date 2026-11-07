//! `pod_shots <pod url> <out dir> <script>`: a live session on a pod
//! (CONSTRUCT_POD_TOKEN: a pod code or session token), driven by a script and
//! drawn on the CPU at 390 × 844 @3, as the simulator's phone, to compare
//! with `:desktopApp:simulate -PbaseUrl`. Script steps, `;`-separated:
//! `open <layout>`, `present <layout> <Sheet|Panel|Overlay> [edge]`, `tap <text or id>`, `type <field> <text>`, `scroll <dy>`,
//! `back`, `wait <ms>` (images keep loading meanwhile), `shot <name>`.

use construct_render::Viewport;
use construct_render::interact::Target;
use construct_render::remote::PodClient;
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url, out, script] = &args[..] else {
        eprintln!("usage: pod_shots <pod url> <out dir> <script>");
        std::process::exit(2);
    };
    let token = std::env::var("CONSTRUCT_POD_TOKEN").expect("CONSTRUCT_POD_TOKEN");
    let vp = Viewport {
        width: 390.0,
        height: 844.0,
        scale: 3.0,
    };
    let mut s = PodClient::connect(url, &token)
        .and_then(|c| c.session("home", vp))
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir_all(out).unwrap();
    for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (cmd, arg) = step.split_once(' ').unwrap_or((step, ""));
        let effects = match cmd {
            "open" => s
                .dispatch(&json!({ "intent": "action.navigation.open", "layout": arg, "presentation": "Push" }))
                .map(|o| o.effects),
            "present" => {
                // `present <layout> <Sheet|Panel|Overlay> [edge]`.
                let mut it = arg.split(' ');
                let (layout, how, edge) = (it.next().unwrap_or(""), it.next().unwrap_or("Sheet"), it.next());
                s.dispatch(&json!({ "intent": "action.navigation.open", "layout": layout,
                    "presentation": how, "edge": edge }))
                    .map(|o| o.effects)
            }
            "tap" => s
                .tap(&Target::Id(arg.into()))
                .or_else(|_| s.tap(&Target::Text(arg.into())))
                .map(|o| o.effects),
            "type" => {
                let (field, text) = arg.split_once(' ').unwrap_or((arg, ""));
                Ok(s.type_text(field, text).effects)
            }
            "scroll" => s.scroll_by(None, arg.parse().unwrap_or(400.0)).map(|o| o.effects),
            "back" => s
                .dispatch(&json!({ "intent": "action.navigation.back" }))
                .map(|o| o.effects),
            "wait" => {
                // Draw once so the frame's images are asked for, then wait.
                let _ = s.render_cpu();
                std::thread::sleep(std::time::Duration::from_millis(arg.parse().unwrap_or(1000)));
                Ok(Vec::new())
            }
            "shot" => {
                s.settle(std::time::Duration::from_secs(10));
                let img = s.render_cpu().expect("render");
                let path = format!("{out}/{arg}.png");
                std::fs::write(&path, construct_render::encode::png(&img)).unwrap();
                println!("{step}: {path}");
                Ok(Vec::new())
            }
            other => panic!("unknown step {other}"),
        };
        match effects {
            Ok(e) if !e.is_empty() => println!("{step}: {}", serde_json::to_string(&e).unwrap()),
            Ok(_) => {}
            Err(e) => println!("{step}: FAILED {e}"),
        }
    }
}
