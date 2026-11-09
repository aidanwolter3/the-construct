//! Following the runner: the snapshot, the stream resumed from its cursor,
//! and the last state served while the runner is away.

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::json;

use crate::runner::{TOKEN, agents_layout, call, pod, signed_in, until, workspace};

#[tokio::test]
async fn the_stream_resumes_from_its_cursor() {
    let (pod, fake, _) = signed_in().await;
    fake.end_streams.store(true, Ordering::SeqCst);
    fake.push(
        json!({"type": "task.launched", "session_id": "session_01New", "task_id": "pod/2",
        "data": {"title": "Add a changelog", "workspace": "sample-repo"}}),
    );
    fake.push(json!({"type": "session.started", "session_id": "session_01New", "sprite": "box-1"}));
    fake.push(json!({"type": "tool.used", "session_id": "session_01New",
        "data": {"tool": "Edit", "detail": "CHANGELOG.md"}}));
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
    assert_eq!(
        e["agents://sessions/session_01New"]["current_tool"],
        "Edit: CHANGELOG.md"
    );
    assert_eq!(
        e["agents://sessions/session_01New"]["status_text"],
        "Working"
    );
    assert_eq!(e["agents://runner/status"]["summary_text"], "2 working");
    assert_eq!(
        ws["queries"]["agents:sandboxes"][0],
        "agents://sandboxes/box-1"
    );
    let nightly = &e["agents://schedules/nightly-deps"];
    assert_eq!(nightly["state_text"], "Active");
    assert_eq!(nightly["last_cost_text"], "$0.31");
    assert_eq!(nightly["last_summary"], "Bumped two packages.");
    assert_eq!(e["agents://checks/egress"]["status_text"], "OK");

    // The timeline.
    let (status, t) = call(
        &pod,
        "GET",
        "/v1/agents/sessions/timeline?id=session_01New",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    let lines: Vec<&str> = t["queries"]["agents:timeline:session_01New"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| t["entities"][u.as_str().unwrap()]["text"].as_str().unwrap())
        .collect();
    assert_eq!(lines, ["Launched", "Started", "Used Edit: CHANGELOG.md"]);
    assert_eq!(
        t["entities"]["agents://sessions/session_01New"]["title"],
        "Add a changelog"
    );
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
        ws["queries"]["agents:sessions"],
        json!(["agents://sessions/session_01Work"]),
        "the last sessions are still served"
    );
    assert!(
        ws["entities"]["agents://runner/status"]["status_text"]
            .as_str()
            .unwrap()
            .starts_with("Can't reach 127.0.0.1"),
        "{ws}"
    );
    // Back again: reconnected, and what happened meanwhile arrives.
    fake.push(
        json!({"type": "session.finished", "session_id": "session_01Work",
        "data": {"summary": "Tidied.", "cost_usd": 0.12}}),
    );
    fake.down.store(false, Ordering::SeqCst);
    until("reconnected with the news", || async {
        let ws = workspace(&pod, &app).await;
        ws["entities"]["agents://runner/status"]["connected"] == true
            && ws["entities"]["agents://sessions/session_01Work"]["status"] == "finished"
    })
    .await;
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
        Some(json!({"url": format!("{url}/v1"), "token": TOKEN})),
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
}

#[tokio::test]
async fn a_reconnect_reads_the_runners_schedules_again() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["entities"]["agents://schedules/nightly-deps"]["cron"],
        "0 3 * * *"
    );

    // The runner is redeployed with a new time and a new schedule; no event
    // says so. The stream it was following ends and the pod reconnects soon
    // after, with its stored state still fresh.
    {
        let mut snap = fake.snapshot.lock().unwrap();
        snap["schedules"][0]["cron"] = json!("30 4 * * *");
        let mut weekly = snap["schedules"][0].clone();
        weekly["name"] = json!("weekly-audit");
        snap["schedules"].as_array_mut().unwrap().push(weekly);
    }
    fake.end_streams.store(true, Ordering::SeqCst);

    until("the new schedule times", || async {
        let ws = workspace(&pod, &app).await;
        ws["entities"]["agents://schedules/nightly-deps"]["cron"] == "30 4 * * *"
            && ws["entities"]["agents://schedules/weekly-audit"]["cron"] == "30 4 * * *"
    })
    .await;
    // Only the set-up was read again: the stream still resumes from its cursor.
    assert!(
        fake.sinces().iter().all(|s| *s == 10),
        "{:?}",
        fake.sinces()
    );
}
