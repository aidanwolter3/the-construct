//! Notifications in the layout that shows the source (decision
//! `notification-owner`).

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::runner::{agents_layout, call, signed_in, until};

async fn notifications(pod: &pod_core::Pod) -> Vec<Value> {
    let (status, body) = call(pod, "GET", "/v1/notifications", Some("phone"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["notifications"].as_array().unwrap().clone()
}

#[tokio::test]
async fn runner_events_notify_the_agents_layout() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;

    let (status, body) = call(
        &pod,
        "GET",
        "/v1/notifications/channels",
        Some("phone"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mine: Vec<&str> = body["channels"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["app_id"] == app.as_str())
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(mine, ["needs_you", "sessions", "warnings"]);

    let s = "session_01Work";
    fake.push(json!({"type": "permission.requested", "session_id": s,
        "data": {"tool": "Bash", "detail": "rm -rf build"}}));
    fake.push(json!({"type": "session.status", "session_id": s, "status": "waiting_permission"}));
    fake.push(json!({"type": "session.status", "session_id": s, "status": "working"}));
    fake.push(json!({"type": "session.finished", "session_id": s, "data": {
        "summary": "Tidied the readme.", "pr_url": "https://github.com/sample-owner/sample-repo/pull/12",
        "cost_usd": 0.42, "input_tokens": 1000, "output_tokens": 200, "duration_s": 300, "reason": "done"}}));
    fake.push(
        json!({"type": "session.failed", "session_id": "session_01Other",
        "data": {"error": "The sandbox ran out of disk"}}),
    );
    for status in ["warn", "warn", "ok", "warn"] {
        fake.push(json!({"type": "security.check", "status": status,
            "data": {"id": "egress", "title": "Network egress", "detail": "An unknown host was contacted"}}));
    }
    fake.push(json!({"type": "schedule.missed", "data": {"schedule": "nightly-deps", "error": "The runner was asleep"}}));
    fake.push(json!({"type": "schedule.timeout",
        "data": {"schedule": "nightly-deps", "max_runtime_minutes": 30, "stopped": true}}));

    until("eight notifications", || async {
        notifications(&pod).await.len() >= 7
    })
    .await;
    // Let anything extra arrive before counting.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let got = notifications(&pod).await;
    let mut titles: Vec<String> = got
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_owned())
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        [
            "Failed: a session",
            "Finished: Tidy the readme",
            "Needs you: Tidy the readme",
            "Runner: Network egress",
            "Runner: Network egress",
            "Schedule missed: nightly-deps",
            "Schedule ran too long: nightly-deps",
        ],
        "{got:#?}"
    );
    assert!(got.iter().all(|n| n["app_id"] == app.as_str()));
    let finished = got
        .iter()
        .find(|n| n["title"] == "Finished: Tidy the readme")
        .unwrap();
    assert_eq!(
        finished["body"],
        "Tidied the readme.\nhttps://github.com/sample-owner/sample-repo/pull/12"
    );
    let needs = got
        .iter()
        .find(|n| n["title"].as_str().unwrap().starts_with("Needs"))
        .unwrap();
    assert_eq!(needs["body"], "Wants to use Bash: rm -rf build");
}
