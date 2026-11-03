//! `lookup` computes and compute lists (decision `collection-lookups`,
//! issue #70) on a real pod: a groceries collection whose `section` is
//! `[lookup section_rules (learn), model]`. Rules match by `contains` or
//! `regex`, the longest key wins, the (fake) model answers only when no
//! rule matches and its answer becomes a rule, a hand edit changes the
//! rule, and an invalid regex is skipped and reported.

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

const SECTIONS: [&str; 5] = ["Unsorted", "Produce", "Pantry", "Dairy", "Other"];
const PROMPT: &str = "Which grocery store section is this item in?";

/// Answers from a fixed table and records every text it was asked about.
#[derive(Debug, Default)]
struct Fake {
    asked: Mutex<Vec<String>>,
}

impl Fake {
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl FieldModel for Fake {
    fn answer<'a>(&'a self, q: Question<'a>) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            assert_eq!(q.prompt, PROMPT);
            let text = q.inputs[0].1.clone();
            self.asked.lock().unwrap().push(text.clone());
            let t = text.to_lowercase();
            Some(
                if t.contains("cucumber") || t.contains("kale") {
                    "Produce"
                } else if t.contains("milk") {
                    "Dairy"
                } else if t.contains("rice") {
                    "pantry."
                } else {
                    "Other"
                }
                .to_owned(),
            )
        })
    }
}

fn pod(fake: Arc<Fake>) -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_model(Some(fake as Arc<dyn FieldModel>));
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

/// The rules collection, and the rules `(pattern, section)`.
async fn define_rules(pod: &Pod, rules: &[(&str, &str)]) {
    ok(
        pod,
        "define_collection",
        json!({"name": "section_rules", "title": "Section rules", "fields": [
            {"name": "pattern", "type": "text"},
            {"name": "section", "type": "text"}]}),
    )
    .await;
    for (p, s) in rules {
        add(pod, "section_rules", json!({"pattern": p, "section": s})).await;
    }
}

fn section_compute(mode: &str) -> Value {
    json!([
        {"kind": "lookup", "from": "name", "collection": "section_rules", "key": "pattern",
         "value": "section", "match": mode, "learn": true},
        {"kind": "model", "from": "name", "prompt": PROMPT}])
}

async fn define_groceries(pod: &Pod, compute: Value) -> Value {
    ok(
        pod,
        "define_collection",
        json!({"name": "groceries", "title": "Groceries", "fields": [
            {"name": "name", "type": "text"},
            {"name": "done", "type": "bool", "default": false},
            {"name": "section", "type": "choice", "options": SECTIONS, "default": "Unsorted",
             "compute": compute}]}),
    )
    .await
}

