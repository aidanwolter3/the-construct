//! The Tasks wiring (opt-in per collection, decision `agents-runs`): a row
//! moving to `queued` sends `start_run` once, with the row's id as the task
//! id, and the row's `status` then follows its current run through one
//! fixed table ([`task_status`]):
//!
//! | current run | row status |
//! |---|---|
//! | queued | queued |
//! | starting, working | running |
//! | needs_you | needs_you |
//! | ended, outcome done | done |
//! | ended, outcome review; or reported, exited, stopped without one | review |
//! | outcome failed or blocked; or lost, start_failed, timed_out, expired | failed |
//!
//! The current run is the one the row's `run_id` names, else the newest run
//! whose `task_id` is the row's id. When it ends, its outcome (summary and
//! pull request, or why it failed) is appended to `diagnosis`, once.
//!
//! A row you set to `done` (or tick `done`) or `todo` keeps that status:
//! its run only adds to `diagnosis`, and a run that hasn't ended gets
//! `stop_run`, once. Reopen ([`reopen`]) starts a run that resumes the
//! last one's conversation with your message.
//!
//! The collection is linked with the MCP tool `agents_link_tasks`, which
//! adds what the wiring needs (the status options and the `run_id`,
//! `session_id`, `workspace` and `diagnosis` fields) through Collections'
//! own `apply_change`.

use pod_core::Pod;
use pod_core::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use app_collections::{db as cdb, model};

use crate::cap::{self, CapError, Clients, Command, Runner};
use crate::db;
use crate::state::{self as st, Run, State};

/// Pod setting: the linked collection (`{collection, workspace}` as JSON).
pub const TASKS_SETTING: &str = "agents.tasks";

/// The `status` options the wiring sets; `queued` starts a run.
pub const TODO: &str = "todo";
pub const QUEUED: &str = "queued";
pub const RUNNING: &str = "running";
pub const NEEDS_YOU: &str = "needs_you";
pub const REVIEW: &str = "review";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
const NEEDED_OPTIONS: [&str; 7] = [TODO, QUEUED, RUNNING, NEEDS_YOU, REVIEW, DONE, FAILED];
/// The text fields the wiring writes.
const NEEDED_FIELDS: [&str; 4] = ["run_id", "session_id", "workspace", "diagnosis"];
/// The most of `diagnosis` kept (the newest text).
const DIAGNOSIS_MAX: usize = 3_900;
/// The most of a prompt sent.
const PROMPT_MAX: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub collection: String,
    /// The runner workspace a row without its own `workspace` starts in.
    pub workspace: String,
}

/// The linked collection, if any.
pub fn link(store: &Store) -> rusqlite::Result<Option<Link>> {
    Ok(store
        .pod_setting(TASKS_SETTING)?
        .and_then(|v| serde_json::from_str(&v).ok()))
}

fn bad(detail: impl Into<String>) -> Value {
    json!({"error": "invalid_request", "detail": detail.into()})
}

/// `agents_link_tasks {collection, workspace}`: link a collection (adding
/// the options and fields the wiring needs), or unlink with `collection:
/// null`.
pub async fn set_link(pod: &Pod, args: &Value) -> Result<Value, Value> {
    if args.get("collection").is_none_or(Value::is_null) {
        pod.blocking(|s| s.set_pod_setting(TASKS_SETTING, "null"))
            .await
            .map_err(|_| bad("the pod's settings can't be written"))?;
        return Ok(json!({"ok": true, "linked": null}));
    }
    let collection = args["collection"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("\"collection\" is a collection's name, or null to unlink"))?
        .to_owned();
    let workspace = args["workspace"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 100)
        .ok_or_else(|| bad("\"workspace\" is the runner workspace tasks start in"))?
        .to_owned();
    let name = collection.clone();
    let def = pod
        .blocking(move |s| s.with_conn(|c| cdb::latest_def(c, &name)))
        .await
        .map_err(|_| bad("the collections can't be read"))?
        .map(|(_, d)| d)
        .ok_or_else(|| json!({"error": "collection_not_found", "collection": collection}))?;
    if def.field("title").is_none() {
        return Err(bad(format!(
            "'{collection}' has no title field to name tasks by"
        )));
    }
    let Some(status) = def.field("status") else {
        return Err(bad(format!(
            "'{collection}' has no status field: add a choice field status with the option queued"
        )));
    };
    if status.kind != model::Kind::Choice || !status.options.iter().any(|o| o == QUEUED) {
        return Err(bad(format!(
            "'{collection}'.status must be a choice with the option queued"
        )));
    }
    let mut changes = Vec::new();
    let mut options = status.options.clone();
    for o in NEEDED_OPTIONS {
        if !options.iter().any(|x| x == o) {
            options.push(o.to_owned());
        }
    }
    if options != status.options {
        changes.push(json!({"op": "update_field", "field": "status", "options": options}));
    }
    for f in NEEDED_FIELDS {
        if def.field(f).is_none() {
            changes.push(json!({"op": "add_field", "name": f, "type": "text", "optional": true}));
        }
    }
    if !changes.is_empty() {
        let Some((collections, _)) = pod.catalog().source(app_collections::NAME) else {
            return Err(bad("this pod has no Collections source"));
        };
        let args = json!({"collection": collection, "changes": changes,
            "note": "Linked to the agent runner (agents_link_tasks)"});
        let Some(call) = collections.mcp_call(pod, "apply_change", &args) else {
            return Err(bad("Collections can't change definitions here"));
        };
        call.await?;
    }
    let link = Link {
        collection: collection.clone(),
        workspace,
    };
    let v = serde_json::to_string(&link).expect("serializes");
    pod.blocking(move |s| s.set_pod_setting(TASKS_SETTING, &v))
        .await
        .map_err(|_| bad("the pod's settings can't be written"))?;
    Ok(json!({"ok": true, "linked": link, "changes": changes,
        "next": "Rows whose status becomes queued now start a run on the runner."}))
}

