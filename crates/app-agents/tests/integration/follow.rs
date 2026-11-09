//! Following the runner: the snapshot, the stream resumed from its cursor,
//! records kept by version, and the last state served while the runner is
//! away.

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::json;

use crate::runner::{TOKEN, agents_layout, call, pod, signed_in, until, workspace};

#[tokio::test]
async fn the_mirror_keeps_the_newest_record() {
    let (pod, fake, _) = signed_in().await;
    fake.end_streams.store(true, Ordering::SeqCst);
    fake.run(
        "run_new",
        json!({"task_id": "pod/2", "title": "Add a changelog", "state": "working",
            "current_tool": "Edit: CHANGELOG.md", "session_id": "session_01New"}),
    );
    // A late copy of an older version of a run changes nothing.
    fake.push(
        "run.updated",
        json!({"id": "run_work", "version": 2, "state": "queued", "title": "Tidy the readme"}),
    );
    fake.push(
        "sprite.updated",
        json!({"name": "box-2", "version": 2, "state": "waking"}),
    );
    // The first stream starts at the snapshot's cursor and ends after the
    // backlog; the next one resumes after the last event.
    until("a stream resumed after event 13", || async {
        fake.sinces().contains(&13)
    })
    .await;
    assert_eq!(fake.sinces()[0], 10);
    let resumed = fake
        .streams
        .lock()
        .unwrap()
        .iter()
        .find(|(s, _)| *s == 13)
        .cloned();
    assert_eq!(resumed.unwrap().1.as_deref(), Some("13"));

    let app = agents_layout(&pod).await;
    let ws = workspace(&pod, &app).await;
    let e = &ws["entities"];
    let new = &e["agents://sessions/run_new"];
    assert_eq!(new["current_tool"], "Edit: CHANGELOG.md");
    assert_eq!(new["status_text"], "Working");
    assert_eq!(new["claude_url"], "https://claude.ai/code/session_01New");
    assert_eq!(e["agents://sessions/run_work"]["state"], "working");
    assert_eq!(e["agents://runner/status"]["summary_text"], "2 working");
    let old = &e["agents://sessions/run_old"];
    assert_eq!(
        (&old["status_text"], &old["active"]),
        (&json!("Done"), &json!(false))
    );
    assert_eq!(
        ws["queries"]["agents:sessions"],
        json!([
            "agents://sessions/run_new",
            "agents://sessions/run_work",
            "agents://sessions/run_old"
        ])
    );
    assert_eq!(
        ws["queries"]["agents:sandboxes"][0],
        "agents://sandboxes/box-1"
    );
    assert_eq!(e["agents://sandboxes/box-2"]["state_text"], "Waking");
    let nightly = &e["agents://schedules/nightly-deps"];
    assert_eq!(nightly["state_text"], "Active");
    assert_eq!(nightly["last_status_text"], "Done");
    assert_eq!(nightly["last_cost_text"], "$0.31");
    assert_eq!(nightly["last_summary"], "Bumped two packages.");
    assert_eq!(e["agents://checks/egress"]["status_text"], "OK");
}

#[tokio::test]
async fn a_runs_log_is_read_from_the_runner() {
    let (pod, fake, _) = signed_in().await;
    let path = "/v1/agents/sessions/timeline?id=run_work";
    let (status, t) = call(&pod, "GET", path, Some("phone"), None).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    let lines: Vec<&str> = t["queries"]["agents:timeline:run_work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| t["entities"][u.as_str().unwrap()]["text"].as_str().unwrap())
        .collect();
    assert_eq!(lines, ["Queued", "Working", "Edit: CHANGELOG.md"]);
    assert_eq!(
        t["entities"]["agents://sessions/run_work"]["title"],
        "Tidy the readme"
    );
    // Read again soon after: from the short cache.
    call(&pod, "GET", path, Some("phone"), None).await;
    assert_eq!(fake.log_reads.lock().unwrap()["run_work"], 1);
    // A run the runner doesn't know: an empty log, not an error.
    let (status, t) = call(
        &pod,
        "GET",
        "/v1/agents/sessions/timeline?id=run_unknown",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["queries"]["agents:timeline:run_unknown"], json!([]));
    // Not a run id: refused before the runner is asked.
    let (status, _) = call(
        &pod,
        "GET",
        "/v1/agents/sessions/timeline?id=..%2Fx",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unreachable_runner_leaves_its_last_state() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    until("connected", || async {
        workspace(&pod, &app).await["entities"]["agents://runner/status"]["connected"] == true
    })
    .await;
    fake.down.store(true, Ordering::SeqCst);
    until("marked stale", || async {
        workspace(&pod, &app).await["entities"]["agents://runner/status"]["stale"] == true
    })
    .await;
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["queries"]["agents:sessions"][0], "agents://sessions/run_work",
        "the last runs are still served"
    );
    assert!(
        ws["entities"]["agents://runner/status"]["status_text"]
            .as_str()
            .unwrap()
            .starts_with("Can't reach 127.0.0.1"),
        "{ws}"
    );
    // Back again: reconnected, and what happened meanwhile arrives.
    fake.runs
        .lock()
        .unwrap()
        .insert("run_work".into(), snapshot_run_work());
    fake.run(
        "run_work",
        json!({"state": "ended", "end_reason": "reported",
            "outcome": {"status": "review", "summary": "Tidied.", "pr_url": null}}),
    );
    fake.down.store(false, Ordering::SeqCst);
    until("reconnected with the news", || async {
        let ws = workspace(&pod, &app).await;
        ws["entities"]["agents://runner/status"]["connected"] == true
            && ws["entities"]["agents://sessions/run_work"]["status_text"] == "Needs review"
    })
    .await;
}

/// The snapshot's `run_work`, for changing it with `Fake::run`.
fn snapshot_run_work() -> serde_json::Value {
    crate::runner::snapshot_data()["runs"][0].clone()
}

#[tokio::test]
async fn a_wrong_token_or_url_is_refused() {
    let (_pod, _fake, url) = signed_in().await;
    let pod = pod();
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/agents/sign-in",
        Some("phone"),
        Some(json!({"url": url, "token": "wrong"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "runner_refused_token");
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/agents/sign-in",
        Some("phone"),
        Some(json!({"url": format!("{url}/v2"), "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "invalid_url");
    // Nobody home: 502.
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/agents/sign-in",
        Some("phone"),
        Some(json!({"url": "http://127.0.0.1:9", "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    // Without a session: refused before anything is sent.
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/agents/sign-in",
        None,
        Some(json!({"url": url, "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn signing_out_forgets_the_runner() {
    let (pod, _fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/agents/sign-out",
        Some("phone"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ws = workspace(&pod, &app).await;
    assert_eq!(ws["entities"]["agents://runner/status"]["signed_in"], false);
    assert_eq!(ws["queries"]["agents:sessions"], json!([]));
    let stored: i64 = pod
        .store()
        .with_conn(|c| c.query_row("SELECT COUNT(*) FROM agents_runner", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(stored, 0);
    // Nothing keeps a signed-out pod open.
    assert_eq!(pod.poll_due().await, None);
}