async fn add(pod: &Pod, collection: &str, body: Value) -> String {
    let (status, v) = call(
        pod,
        "POST",
        &format!("/v1/collections/{collection}/create"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["id"].as_str().unwrap().to_owned()
}

async fn update(pod: &Pod, collection: &str, body: Value) {
    let (status, v) = call(
        pod,
        "POST",
        &format!("/v1/collections/{collection}/update"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

/// `(id, a, b)` of each row of `collection` in order, `a` and `b` its
/// fields `fa` and `fb` as text.
async fn rows(pod: &Pod, collection: &str, fa: &str, fb: &str) -> Vec<(String, String, String)> {
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
        .map(|u| {
            let e = &b["entities"][u.as_str().unwrap()];
            let s = |f: &str| e[f].as_str().unwrap_or("").to_owned();
            (s("id"), s(fa), s(fb))
        })
        .collect()
}

/// The section of grocery `name`.
async fn section(pod: &Pod, name: &str) -> String {
    rows(pod, "groceries", "name", "section")
        .await
        .into_iter()
        .find(|r| r.1 == name)
        .map(|r| r.2)
        .unwrap_or_default()
}

/// The rules as `(pattern, section)`.
async fn rules(pod: &Pod) -> Vec<(String, String)> {
    rows(pod, "section_rules", "pattern", "section")
        .await
        .into_iter()
        .map(|r| (r.1, r.2))
        .collect()
}

async fn wait_section(pod: &Pod, name: &str, want: &str) {
    for _ in 0..200 {
        if section(pod, name).await == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{name} never got section {want}: {:?}",
        rows(pod, "groceries", "name", "section").await
    );
}

#[tokio::test]
async fn rules_answer_first_the_model_fills_in_and_its_answers_become_rules() {
    let fake = Arc::new(Fake::default());
    let pod = pod(fake.clone());
    define_rules(
        &pod,
        &[
            ("milk", "Dairy"),
            ("coconut milk", "pantry"),
            ("cucumber", "Produce"),
        ],
    )
    .await;
    let v = define_groceries(&pod, section_compute("contains")).await;
    assert_eq!(
        v["definition"]["fields"][2]["compute"],
        json!([{"kind": "lookup", "from": "name", "collection": "section_rules", "key": "pattern",
                "value": "section", "match": "contains", "learn": true},
               {"kind": "model", "from": ["name"], "prompt": PROMPT}]),
        "described by names: {v}"
    );

    // Rules answer inside the write, by whole words; the longest key wins
    // and the answer is matched to the choice's option ("pantry").
    add(&pod, "groceries", json!({"name": "Oat milk"})).await;
    add(&pod, "groceries", json!({"name": "Coconut milk (can)"})).await;
    add(&pod, "groceries", json!({"name": "English cucumber"})).await;
    assert_eq!(section(&pod, "Oat milk").await, "Dairy");
    assert_eq!(section(&pod, "Coconut milk (can)").await, "Pantry");
    assert_eq!(section(&pod, "English cucumber").await, "Produce");
    assert!(fake.asked().is_empty(), "no model call: {:?}", fake.asked());

    // No rule ("buttermilk" is not the word "milk"): the model answers in
    // the background, and its answer is learned as a rule.
    add(&pod, "groceries", json!({"name": "Buttermilk"})).await;
    assert_eq!(
        section(&pod, "Buttermilk").await,
        "Unsorted",
        "the default meanwhile"
    );
    wait_section(&pod, "Buttermilk", "Dairy").await;
    add(&pod, "groceries", json!({"name": "Basmati  RICE"})).await;
    wait_section(&pod, "Basmati  RICE", "Pantry").await;
    assert_eq!(fake.asked(), ["Buttermilk", "Basmati  RICE"]);
    let r = rules(&pod).await;
    assert!(
        r.contains(&("buttermilk".into(), "Dairy".into()))
            && r.contains(&("basmati rice".into(), "Pantry".into())),
        "learned rules are rows: {r:?}"
    );
    // The next one comes from the learned rule at once.
    add(&pod, "groceries", json!({"name": "basmati rice"})).await;
    assert_eq!(section(&pod, "basmati rice").await, "Pantry");
    assert_eq!(fake.asked().len(), 2);

    // A hand edit changes the rule for that input, and the next row gets it.
    let kale = add(&pod, "groceries", json!({"name": "Kale"})).await;
    wait_section(&pod, "Kale", "Produce").await;
    update(&pod, "groceries", json!({"id": kale, "section": "Other"})).await;
    assert!(rules(&pod).await.contains(&("kale".into(), "Other".into())));
    assert!(
        !rules(&pod)
            .await
            .contains(&("kale".into(), "Produce".into()))
    );
    // A hand edit of an item a rule answered adds a more specific rule.
    let oat = rows(&pod, "groceries", "name", "section").await;
    let oat = &oat.iter().find(|r| r.1 == "Oat milk").unwrap().0;
    update(&pod, "groceries", json!({"id": oat, "section": "Other"})).await;
    assert!(
        rules(&pod)
            .await
            .contains(&("oat milk".into(), "Other".into()))
    );
    add(&pod, "groceries", json!({"name": "OAT MILK"})).await;
    assert_eq!(section(&pod, "OAT MILK").await, "Other", "longer key wins");
    assert_eq!(section(&pod, "Coconut milk (can)").await, "Pantry");
    assert_eq!(fake.asked().len(), 3, "only Kale went to the model");

    // Editing rules doesn't move rows until the field is computed again;
    // recompute_field does, without asking the model about the rest.
    let rule_rows = rows(&pod, "section_rules", "pattern", "section").await;
    let cucumber = &rule_rows.iter().find(|r| r.1 == "cucumber").unwrap().0;
    update(
        &pod,
        "section_rules",
        json!({"id": cucumber, "section": "Other"}),
    )
    .await;
    assert_eq!(section(&pod, "English cucumber").await, "Produce");
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [{"op": "recompute_field", "field": "section"}]}),
    )
    .await;
    assert_eq!(v["computing_rows"], 0, "{v}");
    assert_eq!(section(&pod, "English cucumber").await, "Other");
    assert_eq!(section(&pod, "Basmati  RICE").await, "Pantry");
    assert_eq!(fake.asked().len(), 3);

    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "groceries"}),
    )
    .await;
    assert_eq!(d["lookups"][0]["collection"], "section_rules", "{d}");
    assert_eq!(d["lookups"][0]["invalid_rules"], json!([]));
}

