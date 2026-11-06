//! Collections (decision `collections`, issue #54) on a real pod: Claude
//! defines a `todos` collection and a todo app over MCP; clients add, tick
//! and delete todos through the layout's routes (outbox retries and pending
//! ids included) and other sessions hear about it; change sets rename and
//! add fields together with the app, refuse one that would break another
//! app, soft-remove and restore fields, convert types (keeping what doesn't
//! convert aside); row edits are previewed, confirmed and undone; caps hold.

use std::sync::Arc;
use std::time::Duration;

use app_collections::{Caps, CollectionsAdapter, KEEP_SECS};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

fn pod_with(caps: Caps) -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(
        CollectionsAdapter::with_caps(caps),
    )]));
    // The pod token's session (Claude through MCP, in tests) and two
    // devices' sessions.
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod.store().create_session(&session_hash("laptop")).unwrap();
    pod
}

fn pod() -> Pod {
    pod_with(Caps::default())
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

async fn define_todos(pod: &Pod) {
    ok(
        pod,
        "define_collection",
        json!({"name": "todos", "title": "Todos", "fields": [
            {"name": "title", "type": "text"},
            {"name": "done", "type": "bool", "default": false},
            {"name": "due", "type": "date", "optional": true},
            {"name": "position", "type": "number", "optional": true}],
            "note": "A todo list"}),
    )
    .await;
}

/// A todo app: a field to add one, and per todo its title, due date, a tick
/// and a delete button. `extra`: one more text binding per row.
fn todo_layout(extra: &str) -> Value {
    json!({"type": "core://Workspace", "id": "x", "title": "Todos", "slots": [
        {"slot_id": "add", "provider_uri": "core://custom", "content": {
            "type": "core://TextField", "field": "title", "placeholder": "Add a todo",
            "on_submit": {"intent": "action.mutation.create", "query_key": "collection:todos",
                "entity_uri": "collections://todos/pending-$nonce",
                "entity": {"id": "pending-$nonce", "title": "$form.title", "done": false, "deleted": false},
                "endpoint": "/v1/collections/todos/create",
                "payload": {"title": "$form.title", "client_id": "$nonce"},
                "refresh_workspace": true}}},
        {"slot_id": "list", "provider_uri": "core://custom", "content": {
            "type": "core://List", "query_key": "collection:todos",
            "where": {"field": "deleted", "equals": "false"}, "empty_text": "Nothing to do",
            "item_template": {"type": "core://Row", "children": [
                {"type": "core://IconButton", "icon": "check_box_outline_blank",
                 "active": "$item.done", "active_icon": "check_box", "content_description": "Done",
                 "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": "done", "value_expr": "!entity.done", "rollback_value": false,
                    "endpoint": "/v1/collections/todos/update",
                    "payload": {"id": "$item.id", "done": "$value"}}},
                {"type": "core://Text", "style": "Body", "value_expr": format!("$item.title {extra}")},
                {"type": "core://IconButton", "icon": "delete", "content_description": "Delete",
                 "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": "deleted", "value_expr": "true", "rollback_value": false,
                    "endpoint": "/v1/collections/todos/delete", "payload": {"id": "$item.id"}}}]}}}]})
}

async fn create_app(pod: &Pod, title: &str, layout: Value) -> String {
    let v = ok(
        pod,
        "create_app",
        json!({"title": title, "icon": "dashboard", "feeds": [["collections", "todos"]], "layout": layout}),
    )
    .await;
    v["id"].as_str().unwrap().to_owned()
}

/// The app's rows as served: `[(title, entity)]`.
async fn todos(pod: &Pod, app: &str) -> Vec<(String, Value)> {
    let (status, ws) = call(pod, "GET", &format!("/v1/workspaces/{app}"), "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    ws["queries"]["collection:todos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let e = ws["entities"][u.as_str().unwrap()].clone();
            (e["title"].as_str().unwrap_or("").to_owned(), e)
        })
        .collect()
}

async fn add(pod: &Pod, body: Value) -> (StatusCode, Value) {
    call(
        pod,
        "POST",
        "/v1/collections/todos/create",
        "phone",
        Some(body),
    )
    .await
}