/// A row's values by field name.
type Named = Map<String, Value>;

/// Whether a row is marked done: `status = done`, or a `done` box ticked.
fn is_done(row: &Named) -> bool {
    text(row, "status") == DONE || row.get("done") == Some(&Value::Bool(true))
}

/// Whether a row keeps the status you gave it (done or todo): its run only
/// adds to `diagnosis`.
fn is_kept(row: &Named) -> bool {
    is_done(row) || text(row, "status") == TODO
}

fn rows(store: &Store, collection: &str) -> rusqlite::Result<Vec<(String, Named)>> {
    store.with_conn(|c| {
        let Some((_, def)) = cdb::latest_def(c, collection)? else {
            return Ok(Vec::new());
        };
        Ok(cdb::rows(c, &def)?
            .into_iter()
            .map(|r| {
                let e = model::row_entity(&def, &r.id, &r.values, "", "");
                (r.id, e)
            })
            .collect())
    })
}

fn text(v: &Named, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_owned()
}

/// The prompt for a row: its title, then its notes.
fn prompt(row: &Named) -> String {
    let title = text(row, "title");
    let notes = ["notes", "description", "details"]
        .iter()
        .map(|k| text(row, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let p = if notes.is_empty() {
        title
    } else {
        format!("{title}\n\n{notes}")
    };
    p.chars().take(PROMPT_MAX).collect()
}

/// `before` with `more` appended as a new paragraph, keeping the newest
/// [`DIAGNOSIS_MAX`] characters.
pub fn append(before: &str, more: &str) -> String {
    let joined = if before.trim().is_empty() {
        more.to_owned()
    } else {
        format!("{}\n\n{more}", before.trim_end())
    };
    let n = joined.chars().count();
    if n <= DIAGNOSIS_MAX {
        joined
    } else {
        joined.chars().skip(n - DIAGNOSIS_MAX).collect()
    }
}

async fn update(pod: &Pod, collection: &str, id: &str, values: Value) {
    let Value::Object(values) = values else {
        return;
    };
    if let Err(e) = app_collections::update_row(pod, collection, id, values).await {
        tracing::warn!(error = %e["error"], detail = %e["detail"], "a task row could not be updated");
    }
}

/// The task status run `r` maps to: the one table (module docs).
pub fn task_status(r: &Run) -> &'static str {
    match r.state.as_str() {
        st::QUEUED => QUEUED,
        st::STARTING | st::WORKING => RUNNING,
        st::NEEDS_YOU => NEEDS_YOU,
        _ => match (r.outcome.as_str(), r.end_reason.as_str()) {
            ("done", _) => DONE,
            ("review", _) => REVIEW,
            ("failed" | "blocked", _) => FAILED,
            (_, "lost" | "start_failed" | "timed_out" | "expired") => FAILED,
            _ => REVIEW,
        },
    }
}

/// What an ended run adds to its row's `diagnosis`: the outcome's summary
/// and pull request, or why it failed.
pub fn end_note(r: &Run) -> String {
    let (word, _) = crate::bundle::run_look(&r.state, &r.end_reason, &r.outcome);
    let summary = if r.outcome_summary.is_empty() {
        r.summary.as_str()
    } else {
        r.outcome_summary.as_str()
    };
    let mut note = match (r.outcome.as_str(), task_status(r)) {
        ("done" | "review", _) if !summary.is_empty() => summary.to_owned(),
        ("done" | "review", _) => format!("{word}."),
        (_, FAILED) => {
            let why = if !r.outcome.is_empty() && !summary.is_empty() {
                summary.to_owned()
            } else if !r.error.is_empty() {
                r.error.clone()
            } else {
                crate::alerts::end_reason_text(&r.end_reason).to_owned()
            };
            format!("{word}: {why}")
        }
        _ if !summary.is_empty() => summary.to_owned(),
        _ => crate::alerts::end_reason_text(&r.end_reason).to_owned(),
    };
    if !r.pr().is_empty() {
        note.push_str(&format!("\nPR: {}", r.pr()));
    }
    note
}

/// A refused `start_run`'s reason, when the runner refused it for good
/// (400, 404, 409); `None` when it may work later.
fn refusal(e: &CapError) -> Option<String> {
    match e {
        CapError::Status(400 | 404 | 409, d) if d.is_empty() => {
            Some("the runner refused the task".to_owned())
        }
        CapError::Status(400 | 404 | 409, d) => Some(d.clone()),
        _ => None,
    }
}

/// One pass over the linked collection: send `start_run` for queued rows
/// that need one, then set each row from its current run, note ended runs
/// in `diagnosis` and stop the runs of rows you set to done or todo.
pub async fn sync(pod: &Pod, clients: &Clients, runner: &Runner, state: &State) {
    let Ok(Some(link)) = pod.blocking(link).await else {
        return;
    };
    let coll = link.collection.clone();
    let Ok((rows, launches)) = pod
        .blocking(move |s| Ok((rows(s, &coll)?, db::launches(s, &coll)?)))
        .await
    else {
        return;
    };
    let launch_of = |id: &str| {
        launches
            .iter()
            .find(|(t, _, _)| t == id)
            .map(|(_, r, _)| r.clone())
    };
    // Sends are forgotten once their row moves on.
    for (id, _, _) in &launches {
        if !rows
            .iter()
            .any(|(r, v)| r == id && text(v, "status") == QUEUED)
        {
            let id = id.clone();
            let _ = pod.blocking(move |s| db::remove_launch(s, &id)).await;
        }
    }
    let mut sending = true;
    for (id, row) in rows {
        let launched = launch_of(&id);
        let named = text(&row, "run_id");
        let run_id = if named.is_empty() {
            launched.clone().unwrap_or_default()
        } else {
            named.clone()
        };
        let queued = text(&row, "status") == QUEUED && !is_done(&row);
        // A queued row needs a run unless it has the one this pod started
        // for it; a row queued again after its run ended (a retry) gets a
        // new one.
        let ours = launched.as_deref() == Some(run_id.as_str()) && launched.is_some();
        let needs_run = queued
            && !ours
            && (run_id.is_empty() || state.runs.get(&run_id).is_none_or(|r| !r.is_active()));
        if needs_run {
            if sending {
                sending = start(pod, clients, runner, &link, &id, &row, None, None).await;
            }
            continue;
        }
        let Some(run) = state.current_run(&id, &run_id) else {
            continue;
        };
        follow(pod, clients, runner, &link.collection, &id, &row, run).await;
    }
}

/// Send `start_run` for row `id` (`resume_from` and `prompt` for Reopen),
/// remember the run it answers and put it in the row. Whether the runner
/// answered (a refusal for good fails the row).
#[allow(clippy::too_many_arguments)]
async fn start(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    link: &Link,
    id: &str,
    row: &Named,
    resume_from: Option<String>,
    message: Option<String>,
) -> bool {
    match start_run(pod, clients, runner, link, id, row, resume_from, message).await {
        Ok(_) => true,
        Err(e) => match refusal(&e) {
            Some(reason) => {
                let diagnosis = append(
                    &text(row, "diagnosis"),
                    &format!("Couldn't start: {reason}"),
                );
                update(
                    pod,
                    &link.collection,
                    id,
                    json!({"status": FAILED, "diagnosis": diagnosis}),
                )
                .await;
                true
            }
            // The runner is away: try again on the next pass.
            None => {
                tracing::info!(error = %e, "a queued task waits for the runner");
                false
            }
        },
    }
}

/// `start_run` for row `id`; the new run's id, remembered and put in the
/// row (with `status = queued`).
#[allow(clippy::too_many_arguments)]
async fn start_run(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    link: &Link,
    id: &str,
    row: &Named,
    resume_from: Option<String>,
    message: Option<String>,
) -> Result<String, CapError> {
    let ws = text(row, "workspace");
    let cmd = Command::StartRun {
        workspace: if ws.is_empty() {
            link.workspace.clone()
        } else {
            ws
        },
        task_id: id.to_owned(),
        title: text(row, "title").chars().take(200).collect(),
        prompt: message.unwrap_or_else(|| prompt(row)),
        resume_from,
    };
    let answer = cap::command(clients, runner, &cmd).await?;
    let run_id = cap::run_id_of(&answer).unwrap_or_default();
    let (coll, task, run, now) = (
        link.collection.clone(),
        id.to_owned(),
        run_id.clone(),
        pod_core::util::now_secs(),
    );
    // Remembered before the row changes: the change wakes another pass.
    let _ = pod
        .blocking(move |s| db::add_launch(s, &coll, &task, &run, now))
        .await;
    update(
        pod,
        &link.collection,
        id,
        json!({"status": QUEUED, "run_id": run_id}),
    )
    .await;
    Ok(run_id)
}

/// Set row `id` from its current run `run`.
async fn follow(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    collection: &str,
    id: &str,
    row: &Named,
    run: &Run,
) {
    let mut values = Map::new();
    if text(row, "run_id") != run.id {
        values.insert("run_id".into(), json!(run.id));
    }
    if !run.session_id.is_empty() && text(row, "session_id") != run.session_id {
        values.insert("session_id".into(), json!(run.session_id));
    }
    let kept = is_kept(row);
    if !kept && text(row, "status") != task_status(run) {
        values.insert("status".into(), json!(task_status(run)));
    }
    let noted = format!("noted:{}", run.id);
    let note = !run.is_active() && {
        let k = noted.clone();
        !pod.blocking(move |s| db::marked(s, &k))
            .await
            .unwrap_or(true)
    };
    if note {
        values.insert(
            "diagnosis".into(),
            json!(append(&text(row, "diagnosis"), &end_note(run))),
        );
    }
    if !values.is_empty() {
        let ok = app_collections::update_row(pod, collection, id, values).await;
        match ok {
            Ok(_) if note => {
                let now = pod_core::util::now_secs();
                let _ = pod.blocking(move |s| db::mark(s, &noted, now)).await;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e["error"], detail = %e["detail"], "a task row could not be updated");
            }
        }
    }
    // Set to done or todo while its run goes on: stop the run, once.
    if kept && run.is_active() {
        let key = format!("stop:{}", run.id);
        let k = key.clone();
        if pod
            .blocking(move |s| db::marked(s, &k))
            .await
            .unwrap_or(true)
        {
            return;
        }
        let cmd = Command::StopRun {
            run_id: run.id.clone(),
        };
        match cap::command(clients, runner, &cmd).await {
            Ok(_) | Err(CapError::Status(400 | 404 | 409, _)) => {
                let now = pod_core::util::now_secs();
                let _ = pod.blocking(move |s| db::mark(s, &key, now)).await;
            }
            Err(e) => tracing::info!(error = %e, "a task's run waits to be stopped"),
        }
    }
}

