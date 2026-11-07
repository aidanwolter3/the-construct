//! The layout's actions reach the runner as CAP commands.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::runner::{call, pod, signed_in};

#[tokio::test]
async fn actions_send_their_commands() {
    let (pod, fake, _) = signed_in().await;
    let cases: [(&str, Value, Value, u16); 6] = [
        (
            "/v1/agents/sessions/stop",
            json!({"session_id": "session_01Work"}),
            json!({"type": "stop_session", "session_id": "session_01Work"}),
            200,
        ),
        (
            "/v1/agents/sandboxes/wake",
            json!({"workspace": "sample-repo"}),
            json!({"type": "wake", "workspace": "sample-repo", "minutes": 30}),
            200,
        ),
        (
            "/v1/agents/tasks/start",
            json!({"workspace": "sample-repo", "title": "Add tests", "prompt": "Cover the parser", "task_id": "pod/abc"}),
            json!({"type": "start_task", "workspace": "sample-repo", "task_id": "pod/abc",
                "title": "Add tests", "prompt": "Cover the parser"}),
            200,
        ),
        (
            "/v1/agents/schedules/run",
            json!({"name": "nightly-deps"}),
            json!({"type": "run_schedule", "name": "nightly-deps"}),
            200,
        ),
        (
            "/v1/agents/schedules/pause",
            json!({"name": "nightly-deps"}),
            json!({"type": "pause_schedule", "name": "nightly-deps"}),
            200,
        ),
        (
            "/v1/agents/schedules/resume",
            json!({"name": "nightly-deps"}),
            json!({"type": "resume_schedule", "name": "nightly-deps"}),
            200,
        ),
    ];
    for (path, body, sent, status) in cases {
        let (s, answer) = call(&pod, "POST", path, Some("phone"), Some(body)).await;
        assert_eq!(s.as_u16(), status, "{path}: {answer}");
        assert_eq!(fake.commands.lock().unwrap().last(), Some(&sent), "{path}");
        if path.ends_with("/run") {
            assert_eq!(answer["run_key"], "nightly-deps/manual-1");
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
        .push_back((429, json!({"error": "at the concurrency limit"})));
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/tasks/start",
        Some("phone"),
        Some(json!({"workspace": "w", "title": "t", "prompt": "p"})),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(b["error"], "runner_busy");
    assert_eq!(b["detail"], "at the concurrency limit");
    fake.answers
        .lock()
        .unwrap()
        .push_back((404, json!({"error": "no such schedule"})));
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/schedules/run",
        Some("phone"),
        Some(json!({"name": "nope"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(b["detail"], "no such schedule");
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
        Some(json!({"session_id": "x"})),
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