#[tokio::test]
async fn a_todo_app_adds_ticks_and_removes_todos() {
    let pod = pod();
    define_todos(&pod).await;
    let app = create_app(&pod, "Todos", todo_layout("")).await;
    assert!(todos(&pod, &app).await.is_empty());

    let (s, milk) = add(&pod, json!({"title": "Milk", "client_id": "n1"})).await;
    assert_eq!(s, StatusCode::OK, "{milk}");
    // The outbox retries a create whose answer it never saw: the same row.
    let (_, again) = add(&pod, json!({"title": "Milk", "client_id": "n1"})).await;
    assert_eq!(again["id"], milk["id"]);
    add(
        &pod,
        json!({"title": "Eggs", "due": "2026-11-05", "client_id": "n2"}),
    )
    .await;
    let rows = todos(&pod, &app).await;
    assert_eq!(
        rows.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
        ["Milk", "Eggs"]
    );
    let (_, eggs) = &rows[1];
    assert_eq!(eggs["done"], false, "the default");
    assert_eq!(eggs["due"], "2026-11-05");
    assert_eq!(eggs["deleted"], false);
    assert_eq!(rows[0].1["due"], Value::Null);

    // Ticked before the pod answered the create: the pending id still finds it.
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/todos/update",
        "phone",
        Some(json!({"id": "pending-n2", "done": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(todos(&pod, &app).await[1].1["done"], true);
    // A form's text for a bool and a number are read leniently.
    let id = milk["id"].as_str().unwrap();
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/todos/update",
        "phone",
        Some(json!({"id": id, "done": "true", "position": "2"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(todos(&pod, &app).await[0].1["position"], 2);
    // Wrong types and missing titles are refused (the client rolls back).
    for bad in [
        json!({"id": id, "due": "someday"}),
        json!({"id": id, "title": ""}),
        json!({"id": id, "colour": "red"}),
    ] {
        let (s, e) = call(
            &pod,
            "POST",
            "/v1/collections/todos/update",
            "phone",
            Some(bad),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    }
    let (s, _) = add(&pod, json!({"due": "2026-11-05"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "a title is required");
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/todos/update",
        "phone",
        Some(json!({"id": "r_nope", "done": true})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Delete: gone from the list, still in the export (restorable).
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/todos/delete",
        "phone",
        Some(json!({"id": id})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let rows = todos(&pod, &app).await;
    assert_eq!(rows.len(), 1);
    let (_, export) = call(&pod, "GET", "/v1/collections/todos/export", "phone", None).await;
    assert_eq!(export["rows"].as_array().unwrap().len(), 2);
    assert!(export["rows"][0]["deleted_at"].is_string());
    // The data path serves the same list.
    let (_, data) = call(&pod, "GET", "/v1/collections/todos/rows", "phone", None).await;
    assert_eq!(
        data["queries"]["collection:todos"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (s, _) = call(&pod, "GET", "/v1/collections/notes/rows", "phone", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

async fn sse_text(body: &mut Body, wait: Duration) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, body.frame()).await {
        if let Some(d) = frame.data_ref() {
            text.push_str(&String::from_utf8_lossy(d));
        }
    }
    text
}

#[tokio::test]
async fn a_write_on_one_device_reloads_the_app_on_every_device() {
    let pod = pod();
    define_todos(&pod).await;
    let app = create_app(&pod, "Todos", todo_layout("")).await;
    let mut laptop = pod_core::app(pod.clone())
        .oneshot(
            Request::get(format!("/v1/workspaces/{app}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body();
    assert!(
        sse_text(&mut laptop, Duration::from_millis(200))
            .await
            .contains("event: layout")
    );
    add(&pod, json!({"title": "Milk"})).await;
    let t = sse_text(&mut laptop, Duration::from_millis(300)).await;
    assert!(t.contains("event: data") && t.contains(&app), "{t}");
}

#[tokio::test]
async fn bindings_must_fit_the_collection() {
    let pod = pod();
    define_todos(&pod).await;
    let (err, e) = tool(
        &pod,
        "create_app",
        json!({"title": "T", "feeds": [["collections", "todos"]],
        "layout": todo_layout("$item.colour")}),
    )
    .await;
    assert!(err);
    assert_eq!(e["error"], "invalid_layout", "{e}");
    assert!(
        e["detail"].as_str().unwrap().contains("$item.colour"),
        "{e}"
    );
    assert!(e["allowed"].as_array().unwrap().contains(&json!("due")));
    let (err, e) = tool(
        &pod,
        "create_app",
        json!({"title": "T", "feeds": [["collections", "notes"]],
        "layout": todo_layout("")}),
    )
    .await;
    assert!(
        err && e["detail"]
            .as_str()
            .unwrap()
            .contains("names no collection"),
        "{e}"
    );
    // The data source is listed with its routes, and the tools are there.
    let sources = ok(&pod, "list_data_sources", json!({})).await;
    assert_eq!(sources["sources"][0]["id"], "collections");
    let (_, tools) = call(
        &pod,
        "POST",
        "/v1/mcp",
        "pod",
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
    )
    .await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in [
        "define_collection",
        "apply_change",
        "edit_rows",
        "add_row",
        "undo_edit",
        "export_collection",
        "create_layout",
    ] {
        assert!(names.contains(&t), "{t}");
    }
    let d = ok(&pod, "describe_collection", json!({"collection": "todos"})).await;
    assert_eq!(d["used_by"], json!([]));
    assert!(d["layout_hints"]["toggle"].is_object());
    // The snippets are a valid app as they are.
    let h = &d["layout_hints"];
    let mut list = h["list"].clone();
    let mut row = h["row"].clone();
    // The due date goes under the title (the row's text column).
    assert_eq!(h["date"]["content"]["format"]["past"], "Overdue", "{h}");
    row["children"][1]["children"]
        .as_array_mut()
        .unwrap()
        .push(h["date"].clone());
    list["item_template"] = row;
    assert_eq!(h["count"]["value_expr"], "$count left");
    let layout = json!({"type": "core://Workspace", "title": "Todos", "slots": [
        {"slot_id": "count", "provider_uri": "core://custom", "content": h["count"]},
        {"slot_id": "add", "provider_uri": "core://custom", "content": h["add"]},
        {"slot_id": "list", "provider_uri": "core://custom", "content": list}]});
    let hinted = ok(
        &pod,
        "create_app",
        json!({"title": "Hinted", "feeds": h["feed"].as_array().map(|f| json!([f])).unwrap(),
        "layout": layout}),
    )
    .await;
    assert_eq!(d["definition"]["fields"][1]["name"], "done");
    // Only clients that draw dates and counts get it.
    let hinted = hinted["id"].as_str().unwrap();
    let (_, v) = call(
        &pod,
        "GET",
        &format!("/v1/apps/{hinted}/versions"),
        "phone",
        None,
    )
    .await;
    let features = v["versions"][0]["features"].as_array().unwrap();
    for f in ["count", "date_format", "todo_icons", "list", "row"] {
        assert!(features.contains(&json!(f)), "{f}: {v}");
    }
}

#[tokio::test]
async fn a_change_set_adds_and_renames_with_the_app_and_old_writes_still_land() {
    let pod = pod();
    define_todos(&pod).await;
    let app = create_app(&pod, "Todos", todo_layout("· due $item.due")).await;
    add(&pod, json!({"title": "Milk", "due": "2026-11-05"})).await;

    // The app must change with the rename: alone, the rename would keep
    // working through the alias, so check that the new layout uses it.
    let r = ok(&pod, "apply_change", json!({"collection": "todos", "base_version": 1,
        "note": "Priority, and due is now deadline",
        "changes": [
            {"op": "add_field", "name": "priority", "type": "choice", "options": ["low", "high"], "default": "low"},
            {"op": "rename_field", "field": "due", "to": "deadline"}],
        "apps": [{"app": app, "json_patch": [{"op": "replace",
            "path": "/slots/1/content/item_template/children/1/value_expr",
            "value": "$item.title · $item.priority · due $item.deadline"}]}]})).await;
    assert_eq!(r["version"], 2);
    assert_eq!(r["apps"][0]["app"], app);
    let (_, ws) = call(&pod, "GET", &format!("/v1/workspaces/{app}"), "phone", None).await;
    assert!(ws["workspace"].to_string().contains("$item.deadline"));
    let rows = todos(&pod, &app).await;
    assert_eq!(
        rows[0].1["priority"], "low",
        "existing rows read the default"
    );
    assert_eq!(rows[0].1["deadline"], "2026-11-05", "renamed, data unmoved");
    assert_eq!(
        rows[0].1["due"], "2026-11-05",
        "the old name stays an alias"
    );

    // A todo typed offline before the change (old field names) still arrives.
    let (s, e) = add(
        &pod,
        json!({"title": "Eggs", "due": "2026-11-06", "client_id": "offline1"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{e}");
    let rows = todos(&pod, &app).await;
    assert_eq!(rows[1].1["deadline"], "2026-11-06");
    // An old pinned layout still validates (aliases bind).
    let v1 = ok(&pod, "restore_version", json!({"app": app, "version": 1})).await;
    assert_eq!(v1["version"], 3);
    // A stale base is refused.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "todos", "base_version": 1,
        "changes": [{"op": "set_title", "title": "Tasks"}]}),
    )
    .await;
    assert!(err && e["error"] == "stale_base", "{e}");
}

#[tokio::test]
async fn a_change_that_would_break_another_app_is_refused_whole() {
    let pod = pod();
    define_todos(&pod).await;
    let list = create_app(&pod, "Todos", todo_layout("")).await;
    let calendar = create_app(&pod, "Calendar", todo_layout("· $item.due")).await;
    add(&pod, json!({"title": "Milk", "due": "2026-11-05"})).await;
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "todos",
        "changes": [{"op": "remove_field", "field": "due"}],
        "apps": [{"app": list, "layout": todo_layout("")}]}),
    )
    .await;
    assert!(err, "{e}");
    assert_eq!(e["error"], "breaks_app");
    assert_eq!(e["app"], calendar);
    assert_eq!(e["in_change_set"], false);
    assert!(e["detail"].as_str().unwrap().contains("$item.due"), "{e}");
    assert!(e["detail"].as_str().unwrap().contains("Calendar"), "{e}");
    // Nothing applied: the definition and both apps are unchanged.
    let d = ok(&pod, "describe_collection", json!({"collection": "todos"})).await;
    assert_eq!(d["latest_version"], 1);
    let apps = ok(&pod, "list_layouts", json!({})).await;
    assert!(
        apps["layouts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["latest_version"] == 1),
        "{apps}"
    );
    assert_eq!(d["used_by"].as_array().unwrap().len(), 2);
    // With the calendar changed too, it applies.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "todos",
        "changes": [{"op": "remove_field", "field": "due"}],
        "apps": [{"app": calendar, "layout": todo_layout("")}]}),
    )
    .await;
    // Also checked on a plain edit: a layout can't bind a removed field.
    let (err, e) = tool(
        &pod,
        "update_layout",
        json!({"app": list, "layout": todo_layout("$item.due")}),
    )
    .await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("was removed"),
        "{e}"
    );
}

#[tokio::test]
async fn removed_fields_keep_their_data_until_purged() {
    let pod = pod();
    define_todos(&pod).await;
    let app = create_app(&pod, "Todos", todo_layout("")).await;
    add(&pod, json!({"title": "Milk", "due": "2026-11-05"})).await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "todos", "changes": [{"op": "remove_field", "field": "due"}]}),
    )
    .await;
    let rows = todos(&pod, &app).await;
    assert!(rows[0].1.get("due").is_none(), "hidden");
    // A write naming it is ignored, not refused (an older client's).
    let (s, _) = add(&pod, json!({"title": "Eggs", "due": "2026-11-06"})).await;
    assert_eq!(s, StatusCode::OK);
    let export = ok(&pod, "export_collection", json!({"collection": "todos"})).await;
    assert_eq!(export["rows"][0]["removed_fields"]["due"], "2026-11-05");
    let d = ok(&pod, "describe_collection", json!({"collection": "todos"})).await;
    assert_eq!(
        d["definition"]["removed_fields"][0]["restorable_for_days"],
        30
    );

    // Restoring brings the data back.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "todos", "changes": [{"op": "restore_field", "field": "due"}]}),
    )
    .await;
    let rows = todos(&pod, &app).await;
    assert_eq!(rows[0].1["due"], "2026-11-05");
    assert_eq!(
        rows[1].1["due"],
        Value::Null,
        "written while removed: ignored"
    );

    // Removed again and past the window, the data is purged for good.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "todos", "changes": [{"op": "remove_field", "field": "due"}]}),
    )
    .await;
    let later = pod_core::util::now_secs() + KEEP_SECS + 1;
    let purged = pod
        .store()
        .transaction(|tx| app_collections::db::purge(tx, later, KEEP_SECS))
        .unwrap();
    assert_eq!(purged.fields, 1);
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "todos", "changes": [{"op": "restore_field", "field": "due"}]}),
    )
    .await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("purged"),
        "{e}"
    );
    let export = ok(&pod, "export_collection", json!({"collection": "todos"})).await;
    assert!(export["rows"][0].get("removed_fields").is_none());
}

#[tokio::test]
async fn type_changes_convert_and_keep_what_does_not_convert() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "todos", "fields": [
        {"name": "title", "type": "text"}, {"name": "done", "type": "bool", "default": false},
        {"name": "due", "type": "text", "optional": true}]}),
    )
    .await;
    let app = create_app(&pod, "Todos", todo_layout("")).await;
    for (t, due) in [
        ("A", "2026-11-05"),
        ("B", "next week"),
        ("C", "2026-12-01"),
        ("D", ""),
    ] {
        add(&pod, json!({"title": t, "due": due})).await;
    }
    let r = ok(
        &pod,
        "apply_change",
        json!({"collection": "todos",
        "changes": [{"op": "update_field", "field": "due", "type": "date"}]}),
    )
    .await;
    let c = &r["conversions"][0];
    assert_eq!(c["field"], "due");
    assert_eq!(c["set_aside"], 1);
    assert!(
        c["detail"]
            .as_str()
            .unwrap()
            .starts_with("1 todos had due values"),
        "{c}"
    );
    assert_eq!(c["samples"][0]["value"], "next week");
    let rows = todos(&pod, &app).await;
    assert_eq!(rows[0].1["due"], "2026-11-05");
    assert_eq!(rows[1].1["due"], Value::Null);
    let export = ok(&pod, "export_collection", json!({"collection": "todos"})).await;
    assert_eq!(export["kept_aside"][0]["value"], "next week");
    // Back to text (restoring version 1): the value kept aside returns.
    let r = ok(
        &pod,
        "restore_collection_version",
        json!({"collection": "todos", "version": 1}),
    )
    .await;
    assert_eq!(r["version"], 3);
    assert_eq!(r["conversions"][0]["restored"], 1);
    let rows = todos(&pod, &app).await;
    assert_eq!(rows[1].1["due"], "next week");
}

