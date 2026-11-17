//! The Tasks wiring (opt-in per collection, decisions `agents-runs`,
//! `agents-cap-v3`): a row moving to `queued` sends `start_run` once, with
//! the row's id as the work item, and the row's `status` then follows its
//! current run through one fixed table ([`task_status`]):
//!
//! | current run | row status |
//! |---|---|
//! | queued, launching | queued |
//! | working, stalled, interrupted, stopping, closing | running |
//! | needs_you, parked | needs_you |
//! | ended, outcome pending (or not settled yet) | waiting, `stage` ci, review or deploy |
//! | outcome done | done |
//! | outcome failed; or ended lost, start_failed, timed_out, expired | failed |
//! | outcome needs_review | review |
//!
//! The current run is the one the row's `run_id` names, else the newest run
//! whose `work_item_id` is the row's id. When its outcome settles, what it
//! did (summary and pull request, or why it failed) is appended to
//! `diagnosis`, once.
//!
//! **Origin** (issue #243): only a row you (or Claude) queued starts a run.
//! A row a filler made, or whose status, title, notes, workspace, model or
//! runtime a filler or a model wrote last (Collections records who wrote
//! each field: `collections_field_writers`), is failed with the reason
//! instead; setting those fields yourself makes it startable again.
//!
//! A row you set to `done` (or tick `done`) or `todo` keeps that status:
//! its run only adds to `diagnosis`, and a run that hasn't ended gets
//! `stop_run`, once. Reopen ([`reopen`]) starts a run that resumes the
//! last one's conversation with your message (`resume_from`).
//!
//! The collection is linked with the MCP tool `agents_link_tasks`, which
//! adds what the wiring needs (the status options and the `run_id`,
//! `session_id`, `workspace`, `stage` and `diagnosis` fields) through
//! Collections' own `apply_change`; a collection linked before an option or
//! field was added gets it on the next pass.

use std::collections::BTreeMap;

use pod_core::Pod;
use pod_core::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use source_collections::{db as cdb, model};

use crate::cap::{self, CapError, Clients, Command, Runner, StartRun};
use crate::db;
use crate::state::{self as st, Run, State};

/// Pod setting: the linked collection (`{collection, workspace, ...}` as
/// JSON).
pub const TASKS_SETTING: &str = "agents.tasks";

/// The `status` options the wiring sets; `queued` starts a run.
pub const TODO: &str = "todo";
pub const QUEUED: &str = "queued";
pub const RUNNING: &str = "running";
pub const NEEDS_YOU: &str = "needs_you";
pub const WAITING: &str = "waiting";
pub const REVIEW: &str = "review";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
const NEEDED_OPTIONS: [&str; 8] = [
    TODO, QUEUED, RUNNING, NEEDS_YOU, WAITING, REVIEW, DONE, FAILED,
];
/// The text fields the wiring writes.
const NEEDED_FIELDS: [&str; 5] = ["run_id", "session_id", "workspace", "stage", "diagnosis"];
/// The fields a run is made from: only a person (or Claude) may have
/// written them last for a row to start a run (issue #243).
const TRUSTED_FIELDS: [&str; 8] = [
    "status",
    "title",
    "notes",
    "description",
    "details",
    "workspace",
    "model",
    "max_runtime_min",
];
/// The most of `diagnosis` kept (the newest text).
const DIAGNOSIS_MAX: usize = 3_900;
/// The most of a prompt sent.
const PROMPT_MAX: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub collection: String,
    /// The runner workspace a row without its own `workspace` starts in.
    pub workspace: String,
    /// The model a row without its own `model` asks for (empty: the
    /// Sprite's default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// The max runtime a row without its own asks for (0: the runner's).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_runtime_min: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
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

/// The definition changes `def` needs for the wiring: the status options
/// and the text fields it lacks.
fn shape_changes(def: &model::Definition) -> Vec<Value> {
    let mut changes = Vec::new();
    if let Some(status) = def.field("status") {
        let mut options = status.options.clone();
        for o in NEEDED_OPTIONS {
            if !options.iter().any(|x| x == o) {
                options.push(o.to_owned());
            }
        }
        if options != status.options {
            changes.push(json!({"op": "update_field", "field": "status", "options": options}));
        }
    }
    for f in NEEDED_FIELDS {
        if def.field(f).is_none() {
            changes.push(json!({"op": "add_field", "name": f, "type": "text", "optional": true}));
        }
    }
    changes
}

