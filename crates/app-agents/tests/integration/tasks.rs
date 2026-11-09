//! The Tasks wiring: a linked collection's queued rows start sessions, and
//! the sessions' events come back to the rows.

use std::sync::atomic::Ordering;

use pod_core::Pod;
use serde_json::{Map, Value, json};

use crate::runner::{signed_in, tool, until};

fn row(pod: &Pod, id: &str) -> Map<String, Value> {
    pod.store()
        .with_conn(|c| {
            let (_, def) = app_collections::db::latest_def(c, "tasks")?.unwrap();
            let r = app_collections::db::row(c, "tasks", id)?.unwrap();
            Ok::<_, rusqlite::Error>(app_collections::model::row_entity(
                &def, &r.id, &r.values, "", "",
            ))
        })
        .unwrap()
}

fn starts(fake: &crate::runner::Fake) -> Vec<Value> {
    fake.commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c["type"] == "start_task")
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_queued_task_runs_to_review() {
    let (pod, fake, _) = signed_in().await;
    tool(
        &pod,
        "define_collection",
        json!({"name": "tasks", "title": "Tasks", "fields": [
            {"name": "title", "type": "text"},
            {"name": "notes", "type": "text", "optional": true},
            {"name": "status", "type": "choice", "options": ["todo", "queued", "started"], "default": "todo"}]}),
    )
    .await;
    let linked = tool(
        &pod,
        "agents_link_tasks",
        json!({"collection": "tasks", "workspace": "sample-repo"}),
    )
    .await;
    assert_eq!(linked["ok"], true, "{linked}");
    let def = tool(&pod, "describe_collection", json!({"collection": "tasks"})).await;
    let text = def.to_string();
    for want in ["review", "failed", "session_id", "diagnosis", "workspace"] {
        assert!(text.contains(want), "{want} in {def}");
    }

    // A row that isn't queued starts nothing.
    let idle = tool(
        &pod,
        "add_row",
        json!({"collection": "tasks", "values": {"title": "Later"}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = tool(
        &pod,
        "add_row",
        json!({"collection": "tasks", "values": {"title": "Fix the footer", "notes": "It overlaps on phones", "status": "queued"}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    until("start_task sent", || async { !starts(&fake).is_empty() }).await;
    let sent = starts(&fake);
    assert_eq!(
        sent[0],
        json!({"type": "start_task", "workspace": "sample-repo", "task_id": id,
            "title": "Fix the footer", "prompt": "Fix the footer\n\nIt overlaps on phones"})
    );
    // Still queued: not sent again.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(starts(&fake).len(), 1);

    fake.push(json!({"type": "session.started", "session_id": "session_01Task", "task_id": id}));
    until("started", || async {
        row(&pod, &id)["status"] == "started"
    })
    .await;
    assert_eq!(row(&pod, &id)["session_id"], "session_01Task");
    fake.push(
        json!({"type": "session.finished", "session_id": "session_01Task", "task_id": id,
        "data": {"summary": "Moved the footer below the fold.",
                 "pr_url": "https://github.com/sample-owner/sample-repo/pull/31"}}),
    );
    until("in review", || async {
        row(&pod, &id)["status"] == "review"
    })
    .await;
    assert_eq!(
        row(&pod, &id)["diagnosis"],
        "Moved the footer below the fold.\nPR: https://github.com/sample-owner/sample-repo/pull/31"
    );
    assert_eq!(row(&pod, &idle)["status"], "todo");
    assert!(starts(&fake).iter().all(|c| c["task_id"] != idle.as_str()));
}

#[tokio::test]
async fn busy_refused_and_failed_tasks() {
    let (pod, fake, _) = signed_in().await;
    tool(
        &pod,
        "define_collection",
        json!({"name": "tasks", "title": "Tasks", "fields": [
            {"name": "title", "type": "text"},
            {"name": "status", "type": "choice", "options": ["todo", "queued"], "default": "todo"}]}),
    )
    .await;
    tool(
        &pod,
        "agents_link_tasks",
        json!({"collection": "tasks", "workspace": "sample-repo"}),
    )
    .await;
    // At the limit: the row stays queued and is sent again.
    fake.answers
        .lock()
        .unwrap()
        .push_back((429, json!({"error": "at the concurrency limit"})));
    let busy = tool(
        &pod,
        "add_row",
        json!({"collection": "tasks", "values": {"title": "Busy one", "status": "queued"}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    until("sent again after a 429", || async {
        starts(&fake).len() >= 2
    })
    .await;
    assert_eq!(row(&pod, &busy)["status"], "queued");

    // A workspace the runner doesn't know: the row fails with the reason.
    fake.answers
        .lock()
        .unwrap()
        .push_back((404, json!({"error": "no workspace nope"})));
    let refused = tool(
        &pod,
        "add_row",
        json!({"collection": "tasks", "values": {"title": "Elsewhere", "status": "queued", "workspace": "nope"}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    until("refused", || async {
        row(&pod, &refused)["status"] == "failed"
    })
    .await;
    assert_eq!(
        row(&pod, &refused)["diagnosis"],
        "Couldn't start: no workspace nope"
    );

    // A session that fails puts the row back to todo with the error.
    fake.push(json!({"type": "session.started", "session_id": "session_01Busy", "task_id": busy}));
    fake.push(
        json!({"type": "session.failed", "session_id": "session_01Busy", "task_id": busy,
        "data": {"error": "Tests kept failing"}}),
    );
    until("back to todo", || async {
        row(&pod, &busy)["status"] == "todo"
    })
    .await;
    assert_eq!(
        row(&pod, &busy)["diagnosis"],
        "The session failed: Tests kept failing"
    );
    // Unlinked: nothing more is sent.
    fake.end_streams.store(false, Ordering::SeqCst);
    tool(&pod, "agents_link_tasks", json!({"collection": null})).await;
    let n = starts(&fake).len();
    tool(
        &pod,
        "add_row",
        json!({"collection": "tasks", "values": {"title": "After", "status": "queued"}}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(starts(&fake).len(), n);
}

fn stops(fake: &crate::runner::Fake) -> Vec<Value> {
    fake.commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c["type"] == "stop_session")
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_done_task_closes_its_waiting_session() {
    let (pod, fake, _) = signed_in().await;
    tool(
        &pod,
        "define_collection",
        json!({"name": "tasks", "title": "Tasks", "fields": [
            {"name": "title", "type": "text"},
            {"name": "status", "type": "choice", "options": ["todo", "queued", "done"], "default": "todo"}]}),
    )
    .await;
    tool(
        &pod,
        "agents_link_tasks",
        json!({"collection": "tasks", "workspace": "sample-repo"}),
    )
    .await;
    let id = tool(
        &pod,
        "add_row",
        json!({"collection": "tasks", "values": {"title": "Shade pressed rows", "status": "queued"}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    until("start_task sent", || async { !starts(&fake).is_empty() }).await;
    fake.push(
        json!({"type": "session.started", "session_id": "session_01Shade", "task_id": id,
        "data": {"title": "Shade pressed rows"}}),
    );
    fake.push(
        json!({"type": "permission.requested", "session_id": "session_01Shade",
        "data": {"tool": "Bash", "detail": "git push"}}),
    );
    let app = crate::runner::agents_layout(&pod).await;
    let uri = "agents://sessions/session_01Shade";
    until("waiting for you", || async {
        crate::runner::workspace(&pod, &app).await["entities"][uri]["needs_you"] == true
    })
    .await;

    // Marked done (by the session or by hand): the session is stopped, and
    // stops waiting for you even though the runner still says it waits.
    app_collections::update_row(
        &pod,
        "tasks",
        &id,
        serde_json::from_value(json!({"status": "done"})).unwrap(),
    )
    .await
    .unwrap();
    until("stop_session sent", || async { !stops(&fake).is_empty() }).await;
    assert_eq!(
        stops(&fake)[0],
        json!({"type": "stop_session", "session_id": "session_01Shade"})
    );
    let ws = crate::runner::workspace(&pod, &app).await;
    assert_eq!(ws["entities"][uri]["needs_you"], false);
    assert_eq!(ws["entities"][uri]["status_text"], "Finished");
    assert_eq!(
        ws["entities"]["agents://runner/status"]["needs_you_count"],
        0
    );
    // Sent once, not on every pass.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(stops(&fake).len(), 1);

    // What the session sends after doesn't reopen the row.
    fake.push(
        json!({"type": "session.finished", "session_id": "session_01Shade", "task_id": id,
        "data": {"summary": "Stopped."}}),
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(row(&pod, &id)["status"], "done");
}