#[tokio::test]
async fn row_edits_are_previewed_confirmed_and_undone() {
    let pod = pod();
    define_todos(&pod).await;
    let app = create_app(&pod, "Todos", todo_layout("")).await;
    let (_, milk) = add(&pod, json!({"title": "Milk"})).await;
    let milk = milk["id"].as_str().unwrap().to_owned();
    let edits = json!([
        {"op": "create", "values": {"title": "Bread", "due": "2026-11-07"}},
        {"op": "create", "values": {"title": "Jam"}},
        {"op": "update", "id": milk, "values": {"done": true}}]);
    let preview = ok(
        &pod,
        "edit_rows",
        json!({"collection": "todos", "edits": edits, "note": "From my note"}),
    )
    .await;
    assert_eq!(preview["needs_confirmation"], true);
    assert_eq!(
        (preview["count"].as_i64(), preview["creates"].as_i64()),
        (Some(3), Some(2))
    );
    assert_eq!(preview["sample"][2]["before"]["done"], false);
    assert_eq!(preview["sample"][2]["after"]["done"], true);
    assert_eq!(
        todos(&pod, &app).await.len(),
        1,
        "a preview changes nothing"
    );
    // A wrong token, or rows that changed since, need a new preview.
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "todos", "edits": edits, "confirm": "nope"}),
    )
    .await;
    assert!(err && e["error"] == "stale_preview", "{e}");
    let done = ok(
        &pod,
        "edit_rows",
        json!({"collection": "todos", "edits": edits,
        "confirm": preview["confirm"], "note": "From my note"}),
    )
    .await;
    assert_eq!(done["ok"], true);
    let rows = todos(&pod, &app).await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].1["done"], true);
    // The user ticks Jam afterwards; undo leaves it alone and reverts the rest.
    let jam = rows[2].1["id"].as_str().unwrap();
    call(
        &pod,
        "POST",
        "/v1/collections/todos/update",
        "phone",
        Some(json!({"id": jam, "done": true})),
    )
    .await;
    let undo = ok(&pod, "undo_edit", json!({"edit": done["edit"]})).await;
    assert_eq!(undo["restored"], 2);
    assert_eq!(undo["skipped"], json!([jam]));
    let rows = todos(&pod, &app).await;
    assert_eq!(
        rows.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
        ["Milk", "Jam"]
    );
    assert_eq!(rows[0].1["done"], false);
    let (err, _) = tool(&pod, "undo_edit", json!({"edit": done["edit"]})).await;
    assert!(err, "undone once");
    // A bad value names the edit.
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "todos",
        "edits": [{"op": "create", "values": {"title": "X", "due": "soon"}}]}),
    )
    .await;
    assert!(err && e["at"] == "/edits/0", "{e}");
}

