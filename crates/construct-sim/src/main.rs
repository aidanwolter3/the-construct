//! `construct-sim`: runs a simulator script and writes PNGs (docs/simulator.md).
//!
//! ```text
//! construct-sim --workspace pixelfed_sample_no_login --screen phone --out /tmp/sim \
//!   --script "shot feed; click Like; assert-text Unlike; shot liked"
//! ```
//!
//! Exit codes: 0 every step passed, 1 a step failed, 2 bad flags or script.

use std::path::{Path, PathBuf};
use std::time::Duration;

use construct_sim::fixture::{FixturePod, repo_root};
use construct_sim::run::{Options, Runner, Source};
use construct_sim::screen::Screen;
use construct_sim::script;

const USAGE: &str =
    "usage: construct-sim [--workspace <id>] [--screen phone|tablet|desktop|WxH] [--density <n>]
  [--script <script> | --script-file <file>] [--fixture <bundle.json>] [--base-url <pod>]
  [--timeout-ms <ms>] [--out <dir>]
Live mode (--base-url) reads the pod's session token or pod code from CONSTRUCT_POD_TOKEN
(unset: the pod's sign-in screen). See docs/simulator.md.";

const FLAGS: &[&str] = &[
    "--workspace",
    "--screen",
    "--density",
    "--script",
    "--script-file",
    "--fixture",
    "--base-url",
    "--timeout-ms",
    "--out",
];

fn usage(msg: &str) -> ! {
    eprintln!("construct-sim: {msg}\n{USAGE}");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let mut flags = std::collections::BTreeMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (name, value) = match a.split_once('=') {
            Some((n, v)) => (n.to_owned(), v.to_owned()),
            None => (
                a.clone(),
                it.next()
                    .cloned()
                    .unwrap_or_else(|| usage(&format!("{a} needs a value"))),
            ),
        };
        if !FLAGS.contains(&name.as_str()) {
            usage(&format!("unknown flag {name}"));
        }
        flags.insert(name, value);
    }
    let flag = |n: &str| flags.get(n).cloned();

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = repo_root(&cwd);
    // Relative paths resolve against the repository root, as the Compose
    // simulator's do.
    let resolve = |p: String| -> PathBuf {
        let p = PathBuf::from(p);
        match (&root, p.is_relative()) {
            (Some(r), true) if !cwd.join(&p).exists() => r.join(p),
            _ => cwd.join(p),
        }
    };

    let script_text = match (flag("--script"), flag("--script-file")) {
        (Some(_), Some(_)) => usage("pass --script or --script-file, not both"),
        (Some(s), None) => s,
        (None, Some(f)) => std::fs::read_to_string(resolve(f.clone()))
            .unwrap_or_else(|e| usage(&format!("{f}: {e}"))),
        (None, None) => "shot initial".into(),
    };
    let lines = script::parse(&script_text).unwrap_or_else(|e| usage(&e));
    let screen = Screen::parse(&flag("--screen").unwrap_or_else(|| "phone".into()))
        .unwrap_or_else(|e| usage(&e));
    let density = flag("--density").map(|d| {
        d.parse::<f32>()
            .ok()
            .filter(|d| *d > 0.0 && *d <= 8.0)
            .unwrap_or_else(|| usage("--density needs a number in (0, 8]"))
    });
    let timeout = Duration::from_millis(
        flag("--timeout-ms")
            .map(|t| {
                t.parse()
                    .unwrap_or_else(|_| usage("--timeout-ms needs a number"))
            })
            .unwrap_or(10_000),
    );
    let live = flag("--base-url");
    let workspace = flag("--workspace")
        .unwrap_or_else(|| if live.is_some() { "home" } else { "pixelfed" }.to_owned());
    let out = flag("--out").map(resolve).unwrap_or_else(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        root.as_deref()
            .unwrap_or(Path::new("."))
            .join(format!("target/sim/{secs}"))
    });

    let source = match live {
        Some(base) => {
            if flags.contains_key("--fixture") {
                usage("--fixture is for fixture mode; drop --base-url");
            }
            if let Some(l) = lines.iter().find(|l| l.step.fixture_only()) {
                usage(&format!("'{}' runs in fixture mode only", l.command));
            }
            Source::Live {
                base,
                token: std::env::var("CONSTRUCT_POD_TOKEN").unwrap_or_default(),
            }
        }
        None => {
            let Some(root) = &root else {
                usage("run from inside the repository (crates/construct-sim/data not found)");
            };
            Source::Fixture {
                pod: FixturePod::new(root).unwrap_or_else(|e| usage(&e)),
                bundle: flag("--fixture").map(resolve),
            }
        }
    };
    let opts = Options {
        workspace,
        screen,
        density,
        out: out.clone(),
        timeout,
    };
    let mut runner = match Runner::new(source, opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("construct-sim: {e}");
            std::process::exit(2);
        }
    };
    let results = runner.run(&lines);
    println!("output: {}", out.display());
    if !results.iter().all(|r| r.ok) {
        std::process::exit(1);
    }
}
