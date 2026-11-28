//! The newcomer test (decision `newcomer-journeys`): for each persona, a new
//! empty pod, an agent with nothing but that pod's MCP tools, a simulated
//! user sending the persona's wishes, then checks that ask the pod, not the
//! agent: the simulator at phone size, what was rebuilt, and a reviewer
//! reading the screenshots.
//!
//! ```text
//! newcomer --out <dir> [--base-url <pod server>] [--only a,b] [--personas <dir>]
//!          [--sim <construct-sim binary>] [--label <X-Construct-Client>]
//! ```
//!
//! Writes `<out>/report.json`, `<out>/report.md` and a folder per persona
//! (transcript, screenshots, final layouts, the reviewer's answers). Pod
//! tokens stay in memory.

mod claude;
mod measure;
mod persona;
mod pod;
mod report;

use persona::{Persona, Tier};
use pod::Pod;
use report::{Answer, Outcome, PersonaReport, Report};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const STAGING: &str = "https://the-construct-pod.fly.dev";

struct Args {
    out: PathBuf,
    base_url: String,
    only: Vec<String>,
    personas: PathBuf,
    sim: PathBuf,
    label: String,
}

fn args() -> Result<Args, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let mut a = Args {
        out: PathBuf::new(),
        base_url: STAGING.into(),
        only: Vec::new(),
        personas: Path::new(env!("CARGO_MANIFEST_DIR")).join("personas"),
        sim: exe_dir.join("construct-sim"),
        label: format!(
            "newcomer-test session {}",
            std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_else(|_| "manual".into())
        ),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--out" => a.out = value()?.into(),
            "--base-url" => a.base_url = value()?,
            "--only" => a.only = value()?.split(',').map(str::to_owned).collect(),
            "--personas" => a.personas = value()?.into(),
            "--sim" => a.sim = value()?.into(),
            "--label" => a.label = value()?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    if a.out.as_os_str().is_empty() {
        return Err("--out <dir> is required".into());
    }
    Ok(a)
}

fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("newcomer: {e}");
            return ExitCode::from(2);
        }
    };
    let mut personas = match load_personas(&a.personas) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("newcomer: {e}");
            return ExitCode::from(2);
        }
    };
    if !a.only.is_empty() {
        // A persona that starts from another runs with it.
        let mut want = a.only.clone();
        for p in &personas {
            if want.contains(&p.id)
                && let Some(s) = &p.starts_from
                && !want.contains(s)
            {
                want.push(s.clone());
            }
        }
        personas.retain(|p| want.contains(&p.id));
    }
    if let Err(e) = std::fs::create_dir_all(&a.out) {
        eprintln!("newcomer: {}: {e}", a.out.display());
        return ExitCode::from(2);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut pods: HashMap<String, Pod> = HashMap::new();
    let mut reports = Vec::new();
    for p in &personas {
        eprintln!("newcomer: {} ...", p.id);
        let r = run_persona(&a, p, &mut pods);
        eprintln!("newcomer: {}: {:?} {}", p.id, r.result, r.reason);
        reports.push(r);
    }
    drop(pods); // deletes the temporary pods

    let report = Report {
        date: report::date(now),
        base_url: a.base_url.clone(),
        personas: reports,
    };
    let json = serde_json::to_string_pretty(&report).unwrap_or_default();
    let written = std::fs::write(a.out.join("report.json"), json)
        .and_then(|_| std::fs::write(a.out.join("report.md"), report.markdown()));
    if let Err(e) = written {
        eprintln!("newcomer: cannot write the report: {e}");
        return ExitCode::from(2);
    }
    println!("{}", a.out.join("report.md").display());
    ExitCode::SUCCESS
}

/// The personas, those that start from another after it.
fn load_personas(dir: &Path) -> Result<Vec<Persona>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    paths.sort();
    let mut all = paths
        .iter()
        .map(|p| Persona::load(p))
        .collect::<Result<Vec<_>, _>>()?;
    all.sort_by_key(|p| p.starts_from.is_some());
    Ok(all)
}