#[tokio::test]
async fn add_row_writes_one_row_at_once_and_undoes() {
    let pod = pod();
    define_todos(&pod).await;
    let app = create_app(&pod, "Todos", todo_layout("")).await;
    let added = ok(
        &pod,
        "add_row",
        json!({"collection": "todos", "values": {"title": "Oil change", "due": "2026-11-03"},
        "note": "Logged the oil change"}),
    )
    .await;
    assert_eq!(added["ok"], true, "{added}");
    let rows = todos(&pod, &app).await;
    assert_eq!(rows.len(), 1, "no preview step");
    assert_eq!(rows[0].1["id"], added["id"]);
    assert_eq!(rows[0].1["done"], false, "defaults fill the rest");
    // A bad value names the field's place in the call.
    let (err, e) = tool(
        &pod,
        "add_row",
        json!({"collection": "todos", "values": {"title": "X", "due": "soon"}}),
    )
    .await;
    assert!(err && e["at"] == "/values", "{e}");
    let (err, _) = tool(&pod, "add_row", json!({"collection": "todos"})).await;
    assert!(err, "values are required");
    ok(&pod, "undo_edit", json!({"edit": added["edit"]})).await;
    assert!(todos(&pod, &app).await.is_empty());
}

#[tokio::test]
async fn caps_hold() {
    let pod = pod_with(Caps {
        collections: 1,
        fields: 3,
        rows: 2,
        bytes: 200,
        edit_rows: 2,
        text_chars: 50,
        ..Caps::default()
    });
    let (err, e) = tool(
        &pod,
        "define_collection",
        json!({"name": "todos", "fields": [
        {"name": "a", "type": "text"}, {"name": "b", "type": "text"},
        {"name": "c", "type": "text"}, {"name": "d", "type": "text"}]}),
    )
    .await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("at most 3 fields"),
        "{e}"
    );
    ok(
        &pod,
        "define_collection",
        json!({"name": "todos", "fields": [{"name": "title", "type": "text"}]}),
    )
    .await;
    let (err, e) = tool(
        &pod,
        "define_collection",
        json!({"name": "notes", "fields": [{"name": "t", "type": "text"}]}),
    )
    .await;
    assert!(err && e["error"] == "too_many_collections", "{e}");
    let (s, _) = add(&pod, json!({"title": "x".repeat(51)})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    add(&pod, json!({"title": "a"})).await;
    add(&pod, json!({"title": "b"})).await;
    let (s, e) = add(&pod, json!({"title": "c"})).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["error"], "too_many_rows");
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "todos", "edits": [
        {"op": "create", "values": {"title": "1"}}, {"op": "create", "values": {"title": "2"}},
        {"op": "create", "values": {"title": "3"}}]}),
    )
    .await;
    assert!(err && e["error"] == "too_many_edits", "{e}");
}

