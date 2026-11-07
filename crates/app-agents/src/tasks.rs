//! The Tasks wiring (opt-in per collection): a row moving to `queued`
//! sends `start_task` with the row's id as the task id, and the session's
//! events flow back to the row.
//!
//! - `session.started`: `status = started`, `session_id` set.
//! - `session.finished`: the summary and PR link appended to `diagnosis`,
//!   `status = review`.
//! - `session.failed`: `status = todo`, the error appended to `diagnosis`.
//! - A `start_task` the runner refuses (400, 404): `status = failed` with
//!   the reason. At the concurrency limit (429) the row stays queued and is
//!   sent again later.
//!
//! The collection is linked with the MCP tool `agents_link_tasks`, which
//! adds what the wiring needs (the status options and the `session_id`,
//! `workspace` and `diagnosis` fields) through Collections' own
//! `apply_change`.

use pod_core::Pod;
use pod_core::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use app_collections::{db as cdb, model};

use crate::cap::{self, CapError, Clients, Command, Runner};
use crate::db;
use crate::state::Effect;

/// Pod setting: the linked collection (`{collection, workspace}` as JSON).
pub const TASKS_SETTING: &str = "agents.tasks";

/// The `status` options the wiring sets, and the one that starts a task.
pub const QUEUED: &str = "queued";
const NEEDED_OPTIONS: [&str; 4] = ["todo", "started", "review", "failed"];
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
    for f in ["session_id", "workspace", "diagnosis"] {
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
        "next": "Rows whose status becomes queued now start a session on the runner."}))
}

/// A row's values by field name.
type Named = Map<String, Value>;

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

/// Send `start_task` for every queued row of the linked collection that
/// hasn't been sent, and forget sends for rows no longer queued.
pub async fn launch_queued(pod: &Pod, clients: &Clients, runner: &Runner) {
    let Ok(Some(link)) = pod.blocking(link).await else {
        return;
    };
    let coll = link.collection.clone();
    let Ok((rows, pending)) = pod
        .blocking(move |s| Ok((rows(s, &coll)?, db::launches(s, &coll)?)))
        .await
    else {
        return;
    };
    for id in &pending {
        if !rows
            .iter()
            .any(|(r, v)| r == id && text(v, "status") == QUEUED)
        {
            let id = id.clone();
            let _ = pod.blocking(move |s| db::remove_launch(s, &id)).await;
        }
    }
    for (id, row) in rows {
        if text(&row, "status") != QUEUED || pending.contains(&id) {
            continue;
        }
        let ws = text(&row, "workspace");
        let cmd = Command::StartTask {
            workspace: if ws.is_empty() {
                link.workspace.clone()
            } else {
                ws
            },
            task_id: id.clone(),
            title: text(&row, "title"),
            prompt: prompt(&row),
        };
        match cap::command(clients, runner, &cmd).await {
            Ok(_) => {
                let (coll, id) = (link.collection.clone(), id.clone());
                let now = pod_core::util::now_secs();
                let _ = pod
                    .blocking(move |s| db::add_launch(s, &coll, &id, now))
                    .await;
            }
            Err(CapError::Status(400 | 404, e)) => {
                let reason = if e.is_empty() {
                    "the runner refused the task".to_owned()
                } else {
                    e
                };
                let diagnosis = append(
                    &text(&row, "diagnosis"),
                    &format!("Couldn't start: {reason}"),
                );
                update(
                    pod,
                    &link.collection,
                    &id,
                    json!({"status": "failed", "diagnosis": diagnosis}),
                )
                .await;
            }
            // At the limit, or the runner is away: try again later.
            Err(e) => {
                tracing::info!(error = %e, "a queued task waits for the runner");
                return;
            }
        }
    }
}

/// Apply the task effects among `effects` to the linked collection's rows.
pub async fn apply(pod: &Pod, effects: &[Effect]) {
    if !effects.iter().any(|e| {
        matches!(
            e,
            Effect::TaskStarted { .. } | Effect::TaskFinished { .. } | Effect::TaskFailed { .. }
        )
    }) {
        return;
    }
    let Ok(Some(link)) = pod.blocking(link).await else {
        return;
    };
    for e in effects {
        let task_id = match e {
            Effect::TaskStarted { task_id, .. }
            | Effect::TaskFinished { task_id, .. }
            | Effect::TaskFailed { task_id, .. } => task_id.clone(),
            _ => continue,
        };
        let (coll, id) = (link.collection.clone(), task_id.clone());
        let row = pod
            .blocking(move |s| {
                s.with_conn(|c| {
                    let Some((_, def)) = cdb::latest_def(c, &coll)? else {
                        return Ok(None);
                    };
                    Ok(cdb::row(c, &coll, &id)?
                        .filter(|r| r.deleted_at.is_none())
                        .map(|r| model::row_entity(&def, &r.id, &r.values, "", "")))
                })
            })
            .await;
        let Ok(Some(row)) = row else {
            continue;
        };
        let id = task_id.clone();
        let _ = pod.blocking(move |s| db::remove_launch(s, &id)).await;
        let values = match e {
            Effect::TaskStarted { session_id, .. } => {
                json!({"status": "started", "session_id": session_id})
            }
            Effect::TaskFinished {
                summary, pr_url, ..
            } => {
                let mut more = if summary.is_empty() {
                    "Finished.".to_owned()
                } else {
                    summary.clone()
                };
                if !pr_url.is_empty() {
                    more.push_str(&format!("\nPR: {pr_url}"));
                }
                json!({"status": "review", "diagnosis": append(&text(&row, "diagnosis"), &more)})
            }
            Effect::TaskFailed { error, .. } => {
                let more = if error.is_empty() {
                    "The session failed.".to_owned()
                } else {
                    format!("The session failed: {error}")
                };
                json!({"status": "todo", "diagnosis": append(&text(&row, "diagnosis"), &more)})
            }
            _ => continue,
        };
        update(pod, &link.collection, &task_id, values).await;
    }
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

    #[test]
    fn a_prompt_is_the_title_and_notes() {
        let row: Named =
            serde_json::from_value(json!({"title": "Fix login", "notes": "It 500s"})).unwrap();
        assert_eq!(prompt(&row), "Fix login\n\nIt 500s");
    }
}
