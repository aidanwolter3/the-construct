//! Continue: a run that timed out reads Timed out (even when its outcome
//! needs review) and a new run carries it on, resuming its conversation;
//! a schedule's run can be reopened too.

use axum::http::StatusCode;
use serde_json::{Value, json};
use source_agents::CONTINUE_MESSAGE;

use crate::runner::{agents_layout, call, signed_in, until, workspace};

async fn post(pod: &pod_core::Pod, path: &str, body: Value) -> (StatusCode, Value) {
    call(pod, "POST", path, Some("phone"), Some(body)).await
}

#[tokio::test]
async fn a_timed_out_schedule_run_is_continued_once() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    let (s, b) = post(
        &pod,
        "/v1/agents/schedules/create",
        json!({"name": "audit", "cron": "0 4 * * *", "workspace": "sample-repo",
            "prompt": "Audit it.", "max_runtime_min": 90, "enabled": false}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let sched = b["schedule"]["id"].as_str().unwrap().to_owned();
    fake.run(
        "run_to",
        json!({"work_item_id": sched, "origin": "schedule", "title": "audit",
            "occurrence": "2026-11-15T04:58:00-07:00", "state": "ended",
            "end_reason": "timed_out", "outcome": {"state": "needs_review"},
            "session_id": "c5ad759a", "max_runtime_min": 90}),
    );
    let run = |ws: &Value| ws["entities"]["agents://sessions/run_to"].clone();
    until("the timed-out run is mirrored", || async {
        run(&workspace(&pod, &app).await)["state"] == "ended"
    })
    .await;
    let ws = workspace(&pod, &app).await;
    let e = run(&ws);
    assert_eq!(e["status_text"], "Timed out", "{e}");
    assert_eq!(e["needs_review"], true, "Mark reviewed still applies");
    assert_eq!(e["can_continue"], true);
    assert_eq!(
        ws["entities"]["agents://schedules/audit"]["last_status_text"],
        "Timed out"
    );

    // The limit was raised since: the continuation takes the new one.
    let (s, b) = post(
        &pod,
        "/v1/agents/schedules/update",
        json!({"name": "audit", "max_runtime_min": 120}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    fake.next_runs.lock().unwrap().push_back("run_on".into());
    let (s, b) = post(
        &pod,
        "/v1/agents/sessions/continue",
        json!({"run_id": "run_to"}),
    )
    .await;
    assert_eq!((s, &b["run_id"]), (StatusCode::OK, &json!("run_on")), "{b}");
    let sent = fake.sent("start_run").last().unwrap().clone();
    assert_eq!(sent["resume_from"], "run_to");
    assert_eq!(sent["origin"], "schedule");
    assert_eq!(sent["work_item_id"], json!(sched));
    assert_eq!(sent["max_runtime_min"], 120);
    assert_eq!(sent["prompt"], CONTINUE_MESSAGE);
    assert_eq!(sent["request_id"], "continue_run_to");
    assert!(sent.get("occurrence").is_none(), "{sent}");

    // Shown at once: the run is continued, and the schedule follows the
    // new run.
    let ws = workspace(&pod, &app).await;
    let e = run(&ws);
    assert_eq!(e["can_continue"], false, "{e}");
    assert_eq!(e["continued_by"], "run_on");
    assert_eq!(
        ws["entities"]["agents://sessions/run_on"]["resumed_from"],
        "run_to"
    );
    assert_eq!(
        ws["entities"]["agents://schedules/audit"]["last_run_id"],
        "run_on"
    );
    // A second press answers the same run and sends nothing.
    let n = fake.sent("start_run").len();
    let (s, b) = post(
        &pod,
        "/v1/agents/sessions/continue",
        json!({"run_id": "run_to"}),
    )
    .await;
    assert_eq!((s, &b["run_id"]), (StatusCode::OK, &json!("run_on")), "{b}");
    assert_eq!(fake.sent("start_run").len(), n);

    // Only a run that timed out can be continued.
    let (s, b) = post(
        &pod,
        "/v1/agents/sessions/continue",
        json!({"run_id": "run_old"}),
    )
    .await;
    assert_eq!(
        (s, &b["error"]),
        (StatusCode::CONFLICT, &json!("not_timed_out"))
    );
    let (s, _) = post(
        &pod,
        "/v1/agents/sessions/continue",
        json!({"run_id": "nope"}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_schedule_run_can_be_reopened() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    until("the snapshot is mirrored", || async {
        workspace(&pod, &app).await["entities"]["agents://sessions/run_old"]["state"] == "ended"
    })
    .await;
    // run_old is a schedule's (its schedule is gone): its own settings.
    let (s, b) = post(
        &pod,
        "/v1/agents/tasks/reopen",
        json!({"run_id": "run_old", "message": "Also bump the dev tools"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let sent = fake.sent("start_run").last().unwrap().clone();
    assert_eq!(sent["resume_from"], "run_old");
    assert_eq!(sent["origin"], "schedule");
    assert_eq!(sent["prompt"], "Also bump the dev tools");
    assert!(
        sent["request_id"]
            .as_str()
            .unwrap()
            .starts_with("reopen_run_old_")
    );
}