/// What is kept for undoing (deleted collections and rows, undo records)
/// stays under twice the live cap (trimmed before each write, so plus that
/// write's own undo record): deleting and re-adding, or editing over and
/// over, can't grow a pod's storage past its caps.
#[tokio::test]
async fn kept_data_stays_under_the_caps() {
    let caps = Caps {
        collections: 1,
        rows: 4,
        bytes: 2_000,
        ..Caps::default()
    };
    let pod = pod_with(caps);
    let stored = |pod: &Pod| {
        pod.store()
            .with_conn(|c| {
                c.query_row(
                    "SELECT (SELECT COALESCE(SUM(LENGTH(values_json)), 0) FROM collections_rows)
                          + (SELECT COALESCE(SUM(COALESCE(LENGTH(before_json), 0) + LENGTH(after_json)), 0)
                             FROM collections_edit_rows)",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap()
    };
    // edit_rows previews, then applies with the preview's token.
    let edit = |pod: Pod, name: String, edits: Vec<Value>| async move {
        let p = ok(
            &pod,
            "edit_rows",
            json!({"collection": name, "edits": edits}),
        )
        .await;
        ok(
            &pod,
            "edit_rows",
            json!({"collection": name, "edits": edits, "confirm": p["confirm"]}),
        )
        .await;
    };
    let rows = |n: usize, c: char| -> Vec<Value> {
        (0..n)
            .map(|_| json!({"op": "create", "values": {"t": c.to_string().repeat(400)}}))
            .collect()
    };
    // Fill a collection to the cap, delete it, and again under a new name.
    for i in 0..12 {
        let name = format!("c{i}");
        ok(
            &pod,
            "define_collection",
            json!({"name": name, "fields": [{"name": "t", "type": "text"}]}),
        )
        .await;
        edit(pod.clone(), name.clone(), rows(4, 'x')).await;
        ok(&pod, "delete_collection", json!({"collection": name})).await;
    }
    let l = ok(&pod, "list_collections", json!({})).await;
    let deleted = l["deleted"].as_array().map_or(0, Vec::len);
    assert!((1..12).contains(&deleted), "{l}");
    assert!(stored(&pod) <= 5 * caps.bytes as i64, "{}", stored(&pod));
    // The newest deleted collection can still be restored.
    ok(&pod, "restore_collection", json!({"collection": "c11"})).await;

    // Editing every row over and over keeps only so many undo records.
    let ids: Vec<String> = pod
        .store()
        .with_conn(|c| {
            c.prepare("SELECT id FROM collections_rows WHERE collection = 'c11'")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap();
    for i in 0..30 {
        let c = if i % 2 == 0 { 'y' } else { 'z' };
        let edits: Vec<Value> = ids
            .iter()
            .map(|id| json!({"op": "update", "id": id, "values": {"t": c.to_string().repeat(400)}}))
            .collect();
        edit(pod.clone(), "c11".into(), edits).await;
    }
    assert!(stored(&pod) <= 5 * caps.bytes as i64, "{}", stored(&pod));
}

/// Issue #107: a collection is deleted once nothing shows it (its stale
/// feed leaves home's), restored within the window, purged after it.
#[tokio::test]
async fn a_collection_is_deleted_restored_and_purged() {
    let pod = pod();
    define_todos(&pod).await;
    let app = create_app(&pod, "Todos", todo_layout("")).await;
    add(&pod, json!({"title": "Milk"})).await;

    // Refused while a layout shows it, naming the layout.
    let (err, e) = tool(&pod, "delete_collection", json!({"collection": "todos"})).await;
    assert!(err, "{e}");
    assert_eq!(e["error"], "collection_in_use");
    assert_eq!(e["used_by"][0]["app"], app.as_str());
    assert!(e["detail"].as_str().unwrap().contains("'Todos'"), "{e}");

    // Refused while another collection's lookup reads it.
    ok(
        &pod,
        "define_collection",
        json!({"name": "notes", "fields": [
            {"name": "text", "type": "text"},
            {"name": "status", "type": "text", "optional": true,
             "compute": {"kind": "lookup", "from": "text", "collection": "todos",
                         "key": "title", "value": "title"}}]}),
    )
    .await;
    ok(&pod, "delete_layout", json!({"id": app})).await;
    let (err, e) = tool(&pod, "delete_collection", json!({"collection": "todos"})).await;
    assert!(err, "{e}");
    assert_eq!(
        e["lookups"][0],
        json!({"collection": "notes", "field": "status"})
    );
    ok(&pod, "delete_collection", json!({"collection": "notes"})).await;

    // A stale base version is refused.
    let (err, e) = tool(
        &pod,
        "delete_collection",
        json!({"collection": "todos", "base_version": 99}),
    )
    .await;
    assert!(err && e["error"] == "stale_version", "{e}");

    let home_feeds = |pod: Pod| async move {
        pod.store()
            .with_conn(pod_core::custom::home_feeds_on)
            .unwrap()
    };
    let had_feed = home_feeds(pod.clone())
        .await
        .is_some_and(|f| f.contains(&("collections".into(), "todos".into())));
    let d = ok(
        &pod,
        "delete_collection",
        json!({"collection": "todos", "base_version": 1}),
    )
    .await;
    assert_eq!(d["dropped_from_home_feeds"], had_feed);
    if let Some(f) = home_feeds(pod.clone()).await {
        assert!(
            !f.contains(&("collections".into(), "todos".into())),
            "{f:?}"
        );
    }
    let l = ok(&pod, "list_collections", json!({})).await;
    assert_eq!(l["collections"], json!([]));
    let gone = l["deleted"].as_array().unwrap();
    let todos_gone = gone.iter().find(|c| c["name"] == "todos").expect("listed");
    assert_eq!(todos_gone["rows"], 1);
    assert_eq!(
        todos_gone["purged_after"],
        todos_gone["deleted_at"].as_i64().unwrap() + KEEP_SECS
    );
    let (s, _) = add(&pod, json!({"title": "Eggs"})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "its routes are gone");
    let (err, e) = tool(&pod, "describe_collection", json!({"collection": "todos"})).await;
    assert!(err && e["error"] == "collection_not_found", "{e}");
    let (err, e) = tool(
        &pod,
        "define_collection",
        json!({"name": "todos", "fields": [{"name": "title", "type": "text"}]}),
    )
    .await;
    assert!(err && e["error"] == "collection_deleted", "{e}");

    // Restored with its rows.
    ok(&pod, "restore_collection", json!({"collection": "todos"})).await;
    let l = ok(&pod, "list_collections", json!({})).await;
    assert_eq!(l["collections"][0]["name"], "todos");
    assert_eq!(l["collections"][0]["rows"], 1);
    assert!(
        l.get("deleted")
            .is_none_or(|d| d.as_array().unwrap().iter().all(|c| c["name"] != "todos")),
        "{l}"
    );
    let (err, _) = tool(&pod, "restore_collection", json!({"collection": "todos"})).await;
    assert!(err, "not deleted any more");

    // Deleted again and past the window: purged, and the name is free.
    ok(&pod, "delete_collection", json!({"collection": "todos"})).await;
    let later = pod_core::util::now_secs() + KEEP_SECS + 1;
    let purged = pod
        .store()
        .transaction(|tx| app_collections::db::purge(tx, later, KEEP_SECS))
        .unwrap();
    assert_eq!(purged.collections, 2, "todos and notes");
    let l = ok(&pod, "list_collections", json!({})).await;
    assert!(l.get("deleted").is_none(), "{l}");
    let (err, _) = tool(&pod, "restore_collection", json!({"collection": "todos"})).await;
    assert!(err);
    define_todos(&pod).await;
    let l = ok(&pod, "list_collections", json!({})).await;
    assert_eq!(l["collections"][0]["rows"], 0);
    assert_eq!(l["collections"][0]["version"], 1);
}