fn run_persona(a: &Args, p: &Persona, pods: &mut HashMap<String, Pod>) -> PersonaReport {
    let mut r = PersonaReport::new(&p.id, p.tier);
    if p.tier == Tier::Fresh {
        r.result = Outcome::Skipped;
        r.reason = "fresh tier: needs a new admin pod, which this run doesn't make".into();
        return r;
    }
    let dir = a.out.join(&p.id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        r.reason = format!("{}: {e}", dir.display());
        return r;
    }

    // The pod: a new one, or the one the persona starts from.
    let pod = match &p.starts_from {
        Some(from) => match pods.remove(from) {
            Some(pod) => pod,
            None => {
                r.reason = format!("{from} has no pod to start from");
                return r;
            }
        },
        None => match Pod::temporary(&a.base_url, &a.label) {
            Ok(pod) => pod,
            Err(pod::Unavailable(e)) => {
                r.reason = e;
                return r;
            }
        },
    };
    r.pod = pod.name.clone();

    let stuck = converse(p, &pod, &dir, &mut r);
    if r.result == Outcome::Broken && !r.reason.is_empty() {
        pods.insert(p.id.clone(), pod);
        return r;
    }
    check(a, p, &pod, &dir, &mut r);
    r.decide(stuck);
    pods.insert(p.id.clone(), pod);
    r
}

