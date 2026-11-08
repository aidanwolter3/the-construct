//! A computed field that reads a row's system `id` (decision
//! `live-computed-fields`, "Row ids"): a lookup whose `value` is `id`
//! answers the matched row's id and follows that collection like any
//! lookup; an expression may read `entity.id`. Here `ingredients` point at
//! the matching row of `groceries`, so a button can delete that grocery.

use std::sync::Arc;

use app_collections::{Caps, CollectionsAdapter};
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
    let adapter = CollectionsAdapter::with_caps(Caps::default());
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

async fn call(pod: &Pod, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let bearer = if path == "/v1/mcp" { "pod" } else { "phone" };
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

async fn write(pod: &Pod, collection: &str, op: &str, body: Value) -> Value {
    let (status, v) = call(
        pod,
        "POST",
        &format!("/v1/collections/{collection}/{op}"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{op} {collection}: {v}");
    v
}

async fn add(pod: &Pod, collection: &str, body: Value) -> String {
    write(pod, collection, "create", body).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The live rows of `collection` as entities, in order.
async fn rows(pod: &Pod, collection: &str) -> Vec<Value> {
    let (status, b) = call(
        pod,
        "GET",
        &format!("/v1/collections/{collection}/rows"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["queries"][format!("collection:{collection}")]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()].clone())
        .filter(|e| e["deleted"] != true)
        .collect()
}

/// groceries (name) and ingredients (name, `grocery_id`: a lookup of the
/// grocery row's id by name).
async fn define(pod: &Pod) {
    ok(
        pod,
        "define_collection",
        json!({"name": "groceries", "title": "Groceries", "fields": [
            {"name": "name", "type": "text"}]}),
    )
    .await;
    ok(
        pod,
        "define_collection",
        json!({"name": "ingredients", "title": "Ingredients", "fields": [
            {"name": "name", "type": "text"},
            {"name": "grocery_id", "type": "text", "optional": true,
             "compute": {"kind": "lookup", "from": "name", "collection": "groceries",
                         "key": "name", "value": "id"}}]}),
    )
    .await;
}

/// The `grocery_id` of the ingredient named `name` (`None`: empty).
async fn grocery_of(pod: &Pod, name: &str) -> Option<String> {
    let e = rows(pod, "ingredients")
        .await
        .into_iter()
        .find(|e| e["name"] == name)
        .unwrap();
    e["grocery_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[tokio::test]
async fn a_lookup_of_id_answers_the_matching_rows_id() {
    let pod = pod();
    define(&pod).await;
    let milk = add(&pod, "groceries", json!({"name": "Oat milk"})).await;
    add(&pod, "groceries", json!({"name": "Eggs"})).await;
    add(&pod, "ingredients", json!({"name": "oat  MILK"})).await;
    add(&pod, "ingredients", json!({"name": "Flour"})).await;
    assert_eq!(grocery_of(&pod, "oat  MILK").await, Some(milk.clone()));
    assert_eq!(grocery_of(&pod, "Flour").await, None);

    // describe_collection shows it as written.
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "ingredients"}),
    )
    .await;
    let field = d["definition"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "grocery_id")
        .unwrap()
        .clone();
    assert_eq!(field["compute"]["value"], "id", "{field}");

    // The id deletes that grocery through the client write path.
    let id = grocery_of(&pod, "oat  MILK").await.unwrap();
    write(&pod, "groceries", "delete", json!({"id": id})).await;
    assert_eq!(rows(&pod, "groceries").await.len(), 1);
    assert_eq!(grocery_of(&pod, "oat  MILK").await, None);
}

#[tokio::test]
async fn a_lookup_of_id_clears_on_delete_and_comes_back_on_restore() {
    let pod = pod();
    define(&pod).await;
    let eggs = add(&pod, "groceries", json!({"name": "Eggs"})).await;
    add(&pod, "ingredients", json!({"name": "eggs"})).await;
    assert_eq!(grocery_of(&pod, "eggs").await, Some(eggs.clone()));

    write(&pod, "groceries", "delete", json!({"id": eggs})).await;
    assert_eq!(grocery_of(&pod, "eggs").await, None);

    write(
        &pod,
        "groceries",
        "update",
        json!({"id": eggs, "deleted": false}),
    )
    .await;
    assert_eq!(grocery_of(&pod, "eggs").await, Some(eggs.clone()));

    // Its key changes: the old name's ingredient clears, the new one's fills.
    add(&pod, "ingredients", json!({"name": "duck eggs"})).await;
    assert_eq!(grocery_of(&pod, "duck eggs").await, None);
    write(
        &pod,
        "groceries",
        "update",
        json!({"id": eggs, "name": "Duck eggs"}),
    )
    .await;
    assert_eq!(grocery_of(&pod, "eggs").await, None);
    assert_eq!(grocery_of(&pod, "duck eggs").await, Some(eggs));
}

#[tokio::test]
async fn a_lookup_of_id_follows_new_rows_and_the_oldest_match_wins() {
    let pod = pod();
    define(&pod).await;
    add(&pod, "ingredients", json!({"name": "Butter"})).await;
    assert_eq!(grocery_of(&pod, "Butter").await, None);

    // A grocery with that key is created: the ingredient points at it.
    let first = add(&pod, "groceries", json!({"name": "butter"})).await;
    assert_eq!(grocery_of(&pod, "Butter").await, Some(first.clone()));

    // A second match doesn't take over: the oldest live row wins.
    let second = add(&pod, "groceries", json!({"name": "Butter"})).await;
    assert_eq!(grocery_of(&pod, "Butter").await, Some(first.clone()));

    // Until it is deleted: then the next match's id.
    write(&pod, "groceries", "delete", json!({"id": first})).await;
    assert_eq!(grocery_of(&pod, "Butter").await, Some(second.clone()));
    write(&pod, "groceries", "delete", json!({"id": second})).await;
    assert_eq!(grocery_of(&pod, "Butter").await, None);

    // Restored, the oldest is the answer again.
    for id in [&second, &first] {
        write(
            &pod,
            "groceries",
            "update",
            json!({"id": id, "deleted": false}),
        )
        .await;
    }
    assert_eq!(grocery_of(&pod, "Butter").await, Some(first));
}

#[tokio::test]
async fn a_lookup_of_id_can_be_added_with_apply_change_and_is_checked() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "groceries", "fields": [{"name": "name", "type": "text"}]}),
    )
    .await;
    ok(
        &pod,
        "define_collection",
        json!({"name": "ingredients", "fields": [{"name": "name", "type": "text"}]}),
    )
    .await;
    let rice = add(&pod, "groceries", json!({"name": "Rice"})).await;
    add(&pod, "ingredients", json!({"name": "rice"})).await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "ingredients", "changes": [
            {"op": "add_field", "name": "grocery_id", "type": "text", "optional": true,
             "compute": {"kind": "lookup", "from": "name", "collection": "groceries",
                         "key": "name", "value": "id"}}]}),
    )
    .await;
    assert_eq!(grocery_of(&pod, "rice").await, Some(rice));

    // Only `id` among the system names; only into text; never learning.
    let refused = [
        json!({"name": "a", "type": "text", "optional": true,
               "compute": {"kind": "lookup", "from": "name", "collection": "groceries",
                           "key": "name", "value": "created_at"}}),
        json!({"name": "b", "type": "number", "optional": true,
               "compute": {"kind": "lookup", "from": "name", "collection": "groceries",
                           "key": "name", "value": "id"}}),
        json!({"name": "c", "type": "text", "optional": true,
               "compute": {"kind": "lookup", "from": "name", "collection": "groceries",
                           "key": "name", "value": "id", "learn": true}}),
    ];
    for f in refused {
        let mut change = f.clone();
        change["op"] = json!("add_field");
        let (err, v) = tool(
            &pod,
            "apply_change",
            json!({"collection": "ingredients", "changes": [change]}),
        )
        .await;
        assert!(err, "{f} was accepted: {v}");
    }
}

