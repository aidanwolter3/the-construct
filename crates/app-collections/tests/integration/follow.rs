//! Live lookups and counts (decision `live-computed-fields`) on a real pod:
//! meals → meal_ingredients (a lookup of the meal's `favorite`) →
//! ingredients (a count of favorite meal ingredients). A change to a row a
//! computed field reads moves that field at once, through the chain, on
//! create, update, delete and restore; a cycle of computes ends.

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

/// `(a, b)` of each live row of `collection`, its fields `fa` and `fb`.
async fn pairs(pod: &Pod, collection: &str, fa: &str, fb: &str) -> Vec<(String, Value)> {
    rows(pod, collection)
        .await
        .into_iter()
        .map(|e| (e[fa].as_str().unwrap_or("").to_owned(), e[fb].clone()))
        .collect()
}

/// meals, meal_ingredients (with the live lookup) and ingredients (with
/// the count), plus a few rows. Answers the ids of the meals by name.
async fn define_chain(pod: &Pod) {
    ok(
        pod,
        "define_collection",
        json!({"name": "meals", "title": "Meals", "fields": [
            {"name": "name", "type": "text"},
            {"name": "favorite", "type": "bool", "default": false}]}),
    )
    .await;
    ok(
        pod,
        "define_collection",
        json!({"name": "meal_ingredients", "title": "Meal ingredients", "fields": [
            {"name": "meal", "type": "text"},
            {"name": "item", "type": "text"},
            {"name": "storage", "type": "text", "optional": true},
            {"name": "favorite", "type": "bool", "default": false,
             "compute": {"kind": "lookup", "from": "meal", "collection": "meals",
                         "key": "name", "value": "favorite"}}]}),
    )
    .await;
    let v = ok(
        pod,
        "define_collection",
        json!({"name": "ingredients", "title": "Ingredients", "fields": [
            {"name": "name", "type": "text"},
            {"name": "storage", "type": "text", "optional": true},
            {"name": "favorite_count", "type": "number", "default": 0,
             "compute": {"kind": "count", "from": "name", "collection": "meal_ingredients",
                         "key": "item", "where": [{"field": "favorite", "equals": true}]}}]}),
    )
    .await;
    assert_eq!(
        v["definition"]["fields"][2]["compute"],
        json!({"kind": "count", "from": "name", "collection": "meal_ingredients", "key": "item",
               "where": [{"field": "favorite", "equals": "true"}]}),
        "described by names: {v}"
    );
}

async fn favorite_of(pod: &Pod, meal: &str, item: &str) -> Value {
    rows(pod, "meal_ingredients")
        .await
        .into_iter()
        .find(|e| e["meal"] == meal && e["item"] == item)
        .map(|e| e["favorite"].clone())
        .unwrap_or(Value::Null)
}

async fn count_of(pod: &Pod, ingredient: &str) -> Value {
    pairs(pod, "ingredients", "name", "favorite_count")
        .await
        .into_iter()
        .find(|(n, _)| n == ingredient)
        .map(|(_, c)| c)
        .unwrap_or(Value::Null)
}