async fn apply_shape(pod: &Pod, collection: &str, changes: &[Value]) -> Result<(), Value> {
    let Some((collections, _)) = pod.catalog().source(source_collections::NAME) else {
        return Err(bad("this pod has no Collections source"));
    };
    let args = json!({"collection": collection, "changes": changes,
        "note": "Linked to the agent runner (agents_link_tasks)"});
    let Some(call) = collections.mcp_call(pod, "apply_change", &args) else {
        return Err(bad("Collections can't change definitions here"));
    };
    call.await.map(|_| ())
}

/// `agents_link_tasks {collection, workspace, model?, max_runtime_min?}`:
/// link a collection (adding the options and fields the wiring needs), or
/// unlink with `collection: null`.
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
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    if model.len() > 100 || !model.chars().all(|c| c.is_ascii_graphic()) {
        return Err(bad("\"model\" is one word of at most 100 characters"));
    }
    let max_runtime_min = match args.get("max_runtime_min") {
        None | Some(Value::Null) => 0,
        Some(v) => match v.as_u64() {
            Some(n @ 1..=480) => n as u32,
            _ => return Err(bad("\"max_runtime_min\" is 1 to 480 minutes")),
        },
    };
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
    let changes = shape_changes(&def);
    if !changes.is_empty() {
        apply_shape(pod, &collection, &changes).await?;
    }
    let link = Link {
        collection: collection.clone(),
        workspace,
        model,
        max_runtime_min,
    };
    let v = serde_json::to_string(&link).expect("serializes");
    pod.blocking(move |s| s.set_pod_setting(TASKS_SETTING, &v))
        .await
        .map_err(|_| bad("the pod's settings can't be written"))?;
    Ok(json!({"ok": true, "linked": link, "changes": changes,
        "next": "Rows you or Claude set to queued now start a run on the runner."}))
}

/// A row's values by field name.
type Named = Map<String, Value>;

/// One row of the linked collection, with who wrote what.
#[derive(Debug, Clone)]
struct Task {
    id: String,
    values: Named,
    /// The filler that made it.
    filler: Option<String>,
    /// Name a run reads -> the filler, model or other rows
    /// ([`cdb::WRITER_DERIVED`]) that wrote it last.
    untrusted: BTreeMap<String, String>,
}

/// Whether a row is marked done: `status = done`, or a `done` box ticked.
fn is_done(row: &Named) -> bool {
    text(row, "status") == DONE || row.get("done") == Some(&Value::Bool(true))
}

/// Whether a row keeps the status you gave it (done or todo): its run only
/// adds to `diagnosis`.
fn is_kept(row: &Named) -> bool {
    is_done(row) || text(row, "status") == TODO
}

