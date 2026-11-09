//! The Tasks wiring: a linked collection's queued rows start runs, and each
//! row's status follows its run through the one table.

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use pod_core::Pod;
use serde_json::{Map, Value, json};

use crate::runner::{Fake, call, signed_in, tool, until};

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

fn field(pod: &Pod, id: &str, k: &str) -> String {
    row(pod, id)[k].as_str().unwrap_or("").to_owned()
}

/// A `tasks` collection with `options`, linked to the runner.
async fn linked(pod: &Pod, options: Value) {
    tool(
        pod,
        "define_collection",
        json!({"name": "tasks", "title": "Tasks", "fields": [
            {"name": "title", "type": "text"},
            {"name": "notes", "type": "text", "optional": true},
            {"name": "status", "type": "choice", "options": options, "default": "todo"}]}),
    )
    .await;
    let linked = tool(
        pod,
        "agents_link_tasks",
        json!({"collection": "tasks", "workspace": "sample-repo"}),
    )
    .await;
    assert_eq!(linked["ok"], true, "{linked}");
}

async fn add(pod: &Pod, values: Value) -> String {
    tool(
        pod,
        "add_row",
        json!({"collection": "tasks", "values": values}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn set(pod: &Pod, id: &str, values: Value) {
    app_collections::update_row(pod, "tasks", id, serde_json::from_value(values).unwrap())
        .await
        .unwrap();
}

/// Row `id` once its `start_run` was answered: its run id.
async fn started(pod: &Pod, id: &str) -> String {
    until("the row has its run", || async {
        !field(pod, id, "run_id").is_empty()
    })
    .await;
    field(pod, id, "run_id")
}

async fn status_is(pod: &Pod, id: &str, want: &str) {
    until(&format!("{id} is {want}"), || async {
        field(pod, id, "status") == want
    })
    .await;
}

#[tokio::test]
async fn a_queued_task_follows_its_run() {
    let (pod, fake, _) = signed_in().await;
    linked(&pod, json!(["todo", "queued", "started"])).await;
    let def = tool(&pod, "describe_collection", json!({"collection": "tasks"})).await;
    let text = def.to_string();
    for want in [
        "running",
        "needs_you",
        "review",
        "done",
        "failed",
        "started",
        "run_id",
        "session_id",
        "diagnosis",
        "workspace",
    ] {
        assert!(text.contains(want), "{want} in {def}");
    }

    // A row that isn't queued starts nothing.
    let idle = add(&pod, json!({"title": "Later"})).await;
    fake.next_runs.lock().unwrap().push_back("run_t1".into());
    let id = add(
        &pod,
        json!({"title": "Fix the footer", "notes": "It overlaps on phones", "status": "queued"}),
    )
    .await;
    assert_eq!(started(&pod, &id).await, "run_t1");
    assert_eq!(
        fake.sent("start_run")[0],
        json!({"type": "start_run", "workspace": "sample-repo", "task_id": id,
            "title": "Fix the footer", "prompt": "Fix the footer\n\nIt overlaps on phones"})
    );
    // Still queued: not sent again.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.sent("start_run").len(), 1);
    assert_eq!(field(&pod, &id, "status"), "queued");

    let run = |fields: Value| {
        let mut f = fields;
        f["task_id"] = json!(id);
        fake.run("run_t1", f);
    };
    run(json!({"state": "starting"}));
    status_is(&pod, &id, "running").await;
    run(json!({"state": "working", "session_id": "session_01Task"}));
    until("session id", || async {
        field(&pod, &id, "session_id") == "session_01Task"
    })
    .await;
    run(json!({"state": "needs_you", "needs_you": {"reason": "question", "text": "Merge?"}}));
    status_is(&pod, &id, "needs_you").await;
    run(json!({"state": "working", "needs_you": null}));
    status_is(&pod, &id, "running").await;
    run(
        json!({"state": "ended", "end_reason": "reported", "outcome": {"status": "review",
        "summary": "Moved the footer below the fold.",
        "pr_url": "https://github.com/sample-owner/sample-repo/pull/31"}}),
    );
    status_is(&pod, &id, "review").await;
    assert_eq!(
        field(&pod, &id, "diagnosis"),
        "Moved the footer below the fold.\nPR: https://github.com/sample-owner/sample-repo/pull/31"
    );
    // Noted once, however often the run is sent again.
    run(json!({"cost_usd": 0.5}));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!field(&pod, &id, "diagnosis").contains("\n\n"));
    assert_eq!(field(&pod, &idle, "status"), "todo");
    assert!(
        fake.sent("start_run")
            .iter()
            .all(|c| c["task_id"] != idle.as_str())
    );
}

#[tokio::test]
async fn each_ending_maps_to_a_status() {
    let (pod, fake, _) = signed_in().await;
    linked(&pod, json!(["todo", "queued"])).await;
    fake.end_streams.store(false, Ordering::SeqCst);
    let endings = [
        (
            json!({"end_reason": "reported", "outcome": {"status": "done", "summary": "Merged."}}),
            "done",
            "Merged.",
        ),
        (
            json!({"end_reason": "reported", "outcome": {"status": "blocked", "summary": "Needs a key."}}),
            "failed",
            "Blocked: Needs a key.",
        ),
        (
            json!({"end_reason": "reported", "outcome": {"status": "failed", "summary": "Tests fail."}}),
            "failed",
            "Failed: Tests fail.",
        ),
        (
            json!({"end_reason": "exited"}),
            "review",
            "The run ended without a report.",
        ),
        (
            json!({"end_reason": "stopped"}),
            "review",
            "The run was stopped.",
        ),
        (
            json!({"end_reason": "lost", "error": "The sandbox was reset"}),
            "failed",
            "Lost: The sandbox was reset",
        ),
        (
            json!({"end_reason": "start_failed"}),
            "failed",
            "Didn't start: The run didn't start.",
        ),
        (
            json!({"end_reason": "timed_out"}),
            "failed",
            "Timed out: The run passed its time limit and was stopped.",
        ),
    ];
    let mut rows = Vec::new();
    for (n, _) in endings.iter().enumerate() {
        rows.push(
            add(
                &pod,
                json!({"title": format!("Task {n}"), "status": "queued"}),
            )
            .await,
        );
    }
    for ((end, want, note), id) in endings.iter().zip(&rows) {
        let run_id = started(&pod, id).await;
        fake.run(&run_id, json!({"task_id": id, "state": "working"}));
        let mut end = end.clone();
        end["state"] = json!("ended");
        end["task_id"] = json!(id);
        fake.run(&run_id, end);
        status_is(&pod, id, want).await;
        until("noted", || async { field(&pod, id, "diagnosis") == *note }).await;
    }
}

#[tokio::test]
async fn refused_and_waiting_starts() {
    let (pod, fake, _) = signed_in().await;
    linked(&pod, json!(["todo", "queued"])).await;
    // The runner is away: the row stays queued and is sent again.
    fake.answers
        .lock()
        .unwrap()
        .push_back((502, json!({"error": "the sprite couldn't be reached"})));
    let waiting = add(&pod, json!({"title": "Busy one", "status": "queued"})).await;
    until("sent again", || async { fake.sent("start_run").len() >= 2 }).await;
    started(&pod, &waiting).await;
    assert_eq!(field(&pod, &waiting, "status"), "queued");

    // A workspace the runner doesn't know: the row fails with the reason.
    fake.answers
        .lock()
        .unwrap()
        .push_back((404, json!({"error": "no workspace nope"})));
    let refused = add(
        &pod,
        json!({"title": "Elsewhere", "status": "queued", "workspace": "nope"}),
    )
    .await;
    status_is(&pod, &refused, "failed").await;
    assert_eq!(
        field(&pod, &refused, "diagnosis"),
        "Couldn't start: no workspace nope"
    );
    // Retried by hand: queued again, a new run.
    set(&pod, &refused, json!({"status": "queued"})).await;
    started(&pod, &refused).await;

    // Unlinked: nothing more is sent.
    tool(&pod, "agents_link_tasks", json!({"collection": null})).await;
    let n = fake.sent("start_run").len();
    add(&pod, json!({"title": "After", "status": "queued"})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.sent("start_run").len(), n);
}

fn stops(fake: &Fake) -> Vec<Value> {
    fake.sent("stop_run")
}

#[tokio::test]
async fn done_and_todo_rows_keep_their_status() {
    let (pod, fake, _) = signed_in().await;
    linked(&pod, json!(["todo", "queued", "done"])).await;
    let done = add(
        &pod,
        json!({"title": "Shade pressed rows", "status": "queued"}),
    )
    .await;
    let todo = add(&pod, json!({"title": "Unqueue me", "status": "queued"})).await;
    let (done_run, todo_run) = (started(&pod, &done).await, started(&pod, &todo).await);
    fake.run(
        &done_run,
        json!({"task_id": done, "state": "needs_you",
        "needs_you": {"reason": "stopped", "text": "Want me to open the PR?"}}),
    );
    fake.run(&todo_run, json!({"task_id": todo, "state": "queued"}));
    status_is(&pod, &done, "needs_you").await;

    // You mark one done and move the other back to todo: their runs are
    // stopped, once each.
    set(&pod, &done, json!({"status": "done"})).await;
    set(&pod, &todo, json!({"status": "todo"})).await;
    until("both stopped", || async { stops(&fake).len() == 2 }).await;
    let mut stopped: Vec<String> = stops(&fake)
        .iter()
        .map(|c| c["run_id"].as_str().unwrap().to_owned())
        .collect();
    stopped.sort();
    let mut want = vec![done_run.clone(), todo_run.clone()];
    want.sort();
    assert_eq!(stopped, want);
    // The runner hasn't ended them yet: sent once, not on every pass.
    fake.run(&done_run, json!({"asleep": true}));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stops(&fake).len(), 2);

    // What the runs send after only adds to diagnosis.
    fake.run(
        &done_run,
        json!({"state": "ended", "end_reason": "stopped",
        "summary": "Stopped before the PR."}),
    );
    fake.run(
        &todo_run,
        json!({"state": "ended", "end_reason": "stopped"}),
    );
    until("noted", || async {
        field(&pod, &done, "diagnosis") == "Stopped before the PR."
            && field(&pod, &todo, "diagnosis") == "The run was stopped."
    })
    .await;
    assert_eq!(field(&pod, &done, "status"), "done");
    assert_eq!(field(&pod, &todo, "status"), "todo");
}

#[tokio::test]
async fn reopen_resumes_the_last_run() {
    let (pod, fake, _) = signed_in().await;
    linked(&pod, json!(["todo", "queued"])).await;
    let id = add(&pod, json!({"title": "Fix the footer", "status": "queued"})).await;
    let first = started(&pod, &id).await;
    fake.run(&first, json!({"task_id": id, "state": "working"}));
    status_is(&pod, &id, "running").await;
    let reopen = |message: &str| {
        call(
            &pod,
            "POST",
            "/v1/agents/tasks/reopen",
            Some("phone"),
            Some(json!({"id": id, "message": message})),
        )
    };
    // Not while the run goes on.
    let (s, b) = reopen("Also fix the header").await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"], "run_not_ended");

    fake.run(
        &first,
        json!({"state": "ended", "end_reason": "reported",
        "outcome": {"status": "review", "summary": "Opened a PR."}}),
    );
    status_is(&pod, &id, "review").await;
    fake.next_runs.lock().unwrap().push_back("run_again".into());
    let (s, b) = reopen("The Android build fails, fix it").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["run_id"], "run_again");
    assert_eq!(
        fake.sent("start_run").last().unwrap(),
        &json!({"type": "start_run", "workspace": "sample-repo", "task_id": id,
            "title": "Fix the footer", "prompt": "The Android build fails, fix it",
            "resume_from": first})
    );
    assert_eq!(field(&pod, &id, "status"), "queued");
    assert_eq!(field(&pod, &id, "run_id"), "run_again");
    // The old run doesn't pull the row back, and nothing else is started.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(field(&pod, &id, "status"), "queued");
    assert_eq!(fake.sent("start_run").len(), 2);
    // The row follows the new run.
    fake.run(
        "run_again",
        json!({"task_id": id, "state": "working", "resumed_from": first}),
    );
    status_is(&pod, &id, "running").await;

    // A row that isn't there, or a refusal from the runner.
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/agents/tasks/reopen",
        Some("phone"),
        Some(json!({"id": "nope", "message": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    fake.run(
        "run_again",
        json!({"state": "ended", "end_reason": "exited"}),
    );
    status_is(&pod, &id, "review").await;
    fake.answers
        .lock()
        .unwrap()
        .push_back((409, json!({"error": "run_again has no session to resume"})));
    let (s, b) = reopen("Once more").await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["detail"], "run_again has no session to resume");
    assert_eq!(field(&pod, &id, "status"), "review");

    // From a run's screen, which knows only the run.
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/tasks/reopen",
        Some("phone"),
        Some(json!({"run_id": "run_again", "message": "Try again"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        fake.sent("start_run").last().unwrap()["resume_from"],
        "run_again"
    );
    assert_eq!(field(&pod, &id, "status"), "queued");
    // A run that isn't a task's can't be reopened.
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/agents/tasks/reopen",
        Some("phone"),
        Some(json!({"run_id": "run_work", "message": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