fn meal_id(rows: &[Value], name: &str) -> String {
    rows.iter()
        .find(|e| e["name"] == name)
        .and_then(|e| e["id"].as_str())
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn toggling_a_meal_moves_its_ingredients_and_their_counts() {
    let pod = pod();
    define_chain(&pod).await;
    let chili = add(&pod, "meals", json!({"name": "Chili"})).await;
    add(&pod, "meals", json!({"name": "Soup", "favorite": true})).await;
    for (meal, item) in [
        ("Chili", "beans"),
        ("Chili", "Onion"),
        ("soup", "onion"),
        ("Soup", "carrot"),
    ] {
        add(
            &pod,
            "meal_ingredients",
            json!({"meal": meal, "item": item}),
        )
        .await;
    }
    for name in ["Beans", "onion", "carrot", "rice"] {
        add(&pod, "ingredients", json!({"name": name})).await;
    }
    // Computed in the writes: the lookup ignores case, and so does the count.
    assert_eq!(favorite_of(&pod, "Chili", "beans").await, false);
    assert_eq!(favorite_of(&pod, "soup", "onion").await, true);
    assert_eq!(count_of(&pod, "onion").await, 1);
    assert_eq!(count_of(&pod, "Beans").await, 0);
    assert_eq!(count_of(&pod, "carrot").await, 1);

    // Toggling the meal moves its ingredients, and the counts with them.
    write(
        &pod,
        "meals",
        "update",
        json!({"id": chili, "favorite": true}),
    )
    .await;
    assert_eq!(favorite_of(&pod, "Chili", "beans").await, true);
    assert_eq!(favorite_of(&pod, "Chili", "Onion").await, true);
    assert_eq!(count_of(&pod, "Beans").await, 1);
    assert_eq!(count_of(&pod, "onion").await, 2);
    assert_eq!(count_of(&pod, "carrot").await, 1, "another meal's");
    assert_eq!(count_of(&pod, "rice").await, 0);

    write(
        &pod,
        "meals",
        "update",
        json!({"id": chili, "favorite": false}),
    )
    .await;
    assert_eq!(favorite_of(&pod, "Chili", "beans").await, false);
    assert_eq!(count_of(&pod, "onion").await, 1);

    // A meal ingredient's own change: its item moves between counts.
    let soup_onion = rows(&pod, "meal_ingredients")
        .await
        .into_iter()
        .find(|e| e["meal"] == "soup")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    write(
        &pod,
        "meal_ingredients",
        "update",
        json!({"id": soup_onion, "item": "rice"}),
    )
    .await;
    assert_eq!(count_of(&pod, "onion").await, 0);
    assert_eq!(count_of(&pod, "rice").await, 1);
    // ...and a new one counts at once.
    add(
        &pod,
        "meal_ingredients",
        json!({"meal": "Soup", "item": "Beans"}),
    )
    .await;
    assert_eq!(count_of(&pod, "Beans").await, 1);
    // An ingredient's own name change counts again.
    let rice = meal_id(&rows(&pod, "ingredients").await, "rice");
    write(
        &pod,
        "ingredients",
        "update",
        json!({"id": rice, "name": "carrot"}),
    )
    .await;
    assert_eq!(
        pairs(&pod, "ingredients", "name", "favorite_count")
            .await
            .into_iter()
            .filter(|(n, _)| n == "carrot")
            .map(|(_, c)| c)
            .collect::<Vec<_>>(),
        vec![json!(1), json!(1)]
    );
}

#[tokio::test]
async fn renaming_a_meal_moves_the_rows_under_the_old_and_the_new_name() {
    let pod = pod();
    define_chain(&pod).await;
    let chili = add(&pod, "meals", json!({"name": "Chili", "favorite": true})).await;
    add(
        &pod,
        "meal_ingredients",
        json!({"meal": "Chili", "item": "beans"}),
    )
    .await;
    add(
        &pod,
        "meal_ingredients",
        json!({"meal": "Chili con carne", "item": "beef"}),
    )
    .await;
    add(&pod, "ingredients", json!({"name": "beans"})).await;
    add(&pod, "ingredients", json!({"name": "beef"})).await;
    assert_eq!(favorite_of(&pod, "Chili", "beans").await, true);
    assert_eq!(favorite_of(&pod, "Chili con carne", "beef").await, false);

    write(
        &pod,
        "meals",
        "update",
        json!({"id": chili, "name": "chili  CON carne"}),
    )
    .await;
    assert_eq!(
        favorite_of(&pod, "Chili", "beans").await,
        false,
        "no meal answers the old name: the default"
    );
    assert_eq!(favorite_of(&pod, "Chili con carne", "beef").await, true);
    assert_eq!(count_of(&pod, "beans").await, 0);
    assert_eq!(count_of(&pod, "beef").await, 1);

    // A lookup that matches by words follows a rename too.
    ok(
        &pod,
        "define_collection",
        json!({"name": "tags", "fields": [
            {"name": "word", "type": "text"}, {"name": "tag", "type": "text"}]}),
    )
    .await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "meal_ingredients", "changes": [
            {"op": "add_field", "name": "tag", "type": "text", "optional": true,
             "compute": {"kind": "lookup", "from": "item", "collection": "tags",
                         "key": "word", "value": "tag", "match": "contains"}}]}),
    )
    .await;
    let tag = add(&pod, "tags", json!({"word": "bean", "tag": "legume"})).await;
    let tag_of = |rows: Vec<Value>, item: &str| {
        rows.into_iter()
            .find(|e| e["item"] == item)
            .map(|e| e["tag"].clone())
            .unwrap()
    };
    assert_eq!(
        tag_of(rows(&pod, "meal_ingredients").await, "beans"),
        Value::Null
    );
    write(&pod, "tags", "update", json!({"id": tag, "word": "beans"})).await;
    assert_eq!(
        tag_of(rows(&pod, "meal_ingredients").await, "beans"),
        "legume"
    );
    // Only the value changes: still followed.
    write(&pod, "tags", "update", json!({"id": tag, "tag": "pulse"})).await;
    assert_eq!(
        tag_of(rows(&pod, "meal_ingredients").await, "beans"),
        "pulse"
    );
}

