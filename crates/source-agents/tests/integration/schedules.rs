//! Schedules owned by the pod (decision `agents-cap-v3`): defined with the
//! routes or the MCP tool, fired by the pod's timer with `start_run` once
//! per occurrence, missed after 2 h, paused and resumed without backfill.

use std::time::Duration;

use axum::http::StatusCode;
use pod_core::Pod;
use pod_core::tz::jiff::{Timestamp, ToSpan};
use serde_json::{Value, json};
use source_agents::{db, schedules};

use crate::notify::notifications;
use crate::runner::{agents_layout, call, signed_in, tool, until, workspace};

fn now() -> Timestamp {
    Timestamp::now()
}

/// A daily cron (UTC) at the minute `ago` before now, and that minute's
/// name (its occurrence: RFC 3339 with its offset).
fn daily_at(ago: pod_core::tz::jiff::Span) -> (String, String, Timestamp) {
    let at = now().checked_sub(ago).unwrap();
    let z = at.to_zoned(pod_core::tz::jiff::tz::TimeZone::UTC);
    (
        format!("{} {} * * *", z.minute(), z.hour()),
        z.strftime("%Y-%m-%dT%H:%M:00+00:00").to_string(),
        at,
    )
}

async fn create(pod: &Pod, body: Value) -> (StatusCode, Value) {
    call(
        pod,
        "POST",
        "/v1/agents/schedules/create",
        Some("phone"),
        Some(body),
    )
    .await
}

/// Make schedule `name` (made paused, so the timer leaves it alone) look
/// for its times from `since` on, as if the pod last looked then, and turn
/// it on when `enable`.
fn looked_from(pod: &Pod, name: &str, since: Timestamp, enable: bool) {
    let mut s = db::schedule(pod.store(), name).unwrap().unwrap();
    s.looked_until = since.as_millisecond();
    s.enabled = enable;
    assert!(db::put_schedule(pod.store(), &s).unwrap());
}

