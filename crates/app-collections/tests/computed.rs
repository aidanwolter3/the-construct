//! Computed fields and multi-key order (decision
//! `collection-computed-fields`, issue #62) on a real pod: a groceries
//! collection whose `section` is a `model` compute over the item's name
//! (a fake model, cached per name and taught by a hand edit), whose `label`
//! is an `expr` compute over the name and quantity, and whose rows listed
//! by `["section", "done"]` walk the store in the options' order.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_collections::compute::{FieldModel, Question};
use app_collections::{Caps, CollectionsAdapter};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, BoxFuture, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

const SECTIONS: [&str; 4] = ["Produce", "Bakery", "Dairy", "Other"];
const PROMPT: &str = "Which grocery store section is this item in?";
const LABEL: &str =
    "entity.quantity ? entity.name + \" (\" + entity.quantity + \")\" : entity.name";

/// Answers from a fixed table and records every text it was asked about.
#[derive(Debug, Default)]
struct Fake {
    asked: Mutex<Vec<String>>,
}

impl FieldModel for Fake {
    fn answer<'a>(&'a self, q: Question<'a>) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            assert_eq!(q.prompt, PROMPT);
            assert_eq!(q.options, SECTIONS);
            assert_eq!(q.inputs.len(), 1);
            assert_eq!(q.inputs[0].0, "name");
            let text = &q.inputs[0].1;
            self.asked.lock().unwrap().push(text.to_owned());
            let t = text.to_lowercase();
            Some(
                if t.contains("apple") || t.contains("banana") {
                    " produce."
                } else if t.contains("bread") || t.contains("bagel") {
                    "Bakery"
                } else if t.contains("milk") {
                    "Dairy"
                } else if t.contains("bogus") {
                    "Hardware"
                } else {
                    "Other"
                }
                .to_owned(),
            )
        })
    }
}

fn pod(fake: Option<Arc<Fake>>) -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_model(fake.map(|f| f as Arc<dyn FieldModel>));
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
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

/// The groceries collection as the issue has it today (no section).
async fn define_groceries(pod: &Pod) {
    ok(
        pod,
        "define_collection",
        json!({"name": "groceries", "title": "Groceries", "fields": [
            {"name": "name", "type": "text"},
            {"name": "quantity", "type": "text", "optional": true},
            {"name": "done", "type": "bool", "default": false}]}),
    )
    .await;
}

fn section_change() -> Value {
    json!([
        {"op": "add_field", "name": "section", "type": "choice", "options": SECTIONS,
         "default": "Other", "compute": {"kind": "model", "from": "name", "prompt": PROMPT}},
        {"op": "set_order", "field": ["section", "done"]}])
}