#[tokio::test]
async fn deleting_and_restoring_rows_moves_lookups_and_counts() {
    let pod = pod();
    define_chain(&pod).await;
    let chili = add(&pod, "meals", json!({"name": "Chili", "favorite": true})).await;
    let beans = add(
        &pod,
        "meal_ingredients",
        json!({"meal": "Chili", "item": "beans"}),
    )
    .await;
    add(
        &pod,
        "meal_ingredients",
        json!({"meal": "Chili", "item": "onion"}),
    )
    .await;
    add(&pod, "ingredients", json!({"name": "beans"})).await;
    let onion = add(&pod, "ingredients", json!({"name": "onion"})).await;
    assert_eq!(count_of(&pod, "beans").await, 1);

    // The meal goes: its ingredients read the default, and aren't counted.
    write(&pod, "meals", "delete", json!({"id": chili})).await;
    assert_eq!(favorite_of(&pod, "Chili", "beans").await, false);
    assert_eq!(count_of(&pod, "beans").await, 0);
    write(
        &pod,
        "meals",
        "update",
        json!({"id": chili, "deleted": false}),
    )
    .await;
    assert_eq!(favorite_of(&pod, "Chili", "beans").await, true);
    assert_eq!(count_of(&pod, "beans").await, 1);

    // A counted row goes and comes back.
    write(&pod, "meal_ingredients", "delete", json!({"id": beans})).await;
    assert_eq!(count_of(&pod, "beans").await, 0);
    assert_eq!(count_of(&pod, "onion").await, 1);
    write(
        &pod,
        "meal_ingredients",
        "update",
        json!({"id": beans, "deleted": false}),
    )
    .await;
    assert_eq!(count_of(&pod, "beans").await, 1);

    // A counting row restored after its count changed is counted again.
    write(&pod, "ingredients", "delete", json!({"id": onion})).await;
    add(
        &pod,
        "meal_ingredients",
        json!({"meal": "Chili", "item": "Onion"}),
    )
    .await;
    write(
        &pod,
        "ingredients",
        "update",
        json!({"id": onion, "deleted": false}),
    )
    .await;
    assert_eq!(count_of(&pod, "onion").await, 2);
}