/// The linked collection's definition and rows (none when it is gone).
fn tasks(
    store: &Store,
    collection: &str,
) -> rusqlite::Result<Option<(model::Definition, Vec<Task>)>> {
    store.with_conn(|c| {
        let Some((_, def)) = cdb::latest_def(c, collection)? else {
            return Ok(None);
        };
        let mut writers = cdb::field_writers(c, collection)?;
        let rows = cdb::rows(c, &def)?
            .into_iter()
            .map(|r| {
                let values = model::row_entity(&def, &r.id, &r.values, "", "");
                // By the name a run reads (an alias too), through the
                // field's id (issue #289).
                let row_writers = writers.remove(&r.id).unwrap_or_default();
                let untrusted = TRUSTED_FIELDS
                    .iter()
                    .filter_map(|name| {
                        let f = def.field(name)?;
                        let w = cdb::writer_of(&def, &row_writers, &f.id)?;
                        Some(((*name).to_owned(), w))
                    })
                    .collect();
                Task {
                    id: r.id,
                    values,
                    filler: r.filler,
                    untrusted,
                }
            })
            .collect();
        Ok(Some((def, rows)))
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

/// Why row `t` may not start a run (issue #243): a filler made it, or a
/// filler or a model wrote one of the fields a run is made from. `None`:
/// you (or Claude) wrote it all.
fn untrusted_origin(def: &model::Definition, t: &Task) -> Option<String> {
    let who = |w: &str| match w.strip_prefix("filler:") {
        Some(id) => match source_collections::filler::find(def, id) {
            Some(f) if !f.title.is_empty() => format!("the filler {}", f.title),
            _ => format!("the filler {id}"),
        },
        None if w == cdb::WRITER_DERIVED => "a value from other rows".to_owned(),
        None => "a model".to_owned(),
    };
    if let Some(f) = &t.filler {
        return Some(format!(
            "{} made this row; only rows you or Claude add can start a run",
            who(&format!("filler:{f}"))
        ));
    }
    let fields: Vec<(&str, &String)> = TRUSTED_FIELDS
        .iter()
        .filter_map(|f| t.untrusted.get(*f).map(|w| (*f, w)))
        .collect();
    let (_, first) = fields.first()?;
    let names: Vec<&str> = fields.iter().map(|(f, _)| *f).collect();
    Some(format!(
        "{} wrote {} last; set {} yourself to start it",
        who(first),
        names.join(", "),
        if names.len() == 1 { "it" } else { "them" }
    ))
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
    if let Err(e) = source_collections::update_row(pod, collection, id, values).await {
        tracing::warn!(error = %e["error"], detail = %e["detail"], "a task row could not be updated");
    }
}

/// The task status run `r` maps to: the one table (module docs).
pub fn task_status(r: &Run) -> &'static str {
    match r.state.as_str() {
        st::QUEUED | st::LAUNCHING => QUEUED,
        st::WORKING | st::STALLED | st::INTERRUPTED | st::STOPPING | st::CLOSING => RUNNING,
        st::NEEDS_YOU | st::PARKED => NEEDS_YOU,
        st::ENDED => match r.outcome_state() {
            st::OUTCOME_DONE => DONE,
            st::OUTCOME_FAILED => FAILED,
            st::OUTCOME_NEEDS_REVIEW => REVIEW,
            st::OUTCOME_PENDING => WAITING,
            _ if st::BAD_ENDS.contains(&r.end_reason.as_str()) => FAILED,
            // Ended, its outcome not worked out yet.
            _ => WAITING,
        },
        // A state this pod doesn't know yet: still going.
        _ => RUNNING,
    }
}

/// What a waiting row waits on: `ci`, `review` or `deploy` (empty
/// otherwise).
pub fn stage(r: &Run) -> &str {
    if task_status(r) == WAITING {
        &r.outcome.stage
    } else {
        ""
    }
}

/// Whether run `r`'s outcome is settled enough to note in `diagnosis`.
fn settled(r: &Run) -> bool {
    !r.is_active() && task_status(r) != WAITING
}

/// What a settled run adds to its row's `diagnosis`: what it did and its
/// pull request, or why it failed.
pub fn end_note(r: &Run) -> String {
    let (word, _) = crate::bundle::run_look(r);
    let said = r.said();
    let mut note = match task_status(r) {
        FAILED => {
            let why = if !said.is_empty() {
                said.to_owned()
            } else if !r.error.is_empty() {
                r.error.clone()
            } else {
                crate::alerts::end_reason_text(&r.end_reason).to_owned()
            };
            format!("{word}: {why}")
        }
        _ if !said.is_empty() => said.to_owned(),
        DONE => format!("{word}."),
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

/// Whether the definition change for an already-linked collection has been
/// tried by this process.
#[derive(Debug, Default)]
pub struct Shaped(std::sync::atomic::AtomicBool);

/// One pass over the linked collection: give it what the wiring needs,
/// send `start_run` for queued rows that need one, then set each row from
/// its current run, note settled runs in `diagnosis` and stop the runs of
/// rows you set to done or todo.
pub async fn sync(pod: &Pod, clients: &Clients, runner: &Runner, state: &State) {
    let Ok(Some(link)) = pod.blocking(link).await else {
        return;
    };
    let coll = link.collection.clone();
    let Ok((Some((def, rows)), launches)) = pod
        .blocking(move |s| Ok((tasks(s, &coll)?, db::launches(s, &coll)?)))
        .await
    else {
        return;
    };
    let shaped = pod.local::<Shaped>();
    let changes = shape_changes(&def);
    if !changes.is_empty() && !shaped.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
        if let Err(e) = apply_shape(pod, &link.collection, &changes).await {
            tracing::warn!(error = %e, "the linked tasks collection could not be updated");
        }
        return;
    }
    let launch_of = |id: &str| launches.iter().find(|l| l.task_id == id).cloned();
    // Sends are forgotten once their row moves on.
    for l in &launches {
        if !rows
            .iter()
            .any(|t| t.id == l.task_id && text(&t.values, "status") == QUEUED)
        {
            let id = l.task_id.clone();
            let _ = pod.blocking(move |s| db::remove_launch(s, &id)).await;
        }
    }
    let has_stage = def.field("stage").is_some();
    let mut sending = true;
    for t in rows {
        let row = &t.values;
        let launched = launch_of(&t.id);
        let named = text(row, "run_id");
        let run_id = if named.is_empty() {
            launched
                .as_ref()
                .map(|l| l.run_id.clone())
                .unwrap_or_default()
        } else {
            named.clone()
        };
        let queued = text(row, "status") == QUEUED && !is_done(row);
        // A queued row needs a run unless it has the one this pod started
        // for it; a row queued again after its run ended (a retry) gets a
        // new one. A send not answered yet is sent again, as it was.
        let unanswered = launched.as_ref().filter(|l| l.run_id.is_empty());
        let ours = launched
            .as_ref()
            .is_some_and(|l| !l.run_id.is_empty() && l.run_id == run_id);
        let needs_run = queued
            && !ours
            && (unanswered.is_some()
                || run_id.is_empty()
                || state.runs.get(&run_id).is_none_or(|r| !r.is_active()));
        if needs_run {
            if let Some(why) = untrusted_origin(&def, &t) {
                let diagnosis = append(&text(row, "diagnosis"), &format!("Not started: {why}."));
                update(
                    pod,
                    &link.collection,
                    &t.id,
                    json!({"status": FAILED, "diagnosis": diagnosis}),
                )
                .await;
                continue;
            }
            if sending {
                let request = unanswered.map(|l| l.request_id.clone());
                sending = start(pod, clients, runner, &link, &t.id, row, request).await;
            }
            continue;
        }
        let Some(run) = state.current_run(&t.id, &run_id) else {
            continue;
        };
        follow(
            pod,
            clients,
            runner,
            &link.collection,
            &t.id,
            row,
            run,
            has_stage,
        )
        .await;
    }
}

/// Send `start_run` for row `id` (`request`: the request id of a send not
/// answered yet), remember the run it answers and put it in the row.
/// Whether the runner answered (a refusal for good fails the row).
async fn start(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    link: &Link,
    id: &str,
    row: &Named,
    request: Option<String>,
) -> bool {
    match start_run(pod, clients, runner, link, id, row, request, None, None).await {
        Ok(_) => true,
        Err(e) => match refusal(&e) {
            Some(reason) => {
                let task = id.to_owned();
                let _ = pod.blocking(move |s| db::remove_launch(s, &task)).await;
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
            // The runner is away: sent again, with the same request id, on
            // the next pass.
            None => {
                tracing::info!(error = %e, "a queued task waits for the runner");
                false
            }
        },
    }
}

/// The model and max runtime row `row` asks for: its own fields, else the
/// link's defaults.
fn run_settings(link: &Link, row: &Named) -> (Option<String>, Option<u32>) {
    let model = Some(text(row, "model"))
        .filter(|m| !m.is_empty() && m.len() <= 100 && m.chars().all(|c| c.is_ascii_graphic()))
        .or_else(|| (!link.model.is_empty()).then(|| link.model.clone()));
    let own = match row.get("max_runtime_min") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n| (1..=cap::MAX_RUNTIME_MIN as u64).contains(n))
    .map(|n| n as u32);
    let runtime = own.or((link.max_runtime_min > 0).then_some(link.max_runtime_min));
    (model, runtime)
}

/// `start_run` for row `id`; the new run's id, remembered and put in the
/// row (with `status = queued`). The request id is stored before sending,
/// so a resend after a crash is the same request.
#[allow(clippy::too_many_arguments)]
async fn start_run(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    link: &Link,
    id: &str,
    row: &Named,
    request: Option<String>,
    resume_from: Option<String>,
    message: Option<String>,
) -> Result<String, CapError> {
    let now = crate::follower::now_ms();
    let reopen = resume_from.is_some();
    let request_id =
        request.unwrap_or_else(|| format!("{}_{id}_{now}", if reopen { "reopen" } else { "task" }));
    let ws = text(row, "workspace");
    let (model, max_runtime_min) = run_settings(link, row);
    let cmd = Command::StartRun(StartRun {
        request_id: request_id.clone(),
        workspace: if ws.is_empty() {
            link.workspace.clone()
        } else {
            ws
        },
        work_item_id: id.to_owned(),
        occurrence: None,
        origin: cap::ORIGIN_TASK.into(),
        title: text(row, "title").chars().take(200).collect(),
        prompt: message.unwrap_or_else(|| prompt(row)),
        model,
        max_runtime_min,
        resume_from,
    });
    if !reopen {
        let (coll, task, req) = (link.collection.clone(), id.to_owned(), request_id.clone());
        let _ = pod
            .blocking(move |s| db::put_launch(s, &coll, &task, &req, "", now))
            .await;
    }
    let answer = cap::command(clients, runner, &cmd).await?;
    let run_id = cap::run_id_of(&answer).unwrap_or_default();
    let (coll, task, run) = (link.collection.clone(), id.to_owned(), run_id.clone());
    // Remembered before the row changes: the change wakes another pass.
    let _ = pod
        .blocking(move |s| db::put_launch(s, &coll, &task, &request_id, &run, now))
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
#[allow(clippy::too_many_arguments)]
async fn follow(
    pod: &Pod,
    clients: &Clients,
    runner: &Runner,
    collection: &str,
    id: &str,
    row: &Named,
    run: &Run,
    has_stage: bool,
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
    if has_stage && text(row, "stage") != stage(run) {
        values.insert("stage".into(), json!(stage(run)));
    }
    let noted = format!("noted:{}", run.id);
    let note = settled(run) && {
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
        let ok = source_collections::update_row(pod, collection, id, values).await;
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
        .blocking(move |s| {
            Ok(tasks(s, &coll)?.and_then(|(_, rows)| rows.into_iter().find(|t| t.id == rid)))
        })
        .await
        .ok()
        .flatten()
        .map(|t| t.values)
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
        None,
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
            outcome: st::Outcome {
                state: outcome.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn the_status_table() {
        for (state, end, outcome, want) in [
            ("queued", "", "", QUEUED),
            ("launching", "", "", QUEUED),
            ("working", "", "", RUNNING),
            ("stalled", "", "", RUNNING),
            ("interrupted", "", "", RUNNING),
            ("stopping", "", "", RUNNING),
            ("closing", "", "pending", RUNNING),
            ("needs_you", "", "", NEEDS_YOU),
            ("parked", "", "", NEEDS_YOU),
            ("ended", "reported", "pending", WAITING),
            // A PR on a lost run's branch still counts.
            ("ended", "lost", "pending", WAITING),
            ("ended", "lost", "done", DONE),
            ("ended", "reported", "done", DONE),
            ("ended", "reported", "failed", FAILED),
            ("ended", "lost", "", FAILED),
            ("ended", "lost", "none", FAILED),
            ("ended", "start_failed", "", FAILED),
            ("ended", "timed_out", "", FAILED),
            ("ended", "expired", "", FAILED),
            ("ended", "stopped", "needs_review", REVIEW),
            ("ended", "reported", "needs_review", REVIEW),
            ("ended", "stopped", "", WAITING),
        ] {
            assert_eq!(
                task_status(&run(state, end, outcome)),
                want,
                "{state} {end} {outcome}"
            );
        }
        let mut r = run("ended", "reported", "pending");
        r.outcome.stage = "deploy".into();
        assert_eq!(stage(&r), "deploy");
        r.outcome.state = "done".into();
        assert_eq!(stage(&r), "");
    }

    #[test]
    fn settled_runs_note_why() {
        let mut r = run("ended", "reported", "done");
        r.claim = Some(st::Claim {
            status: "done".into(),
            summary: "Moved the footer.".into(),
            ..Default::default()
        });
        r.outcome.pr_url = "https://github.com/o/r/pull/2".into();
        assert_eq!(
            end_note(&r),
            "Moved the footer.\nPR: https://github.com/o/r/pull/2"
        );
        let mut r = run("ended", "reported", "failed");
        r.outcome.reason = "ci".into();
        r.summary = "Tests fail.".into();
        assert_eq!(end_note(&r), "CI failed: Tests fail.");
        let r = run("ended", "lost", "");
        assert_eq!(end_note(&r), "Lost: The run was lost: its process is gone.");
        let mut r = run("ended", "start_failed", "");
        r.error = "claude: exit 1: no such flag".into();
        assert_eq!(end_note(&r), "Didn't start: claude: exit 1: no such flag");
        assert_eq!(end_note(&run("ended", "reported", "done")), "Done.");
        assert!(!settled(&run("ended", "reported", "pending")));
        assert!(settled(&run("ended", "stopped", "needs_review")));
    }

    #[test]
    fn a_prompt_is_the_title_and_notes() {
        let row: Named =
            serde_json::from_value(json!({"title": "Fix login", "notes": "It 500s"})).unwrap();
        assert_eq!(prompt(&row), "Fix login\n\nIt 500s");
        let link = Link {
            collection: "tasks".into(),
            workspace: "w".into(),
            model: "a-model".into(),
            max_runtime_min: 0,
        };
        assert_eq!(run_settings(&link, &row), (Some("a-model".into()), None));
        let own: Named =
            serde_json::from_value(json!({"model": "other", "max_runtime_min": "45"})).unwrap();
        assert_eq!(run_settings(&link, &own), (Some("other".into()), Some(45)));
    }
}