async fn add(pod: &Pod, body: Value) -> String {
    let (status, v) = call(
        pod,
        "POST",
        "/v1/collections/groceries/create",
        "phone",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["id"].as_str().unwrap().to_owned()
}

async fn update(pod: &Pod, body: Value) {
    let (status, v) = call(
        pod,
        "POST",
        "/v1/collections/groceries/update",
        "phone",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

/// The rows in order: `[(name, section, done)]`.
async fn rows(pod: &Pod) -> Vec<(String, String, bool)> {
    let (status, b) = call(pod, "GET", "/v1/collections/groceries/rows", "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["queries"]["collection:groceries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let e = &b["entities"][u.as_str().unwrap()];
            (
                e["name"].as_str().unwrap().to_owned(),
                e["section"].as_str().unwrap_or("").to_owned(),
                e["done"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn section_of(rows: &[(String, String, bool)], name: &str) -> String {
    rows.iter()
        .find(|r| r.0 == name)
        .map(|r| r.1.clone())
        .unwrap_or_default()
}

/// Wait until `name`'s section reads `want` (the classifier runs in the
/// background).
async fn wait_section(pod: &Pod, name: &str, want: &str) {
    for _ in 0..200 {
        if section_of(&rows(pod).await, name) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{name} never got section {want}: {:?}", rows(pod).await);
}

#[tokio::test]
async fn a_model_field_is_answered_cached_and_learned() {
    let fake = Arc::new(Fake::default());
    let pod = pod(Some(fake.clone()));
    define_groceries(&pod).await;
    // A row from before the field exists is filled in too.
    add(&pod, json!({"name": "Bagels"})).await;
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": section_change()}),
    )
    .await;
    assert_eq!(v["computing_rows"], 1, "{v}");
    assert_eq!(
        v["definition"]["fields"][3]["compute"],
        json!({"kind": "model", "from": ["name"], "prompt": PROMPT})
    );
    assert_eq!(v["definition"]["order_by"], json!(["section", "done"]));
    wait_section(&pod, "Bagels", "Bakery").await;

    // A create (the recipe screen's too) reads the default, then the answer
    // (the model's " produce." is matched to the option "Produce").
    add(&pod, json!({"name": "Apples", "quantity": "6"})).await;
    wait_section(&pod, "Apples", "Produce").await;
    assert_eq!(fake.asked.lock().unwrap().clone(), vec!["Bagels", "Apples"]);

    // The same name (any case and spacing) comes from the cache at once.
    add(&pod, json!({"name": "  apples "})).await;
    assert_eq!(section_of(&rows(&pod).await, "  apples ".trim()), "Produce");
    assert_eq!(
        fake.asked.lock().unwrap().len(),
        2,
        "classified once per name"
    );

    // A hand edit wins and teaches: the next "Oat milk" gets the person's
    // section without asking.
    let oat = add(&pod, json!({"name": "Oat milk"})).await;
    wait_section(&pod, "Oat milk", "Dairy").await;
    update(&pod, json!({"id": oat, "section": "Other"})).await;
    add(&pod, json!({"name": "oat milk"})).await;
    let r = rows(&pod).await;
    assert!(
        r.iter()
            .filter(|x| x.0.eq_ignore_ascii_case("oat milk"))
            .all(|x| x.1 == "Other"),
        "{r:?}"
    );
    assert_eq!(fake.asked.lock().unwrap().len(), 3);

    // A create that names its section keeps it (and teaches it).
    add(&pod, json!({"name": "Bananas", "section": "Other"})).await;
    add(&pod, json!({"name": "bananas"})).await;
    let r = rows(&pod).await;
    assert!(
        r.iter()
            .filter(|x| x.0.eq_ignore_ascii_case("bananas"))
            .all(|x| x.1 == "Other"),
        "{r:?}"
    );
    assert_eq!(fake.asked.lock().unwrap().len(), 3);

    // Renaming an item computes its section again.
    let bread = add(&pod, json!({"name": "Bogus thing"})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        section_of(&rows(&pod).await, "Bogus thing"),
        "Other",
        "an answer that isn't an option leaves the default"
    );
    update(&pod, json!({"id": bread, "name": "Bread"})).await;
    wait_section(&pod, "Bread", "Bakery").await;

    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "groceries"}),
    )
    .await;
    assert_eq!(d["computed_answers"][0]["field"], "section", "{d}");
    // Clearing a hand-set value computes it again (from the cache).
    update(&pod, json!({"id": oat, "section": null})).await;
    assert_eq!(section_of(&rows(&pod).await, "Oat milk"), "Other");
}

#[tokio::test]
async fn rows_walk_the_store_by_section_option_order_then_unbought_first() {
    let fake = Arc::new(Fake::default());
    let pod = pod(Some(fake));
    define_groceries(&pod).await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": section_change()}),
    )
    .await;
    // Added out of order; "Zucchini" sorts by option order, not its text.
    let milk = add(&pod, json!({"name": "Milk"})).await;
    add(&pod, json!({"name": "Bread"})).await;
    let apples = add(&pod, json!({"name": "Apples"})).await;
    add(&pod, json!({"name": "Bananas"})).await;
    add(&pod, json!({"name": "Soap"})).await;
    add(&pod, json!({"name": "Zucchini", "section": "Produce"})).await;
    for (n, s) in [
        ("Milk", "Dairy"),
        ("Bread", "Bakery"),
        ("Apples", "Produce"),
        ("Bananas", "Produce"),
    ] {
        wait_section(&pod, n, s).await;
    }
    update(&pod, json!({"id": apples, "done": true})).await;
    update(&pod, json!({"id": milk, "done": true})).await;
    let order: Vec<(String, String, bool)> = rows(&pod).await;
    let names: Vec<&str> = order.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        names,
        ["Bananas", "Zucchini", "Apples", "Bread", "Milk", "Soap"],
        "{order:?}"
    );
}

#[tokio::test]
async fn without_a_model_the_field_keeps_its_default_and_an_app_groups_by_it() {
    let pod = pod(None);
    define_groceries(&pod).await;
    let layout = json!({"type": "core://Workspace", "id": "x", "title": "Groceries", "slots": [
        {"slot_id": "list", "provider_uri": "core://custom", "content": {
            "type": "core://List", "query_key": "collection:groceries",
            "where": {"field": "deleted", "equals": "false"},
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.name"}}}]});
    let app = ok(
        &pod,
        "create_app",
        json!({"title": "Groceries", "icon": "dashboard",
        "feeds": [["collections", "groceries"]], "layout": layout.clone()}),
    )
    .await;
    let app = app["id"].as_str().unwrap().to_owned();
    // The section and the app's grouping by it, in one change set.
    let mut grouped = layout;
    grouped["slots"][0]["content"]["group_by"] = json!({"field": "section",
        "header": {"type": "core://Text", "style": "Title", "value_expr": "$item.section"}});
    ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": section_change(),
        "apps": [{"app": app, "layout": grouped}]}),
    )
    .await;
    add(&pod, json!({"name": "Apples"})).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(section_of(&rows(&pod).await, "Apples"), "Other");
    let (status, ws) = call(&pod, "GET", &format!("/v1/workspaces/{app}"), "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    // Bad computes are refused with the op's pointer.
    for bad in [
        json!({"op": "add_field", "name": "aisle", "type": "text", "optional": true,
               "compute": {"kind": "model", "from": "name"}}),
        json!({"op": "add_field", "name": "aisle", "type": "text",
               "compute": {"kind": "expr", "expr": "entity.name"}}),
        json!({"op": "add_field", "name": "aisle", "type": "text", "optional": true,
               "compute": {"kind": "expr", "expr": "upper(entity.name)"}}),
        json!({"op": "add_field", "name": "aisle", "type": "text", "optional": true,
               "compute": {"kind": "expr", "expr": "entity.section"}}),
        json!({"op": "remove_field", "field": "name"}),
    ] {
        let (err, v) = tool(
            &pod,
            "apply_change",
            json!({"collection": "groceries", "changes": [bad]}),
        )
        .await;
        assert!(err && v["error"] == "invalid_change", "{v}");
    }
}

#[tokio::test]
async fn define_collection_takes_compute_and_a_list_order() {
    let pod = pod(Some(Arc::new(Fake::default())));
    let v = ok(
        &pod,
        "define_collection",
        json!({"name": "groceries", "fields": [
        {"name": "section", "type": "choice", "options": SECTIONS, "default": "Other",
         "compute": {"kind": "model", "from": ["name"], "prompt": PROMPT}},
        {"name": "name", "type": "text"},
        {"name": "done", "type": "bool", "default": false}],
        "order_by": ["section", "done"]}),
    )
    .await;
    assert_eq!(v["definition"]["order_by"], json!(["section", "done"]));
    add(&pod, json!({"name": "Milk"})).await;
    wait_section(&pod, "Milk", "Dairy").await;
    // A single string still works.
    let v = ok(
        &pod,
        "define_collection",
        json!({"name": "notes", "fields": [
        {"name": "title", "type": "text"}], "order_by": "title"}),
    )
    .await;
    assert_eq!(v["definition"]["order_by"], json!("title"));
}

/// The rows' `(name, label)` in order.
async fn labels(pod: &Pod) -> Vec<(String, String)> {
    let (status, b) = call(pod, "GET", "/v1/collections/groceries/rows", "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["queries"]["collection:groceries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let e = &b["entities"][u.as_str().unwrap()];
            (
                e["name"].as_str().unwrap().to_owned(),
                e["label"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn an_expr_field_is_computed_in_the_write_and_again_when_its_sources_change() {
    let pod = pod(None);
    define_groceries(&pod).await;
    let apples = add(&pod, json!({"name": "Apples", "quantity": "6"})).await;
    // Added with a compute: existing rows get it in the same change.
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [
            {"op": "add_field", "name": "label", "type": "text", "optional": true,
             "compute": {"kind": "expr", "expr": LABEL}}]}),
    )
    .await;
    assert_eq!(v["computing_rows"], 0, "an expression needs no model: {v}");
    assert_eq!(
        v["definition"]["fields"][3]["compute"],
        json!({"kind": "expr", "expr": LABEL})
    );
    assert_eq!(labels(&pod).await, [("Apples".into(), "Apples (6)".into())]);
    // A create and an update compute it at once, no waiting.
    let milk = add(&pod, json!({"name": "Milk"})).await;
    update(&pod, json!({"id": apples, "quantity": "a dozen"})).await;
    assert_eq!(
        labels(&pod).await,
        [
            ("Apples".into(), "Apples (a dozen)".into()),
            ("Milk".into(), "Milk".into())
        ]
    );
    // A value written by hand stays until a source changes.
    update(&pod, json!({"id": milk, "label": "Milk (oat, for coffee)"})).await;
    update(&pod, json!({"id": milk, "done": true})).await;
    assert_eq!(labels(&pod).await[1].1, "Milk (oat, for coffee)");
    update(&pod, json!({"id": milk, "quantity": "2"})).await;
    assert_eq!(labels(&pod).await[1].1, "Milk (2)");
    // A rename of a source keeps the expression (stored by field id).
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [
            {"op": "rename_field", "field": "quantity", "to": "amount"}]}),
    )
    .await;
    assert_eq!(
        v["definition"]["fields"][3]["compute"]["expr"],
        LABEL.replace("quantity", "amount")
    );
    update(&pod, json!({"id": milk, "amount": "1"})).await;
    assert_eq!(labels(&pod).await[1].1, "Milk (1)");
    // Changing the expression recomputes every row.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [
            {"op": "update_field", "field": "label",
             "compute": {"kind": "expr", "expr": "entity.done ? \"✓ \" + entity.name : entity.name"}}]}),
    )
    .await;
    assert_eq!(
        labels(&pod).await,
        [
            ("Apples".into(), "Apples".into()),
            ("Milk".into(), "✓ Milk".into())
        ]
    );
    // edit_rows computes it too.
    let edits = json!([{"op": "create", "values": {"name": "Bread"}}]);
    let preview = ok(
        &pod,
        "edit_rows",
        json!({"collection": "groceries", "edits": edits}),
    )
    .await;
    ok(
        &pod,
        "edit_rows",
        json!({"collection": "groceries", "edits": edits, "confirm": preview["confirm"]}),
    )
    .await;
    assert_eq!(labels(&pod).await[2], ("Bread".into(), "Bread".into()));
}
