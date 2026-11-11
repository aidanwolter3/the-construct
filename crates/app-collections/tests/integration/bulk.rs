//! Bulk row edits: a preview's edits are kept under its confirm token (the
//! confirming call sends the token alone), edits come from a file (posted
//! to `/import` or fetched from a URL), and a preview checks the whole set,
//! listing every invalid edit with where it is.

use std::sync::Arc;

use app_collections::{Caps, CollectionsAdapter};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

fn pod() -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default()).with_private_hosts(true);
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

async fn send(pod: &Pod, req: Request<Body>) -> (StatusCode, Value) {
    let resp = pod_core::app(pod.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn tool(pod: &Pod, name: &str, args: Value) -> (bool, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/mcp")
        .header("authorization", "Bearer pod")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": name, "arguments": args}})
            .to_string(),
        ))
        .unwrap();
    let (_, body) = send(pod, req).await;
    let r = &body["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("null");
    (r["isError"] == true, serde_json::from_str(text).unwrap())
}

async fn ok(pod: &Pod, name: &str, args: Value) -> Value {
    let (err, v) = tool(pod, name, args).await;
    assert!(!err, "{name}: {v}");
    v
}

async fn rows(pod: &Pod, collection: &str) -> Vec<Value> {
    let req = Request::builder()
        .uri(format!("/v1/collections/{collection}/rows"))
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header("authorization", "Bearer phone")
        .body(Body::empty())
        .unwrap();
    let (status, b) = send(pod, req).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["queries"][format!("collection:{collection}")]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()].clone())
        .collect()
}

async fn import(
    pod: &Pod,
    collection: &str,
    query: &str,
    ct: &str,
    file: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/collections/{collection}/import{query}"))
        .header("authorization", "Bearer phone")
        .header("content-type", ct)
        .body(Body::from(file.to_owned()))
        .unwrap();
    send(pod, req).await
}

/// recipes (name unique) and their lines (a ref to the recipe).
async fn define(pod: &Pod) {
    ok(
        pod,
        "define_collection",
        json!({"name": "recipes", "fields": [
            {"name": "name", "type": "text", "unique": true},
            {"name": "serves", "type": "number", "optional": true}]}),
    )
    .await;
    ok(
        pod,
        "define_collection",
        json!({"name": "lines", "fields": [
            {"name": "recipe", "type": "ref", "collection": "recipes"},
            {"name": "text", "type": "text"},
            {"name": "qty", "type": "quantity", "optional": true}]}),
    )
    .await;
}

#[tokio::test]
async fn a_confirm_token_alone_applies_the_previewed_edits_once() {
    let pod = pod();
    define(&pod).await;
    let edits: Vec<Value> = (0..300)
        .map(|i| json!({"op": "create", "values": {"name": format!("Dish {i}"), "serves": i % 6}}))
        .collect();
    let p = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "edits": edits, "note": "Cookbook"}),
    )
    .await;
    assert_eq!(
        (p["needs_confirmation"].as_bool(), p["count"].as_i64()),
        (Some(true), Some(300))
    );
    assert!(rows(&pod, "recipes").await.is_empty());
    // Just the token: the pod applies what it previewed.
    let done = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": p["confirm"]}),
    )
    .await;
    assert_eq!(
        (done["ok"].as_bool(), done["creates"].as_i64()),
        (Some(true), Some(300))
    );
    assert_eq!(rows(&pod, "recipes").await.len(), 300);
    // The edit kept the preview's note.
    let note = pod
        .blocking(|s| {
            s.with_conn(|c| {
                c.query_row("SELECT note FROM collections_edits", [], |r| {
                    r.get::<_, String>(0)
                })
            })
        })
        .await
        .unwrap();
    assert_eq!(note, "Cookbook");
    // Once applied, the token is spent.
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": p["confirm"]}),
    )
    .await;
    assert!(err && e["error"] == "preview_expired", "{e}");
    // An unknown token, or one of another collection, finds nothing.
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "lines", "confirm": "abc"}),
    )
    .await;
    assert!(err && e["error"] == "preview_expired", "{e}");
    let (err, e) = tool(&pod, "edit_rows", json!({"collection": "lines"})).await;
    assert!(err && e["error"] == "invalid_arguments", "{e}");
}