/// The conversation: the persona's wishes in order, the agent's questions
/// answered from "If asked". `Some(why)` when it got stuck; a broken run
/// sets `r.reason` with the result left `Broken`.
fn converse(p: &Persona, pod: &Pod, dir: &Path, r: &mut PersonaReport) -> Option<String> {
    // An empty directory outside any checkout, so no CLAUDE.md reaches the
    // agent.
    let work = std::env::temp_dir().join(format!("newcomer-{}-{}", p.id, std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    if let Err(e) = std::fs::create_dir_all(&work) {
        r.reason = format!("{}: {e}", work.display());
        return None;
    }
    let mut transcript = match std::fs::File::create(dir.join("transcript.jsonl")) {
        Ok(f) => f,
        Err(e) => {
            r.reason = e.to_string();
            return None;
        }
    };
    let mcp = claude::mcp_config(&pod.base_url, pod.token());
    let mut wishes = p.says.iter();
    let mut message = wishes.next().cloned();
    let mut session: Option<String> = None;
    let mut calls = Vec::new();
    let mut wall_ms = 0u64;
    let mut stuck = None;

    while let Some(m) = message.take() {
        if r.turns >= p.max_turns {
            stuck = Some(format!("{} user messages used", p.max_turns));
            break;
        }
        r.turns += 1;
        let budget = p.max_budget_usd.map(|b| (b - r.cost_usd).max(0.01));
        let turn = match claude::agent_turn(
            &work,
            &mcp,
            &m,
            session.as_deref(),
            budget,
            &mut transcript,
        ) {
            Ok(t) => t,
            Err(e) => {
                r.reason = e;
                return None;
            }
        };
        wall_ms += turn.wall_ms;
        r.cost_usd += turn.cost_usd;
        r.model = turn.model.clone();
        r.claude_version = turn.version.clone();
        r.last_reply = turn.reply.clone();
        session = Some(turn.session_id.clone());
        calls.extend(turn.calls);
        if turn.is_error {
            stuck = Some(format!(
                "the agent's turn ended in an error: {}",
                turn.reply.lines().next().unwrap_or("")
            ));
            break;
        }
        message = match answer_question(p, &turn.reply, &work) {
            Some(answer) => Some(answer),
            None => wishes.next().cloned(),
        };
    }
    let _ = std::fs::remove_dir_all(&work);

    let m = measure::calls(&calls);
    r.tool_calls = m.tool_calls;
    r.refused = m.refused;
    r.gaps = m.gaps;
    r.rebuilding_writes = m.rebuilding_writes;
    r.rebuilding_secs = m.rebuilding_ms as f64 / 1000.0;
    r.agent_secs = wall_ms as f64 / 1000.0;
    r.result = Outcome::Fail; // decided after the checks
    stuck
}

/// The simulated user: when the agent's reply waits on an answer, the
/// answer, in the persona's words, from "If asked".
fn answer_question(p: &Persona, reply: &str, dir: &Path) -> Option<String> {
    let system = "You play a person setting up an app on their phone by talking to an \
        assistant. You are not technical. Answer only with one JSON object.";
    let facts = if p.if_asked.is_empty() {
        "(none)".to_owned()
    } else {
        p.if_asked.join("\n")
    };
    let prompt = format!(
        "Who you are: {}\n\nFacts you know:\n{facts}\n\nThe assistant just said:\n<<<\n{reply}\n>>>\n\n\
         Is the assistant waiting for an answer from you before it can do what you asked (a \
         question it needs answered, or a choice it won't make without you)? An offer of \
         something extra (\"I can also ...\", \"if you want ...\") is not a question.\n\
         If it is waiting, reply {{\"waiting\": true, \"say\": \"<a short answer in your own words, \
         from the facts; if they don't cover it, say you don't mind and it should pick>\"}}.\n\
         Otherwise reply {{\"waiting\": false}}.",
        p.who
    );
    let v = claude::ask(system, &prompt, false, dir)
        .ok()
        .and_then(|t| claude::json_in(&t))?;
    if v["waiting"].as_bool() == Some(true) {
        v["say"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    } else {
        None
    }
}

/// The checks, which ask the pod, never the agent.
fn check(a: &Args, p: &Persona, pod: &Pod, dir: &Path, r: &mut PersonaReport) {
    // The simulator, live on the pod at phone size.
    let shots = dir.join("shots");
    let out = Command::new(&a.sim)
        .args(["--base-url", &pod.base_url, "--screen", "phone"])
        .args(["--timeout-ms", "30000", "--script", &p.sim])
        .arg("--out")
        .arg(&shots)
        .env("CONSTRUCT_POD_TOKEN", pod.token())
        .output();
    match out {
        Ok(o) => {
            r.sim_passed = o.status.success();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            r.sim_output = tail_lines(&text, 20);
        }
        Err(e) => r.sim_output = format!("cannot run {}: {e}", a.sim.display()),
    }
    let mut pngs: Vec<PathBuf> = std::fs::read_dir(&shots)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    pngs.sort();
    r.screenshots = pngs.iter().map(|p| p.display().to_string()).collect();

    // What it built, and what it rebuilt.
    let built = match measure::Built::read(pod) {
        Ok(b) => b,
        Err(e) => {
            r.sim_passed = false;
            r.sim_output = format!("cannot read the pod's layouts: {e}");
            return;
        }
    };
    r.unreached = built.unreached();
    r.shipped_rebuilt = measure::shipped_rebuilt(&p.uses, &built);
    r.hints_left = measure::hints_left(pod, &built);
    let layouts = dir.join("layouts.json");
    let _ = std::fs::write(
        &layouts,
        serde_json::to_string_pretty(&built).unwrap_or_default(),
    );
    if r.sim_passed {
        r.time_to_working_secs = Some(r.agent_secs);
    }

    // The reviewer: the screenshots against the judge lines, never the
    // agent's transcript.
    if !p.judge.is_empty() {
        r.judge = review(p, &pngs, &layouts, dir);
        let _ = std::fs::write(
            dir.join("review.json"),
            serde_json::to_string_pretty(&r.judge).unwrap_or_default(),
        );
    }
}

fn review(p: &Persona, pngs: &[PathBuf], layouts: &Path, dir: &Path) -> Vec<Answer> {
    let system = "You review a phone app that an assistant set up for a person. You judge \
        only what the screenshots and layouts show, strictly, as that person would. Answer \
        only with one JSON object.";
    let shots: Vec<String> = pngs.iter().map(|p| p.display().to_string()).collect();
    let lines: Vec<String> = p.judge.iter().map(|j| format!("- {j}")).collect();
    let prompt = format!(
        "The person: {}\n\nWhat they asked for, in order:\n{}\n\nScreenshots, in the order they \
         were taken (read each one): \n{}\n\nThe layouts the pod serves (read if a screenshot \
         leaves a line unclear): {}\n\nAnswer each question yes or no, with one sentence why:\n{}\n\n\
         Reply {{\"answers\": [{{\"line\": \"<the question>\", \"yes\": true|false, \"why\": \"...\"}}]}}.",
        p.who,
        p.says.join("\n"),
        if shots.is_empty() {
            "(none were taken)".to_owned()
        } else {
            shots.join("\n")
        },
        layouts.display(),
        lines.join("\n"),
    );
    let answer = claude::ask(system, &prompt, true, dir)
        .map_err(|e| e.to_string())
        .and_then(|t| claude::json_in(&t).ok_or_else(|| format!("no JSON in: {t}")));
    match answer {
        Ok(v) => {
            let got: Vec<Answer> = v["answers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| Answer {
                    line: x["line"].as_str().unwrap_or_default().to_owned(),
                    yes: x["yes"].as_bool().unwrap_or(false),
                    why: x["why"].as_str().unwrap_or_default().to_owned(),
                })
                .collect();
            if got.len() == p.judge.len() {
                got
            } else {
                unanswered(
                    p,
                    &format!(
                        "the reviewer answered {} of {} lines",
                        got.len(),
                        p.judge.len()
                    ),
                )
            }
        }
        Err(e) => unanswered(p, &format!("the review failed: {e}")),
    }
}

fn unanswered(p: &Persona, why: &str) -> Vec<Answer> {
    p.judge
        .iter()
        .map(|j| Answer {
            line: j.clone(),
            yes: false,
            why: why.to_owned(),
        })
        .collect()
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