#[tokio::test]
async fn regex_rules_match_and_an_invalid_one_is_skipped_and_reported() {
    let fake = Arc::new(Fake::default());
    let pod = pod(fake.clone());
    define_rules(
        &pod,
        &[
            ("cucumbers?", "Produce"),
            ("^coconut milk", "Pantry"),
            ("milk", "Dairy"),
            ("(unclosed", "Other"),
        ],
    )
    .await;
    define_groceries(&pod, section_compute("regex")).await;
    add(&pod, "groceries", json!({"name": "Mini Cucumbers"})).await;
    add(&pod, "groceries", json!({"name": "Coconut Milk"})).await;
    add(&pod, "groceries", json!({"name": "Buttermilk"})).await;
    assert_eq!(section(&pod, "Mini Cucumbers").await, "Produce");
    assert_eq!(section(&pod, "Coconut Milk").await, "Pantry", "longest key");
    assert_eq!(
        section(&pod, "Buttermilk").await,
        "Dairy",
        "a regex finds it anywhere"
    );
    // "(unclosed" never matches; "unclosed" goes to the model, and the
    // learned rule is an anchored regex that matches only that input.
    add(&pod, "groceries", json!({"name": "Rice (unclosed bag)"})).await;
    wait_section(&pod, "Rice (unclosed bag)", "Pantry").await;
    assert!(
        rules(&pod)
            .await
            .contains(&("^rice \\(unclosed bag\\)$".into(), "Pantry".into())),
        "{:?}",
        rules(&pod).await
    );
    add(&pod, "groceries", json!({"name": "rice (unclosed  bag)"})).await;
    assert_eq!(section(&pod, "rice (unclosed  bag)").await, "Pantry");
    assert_eq!(fake.asked(), ["Rice (unclosed bag)"]);

    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "groceries"}),
    )
    .await;
    let invalid = &d["lookups"][0]["invalid_rules"];
    assert_eq!(invalid.as_array().unwrap().len(), 1, "{d}");
    assert_eq!(invalid[0]["key"], "(unclosed");
    assert!(
        invalid[0]["error"].as_str().unwrap().contains("unclosed"),
        "{invalid}"
    );
}

