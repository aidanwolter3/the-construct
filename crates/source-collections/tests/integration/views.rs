//! Views (decision `collection-views`) on a real pod: Claude declares views
//! on a collection; the feed and the rows path serve each as
//! `collection:<c>:view:<v>`; layouts may list a view (its `$item` fields
//! checked) but not a view that doesn't exist; a change that would drop a
//! view a layout lists is refused; dated views are told to reload when the
//! pod's day changes.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use source_collections::{CollectionsAdapter, remind, views};
use tower::ServiceExt;

fn pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(CollectionsAdapter::new()),
        source_shell::adapter(),
    ]));
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

async fn add(pod: &Pod, title: &str, day: &str, done: bool) -> String {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/collections/chores/create",
        "phone",
        Some(json!({"title": title, "day": day, "done": done})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

fn uri(id: &str) -> String {
    format!("collections://chores/{id}")
}

fn layout(key: &str, binding: &str) -> Value {
    json!({"title": "Chores", "feeds": [["collections", "chores"]],
        "layout": {"type": "core://Workspace", "id": "x", "title": "Chores", "slots": [
            {"slot_id": "list", "provider_uri": "core://custom", "content": {
                "type": "core://List", "query_key": key,
                "item_template": {"type": "core://Text", "style": "Body",
                    "value_expr": binding}}}]}})
}

#[tokio::test]
async fn views_are_served_checked_and_kept() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "chores", "title": "Chores", "fields": [
            {"name": "title", "type": "text"},
            {"name": "done", "type": "bool", "default": false},
            {"name": "day", "type": "date", "optional": true}],
            "views": [
                {"name": "upcoming", "where": {"field": "done", "equals": false},
                 "date": {"field": "day", "from_days": 0}, "order_by": "day", "limit": 2},
                {"name": "today", "date": {"field": "day", "from_days": 0, "to_days": 0}}]}),
    )
    .await;
    let now = pod_core::util::now_secs();
    let utc = pod_core::tz::parse("UTC").unwrap();
    let day = |n: i64| remind::day(now + n * 86_400, &utc);
    let later = add(&pod, "Clean the attic", &day(5), false).await;
    let past = add(&pod, "Water the ferns", &day(-1), false).await;
    let today = add(&pod, "Sweep the porch", &day(0), false).await;
    let done = add(&pod, "Wash the van", &day(1), true).await;
    let soon = add(&pod, "Oil the hinges", &day(2), false).await;

    let (status, data) = call(&pod, "GET", "/v1/collections/chores/rows", "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    let q = &data["queries"];
    assert_eq!(
        q["collection:chores:view:upcoming"],
        json!([uri(&today), uri(&soon)])
    );
    assert_eq!(q["collection:chores:view:today"], json!([uri(&today)]));
    assert_eq!(q["collection:chores"].as_array().unwrap().len(), 5);
    for id in [&later, &past, &done] {
        assert!(data["entities"].get(uri(id)).is_some());
    }

    // A layout may list a view, its rows' fields checked.
    ok(
        &pod,
        "create_layout",
        layout("collection:chores:view:upcoming", "$item.title"),
    )
    .await;
    let (err, e) = tool(
        &pod,
        "create_layout",
        layout("collection:chores:view:upcoming", "$item.colour"),
    )
    .await;
    assert!(err && e.to_string().contains("$item.colour"), "{e}");
    let (err, e) = tool(
        &pod,
        "create_layout",
        layout("collection:chores:view:someday", "$item.title"),
    )
    .await;
    assert!(err && e.to_string().contains("no view 'someday'"), "{e}");
    assert!(
        e.to_string().contains("collection:chores:view:today"),
        "{e}"
    );

    // Removing a view a layout lists is refused; replacing it is fine.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "chores", "changes": [{"op": "remove_view", "view": "upcoming"}]}),
    )
    .await;
    assert!(err, "{e}");
    assert_eq!(e["error"], "breaks_layout");
    let changed = ok(
        &pod,
        "apply_change",
        json!({"collection": "chores", "changes": [{"op": "set_view", "view":
            {"name": "upcoming", "date": {"field": "day", "from_days": 0}, "order_by": "day",
             "descending": true, "limit": 1}}]}),
    )
    .await;
    assert_eq!(changed["definition"]["views"][0]["limit"], 1);
    let (_, data) = call(&pod, "GET", "/v1/collections/chores/rows", "phone", None).await;
    assert_eq!(
        data["queries"]["collection:chores:view:upcoming"],
        json!([uri(&later)])
    );
    // Removing the field a view reads is refused until the view changes.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "chores", "changes": [{"op": "remove_field", "field": "day"}]}),
    )
    .await;
    assert!(
        err && e.to_string().contains("view 'upcoming' reads 'day'"),
        "{e}"
    );

    let d = ok(&pod, "describe_collection", json!({"collection": "chores"})).await;
    let views = d["definition"]["views"].as_array().unwrap();
    assert_eq!(views.len(), 2, "{d}");
    assert_eq!(views[1]["query_key"], "collection:chores:view:today");
    assert_eq!(
        views[1]["date"],
        json!({"field": "day", "from_days": 0, "to_days": 0})
    );

    // Bad views are refused at definition.
    let (err, e) = tool(
        &pod,
        "define_collection",
        json!({"name": "errands", "fields": [{"name": "title", "type": "text"}],
            "views": [{"name": "top", "limit": 500}]}),
    )
    .await;
    assert!(err, "{e}");
    assert_eq!(e["error"], "invalid_view");
    assert_eq!(e["at"], "/views/0");
}

#[tokio::test]
async fn dated_views_reload_when_the_day_changes() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "chores", "fields": [{"name": "title", "type": "text"}]}),
    )
    .await;
    let now = pod_core::util::now_secs();
    // No dated view: nothing to do.
    assert!(views::refresh_at_midnight(&pod, now).await.is_none());
    ok(
        &pod,
        "apply_change",
        json!({"collection": "chores", "changes": [
            {"op": "add_field", "name": "day", "type": "date", "optional": true},
            {"op": "set_view", "view": {"name": "today", "date": {"field": "day", "from_days": 0, "to_days": 0}}}]}),
    )
    .await;
    let mut events = pod.subscribe_data();
    let wait = views::refresh_at_midnight(&pod, now).await.unwrap();
    assert!(wait.as_secs() <= 86_400 + 5);
    let e = events.try_recv().expect("a data event");
    let scope = e.scope.expect("scoped");
    assert_eq!(scope.query_prefixes, ["collection:chores:view"]);
    assert!(scope.uri_prefixes.is_empty());
    // The same day: no event.
    views::refresh_at_midnight(&pod, now).await.unwrap();
    assert!(events.try_recv().is_err());
    // The next day: one again.
    views::refresh_at_midnight(&pod, now + 86_400)
        .await
        .unwrap();
    assert!(events.try_recv().is_ok());
}
