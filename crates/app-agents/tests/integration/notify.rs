//! Notifications in the layout that shows the source (decision
//! `notification-owner`), worked out from the runner's events, replayed
//! ones included (decision `agents-cap-v3`).

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::runner::{agents_layout, call, signed_in, until, workspace};

pub async fn notifications(pod: &pod_core::Pod) -> Vec<Value> {
    let (status, body) = call(pod, "GET", "/v1/notifications", Some("phone"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["notifications"].as_array().unwrap().clone()
}

fn titles(got: &[Value]) -> Vec<String> {
    let mut t: Vec<String> = got
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_owned())
        .collect();
    t.sort();
    t
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
    // Waiting for a permission: one notice, however often the runner says
    // so, and parking is the same wait.
    let ask = json!({"state": "needs_you", "needs_you": {"reason": "permission", "text": "Bash: rm -rf build"}});
    fake.run(r, ask.clone());
    fake.run(r, ask.clone());
    fake.run(r, json!({"state": "parked"}));
    // (A wait answered within the same batch of events isn't news: let the
    // pod see this one first.)
    let asked = |n: usize| {
        let pod = pod.clone();
        async move {
            until("asked", || async {
                notifications(&pod)
                    .await
                    .iter()
                    .filter(|x| x["channel"] == "needs_you")
                    .count()
                    >= n
            })
            .await
        }
    };
    asked(1).await;
    // Answered, then a question: a second notice.
    fake.run(r, json!({"state": "working", "needs_you": null}));
    fake.run(
        r,
        json!({"state": "needs_you", "needs_you": {"reason": "question",
        "text": "Open the PR now or wait for the Android check?"}}),
    );
    asked(2).await;
    let pr = "https://github.com/sample-owner/sample-repo/pull/12";
    fake.run(
        r,
        json!({"state": "closing", "needs_you": null,
        "claim": {"status": "done", "summary": "Tidied the readme.", "pr_url": pr}}),
    );
    // Ended, its outcome waiting on GitHub: nothing yet; done: one notice.
    for stage in ["ci", "review", "deploy"] {
        fake.run(
            r,
            json!({"state": "ended", "end_reason": "reported",
            "outcome": {"state": "pending", "stage": stage, "pr_url": pr}}),
        );
    }
    fake.run(r, json!({"outcome": {"state": "done", "pr_url": pr}}));
    fake.run(r, json!({"cost_usd": 0.42}));
    // A run that is stopped needs review; one that is lost warns; one whose
    // deploy fails warns.
    fake.run(
        "run_stopped",
        json!({"title": "Try a thing", "state": "working"}),
    );
    fake.run(
        "run_stopped",
        json!({"state": "ended", "end_reason": "stopped", "outcome": {"state": "needs_review"}}),
    );
    fake.run(
        "run_lost",
        json!({"title": "Shade rows", "state": "working"}),
    );
    fake.run(
        "run_lost",
        json!({"state": "ended", "end_reason": "lost", "error": "sprite_down"}),
    );
    fake.run(
        "run_deploy",
        json!({"title": "Ship it", "state": "ended", "end_reason": "reported",
            "outcome": {"state": "failed", "reason": "deploy"}}),
    );
    // A sandbox goes down; another fails its disk a second time.
    fake.push(
        "sprite.updated",
        json!({"name": "box-1", "version": 5, "state": "down", "reason": "No heartbeat for 15 min"}),
    );
    fake.push(
        "sprite.updated",
        json!({"name": "box-2", "version": 2, "state": "degraded", "disk_failures_7d": 2}),
    );
    let check = |v: i64, status: &str| {
        json!({"id": "egress", "title": "Network egress", "status": status, "version": v,
            "detail": "An unknown host was contacted"})
    };
    for (v, status) in [(2, "warn"), (3, "warn"), (4, "ok"), (5, "warn")] {
        fake.push("check.updated", check(v, status));
    }

    let want = [
        "Deploy failed: Ship it",
        "Done: Tidy the readme",
        "Lost: Shade rows",
        "Needs review: Try a thing",
        "Needs you: Tidy the readme",
        "Needs you: Tidy the readme",
        "Runner: Network egress",
        "Runner: Network egress",
        "Sandbox box-1 is down",
        "Sandbox box-2 keeps failing its disk",
    ];
    for _ in 0..150 {
        if notifications(&pod).await.len() >= want.len() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // Let anything extra arrive before counting.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let got = notifications(&pod).await;
    assert_eq!(titles(&got), want, "{got:#?}");
    assert!(got.iter().all(|n| n["app_id"] == app.as_str()));
    let done = got
        .iter()
        .find(|n| n["title"] == "Done: Tidy the readme")
        .unwrap();
    assert_eq!(done["body"], format!("Tidied the readme.\n{pr}"));
    assert_eq!(done["channel"], "sessions");
    let lost = got
        .iter()
        .find(|n| n["title"] == "Lost: Shade rows")
        .unwrap();
    assert_eq!(
        (&lost["channel"], &lost["body"]),
        (&json!("warnings"), &json!("sprite_down"))
    );
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
}

#[tokio::test]
async fn a_run_that_started_and_ended_while_away_still_notifies() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    until("following", || async { !fake.sinces().is_empty() }).await;
    fake.down.store(true, Ordering::SeqCst);
    until("marked stale", || async {
        workspace(&pod, &app).await["entities"]["agents://runner/status"]["stale"] == true
    })
    .await;
    // Its whole life while the pod can't reach the runner, a question
    // answered meanwhile included.
    fake.run(
        "run_away",
        json!({"title": "Overnight fix", "state": "working"}),
    );
    fake.run(
        "run_away",
        json!({"state": "needs_you", "needs_you": {"reason": "question"}}),
    );
    fake.run("run_away", json!({"state": "working", "needs_you": null}));
    fake.run(
        "run_away",
        json!({"state": "ended", "end_reason": "reported",
            "claim": {"status": "done", "summary": "Fixed overnight."},
            "outcome": {"state": "done"}}),
    );
    fake.down.store(false, Ordering::SeqCst);
    until("notified", || async {
        notifications(&pod)
            .await
            .iter()
            .any(|n| n["title"] == "Done: Overnight fix")
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // The answered question isn't news any more.
    assert_eq!(
        titles(&notifications(&pod).await),
        ["Done: Overnight fix"],
        "only the ending"
    );
    // The log replayed again (a pod that crashed before saving): nothing
    // twice.
    app_agents::db::save(
        pod.store(),
        &{
            let mut st = app_agents::db::state(pod.store()).unwrap().unwrap().0;
            st.cursor = 10;
            st
        },
        0,
    )
    .unwrap();
    fake.down.store(true, Ordering::SeqCst);
    until("marked stale", || async {
        workspace(&pod, &app).await["entities"]["agents://runner/status"]["stale"] == true
    })
    .await;
    fake.down.store(false, Ordering::SeqCst);
    until("reconnected", || async {
        workspace(&pod, &app).await["entities"]["agents://runner/status"]["connected"] == true
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(notifications(&pod).await.len(), 1);
}

#[tokio::test]
async fn a_review_reminder_reaches_the_inbox_without_push() {
    let (pod, fake, _) = signed_in().await;
    let _app = agents_layout(&pod).await;
    let review = json!({"title": "Wait for me", "state": "ended", "end_reason": "reported",
        "outcome": {"state": "pending", "stage": "review",
            "pr_url": "https://github.com/sample-owner/sample-repo/pull/9"}});
    fake.run("run_rv", review);
    let mut record = fake.runs.lock().unwrap()["run_rv"].clone();
    record["version"] = json!(record["version"].as_i64().unwrap() + 1);
    fake.runs
        .lock()
        .unwrap()
        .insert("run_rv".into(), record.clone());
    {
        let mut events = fake.events.lock().unwrap();
        let seq = events.len() as i64 + 11;
        events.push(
            json!({"v": 3, "seq": seq, "ts": 0, "type": "outcome.updated",
            "reminder": true, "record": record}),
        );
    }
    until("reminded", || async {
        notifications(&pod)
            .await
            .iter()
            .any(|n| n["title"] == "Still waiting for your review: Wait for me")
    })
    .await;
    // Quiet: stored already read, so it is in the inbox and never pushed.
    let read: Option<i64> = pod
        .store()
        .with_conn(|c| {
            c.query_row(
                "SELECT read_at FROM notifications WHERE title LIKE 'Still waiting%'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert!(read.is_some(), "inbox only, no push");
}
