//! Reminders (decision `collection-reminders`, issue #57) on a real pod:
//! Claude adds a reminder to a todos collection; a todo due today notifies
//! once through the D52 pipeline (from the todo app, on the reminder's own
//! channel), a ticked one never, and the pod's time zone is an IANA name.

use std::sync::Arc;

use app_collections::CollectionsAdapter;
use app_collections::remind;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

fn pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(
        CollectionsAdapter::new(),
    )]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

async fn call(
    pod: &Pod,
    method: &str,
    path: &str,
    bearer: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header("authorization", format!("Bearer {bearer}"));
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = pod_core::app(pod.clone())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// An MCP tool call with the pod token: `(is_error, result)`.
async fn tool(pod: &Pod, name: &str, args: Value) -> (bool, Value) {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/mcp",
        "pod",
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": name, "arguments": args}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = &body["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("null");
    (r["isError"] == true, serde_json::from_str(text).unwrap())
}

async fn ok(pod: &Pod, name: &str, args: Value) -> Value {
    let (err, v) = tool(pod, name, args).await;
    assert!(!err, "{name}: {v}");
    v
}

/// The todos collection and an app listing it; answers the app's id.
async fn todo_app(pod: &Pod) -> String {
    ok(
        pod,
        "define_collection",
        json!({"name": "todos", "title": "Todos", "fields": [
            {"name": "title", "type": "text"},
            {"name": "done", "type": "bool", "default": false},
            {"name": "due", "type": "date", "optional": true}]}),
    )
    .await;
    let app = ok(
        pod,
        "create_app",
        json!({"title": "Todos", "feeds": [["collections", "todos"]],
            "layout": {"type": "core://Workspace", "id": "x", "title": "Todos", "slots": [
                {"slot_id": "list", "provider_uri": "core://custom", "content": {
                    "type": "core://List", "query_key": "collection:todos",
                    "item_template": {"type": "core://Text", "style": "Body",
                        "value_expr": "$item.title"}}}]}}),
    )
    .await;
    app["id"].as_str().unwrap().to_owned()
}

async fn add_todo(pod: &Pod, title: &str, due: &str, done: bool) -> String {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/collections/todos/create",
        "phone",
        Some(json!({"title": title, "due": due, "done": done})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

async fn notifications(pod: &Pod) -> Vec<Value> {
    let (status, body) = call(pod, "GET", "/v1/notifications", "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["notifications"].as_array().unwrap().clone()
}

fn today_and_tomorrow() -> (String, String) {
    let now = pod_core::util::now_secs();
    let utc = pod_core::tz::parse("UTC").unwrap();
    (remind::day(now, &utc), remind::day(now + 86_400, &utc))
}

#[tokio::test]
async fn a_todo_due_today_notifies_once_and_a_ticked_one_never() {
    let pod = pod();
    let app = todo_app(&pod).await;
    // The pod polls only for a registered device.
    let (status, _) = call(
        &pod,
        "PUT",
        "/v1/devices/phone-device-1",
        "phone",
        Some(json!({"platform": "android"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let changed = ok(
        &pod,
        "apply_change",
        json!({"collection": "todos", "changes": [{"op": "add_reminder", "field": "due",
            "at": "00:00", "title": "Due today: {title}", "body": "Due {due}",
            "unless": {"field": "done", "equals": true}}]}),
    )
    .await;
    let reminder = changed["definition"]["reminders"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(changed["definition"]["reminders"][0]["label"], "Todos: due");

    // The reminder is a channel of the todo app.
    let (_, channels) = call(&pod, "GET", "/v1/notifications/channels", "phone", None).await;
    let channel = channels["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == reminder.as_str())
        .unwrap_or_else(|| panic!("no channel in {channels}"))
        .clone();
    assert_eq!(channel["app_id"], app.as_str());
    assert_eq!(channel["enabled"], true);

    let (today, tomorrow) = today_and_tomorrow();
    let rent = add_todo(&pod, "Pay rent", &today, false).await;
    add_todo(&pod, "Already done", &today, true).await;
    add_todo(&pod, "Tomorrow's", &tomorrow, false).await;

    pod.poll_due().await;
    let got = notifications(&pod).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["title"], "Due today: Pay rent");
    assert_eq!(got[0]["body"], format!("Due {today}"));
    assert_eq!(got[0]["app_id"], app.as_str());
    assert_eq!(got[0]["channel"], reminder.as_str());
    assert_eq!(
        got[0]["target_uri"],
        format!("collections://todos/{rent}").as_str()
    );

    // Polling again sends nothing new.
    remind::run(&pod, pod_core::util::now_secs()).await;
    assert_eq!(notifications(&pod).await.len(), 1);

    // A todo ticked before its reminder is checked never notifies.
    let plants = add_todo(&pod, "Water plants", &today, false).await;
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/collections/todos/update",
        "phone",
        Some(json!({"id": plants, "done": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    remind::run(&pod, pod_core::util::now_secs()).await;
    assert_eq!(notifications(&pod).await.len(), 1);

    // Tomorrow's notifies tomorrow.
    remind::run(&pod, pod_core::util::now_secs() + 86_400).await;
    let got = notifications(&pod).await;
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0]["title"], "Due today: Tomorrow's");

    // Removing the reminder removes its channel.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "todos", "changes": [{"op": "remove_reminder", "reminder": reminder}]}),
    )
    .await;
    let (_, channels) = call(&pod, "GET", "/v1/notifications/channels", "phone", None).await;
    assert!(channels["channels"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn without_an_app_nothing_is_sent_and_bad_specs_are_refused() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "bills", "fields": [
            {"name": "name", "type": "text"}, {"name": "due", "type": "date"}]}),
    )
    .await;
    let (err, v) = tool(
        &pod,
        "apply_change",
        json!({"collection": "bills", "changes": [{"op": "add_reminder", "field": "name", "title": "x"}]}),
    )
    .await;
    assert!(
        err && v["detail"].as_str().unwrap().contains("not a date"),
        "{v}"
    );
    let (err, v) = tool(
        &pod,
        "apply_change",
        json!({"collection": "bills", "changes": [{"op": "add_reminder", "field": "due",
            "title": "x", "app": "c-aaaaaaaaaa"}]}),
    )
    .await;
    assert!(
        err && v["detail"].as_str().unwrap().contains("no custom app"),
        "{v}"
    );
    ok(
        &pod,
        "apply_change",
        json!({"collection": "bills", "changes": [{"op": "add_reminder", "field": "due",
            "at": "00:00", "title": "Pay {name}"}]}),
    )
    .await;
    let (today, _) = today_and_tomorrow();
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/collections/bills/create",
        "phone",
        Some(json!({"name": "Power", "due": today})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    remind::run(&pod, pod_core::util::now_secs()).await;
    assert!(notifications(&pod).await.is_empty());
}

#[tokio::test]
async fn the_time_zone_is_an_iana_name() {
    let pod = pod();
    assert_eq!(
        ok(&pod, "set_time_zone", json!({})).await["time_zone"],
        "UTC"
    );
    let (err, v) = tool(&pod, "set_time_zone", json!({"time_zone": "-07:00"})).await;
    assert!(err, "{v}");
    assert_eq!(
        ok(
            &pod,
            "set_time_zone",
            json!({"time_zone": "America/Denver"})
        )
        .await["time_zone"],
        "America/Denver"
    );
    assert_eq!(
        ok(&pod, "set_time_zone", json!({})).await["time_zone"],
        "America/Denver"
    );
}
