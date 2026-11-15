//! The layout's actions reach the runner as CAP commands.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::runner::{call, pod, signed_in};

#[tokio::test]
async fn actions_send_their_commands() {
    let (pod, fake, _) = signed_in().await;
    let cases: [(&str, Value, Value); 5] = [
        (
            "/v1/agents/sessions/stop",
            json!({"run_id": "run_work"}),
            json!({"type": "stop_run", "run_id": "run_work"}),
        ),
        // Layouts made before runs send the entity's id as session_id.
        (
            "/v1/agents/sessions/stop",
            json!({"session_id": "run_work"}),
            json!({"type": "stop_run", "run_id": "run_work"}),
        ),
        (
            "/v1/agents/sandboxes/wake",
            json!({"workspace": "sample-repo"}),
            json!({"type": "wake_sprite", "workspace": "sample-repo", "minutes": 30}),
        ),
        (
            "/v1/agents/sandboxes/repair",
            json!({"workspace": "sample-repo"}),
            json!({"type": "repair_sprite", "workspace": "sample-repo"}),
        ),
        (
            "/v1/agents/sandboxes/wake",
            json!({"workspace": "sample-repo", "minutes": "60"}),
            json!({"type": "wake_sprite", "workspace": "sample-repo", "minutes": 60}),
        ),
    ];
    for (path, body, sent) in cases {
        let (s, answer) = call(&pod, "POST", path, Some("phone"), Some(body)).await;
        assert_eq!(s, StatusCode::OK, "{path}: {answer}");
        assert_eq!(fake.commands.lock().unwrap().last(), Some(&sent), "{path}");
    }
    fake.next_runs
        .lock()
        .unwrap()
        .push_back("run_started".to_owned());
    let (s, answer) = call(
        &pod,
        "POST",
        "/v1/agents/tasks/start",
        Some("phone"),
        Some(
            json!({"workspace": "sample-repo", "title": "Add tests", "prompt": "Cover the parser",
            "task_id": "pod/abc", "model": "a-model", "max_runtime_min": 45}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{answer}");
    assert_eq!(answer["run_id"], "run_started");
    let sent = fake.sent("start_run").pop().unwrap();
    assert_eq!(
        (
            &sent["work_item_id"],
            &sent["origin"],
            &sent["model"],
            &sent["max_runtime_min"]
        ),
        (
            &json!("pod/abc"),
            &json!("task"),
            &json!("a-model"),
            &json!(45)
        )
    );
    assert!(sent["request_id"].as_str().unwrap().starts_with("start_"));
    assert_eq!(
        (&sent["title"], &sent["prompt"], &sent["workspace"]),
        (
            &json!("Add tests"),
            &json!("Cover the parser"),
            &json!("sample-repo")
        )
    );
}

#[tokio::test]
async fn mark_reviewed_goes_to_the_runner() {
    let (pod, fake, _) = signed_in().await;
    fake.run(
        "run_rv",
        json!({"title": "Look at me", "state": "ended", "end_reason": "stopped",
            "outcome": {"state": "needs_review"}}),
    );
    let path = "/v1/agents/sessions/timeline?id=run_rv";
    crate::runner::until("listed", || async {
        call(&pod, "GET", path, Some("phone"), None).await.1["entities"]["agents://sessions/run_rv"]
            ["needs_review"]
            == true
    })
    .await;
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/sessions/review",
        Some("phone"),
        Some(json!({"run_id": "run_rv"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        fake.sent("mark_reviewed"),
        [json!({"type": "mark_reviewed", "run_id": "run_rv"})]
    );
    // The runner's answer shows at once: its outcome is done, reviewed.
    let shown = call(&pod, "GET", path, Some("phone"), None).await.1["entities"]
        ["agents://sessions/run_rv"]
        .clone();
    assert_eq!(shown["status_text"], "Reviewed", "{shown}");
    assert_eq!(shown["needs_review"], false);
    assert_eq!(shown["reviewed"], true);
    // A run that doesn't need review: the runner's 409 comes through.
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/sessions/review",
        Some("phone"),
        Some(json!({"run_id": "run_rv"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"], "not_needing_review");
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/agents/sessions/review",
        Some("phone"),
        Some(json!({"run_id": "run_nowhere"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Nothing of it is kept on the pod alone.
    let tables: i64 = pod
        .store()
        .with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'agents_reviewed'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(tables, 0);
}

#[tokio::test]
async fn runner_refusals_come_through() {
    let (pod, fake, _) = signed_in().await;
    fake.answers
        .lock()
        .unwrap()
        .push_back((404, json!({"error": "no workspace w"})));
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/tasks/start",
        Some("phone"),
        Some(json!({"workspace": "w", "title": "t", "prompt": "p"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(b["error"], "runner_refused");
    assert_eq!(b["detail"], "no workspace w");
    fake.answers
        .lock()
        .unwrap()
        .push_back((502, json!({"error": "the sprite couldn't be reached"})));
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/sandboxes/wake",
        Some("phone"),
        Some(json!({"workspace": "sample-repo"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{b}");
    assert_eq!(b["error"], "runner_unreachable");
    // A token the runner no longer takes is never the client's 401.
    fake.answers
        .lock()
        .unwrap()
        .push_back((401, json!({"error": "unauthorized"})));
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/sessions/stop",
        Some("phone"),
        Some(json!({"run_id": "run_x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{b}");
    assert_eq!(b["error"], "runner_refused_token");
    // Bad requests never reach the runner.
    let before = fake.commands.lock().unwrap().len();
    for (path, body) in [
        (
            "/v1/agents/sandboxes/wake",
            json!({"workspace": "w", "minutes": 0}),
        ),
        (
            "/v1/agents/sandboxes/wake",
            json!({"workspace": "w", "minutes": 61}),
        ),
        ("/v1/agents/sandboxes/repair", json!({})),
        (
            "/v1/agents/tasks/start",
            json!({"workspace": "w", "title": "t"}),
        ),
        (
            "/v1/agents/tasks/start",
            json!({"workspace": "w", "title": "t", "prompt": "p", "task_id": "sch_abc"}),
        ),
        ("/v1/agents/sessions/stop", json!({"run_id": "a/b"})),
        ("/v1/agents/tasks/reopen", json!({"id": "r1"})),
        ("/v1/agents/schedules/pause", json!({})),
        (
            "/v1/agents/tasks/start",
            json!({"workspace": "w", "title": "t", "prompt": "p", "max_runtime_min": 481}),
        ),
    ] {
        let (s, b) = call(&pod, "POST", path, Some("phone"), Some(body)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{path}: {b}");
    }
    assert_eq!(fake.commands.lock().unwrap().len(), before);
}

#[tokio::test]
async fn actions_need_a_signed_in_pod_and_a_session() {
    let pod = pod();
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/sessions/stop",
        Some("phone"),
        Some(json!({"session_id": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"], "not_signed_in");
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/agents/sessions/stop",
        None,
        Some(json!({"session_id": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/agents/sessions/stop",
        Some("stale"),
        Some(json!({"session_id": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