#[tokio::test]
async fn a_count_matches_every_where_condition_and_is_computed_for_existing_rows() {
    let pod = pod();
    define_chain(&pod).await;
    add(&pod, "meals", json!({"name": "Chili", "favorite": true})).await;
    add(&pod, "meals", json!({"name": "Salad", "favorite": true})).await;
    add(&pod, "meals", json!({"name": "Stew"})).await;
    for (meal, item, storage) in [
        ("Chili", "onion", "pantry"),
        ("Salad", "onion", "fridge"),
        ("Stew", "onion", "fridge"),
        ("Salad", "kale", "fridge"),
    ] {
        add(
            &pod,
            "meal_ingredients",
            json!({"meal": meal, "item": item, "storage": storage}),
        )
        .await;
    }
    add(&pod, "ingredients", json!({"name": "onion"})).await;
    add(&pod, "ingredients", json!({"name": "kale"})).await;
    add(&pod, "ingredients", json!({"name": "garlic"})).await;

    // Added to a collection with rows: computed for each at once.
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "ingredients", "changes": [
            {"op": "add_field", "name": "cold_favorites", "type": "number", "default": 0,
             "compute": {"kind": "count", "from": "name", "collection": "meal_ingredients",
                         "key": "item", "where": [{"field": "favorite", "equals": "true"},
                                                  {"field": "storage", "equals": "fridge"}]}},
            {"op": "add_field", "name": "uses", "type": "number", "optional": true,
             "compute": {"kind": "count", "from": "name", "collection": "meal_ingredients",
                         "key": "item"}}]}),
    )
    .await;
    assert_eq!(v["ok"], true, "{v}");
    let all = rows(&pod, "ingredients").await;
    let get = |name: &str, f: &str| {
        all.iter()
            .find(|e| e["name"] == name)
            .map(|e| e[f].clone())
            .unwrap()
    };
    assert_eq!(get("onion", "cold_favorites"), 1, "only Salad's");
    assert_eq!(get("onion", "favorite_count"), 2);
    assert_eq!(get("onion", "uses"), 3, "no where: every row");
    assert_eq!(get("kale", "cold_favorites"), 1);
    assert_eq!(get("garlic", "uses"), 0);

    // A where field's change moves it.
    let stew_onion = rows(&pod, "meal_ingredients")
        .await
        .into_iter()
        .find(|e| e["meal"] == "Stew")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stew = meal_id(&rows(&pod, "meals").await, "Stew");
    write(
        &pod,
        "meals",
        "update",
        json!({"id": stew, "favorite": true}),
    )
    .await;
    assert_eq!(count_of(&pod, "onion").await, 3);
    let cold = |pod: Pod| async move {
        pairs(&pod, "ingredients", "name", "cold_favorites")
            .await
            .into_iter()
            .find(|(n, _)| n == "onion")
            .unwrap()
            .1
    };
    assert_eq!(cold(pod.clone()).await, 2);
    write(
        &pod,
        "meal_ingredients",
        "update",
        json!({"id": stew_onion, "storage": "pantry"}),
    )
    .await;
    assert_eq!(cold(pod.clone()).await, 1);

    // recompute_field works for a count (it has nothing to change here).
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "ingredients", "changes": [
            {"op": "recompute_field", "field": "cold_favorites"}]}),
    )
    .await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(cold(pod.clone()).await, 1);

    // describe_collection shows it by names; the counted collection can't
    // lose a field a count reads, nor be deleted.
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "ingredients"}),
    )
    .await;
    assert_eq!(
        d["definition"]["fields"][3]["compute"]["where"][1],
        json!({"field": "storage", "equals": "fridge"}),
        "{d}"
    );
    let (err, v) = tool(
        &pod,
        "apply_change",
        json!({"collection": "meal_ingredients", "changes": [
            {"op": "remove_field", "field": "storage"}]}),
    )
    .await;
    assert!(err && v["error"] == "invalid_change", "{v}");
    let (err, v) = tool(
        &pod,
        "delete_collection",
        json!({"collection": "meal_ingredients"}),
    )
    .await;
    assert!(err && v["error"] == "collection_in_use", "{v}");
}