#[tokio::test]
async fn an_expression_reads_entity_id() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "notes", "fields": [
            {"name": "title", "type": "text"},
            {"name": "link", "type": "text", "optional": true,
             "compute": {"kind": "expr", "expr": "\"note:\" + entity.id"}},
            {"name": "own_id", "type": "text", "optional": true,
             "compute": {"kind": "expr", "expr": "entity.id"}}]}),
    )
    .await;
    let id = add(&pod, "notes", json!({"title": "Hello"})).await;
    let row = rows(&pod, "notes").await.remove(0);
    assert_eq!(row["link"], format!("note:{id}"));
    assert_eq!(row["own_id"], id);

    // An update keeps it; describe_collection shows entity.id.
    write(&pod, "notes", "update", json!({"id": id, "title": "Hi"})).await;
    assert_eq!(rows(&pod, "notes").await[0]["link"], format!("note:{id}"));
    let d = ok(&pod, "describe_collection", json!({"collection": "notes"})).await;
    let link = d["definition"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "link")
        .unwrap()
        .clone();
    assert_eq!(link["compute"]["expr"], "\"note:\" + entity.id", "{link}");

    // Added later with apply_change, it is computed for existing rows.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "notes", "changes": [
            {"op": "add_field", "name": "tag", "type": "text", "optional": true,
             "compute": {"kind": "expr", "expr": "entity.title + \"#\" + entity.id"}}]}),
    )
    .await;
    assert_eq!(rows(&pod, "notes").await[0]["tag"], format!("Hi#{id}"));
}
