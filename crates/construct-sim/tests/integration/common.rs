use std::path::{Path, PathBuf};
use std::time::Duration;

use construct_sim::fixture::{FixturePod, repo_root};
use construct_sim::run::{Options, Runner, Source, StepResult};
use construct_sim::screen::Screen;
use construct_sim::script;

/// A fresh output directory under the system's temp folder.
pub fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("construct-sim-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

pub fn root() -> PathBuf {
    repo_root(Path::new(env!("CARGO_MANIFEST_DIR"))).expect("the repository")
}

/// A fixture-mode run of `script` on `workspace` (phone screen).
pub fn run(workspace: &str, script: &str, out: &Path) -> (Runner, Vec<StepResult>) {
    let pod = FixturePod::new(&root()).expect("fixtures");
    let mut runner = Runner::new(
        Source::Fixture { pod, bundle: None },
        Options {
            workspace: workspace.into(),
            screen: Screen::parse("phone").unwrap(),
            density: Some(1.0),
            out: out.to_path_buf(),
            timeout: Duration::from_secs(10),
        },
    )
    .expect("a session");
    let lines = script::parse(script).expect("a script");
    let results = runner.run(&lines);
    (runner, results)
}

/// Panics with the step log unless every step passed.
pub fn assert_ok(results: &[StepResult]) {
    if !results.iter().all(|r| r.ok) {
        let log: Vec<String> = results.iter().map(StepResult::log_line).collect();
        panic!("a step failed:\n{}", log.join("\n"));
    }
}