#[tokio::test]
async fn counts_are_checked() {
    let pod = pod();
    define_chain(&pod).await;
    let count = |extra: Value| {
        let mut c = json!({"kind": "count", "from": "name", "collection": "meal_ingredients",
                           "key": "item"});
        for (k, v) in extra.as_object().unwrap() {
            c[k] = v.clone();
        }
        c
    };
    for (kind, compute) in [
        ("number", count(json!({"collection": "nope"}))),
        ("number", count(json!({"collection": "ingredients"}))),
        ("number", count(json!({"key": "nope"}))),
        ("number", count(json!({"from": "nope"}))),
        (
            "number",
            count(json!({"where": [{"field": "nope", "equals": "x"}]})),
        ),
        (
            "number",
            count(json!({"where": [{"field": "storage", "equals": " "}]})),
        ),
        ("number", count(json!({"where": [{"field": "storage"}]}))),
        ("number", count(json!({"where": "storage"}))),
        // A number field's only kind.
        ("text", count(json!({}))),
        (
            "number",
            json!([{"kind": "expr", "expr": "entity.name"}, count(json!({}))]),
        ),
    ] {
        let (err, v) = tool(
            &pod,
            "apply_change",
            json!({"collection": "ingredients", "changes": [
                {"op": "add_field", "name": "n", "type": kind, "optional": true,
                 "compute": compute}]}),
        )
        .await;
        assert!(err && v["error"] == "invalid_change", "{compute}: {v}");
    }
    // A single condition object is fine too.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "ingredients", "changes": [
            {"op": "add_field", "name": "n", "type": "number", "optional": true,
             "compute": count(json!({"where": {"field": "storage", "equals": "fridge"}}))}]}),
    )
    .await;
}

#[tokio::test]
async fn a_cycle_of_computes_ends() {
    let pod = pod();
    // a.n counts b's rows with m = 0; b.m looks up a.n: each answer flips
    // the other.
    ok(
        &pod,
        "define_collection",
        json!({"name": "a", "fields": [
            {"name": "name", "type": "text"},
            {"name": "n", "type": "number", "default": 0}]}),
    )
    .await;
    ok(
        &pod,
        "define_collection",
        json!({"name": "b", "fields": [
            {"name": "name", "type": "text"},
            {"name": "m", "type": "number", "optional": true,
             "compute": {"kind": "lookup", "from": "name", "collection": "a",
                         "key": "name", "value": "n"}}]}),
    )
    .await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "a", "changes": [
            {"op": "update_field", "field": "n",
             "compute": {"kind": "count", "from": "name", "collection": "b", "key": "name",
                         "where": [{"field": "m", "equals": "0"}]}}]}),
    )
    .await;
    add(&pod, "b", json!({"name": "k"})).await;
    // The write answers (the waves stop) and the rows are there.
    let id = add(&pod, "a", json!({"name": "k"})).await;
    write(&pod, "a", "update", json!({"id": id, "name": "K"})).await;
    assert_eq!(rows(&pod, "a").await.len(), 1);
    assert_eq!(rows(&pod, "b").await.len(), 1);
    // And the pod still answers other writes.
    add(&pod, "b", json!({"name": "other"})).await;
    assert_eq!(rows(&pod, "b").await.len(), 2);
}

#[tokio::test]
async fn a_write_that_moves_computes_names_them_and_tells_its_device_too() {
    let pod = pod();
    define_chain(&pod).await;
    let stew = add(&pod, "meals", json!({"name": "Stew"})).await;
    add(
        &pod,
        "meal_ingredients",
        json!({"meal": "Stew", "item": "onion"}),
    )
    .await;
    add(&pod, "ingredients", json!({"name": "onion"})).await;
    let mut events = pod.subscribe_data();
    let resp = pod_core::app(pod.clone())
        .oneshot(
            Request::post("/v1/collections/meals/update")
                .header("content-type", "application/json")
                .header("authorization", "Bearer phone")
                .header(protocol::DEVICE_HEADER, "phone-0001")
                .body(Body::from(
                    json!({"id": stew, "favorite": true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let e = events.try_recv().expect("one event");
    assert!(events.try_recv().is_err(), "only one");
    // The device didn't know the computes moved: it is told too.
    assert_eq!(e.except_device, None);
    let scope = e.scope.expect("scoped");
    let named: Vec<&str> = scope.detail["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["collection"].as_str().unwrap())
        .collect();
    assert_eq!(named, ["ingredients", "meal_ingredients", "meals"]);
    assert_eq!(
        scope.uri_prefixes,
        [
            "collections://ingredients/",
            "collections://meal_ingredients/",
            "collections://meals/"
        ]
    );
}
