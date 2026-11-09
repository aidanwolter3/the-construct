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

    let r = "run_work";
    fake.runs
        .lock()
        .unwrap()
        .insert(r.into(), crate::runner::snapshot_data()["runs"][0].clone());
    // Waiting for a permission: one notice, however often the runner says so.
    let ask = json!({"state": "needs_you", "needs_you": {"reason": "permission", "text": "Bash: rm -rf build"}});
    fake.run(r, ask.clone());
    fake.run(r, ask.clone());
    fake.run(r, json!({"asleep": true}));
    // Answered, then a question: a second notice.
    fake.run(
        r,
        json!({"state": "working", "needs_you": null, "asleep": false}),
    );
    fake.run(
        r,
        json!({"state": "needs_you", "needs_you": {"reason": "question",
        "text": "Open the PR now or wait for the Android check?"}}),
    );
    fake.run(
        r,
        json!({"state": "ended", "end_reason": "reported", "needs_you": null,
        "outcome": {"status": "review", "summary": "Tidied the readme.",
        "pr_url": "https://github.com/sample-owner/sample-repo/pull/12"}}),
    );
    // A copy of the same end (a higher version): nothing more.
    fake.run(r, json!({"cost_usd": 0.42}));
    // A run that is stopped notifies nothing; one that is lost warns.
    fake.run(
        "run_stopped",
        json!({"title": "Try a thing", "state": "working"}),
    );
    fake.run(
        "run_stopped",
        json!({"state": "ended", "end_reason": "stopped"}),
    );
    fake.run(
        "run_lost",
        json!({"title": "Shade rows", "state": "working"}),
    );
    fake.run(
        "run_lost",
        json!({"state": "ended", "end_reason": "lost",
        "error": "The sandbox was reset"}),
    );
    let check = |v: i64, status: &str| {
        json!({"id": "egress", "title": "Network egress", "status": status, "version": v,
            "detail": "An unknown host was contacted"})
    };
    for (v, status) in [(2, "warn"), (3, "warn"), (4, "ok"), (5, "warn")] {
        fake.push("check.updated", check(v, status));
    }

    until("six notifications", || async {
        notifications(&pod).await.len() >= 6
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
            "Lost: Shade rows",
            "Needs review: Tidy the readme",
            "Needs you: Tidy the readme",
            "Needs you: Tidy the readme",
            "Runner: Network egress",
            "Runner: Network egress",
        ],
        "{got:#?}"
    );
    assert!(got.iter().all(|n| n["app_id"] == app.as_str()));
    let ended = got
        .iter()
        .find(|n| n["title"] == "Needs review: Tidy the readme")
        .unwrap();
    assert_eq!(
        ended["body"],
        "Tidied the readme.\nhttps://github.com/sample-owner/sample-repo/pull/12"
    );
    assert_eq!(ended["channel"], "sessions");
    let mut asks: Vec<&str> = got
        .iter()
        .filter(|n| n["channel"] == "needs_you")
        .map(|n| n["body"].as_str().unwrap())
        .collect();
    asks.sort();
    assert_eq!(
        asks,
        [
            "Bash: rm -rf build",
            "Open the PR now or wait for the Android check?"
        ]
    );
    let lost = got
        .iter()
        .find(|n| n["title"] == "Lost: Shade rows")
        .unwrap();
    assert_eq!(
        (&lost["channel"], &lost["body"]),
        (&json!("warnings"), &json!("The sandbox was reset"))
    );
}