#[tokio::test]
async fn switching_a_model_field_to_rules_learns_its_cached_answers() {
    let fake = Arc::new(Fake::default());
    let pod = pod(fake.clone());
    // As Groceries is today: a model only.
    define_groceries(
        &pod,
        json!({"kind": "model", "from": "name", "prompt": PROMPT}),
    )
    .await;
    add(&pod, "groceries", json!({"name": "Kale"})).await;
    let rice = add(&pod, "groceries", json!({"name": "Rice"})).await;
    wait_section(&pod, "Kale", "Produce").await;
    wait_section(&pod, "Rice", "Pantry").await;
    update(&pod, "groceries", json!({"id": rice, "section": "Other"})).await;
    define_rules(&pod, &[("rice", "Pantry")]).await;
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [
            {"op": "update_field", "field": "section", "compute": section_compute("exact")}]}),
    )
    .await;
    assert_eq!(v["computing_rows"], 0, "{v}");
    let r = rules(&pod).await;
    assert!(r.contains(&("kale".into(), "Produce".into())), "{r:?}");
    assert!(
        r.contains(&("rice".into(), "Other".into()))
            && !r.contains(&("rice".into(), "Pantry".into())),
        "a person's answer replaces a rule: {r:?}"
    );
    add(&pod, "groceries", json!({"name": "kale"})).await;
    assert_eq!(section(&pod, "kale").await, "Produce");
    assert_eq!(fake.asked().len(), 2);
}

#[tokio::test]
async fn lookups_are_checked_against_the_collection_they_read() {
    let pod = pod(Arc::new(Fake::default()));
    define_rules(&pod, &[]).await;
    ok(
        &pod,
        "define_collection",
        json!({"name": "tags", "fields": [
            {"name": "word", "type": "text"}, {"name": "weight", "type": "number"},
            {"name": "note", "type": "text"}]}),
    )
    .await;
    define_groceries(&pod, section_compute("contains")).await;
    let lookup = |extra: Value| {
        let mut l = json!({"kind": "lookup", "from": "name", "collection": "section_rules",
                           "key": "pattern", "value": "section"});
        for (k, v) in extra.as_object().unwrap() {
            l[k] = v.clone();
        }
        l
    };
    for bad in [
        lookup(json!({"collection": "nope"})),
        lookup(json!({"collection": "groceries"})),
        lookup(json!({"key": "nope"})),
        lookup(json!({"match": "fuzzy"})),
        lookup(json!({"learn": "yes"})),
        lookup(json!({"from": "nope"})),
        // A key must be text; a number value doesn't fit a choice.
        lookup(json!({"collection": "tags", "key": "weight", "value": "word"})),
        lookup(json!({"collection": "tags", "key": "word", "value": "weight"})),
        // Learning needs a collection that takes a key and a value alone.
        lookup(json!({"collection": "tags", "key": "word", "value": "note", "learn": true})),
        // The model answers last.
        json!([{"kind": "model", "from": "name", "prompt": PROMPT}, lookup(json!({}))]),
        json!([]),
    ] {
        let (err, v) = tool(
            &pod,
            "apply_change",
            json!({"collection": "groceries", "changes": [
                {"op": "update_field", "field": "section", "compute": bad}]}),
        )
        .await;
        assert!(err && v["error"] == "invalid_change", "{bad}: {v}");
    }
    // The rules can't lose a field a lookup reads, change the key's type,
    // or (learned into) compute a field.
    for bad in [
        json!({"op": "remove_field", "field": "pattern"}),
        json!({"op": "update_field", "field": "pattern", "type": "number"}),
        json!({"op": "add_field", "name": "label", "type": "text", "optional": true,
               "compute": {"kind": "expr", "expr": "entity.section"}}),
    ] {
        let (err, v) = tool(
            &pod,
            "apply_change",
            json!({"collection": "section_rules", "changes": [bad]}),
        )
        .await;
        assert!(err && v["error"] == "invalid_change", "{bad}: {v}");
    }
    // A rename is fine: the lookup reads field ids.
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "section_rules", "changes": [
            {"op": "rename_field", "field": "pattern", "to": "keyword"}]}),
    )
    .await;
    assert_eq!(v["ok"], true);
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "groceries"}),
    )
    .await;
    assert_eq!(
        d["definition"]["fields"][2]["compute"][0]["key"], "keyword",
        "{d}"
    );
    // recompute_field needs a computed field.
    let (err, v) = tool(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [{"op": "recompute_field", "field": "name"}]}),
    )
    .await;
    assert!(err && v["error"] == "invalid_change", "{v}");
}