#[tokio::test]
async fn a_stale_token_answers_a_new_preview_that_its_token_applies() {
    let pod = pod();
    define(&pod).await;
    let p = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "edits": [{"op": "create", "values": {"name": "Soup"}}]}),
    )
    .await;
    let done = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": p["confirm"]}),
    )
    .await;
    let id = done["created"][0].as_str().unwrap().to_owned();
    let edits = json!([{"op": "update", "id": id, "values": {"serves": 4}}]);
    let p = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "edits": edits}),
    )
    .await;
    // The row changes after the preview.
    let other = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "edits": [{"op": "update", "id": id, "values": {"name": "Stew"}}]}),
    )
    .await;
    ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": other["confirm"]}),
    )
    .await;
    let (err, stale) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": p["confirm"]}),
    )
    .await;
    assert!(err && stale["error"] == "stale_preview", "{stale}");
    assert_eq!(stale["sample"][0]["before"]["name"], "Stew");
    assert_ne!(stale["confirm"], p["confirm"]);
    ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": stale["confirm"]}),
    )
    .await;
    let r = rows(&pod, "recipes").await;
    assert_eq!(
        (r[0]["name"].as_str(), r[0]["serves"].as_i64()),
        (Some("Stew"), Some(4))
    );
}

#[tokio::test]
async fn a_preview_lists_every_invalid_edit_and_changes_nothing() {
    let pod = pod();
    define(&pod).await;
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "edits": [
            {"op": "create", "values": {"name": "A", "serves": "lots"}},
            {"op": "create", "values": {"name": "B"}},
            {"op": "update", "id": "nope", "values": {"name": "C"}},
            {"op": "create", "values": {"colour": "red"}}]}),
    )
    .await;
    assert!(
        err && e["error"] == "invalid_edit" && e["invalid"] == 3,
        "{e}"
    );
    let at: Vec<&str> = e["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["at"].as_str().unwrap())
        .collect();
    assert_eq!(at, ["/edits/0", "/edits/2", "/edits/3"]);
    assert_eq!(e["at"], "/edits/0");
    // Duplicates of a unique field inside the set are found by the preview,
    // not only when applying.
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "edits": [
            {"op": "create", "values": {"name": "Pie"}},
            {"op": "create", "values": {"name": "Tart"}},
            {"op": "create", "values": {"name": "pie "}}]}),
    )
    .await;
    assert!(err && e["error"] == "not_unique", "{e}");
    assert_eq!(e["errors"][0]["at"], "/edits/2");
    assert!(rows(&pod, "recipes").await.is_empty());
}

#[tokio::test]
async fn a_posted_csv_file_previews_and_its_token_applies_it() {
    let pod = pod();
    define(&pod).await;
    let p = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "edits": [
            {"op": "create", "values": {"name": "Bread"}}, {"op": "create", "values": {"name": "Soup"}}]}),
    )
    .await;
    ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": p["confirm"]}),
    )
    .await;
    let csv = "recipe.name,text,qty\nBread,\"Flour, strong\",500 g\nBread,Water,\nSoup,\"Leeks, \"\"washed\"\"\",2\n";
    let (status, p) = import(&pod, "lines", "?note=From%20my%20notes", "text/csv", csv).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(
        (p["needs_confirmation"].as_bool(), p["creates"].as_i64()),
        (Some(true), Some(3))
    );
    assert!(rows(&pod, "lines").await.is_empty());
    ok(
        &pod,
        "edit_rows",
        json!({"collection": "lines", "confirm": p["confirm"]}),
    )
    .await;
    let lines = rows(&pod, "lines").await;
    assert_eq!(lines.len(), 3);
    let bread = rows(&pod, "recipes")
        .await
        .into_iter()
        .find(|r| r["name"] == "Bread")
        .unwrap();
    let flour = lines.iter().find(|l| l["text"] == "Flour, strong").unwrap();
    assert_eq!(flour["recipe"], bread["id"]);
    assert!(lines.iter().any(|l| l["text"] == "Leeks, \"washed\""));
    // A file's invalid rows are named by their line.
    let (status, e) = import(
        &pod,
        "lines",
        "",
        "text/csv",
        "recipe.name,text\nBread,Salt\nCake,Eggs\n",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    assert_eq!(e["errors"][0]["at"], "line 3");
    // A malformed file is refused as a whole.
    let (status, e) = import(&pod, "lines", "?format=json", "text/plain", "{oops").await;
    assert_eq!(
        (status, e["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_file"))
    );
}

#[tokio::test]
async fn a_file_at_a_url_is_fetched_and_previewed() {
    let pod = pod();
    define(&pod).await;
    let jsonl = "{\"name\": \"Curry\", \"serves\": 4}\n{\"op\": \"create\", \"values\": {\"name\": \"Dal\"}}\n";
    let app = Router::new().route("/recipes.jsonl", get(move || async move { jsonl }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/recipes.jsonl");
    let p = ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "url": url}),
    )
    .await;
    assert_eq!(p["creates"], 2, "{p}");
    ok(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "confirm": p["confirm"]}),
    )
    .await;
    let r = rows(&pod, "recipes").await;
    assert_eq!(r.len(), 2);
    assert!(r.iter().any(|x| x["name"] == "Curry" && x["serves"] == 4));
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "recipes", "url": url, "edits": []}),
    )
    .await;
    assert!(err && e["error"] == "invalid_arguments", "{e}");
}