#[tokio::test]
async fn schedules_are_defined_on_the_pod() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    let (s, b) = create(
        &pod,
        json!({"name": "nightly-deps", "cron": "0 3 * * *", "timezone": "America/Denver",
            "workspace": "sample-repo", "prompt": "Bump what is out of date.",
            "max_runtime_min": "30"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["schedule"]["timezone"], "America/Denver");
    // Through the MCP tool, too; names are unique.
    let made = tool(
        &pod,
        "agents_schedules",
        json!({"op": "create", "name": "weekly-sweep", "cron": "30 6 * * mon",
            "workspace": "sample-repo", "prompt": "Sweep the logs.", "model": "a-model"}),
    )
    .await;
    assert_eq!(made["schedule"]["max_runtime_min"], 240);
    assert_eq!(made["schedule"]["timezone"], "UTC", "the pod's zone");
    let (s, b) = create(
        &pod,
        json!({"name": "nightly-deps", "cron": "0 4 * * *", "workspace": "w", "prompt": "p"}),
    )
    .await;
    assert_eq!(
        (s, &b["error"]),
        (StatusCode::CONFLICT, &json!("name_taken"))
    );
    let (s, _) = create(
        &pod,
        json!({"name": "bad", "cron": "0 25 * * *", "workspace": "w", "prompt": "p"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["queries"]["agents:schedules"],
        json!([
            "agents://schedules/nightly-deps",
            "agents://schedules/weekly-sweep"
        ])
    );
    let e = &ws["entities"]["agents://schedules/nightly-deps"];
    assert_eq!(e["state_text"], "Active");
    assert!(
        e["next_run_text"].as_str().unwrap().ends_with("03:00"),
        "{e}"
    );
    assert_eq!(e["max_runtime_min"], 30);
    assert_eq!(e["model_text"], "Default model");
    assert_eq!(e["ran"], false);

    // Edit: blank fields keep their value.
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/schedules/update",
        Some("phone"),
        Some(
            json!({"name": "nightly-deps", "new_name": "", "cron": "15 3 * * *", "prompt": "",
            "model": "", "max_runtime_min": ""}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["schedule"]["cron"], "15 3 * * *");
    assert_eq!(b["schedule"]["prompt"], "Bump what is out of date.");
    // Delete.
    let gone = tool(
        &pod,
        "agents_schedules",
        json!({"op": "delete", "name": "weekly-sweep"}),
    )
    .await;
    assert_eq!(gone["deleted"], "weekly-sweep");
    let listed = tool(&pod, "agents_schedules", json!({"op": "list"})).await;
    assert_eq!(listed["schedules"].as_array().unwrap().len(), 1);
    // Nothing went to the runner: it keeps no schedules.
    assert!(fake.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_due_occurrence_is_sent_once() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    let (cron, minute, at) = daily_at(30.minutes());
    // The runner is away for the first try: the occurrence waits.
    fake.answers
        .lock()
        .unwrap()
        .push_back((502, json!({"error": "unreachable"})));
    fake.next_runs.lock().unwrap().push_back("run_sched".into());
    let (s, b) = create(
        &pod,
        json!({"name": "tidy", "cron": cron, "timezone": "UTC", "workspace": "sample-repo",
            "prompt": "Tidy the docs.", "model": "a-model", "max_runtime_min": 45,
            "enabled": false}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let id = b["schedule"]["id"].as_str().unwrap().to_owned();
    looked_from(&pod, "tidy", at.checked_sub(10.minutes()).unwrap(), true);
    until("sent and answered", || async {
        fake.sent("start_run").len() >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let sent = fake.sent("start_run");
    assert_eq!(sent.len(), 2, "sent again once, then never: {sent:#?}");
    assert_eq!(sent[0], sent[1], "the same request both times");
    assert_eq!(
        sent[0],
        json!({"type": "start_run", "request_id": schedules::request_id(&id, &minute, false),
            "workspace": "sample-repo", "work_item_id": id, "occurrence": minute,
            "origin": "schedule", "title": "tidy", "prompt": "Tidy the docs.",
            "model": "a-model", "max_runtime_min": 45})
    );
    // The feed shows the run it answered, then what the runner says of it.
    fake.run(
        "run_sched",
        json!({"work_item_id": id, "origin": "schedule", "occurrence": minute, "title": "tidy",
            "state": "ended", "end_reason": "reported",
            "outcome": {"state": "pending", "stage": "ci"}}),
    );
    until("the last run shows", || async {
        let ws = workspace(&pod, &app).await;
        ws["entities"]["agents://schedules/tidy"]["last_status_text"] == "Waiting on CI"
    })
    .await;
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["entities"]["agents://schedules/tidy"]["last_run_id"],
        "run_sched"
    );
    assert_eq!(
        ws["entities"]["agents://sessions/run_sched"]["schedule"],
        "tidy"
    );
    // Run now: its own occurrence, this minute.
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/schedules/run",
        Some("phone"),
        Some(json!({"name": "tidy"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let manual = fake.sent("start_run").pop().unwrap();
    assert!(
        manual["request_id"].as_str().unwrap().ends_with("_now"),
        "{manual}"
    );
    assert_eq!(manual["work_item_id"], id.as_str());
}

#[tokio::test]
async fn an_occurrence_not_sent_in_two_hours_is_missed() {
    let (pod, fake, _) = signed_in().await;
    let _app = agents_layout(&pod).await;
    let (cron, minute, at) = daily_at(3.hours());
    let (s, b) = create(
        &pod,
        json!({"name": "late", "cron": cron, "timezone": "UTC", "workspace": "sample-repo",
            "prompt": "p", "enabled": false}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    // The pod was down from before it was due until now.
    looked_from(&pod, "late", at.checked_sub(10.minutes()).unwrap(), true);
    until("told", || async {
        notifications(&pod)
            .await
            .iter()
            .any(|n| n["title"] == "Missed: late")
    })
    .await;
    let n = notifications(&pod).await;
    let missed = n.iter().find(|n| n["title"] == "Missed: late").unwrap();
    assert_eq!(missed["channel"], "warnings");
    assert!(
        missed["body"]
            .as_str()
            .unwrap()
            .contains(&source_agents::cron::shown(&minute)),
        "{missed}"
    );
    assert!(fake.sent("start_run").is_empty());
    let last = db::last_occurrence(pod.store(), b["schedule"]["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(last.state, db::OCC_MISSED);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        notifications(&pod)
            .await
            .iter()
            .filter(|n| n["title"] == "Missed: late")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_paused_schedule_sends_nothing_and_resumes_from_now() {
    let (pod, fake, _) = signed_in().await;
    let app = agents_layout(&pod).await;
    let (cron, _, at) = daily_at(20.minutes());
    let (s, b) = create(
        &pod,
        json!({"name": "quiet", "cron": cron, "timezone": "UTC", "workspace": "sample-repo",
            "prompt": "p", "enabled": false}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    looked_from(&pod, "quiet", at.checked_sub(10.minutes()).unwrap(), false);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(fake.sent("start_run").is_empty());
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["entities"]["agents://schedules/quiet"]["next_run_text"],
        "Paused"
    );
    // Resumed: from now on, so the time it passed while paused isn't sent
    // (nor missed).
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/schedules/resume",
        Some("phone"),
        Some(json!({"name": "quiet"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(fake.sent("start_run").is_empty());
    assert!(notifications(&pod).await.is_empty());
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["entities"]["agents://schedules/quiet"]["state_text"],
        "Active"
    );
    // Pause asks first in the layout; here it just pauses.
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/agents/schedules/pause",
        Some("phone"),
        Some(json!({"name": "quiet"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["schedule"]["enabled"], false);
}
