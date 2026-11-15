//! Following the runner: the snapshot, catching up from the cursor with the
//! event log's pages, the stream resumed from there, a snapshot only when
//! the cursor is older than the log (410), records kept by version, and the
//! last state served while the runner is away.

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
        json!({"work_item_id": "pod/2", "title": "Add a changelog", "state": "working",
            "current_tool": "Edit: CHANGELOG.md", "session_id": "session_01New"}),
    );
    // A late copy of an older version of a run changes nothing.
    fake.push(
        "run.updated",
        json!({"id": "run_work", "version": 2, "state": "queued", "title": "Tidy the readme"}),
    );
    fake.push(
        "sprite.updated",
        json!({"name": "box-2", "version": 2, "state": "degraded", "reason": "Storage not ready",
            "preflight": {"ok": false, "failures": ["storage"]}}),
    );
    // Each stream ends after its backlog; the next connection catches up
    // from the cursor with a page (no second snapshot) and resumes after the
    // last event.
    until("a stream resumed after event 13", || async {
        fake.sinces().contains(&13)
    })
    .await;
    // The events came before the stream opened: a page from the cursor
    // read them, and every connection after resumes from 13.
    assert_eq!(fake.pages.lock().unwrap()[0], 10);
    assert_eq!(fake.snapshots.load(Ordering::SeqCst), 1);
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
    let box2 = &e["agents://sandboxes/box-2"];
    assert_eq!(box2["state_text"], "Degraded");
    assert_eq!(box2["reason"], "Storage not ready");
    assert_eq!(box2["preflight_text"], "Preflight failed: storage");
    assert_eq!(
        ws["queries"]["agents:sandboxes"][0], "agents://sandboxes/box-2",
        "troubled sandboxes come first"
    );
    assert_eq!(
        e["agents://sessions/run_work"]["state_detail"],
        "waiting on 1 background task"
    );
    assert_eq!(
        e["agents://sessions/run_old"]["outcome_summary"],
        "Bumped two packages."
    );
    // The runner keeps no schedules: the pod has none yet.
    assert_eq!(ws["queries"]["agents:schedules"], json!([]));
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
        json!({"state": "ended", "end_reason": "stopped", "outcome": {"state": "needs_review"}}),
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
        Some(json!({"url": format!("{url}/v3"), "token": TOKEN})),
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
async fn an_oversized_snapshot_is_refused() {
    let fake = std::sync::Arc::new(crate::runner::Fake::default());
    let mut snap = crate::runner::snapshot_data();
    snap["padding"] = json!("x".repeat(app_agents::cap::MAX_BODY + 1));
    *fake.snapshot.lock().unwrap() = snap;
    let url = crate::runner::serve(fake).await;
    let (status, body) = call(
        &pod(),
        "POST",
        "/v1/agents/sign-in",
        Some("phone"),
        Some(json!({"url": url, "token": TOKEN})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
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

#[tokio::test]
async fn a_gap_is_replayed_from_the_cursor() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    until("following", || async { !fake.sinces().is_empty() }).await;
    fake.down.store(true, Ordering::SeqCst);
    until("marked stale", || async {
        workspace(&pod, &app).await["entities"]["agents://runner/status"]["stale"] == true
    })
    .await;
    // While away: a run's whole life, and more than one page of it.
    for (n, state) in ["queued", "launching", "working", "closing"]
        .iter()
        .enumerate()
    {
        fake.run(
            "run_away",
            json!({"title": "While you were out", "state": state, "updated_at": n}),
        );
    }
    fake.run(
        "run_away",
        json!({"state": "ended", "end_reason": "reported", "outcome": {"state": "pending",
            "stage": "deploy", "pr_url": "https://github.com/sample-owner/sample-repo/pull/7",
            "deploy_url": "https://github.com/sample-owner/sample-repo/actions/runs/99"}}),
    );
    let last = fake.last_seq();
    fake.down.store(false, Ordering::SeqCst);
    until("caught up", || async {
        let ws = workspace(&pod, &app).await;
        ws["entities"]["agents://sessions/run_away"]["status_text"] == "Waiting on deploy"
    })
    .await;
    let ws = workspace(&pod, &app).await;
    let away = &ws["entities"]["agents://sessions/run_away"];
    assert_eq!(
        away["deploy_url"],
        "https://github.com/sample-owner/sample-repo/actions/runs/99"
    );
    assert_eq!(away["stage"], "deploy");
    // Caught up with pages, not a snapshot, and the cursor is stored.
    assert_eq!(fake.snapshots.load(Ordering::SeqCst), 1);
    assert!(fake.pages.lock().unwrap().len() >= 2, "more than one page");
    until("the cursor is stored", || async {
        app_agents::db::state(pod.store())
            .unwrap()
            .unwrap()
            .0
            .cursor
            == last
    })
    .await;
}

#[tokio::test]
async fn a_cursor_older_than_the_log_takes_a_snapshot() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    until("following", || async { !fake.sinces().is_empty() }).await;
    fake.down.store(true, Ordering::SeqCst);
    until("marked stale", || async {
        workspace(&pod, &app).await["entities"]["agents://runner/status"]["stale"] == true
    })
    .await;
    fake.run("run_late", json!({"title": "Late", "state": "working"}));
    // The log no longer reaches the pod's cursor (10).
    fake.oldest.store(11, Ordering::SeqCst);
    fake.down.store(false, Ordering::SeqCst);
    until("a snapshot after the 410", || async {
        fake.snapshots.load(Ordering::SeqCst) == 2
    })
    .await;
    until("the run is listed", || async {
        workspace(&pod, &app).await["entities"]["agents://sessions/run_late"]["status_text"]
            == "Working"
    })
    .await;
}
