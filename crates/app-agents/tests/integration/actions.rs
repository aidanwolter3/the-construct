//! The layout's actions reach the runner as CAP commands.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::runner::{call, pod, signed_in};

#[tokio::test]
async fn actions_send_their_commands() {
    let (pod, fake, _) = signed_in().await;
    let cases: [(&str, Value, Value); 7] = [
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
            "/v1/agents/tasks/start",
            json!({"workspace": "sample-repo", "title": "Add tests", "prompt": "Cover the parser", "task_id": "pod/abc"}),
            json!({"type": "start_run", "workspace": "sample-repo", "task_id": "pod/abc",
                "title": "Add tests", "prompt": "Cover the parser"}),
        ),
        (
            "/v1/agents/schedules/run",
            json!({"name": "nightly-deps"}),
            json!({"type": "run_schedule", "name": "nightly-deps"}),
        ),
        (
            "/v1/agents/schedules/pause",
            json!({"name": "nightly-deps"}),
            json!({"type": "pause_schedule", "name": "nightly-deps"}),
        ),
        (
            "/v1/agents/schedules/resume",
            json!({"name": "nightly-deps"}),
            json!({"type": "resume_schedule", "name": "nightly-deps"}),
        ),
    ];
    fake.next_runs
        .lock()
        .unwrap()
        .extend(["run_started".to_owned(), "run_scheduled".to_owned()]);
    for (path, body, sent) in cases {
        let (s, answer) = call(&pod, "POST", path, Some("phone"), Some(body)).await;
        assert_eq!(s, StatusCode::OK, "{path}: {answer}");
        assert_eq!(fake.commands.lock().unwrap().last(), Some(&sent), "{path}");
        if path.ends_with("/start") {
            assert_eq!(answer["run_id"], "run_started");
        }
        if path.ends_with("/run") {
            assert_eq!(answer["run_id"], "run_scheduled");
            assert_eq!(answer["task_id"], "schedule/nightly-deps/manual-1");
        }
    }
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
            "/v1/agents/tasks/start",
            json!({"workspace": "w", "title": "t"}),
        ),
        (
            "/v1/agents/tasks/start",
            json!({"workspace": "w", "title": "t", "prompt": "p", "task_id": "schedule/x/1"}),
        ),
        ("/v1/agents/sessions/stop", json!({"run_id": "a/b"})),
        ("/v1/agents/tasks/reopen", json!({"id": "r1"})),
        ("/v1/agents/schedules/pause", json!({})),
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