/// Run `run` was marked reviewed: its row, when it is a linked task's and
/// this is the row's current run, goes from `review` to `done`. Whether it
/// did.
pub async fn reviewed(pod: &Pod, state: &State, run: &Run) -> bool {
    if run.task_id.is_empty() || st::schedule_of_task(&run.task_id).is_some() {
        return false;
    }
    let Some(link) = pod.blocking(link).await.ok().flatten() else {
        return false;
    };
    let (coll, rid) = (link.collection.clone(), run.task_id.clone());
    let Some(row) = pod
        .blocking(move |s| Ok(rows(s, &coll)?.into_iter().find(|(r, _)| *r == rid)))
        .await
        .ok()
        .flatten()
        .map(|(_, r)| r)
    else {
        return false;
    };
    let current = state.current_run(&run.task_id, &text(&row, "run_id"));
    if current.is_none_or(|c| c.id != run.id) || text(&row, "status") != REVIEW {
        return false;
    }
    let mut values = Map::new();
    values.insert("status".into(), json!(DONE));
    match app_collections::update_row(pod, &link.collection, &run.task_id, values).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e["error"], detail = %e["detail"], "a reviewed task row could not be set done");
            false
        }
    }
}

/// Why Reopen was refused: the status, error and detail for the route.
pub type Refusal = (u16, &'static str, String);

/// Reopen row `id` with `message`: its last run must have ended; a new run
/// resumes that run's conversation (`resume_from`) with `message` as the
/// next prompt, and the row goes back to `queued`. The new run's id.
pub async fn reopen(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    state: &State,
    id: &str,
    message: &str,
) -> Result<String, Refusal> {
    let link = pod.blocking(link).await.ok().flatten().ok_or((
        409,
        "not_linked",
        "no tasks collection is linked to the runner".to_owned(),
    ))?;
    let (coll, rid) = (link.collection.clone(), id.to_owned());
    let row = pod
        .blocking(move |s| Ok(rows(s, &coll)?.into_iter().find(|(r, _)| *r == rid)))
        .await
        .ok()
        .flatten()
        .map(|(_, r)| r)
        .ok_or((
            404,
            "not_found",
            format!("'{}' has no row {id}", link.collection),
        ))?;
    let named = text(&row, "run_id");
    let last = state.current_run(id, &named);
    let last_id = match (last, named.is_empty()) {
        (Some(r), _) if r.is_active() => {
            return Err((
                409,
                "run_not_ended",
                "the task's run hasn't ended; answer it in the Claude app or stop it first".into(),
            ));
        }
        (Some(r), _) => r.id.clone(),
        // A run the pod no longer lists: the runner says whether it ended.
        (None, false) => named,
        (None, true) => {
            return Err((409, "no_run", "the task has no run to reopen".into()));
        }
    };
    start_run(
        pod,
        clients,
        runner,
        &link,
        id,
        &row,
        Some(last_id),
        Some(message.to_owned()),
    )
    .await
    .map_err(|e| match e {
        CapError::Status(s @ (400 | 404 | 409), d) => (
            s,
            "runner_refused",
            if d.is_empty() {
                "the runner refused to reopen it".into()
            } else {
                d
            },
        ),
        CapError::Unauthorized => (
            502,
            "runner_refused_token",
            "the runner refused the pod's token; sign in to the runner again".into(),
        ),
        other => (502, "runner_unreachable", other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnosis_keeps_the_newest_text() {
        assert_eq!(append("", "a"), "a");
        assert_eq!(append("a\n", "b"), "a\n\nb");
        let long = "x".repeat(DIAGNOSIS_MAX);
        let out = append(&long, "end");
        assert_eq!(out.chars().count(), DIAGNOSIS_MAX);
        assert!(out.ends_with("\n\nend"));
    }

    fn run(state: &str, end: &str, outcome: &str) -> Run {
        Run {
            id: "run_1".into(),
            state: state.into(),
            end_reason: end.into(),
            outcome: outcome.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_mapping_table() {
        for (state, end, outcome, want) in [
            ("queued", "", "", QUEUED),
            ("starting", "", "", RUNNING),
            ("working", "", "", RUNNING),
            ("needs_you", "", "", NEEDS_YOU),
            // The outcome survives the run ending, whatever ended it.
            ("ended", "reported", "done", DONE),
            ("ended", "lost", "done", DONE),
            ("ended", "reported", "review", REVIEW),
            ("ended", "reported", "", REVIEW),
            ("ended", "exited", "", REVIEW),
            ("ended", "stopped", "", REVIEW),
            ("ended", "reported", "failed", FAILED),
            ("ended", "reported", "blocked", FAILED),
            ("ended", "lost", "", FAILED),
            ("ended", "start_failed", "", FAILED),
            ("ended", "timed_out", "", FAILED),
            ("ended", "expired", "", FAILED),
        ] {
            assert_eq!(
                task_status(&run(state, end, outcome)),
                want,
                "{state} {end} {outcome}"
            );
        }
    }

    #[test]
    fn ended_runs_note_why() {
        let mut r = run("ended", "reported", "review");
        r.outcome_summary = "Opened a PR.".into();
        r.outcome_pr_url = "https://github.com/o/r/pull/2".into();
        assert_eq!(
            end_note(&r),
            "Opened a PR.\nPR: https://github.com/o/r/pull/2"
        );
        let mut r = run("ended", "reported", "blocked");
        r.outcome_summary = "Needs a key.".into();
        assert_eq!(end_note(&r), "Blocked: Needs a key.");
        let r = run("ended", "lost", "");
        assert_eq!(end_note(&r), "Lost: The run was lost: its process is gone.");
        let mut r = run("ended", "start_failed", "");
        r.error = "The sandbox didn't wake".into();
        assert_eq!(end_note(&r), "Didn't start: The sandbox didn't wake");
        assert_eq!(end_note(&run("ended", "reported", "done")), "Done.");
    }

    #[test]
    fn a_prompt_is_the_title_and_notes() {
        let row: Named =
            serde_json::from_value(json!({"title": "Fix login", "notes": "It 500s"})).unwrap();
        assert_eq!(prompt(&row), "Fix login\n\nIt 500s");
    }
}
