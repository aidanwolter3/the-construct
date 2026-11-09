//! References, rollups, quantities and insert-from-query actions (decision
//! `collection-primitives`) on a real pod, ending with the grocery list
//! they were built for: recipe lines, then "add a recipe to groceries",
//! accumulated totals, checking off, and undo. The data is made up.

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

async fn refused(pod: &Pod, name: &str, args: Value) -> Value {
    let (err, v) = tool(pod, name, args.clone()).await;
    assert!(err, "{name} should be refused: {args} -> {v}");
    v
}

async fn post(pod: &Pod, collection: &str, op: &str, body: Value) -> (StatusCode, Value) {
    call(
        pod,
        "POST",
        &format!("/v1/collections/{collection}/{op}"),
        Some(body),
    )
    .await
}

async fn add(pod: &Pod, collection: &str, body: Value) -> String {
    let (status, v) = post(pod, collection, "create", body).await;
    assert_eq!(status, StatusCode::OK, "create {collection}: {v}");
    v["id"].as_str().unwrap().to_owned()
}

async fn update(pod: &Pod, collection: &str, body: Value) {
    let (status, v) = post(pod, collection, "update", body).await;
    assert_eq!(status, StatusCode::OK, "update {collection}: {v}");
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
        .collect()
}

/// The live row of `collection` whose `field` is `value`.
async fn row(pod: &Pod, collection: &str, field: &str, value: &str) -> Value {
    rows(pod, collection)
        .await
        .into_iter()
        .find(|e| e[field] == value)
        .unwrap_or(Value::Null)
}

/// meals (name unique, favorite, servings as text) and ingredients (name
/// unique).
async fn define_meals_and_ingredients(pod: &Pod) {
    ok(
        pod,
        "define_collection",
        json!({"name": "meals", "fields": [
            {"name": "name", "type": "text", "unique": true},
            {"name": "favorite", "type": "bool", "default": false},
            {"name": "servings", "type": "text", "optional": true}]}),
    )
    .await;
    ok(
        pod,
        "define_collection",
        json!({"name": "ingredients", "fields": [
            {"name": "name", "type": "text", "unique": true},
            {"name": "storage", "type": "text", "optional": true}]}),
    )
    .await;
}

/// recipe_lines: refs to a meal and an ingredient, the amount as text, and
/// whether the meal is a favorite (a lookup through the ref).
async fn define_recipe_lines(pod: &Pod) {
    let v = ok(
        pod,
        "define_collection",
        json!({"name": "recipe_lines", "fields": [
            {"name": "meal", "type": "ref", "collection": "meals"},
            {"name": "amount", "type": "text", "optional": true},
            {"name": "thing", "type": "text"},
            {"name": "ingredient", "type": "ref", "collection": "ingredients", "optional": true},
            {"name": "favorite", "type": "bool", "default": false,
             "compute": {"kind": "lookup", "via": "meal", "value": "favorite"}}]}),
    )
    .await;
    assert_eq!(
        v["definition"]["fields"][0],
        json!({"id": "f1", "name": "meal", "type": "ref", "collection": "meals", "optional": false})
    );
    assert_eq!(
        v["definition"]["fields"][4]["compute"],
        json!({"kind": "lookup", "via": "meal", "collection": "meals", "value": "favorite"}),
        "described by names: {v}"
    );
}

#[tokio::test]
async fn refs_are_validated_resolve_unique_fields_and_follow_their_rows() {
    let pod = pod();
    define_meals_and_ingredients(&pod).await;
    define_recipe_lines(&pod).await;
    let soup = add(
        &pod,
        "meals",
        json!({"name": "Harvest Soup", "servings": "4"}),
    )
    .await;
    let broth = add(&pod, "ingredients", json!({"name": "Chicken broth"})).await;
    // By id, and by a unique field (ignoring case and spacing).
    let line = add(
        &pod,
        "recipe_lines",
        json!({"meal": soup, "amount": "4 cups", "thing": "broth",
               "ingredient": {"name": "  chicken BROTH "}}),
    )
    .await;
    let e = row(&pod, "recipe_lines", "id", &line).await;
    assert_eq!(e["ingredient"], json!(broth), "served as the id: {e}");
    assert_eq!(e["meal"], json!(soup));
    assert_eq!(e["favorite"], false);
    // Refused: no such row, an unknown name, a field that isn't unique.
    for (bad, why) in [
        (
            json!({"meal": "rnope", "thing": "x"}),
            "is not a row of 'meals'",
        ),
        (
            json!({"meal": {"name": "Gruel"}, "thing": "x"}),
            "no row of 'meals'",
        ),
        (
            json!({"meal": {"servings": "4"}, "thing": "x"}),
            "is not a unique field",
        ),
        (json!({"meal": 7, "thing": "x"}), "is not a row"),
    ] {
        let (status, v) = post(&pod, "recipe_lines", "create", bad.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {v}");
        assert!(v["detail"].as_str().unwrap().contains(why), "{bad}: {v}");
    }
    // Unique: on create, update and restore.
    let (status, v) = post(
        &pod,
        "ingredients",
        "create",
        json!({"name": "chicken  broth"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"], "not_unique");
    let onion = add(&pod, "ingredients", json!({"name": "Onion"})).await;
    let (status, _) = post(
        &pod,
        "ingredients",
        "update",
        json!({"id": onion, "name": "CHICKEN BROTH"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    post(&pod, "ingredients", "delete", json!({"id": onion})).await;
    add(&pod, "ingredients", json!({"name": "Onion"})).await;
    let (status, v) = post(
        &pod,
        "ingredients",
        "update",
        json!({"id": onion, "deleted": false}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a restore can't duplicate: {v}"
    );
    // edit_rows and add_row resolve refs too.
    let v = ok(
        &pod,
        "add_row",
        json!({"collection": "recipe_lines",
               "values": {"meal": {"name": "Harvest Soup"}, "thing": "salt"}}),
    )
    .await;
    assert!(v["id"].is_string(), "{v}");
    let v = refused(
        &pod,
        "add_row",
        json!({"collection": "ingredients", "values": {"name": "Onion"}}),
    )
    .await;
    assert_eq!(v["error"], "not_unique", "{v}");

    // A lookup through a ref is live: the meal's favorite moves its lines.
    update(&pod, "meals", json!({"id": soup, "favorite": true})).await;
    assert_eq!(
        row(&pod, "recipe_lines", "id", &line).await["favorite"],
        true
    );
    // The meal deleted: the ref keeps its id, the lookup answers nothing;
    // a restore heals both.
    post(&pod, "meals", "delete", json!({"id": soup})).await;
    let e = row(&pod, "recipe_lines", "id", &line).await;
    assert_eq!(
        (e["meal"].clone(), e["favorite"].clone()),
        (json!(soup), json!(false))
    );
    update(&pod, "meals", json!({"id": soup, "deleted": false})).await;
    assert_eq!(
        row(&pod, "recipe_lines", "id", &line).await["favorite"],
        true
    );
    // Pointing the line elsewhere moves it.
    let chili = add(&pod, "meals", json!({"name": "Weeknight Chili"})).await;
    update(&pod, "recipe_lines", json!({"id": line, "meal": chili})).await;
    assert_eq!(
        row(&pod, "recipe_lines", "id", &line).await["favorite"],
        false
    );

    // A collection a ref points at can't be deleted.
    let v = refused(
        &pod,
        "delete_collection",
        json!({"collection": "ingredients"}),
    )
    .await;
    assert_eq!(v["error"], "collection_in_use", "{v}");
    // A layout reads through a ref with a nested binding; the pod checks it.
    let item = |field: &str| {
        json!({"type": "core://Text", "style": "Body", "value_expr":
            format!("$item.amount ${{collections://ingredients/$item.ingredient#{field}}}")})
    };
    let layout = |field: &str| {
        json!({"type": "core://Workspace", "title": "Lines", "slots": [
            {"slot_id": "list", "provider_uri": "core://custom", "content":
                {"type": "core://List", "query_key": "collection:recipe_lines",
                 "item_template": item(field)}}]})
    };
    let feeds = json!([
        ["collections", "recipe_lines"],
        ["collections", "ingredients"]
    ]);
    ok(
        &pod,
        "create_app",
        json!({"title": "Lines", "feeds": feeds, "layout": layout("name")}),
    )
    .await;
    let v = refused(
        &pod,
        "create_app",
        json!({"title": "Bad", "feeds": feeds, "layout": layout("colour")}),
    )
    .await;
    assert!(v.to_string().contains("colour"), "{v}");
}

#[tokio::test]
async fn unique_and_text_to_ref_conversions_report_what_doesnt_fit() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "ingredients", "fields": [{"name": "name", "type": "text"}]}),
    )
    .await;
    let broth = add(&pod, "ingredients", json!({"name": "Chicken broth"})).await;
    let dup = add(&pod, "ingredients", json!({"name": "chicken broth"})).await;
    // Making it unique fails while two rows share a value.
    let v = refused(
        &pod,
        "apply_change",
        json!({"collection": "ingredients",
               "changes": [{"op": "update_field", "field": "name", "unique": true}]}),
    )
    .await;
    assert_eq!(v["error"], "not_unique", "{v}");
    assert_eq!(v["duplicates"][0]["rows"], json!([broth, dup]), "{v}");
    post(&pod, "ingredients", "delete", json!({"id": dup})).await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "ingredients",
               "changes": [{"op": "update_field", "field": "name", "unique": true}]}),
    )
    .await;
    // A text field becomes a ref: names resolve, the rest is kept aside.
    ok(
        &pod,
        "define_collection",
        json!({"name": "lines", "fields": [
            {"name": "thing", "type": "text"},
            {"name": "ingredient", "type": "text", "optional": true}]}),
    )
    .await;
    let a = add(
        &pod,
        "lines",
        json!({"thing": "a", "ingredient": "CHICKEN broth"}),
    )
    .await;
    let b = add(
        &pod,
        "lines",
        json!({"thing": "b", "ingredient": "Unicorn tears"}),
    )
    .await;
    add(&pod, "lines", json!({"thing": "c"})).await;
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "lines", "changes": [
            {"op": "update_field", "field": "ingredient", "type": "ref", "collection": "ingredients"}]}),
    )
    .await;
    let c = &v["conversions"][0];
    assert_eq!(
        (c["converted"].clone(), c["set_aside"].clone()),
        (json!(1), json!(1)),
        "{v}"
    );
    assert_eq!(c["samples"][0], json!({"id": b, "value": "Unicorn tears"}));
    assert_eq!(
        row(&pod, "lines", "id", &a).await["ingredient"],
        json!(broth)
    );
    assert_eq!(
        row(&pod, "lines", "id", &b).await["ingredient"],
        Value::Null
    );
    // Only text and ref fields are unique; a ref needs its collection.
    for bad in [
        json!({"op": "add_field", "name": "n", "type": "number", "default": 0, "unique": true}),
        json!({"op": "add_field", "name": "r", "type": "ref", "optional": true}),
        json!({"op": "add_field", "name": "r", "type": "ref", "collection": "nope", "optional": true}),
        json!({"op": "add_field", "name": "t", "type": "text", "optional": true, "collection": "ingredients"}),
    ] {
        refused(
            &pod,
            "apply_change",
            json!({"collection": "lines", "changes": [bad]}),
        )
        .await;
    }
}

#[tokio::test]
async fn rollups_aggregate_the_rows_pointing_here_and_stay_live() {
    let pod = pod();
    define_meals_and_ingredients(&pod).await;
    define_recipe_lines(&pod).await;
    ok(
        &pod,
        "define_collection",
        json!({"name": "ratings", "fields": [
            {"name": "meal", "type": "ref", "collection": "meals"},
            {"name": "stars", "type": "number"},
            {"name": "by", "type": "text"}]}),
    )
    .await;
    let v = ok(
        &pod,
        "apply_change",
        json!({"collection": "ingredients", "changes": [
            {"op": "add_field", "name": "favorite_count", "type": "number", "default": 0,
             "compute": {"kind": "rollup", "collection": "recipe_lines", "via": "ingredient",
                         "op": "count", "where": [{"field": "favorite", "equals": true}]}},
            {"op": "add_field", "name": "used", "type": "bool", "default": false,
             "compute": {"kind": "rollup", "collection": "recipe_lines", "via": "ingredient", "op": "any"}}]}),
    )
    .await;
    assert_eq!(
        v["definition"]["fields"][2]["compute"],
        json!({"kind": "rollup", "collection": "recipe_lines", "via": "ingredient", "op": "count",
               "where": [{"field": "favorite", "equals": "true"}]}),
        "{v}"
    );
    ok(
        &pod,
        "apply_change",
        json!({"collection": "meals", "changes": [
            {"op": "add_field", "name": "stars_total", "type": "number", "default": 0,
             "compute": {"kind": "rollup", "collection": "ratings", "via": "meal", "op": "sum", "field": "stars"}},
            {"op": "add_field", "name": "best", "type": "number", "optional": true,
             "compute": {"kind": "rollup", "collection": "ratings", "via": "meal", "op": "max", "field": "stars"}},
            {"op": "add_field", "name": "worst", "type": "number", "optional": true,
             "compute": {"kind": "rollup", "collection": "ratings", "via": "meal", "op": "min", "field": "stars"}},
            {"op": "add_field", "name": "raters", "type": "text", "optional": true,
             "compute": {"kind": "rollup", "collection": "ratings", "via": "meal", "op": "list", "field": "by"}}]}),
    )
    .await;
    let soup = add(&pod, "meals", json!({"name": "Harvest Soup"})).await;
    let chili = add(
        &pod,
        "meals",
        json!({"name": "Weeknight Chili", "favorite": true}),
    )
    .await;
    let onion = add(&pod, "ingredients", json!({"name": "Onion"})).await;
    let count = |e: Value| (e["favorite_count"].clone(), e["used"].clone());
    assert_eq!(
        count(row(&pod, "ingredients", "id", &onion).await),
        (json!(0), json!(false))
    );
    let a = add(
        &pod,
        "recipe_lines",
        json!({"meal": soup, "thing": "onion", "ingredient": onion}),
    )
    .await;
    assert_eq!(
        count(row(&pod, "ingredients", "id", &onion).await),
        (json!(0), json!(true))
    );
    add(
        &pod,
        "recipe_lines",
        json!({"meal": chili, "thing": "onion", "ingredient": onion}),
    )
    .await;
    assert_eq!(
        count(row(&pod, "ingredients", "id", &onion).await),
        (json!(1), json!(true))
    );
    // Through the chain: meal → line favorite (a lookup) → the rollup.
    update(&pod, "meals", json!({"id": soup, "favorite": true})).await;
    assert_eq!(
        count(row(&pod, "ingredients", "id", &onion).await).0,
        json!(2)
    );
    // Delete, restore, re-point.
    post(&pod, "recipe_lines", "delete", json!({"id": a})).await;
    assert_eq!(
        count(row(&pod, "ingredients", "id", &onion).await).0,
        json!(1)
    );
    update(&pod, "recipe_lines", json!({"id": a, "deleted": false})).await;
    assert_eq!(
        count(row(&pod, "ingredients", "id", &onion).await).0,
        json!(2)
    );
    let garlic = add(&pod, "ingredients", json!({"name": "Garlic"})).await;
    update(&pod, "recipe_lines", json!({"id": a, "ingredient": garlic})).await;
    assert_eq!(
        count(row(&pod, "ingredients", "id", &onion).await).0,
        json!(1)
    );
    assert_eq!(
        count(row(&pod, "ingredients", "id", &garlic).await),
        (json!(1), json!(true))
    );

    // sum, min, max and list.
    for (stars, by) in [(4, "Ana"), (2, "Bo"), (5, "ana")] {
        add(
            &pod,
            "ratings",
            json!({"meal": soup, "stars": stars, "by": by}),
        )
        .await;
    }
    let m = row(&pod, "meals", "id", &soup).await;
    assert_eq!(
        (
            m["stars_total"].clone(),
            m["best"].clone(),
            m["worst"].clone(),
            m["raters"].clone()
        ),
        (json!(11), json!(5), json!(2), json!("Ana, Bo")),
        "{m}"
    );
    let m = row(&pod, "meals", "id", &chili).await;
    assert_eq!(
        (
            m["stars_total"].clone(),
            m["best"].clone(),
            m["raters"].clone()
        ),
        (json!(0), Value::Null, Value::Null),
        "no rows: 0, or empty: {m}"
    );

    // Refused: a via that isn't a ref here, a sum of text, a wrong type.
    for bad in [
        json!({"kind": "rollup", "collection": "recipe_lines", "via": "thing", "op": "count"}),
        json!({"kind": "rollup", "collection": "recipe_lines", "via": "meal", "op": "count"}),
        json!({"kind": "rollup", "collection": "recipe_lines", "via": "ingredient", "op": "sum", "field": "thing"}),
        json!({"kind": "rollup", "collection": "recipe_lines", "via": "ingredient", "op": "sum"}),
        json!({"kind": "rollup", "collection": "recipe_lines", "via": "ingredient", "op": "avg"}),
    ] {
        refused(
            &pod,
            "apply_change",
            json!({"collection": "ingredients", "changes": [
                {"op": "add_field", "name": "x", "type": "number", "optional": true, "compute": bad}]}),
        )
        .await;
    }
    // The shipped count still works next to them.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "ingredients", "changes": [
            {"op": "add_field", "name": "lines_by_name", "type": "number", "default": 0,
             "compute": {"kind": "count", "from": "name", "collection": "recipe_lines", "key": "thing"}}]}),
    )
    .await;
    // Two lines are still called "onion" (one now points at garlic).
    assert_eq!(
        row(&pod, "ingredients", "id", &onion).await["lines_by_name"],
        json!(2)
    );
}

#[tokio::test]
async fn quantities_parse_format_and_sum_by_unit() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "pantry", "fields": [{"name": "name", "type": "text"}]}),
    )
    .await;
    ok(
        &pod,
        "define_collection",
        json!({"name": "portions", "fields": [
            {"name": "item", "type": "ref", "collection": "pantry"},
            {"name": "amount", "type": "quantity", "optional": true}]}),
    )
    .await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "pantry", "changes": [
            {"op": "add_field", "name": "total", "type": "quantity", "optional": true,
             "compute": {"kind": "rollup", "collection": "portions", "via": "item", "op": "sum", "field": "amount"}}]}),
    )
    .await;
    let flour = add(&pod, "pantry", json!({"name": "Flour"})).await;
    for (amount, stored, text) in [
        (
            json!("2 cups"),
            json!({"amount": 2, "unit": "cup"}),
            "2 cups",
        ),
        (
            json!("1 Tbsp"),
            json!({"amount": 1, "unit": "tbsp"}),
            "1 tbsp",
        ),
        (
            json!("3 cloves"),
            json!({"amount": 3, "unit": "clove"}),
            "3 cloves",
        ),
        (
            json!({"amount": 0.5, "unit": "cups"}),
            json!({"amount": 0.5, "unit": "cup"}),
            "1/2 cup",
        ),
        (json!("to taste"), json!("to taste"), "to taste"),
    ] {
        let id = add(&pod, "portions", json!({"item": flour, "amount": amount})).await;
        let e = row(&pod, "portions", "id", &id).await;
        assert_eq!(
            (e["amount"].clone(), e["amount_text"].clone()),
            (stored, json!(text))
        );
    }
    let p = row(&pod, "pantry", "id", &flour).await;
    assert_eq!(p["total_text"], "2 9/16 cups + 3 cloves", "{p}");
    assert_eq!(
        p["total"],
        json!([{"amount": 2.5625, "unit": "cup"}, {"amount": 3, "unit": "clove"}])
    );
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "portions"}),
    )
    .await;
    assert_eq!(d["quantities"][0]["field"], "amount");
    assert_eq!(d["quantities"][0]["unparsed"], 1, "{d}");
    // Refused: what isn't a quantity at all.
    let (status, _) = post(
        &pod,
        "portions",
        "create",
        json!({"item": flour, "amount": true}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // A text field turned quantity parses what it can and keeps the rest.
    ok(
        &pod,
        "define_collection",
        json!({"name": "notes", "fields": [{"name": "q", "type": "text"}]}),
    )
    .await;
    let n1 = add(&pod, "notes", json!({"q": "1 1/2 cups"})).await;
    let n2 = add(&pod, "notes", json!({"q": "a handful or so"})).await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "notes", "changes": [{"op": "update_field", "field": "q", "type": "quantity"}]}),
    )
    .await;
    assert_eq!(row(&pod, "notes", "id", &n1).await["q_text"], "1 1/2 cups");
    assert_eq!(
        row(&pod, "notes", "id", &n2).await["q_text"],
        "a handful or so"
    );
}

/// The collections of the grocery list, as the decision describes them,
/// with made-up recipes. Answers the meal ids `(soup, chili)`.
async fn define_groceries(pod: &Pod) -> (String, String) {
    define_meals_and_ingredients(pod).await;
    define_recipe_lines(pod).await;
    ok(
        pod,
        "define_collection",
        json!({"name": "groceries", "fields": [
            {"name": "name", "type": "text"},
            {"name": "ingredient", "type": "ref", "collection": "ingredients", "optional": true},
            {"name": "done", "type": "bool", "default": false}]}),
    )
    .await;
    ok(
        pod,
        "define_collection",
        json!({"name": "grocery_entries", "fields": [
            {"name": "grocery", "type": "ref", "collection": "groceries"},
            {"name": "quantity", "type": "quantity", "optional": true},
            {"name": "recipe", "type": "ref", "collection": "meals", "optional": true},
            {"name": "recipe_name", "type": "text", "optional": true,
             "compute": {"kind": "lookup", "via": "recipe", "value": "name"}}]}),
    )
    .await;
    let v = ok(
        pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [
            {"op": "add_field", "name": "total", "type": "quantity", "optional": true,
             "compute": {"kind": "rollup", "collection": "grocery_entries", "via": "grocery",
                         "op": "sum", "field": "quantity"}},
            {"op": "add_field", "name": "recipes", "type": "text", "optional": true,
             "compute": {"kind": "rollup", "collection": "grocery_entries", "via": "grocery",
                         "op": "list", "field": "recipe_name"}},
            {"op": "set_action", "action": {
                "name": "add_recipe", "title": "Add a recipe to groceries",
                "params": [{"name": "meal", "type": "ref", "collection": "meals"},
                           {"name": "servings", "type": "number", "optional": true}],
                "source": {"collection": "recipe_lines", "where": [
                    {"field": "meal", "equals": "$param.meal"},
                    {"field": "ingredient", "empty": false}]},
                "scale": {"times": "$param.servings", "per": "$row.meal.servings"},
                "match": {"ingredient": "$row.ingredient"},
                "create": {"name": "$row.ingredient.name", "done": false},
                "update": {"done": false},
                "reset_when": [{"field": "done", "equals": true}],
                "child": {"collection": "grocery_entries", "values": {
                    "grocery": "$parent.id",
                    "quantity": {"from": "$row.amount", "scaled": true},
                    "recipe": "$row.meal"}}}}]}),
    )
    .await;
    let a = &v["definition"]["actions"][0];
    assert_eq!(
        a["endpoint"],
        "/v1/collections/groceries/actions/add_recipe"
    );
    assert_eq!(
        a["create"]["name"], "$row.ingredient.name",
        "described by names: {a}"
    );
    assert_eq!(
        a["child"]["values"]["quantity"],
        json!({"from": "$row.amount", "scaled": true})
    );

    let soup = add(
        pod,
        "meals",
        json!({"name": "Harvest Soup", "servings": "4"}),
    )
    .await;
    let chili = add(
        pod,
        "meals",
        json!({"name": "Weeknight Chili", "servings": "Serves 6"}),
    )
    .await;
    for name in ["Chicken broth", "Onion", "Garlic", "Olive oil", "Salt"] {
        add(pod, "ingredients", json!({"name": name})).await;
    }
    for (meal, amount, thing, ingredient) in [
        (&soup, "4 cups", "chicken broth", Some("Chicken broth")),
        (&soup, "1 large", "onion, diced", Some("Onion")),
        (&soup, "2 cloves", "garlic", Some("Garlic")),
        (&soup, "1 tbsp", "olive oil", Some("Olive oil")),
        (&soup, "to taste", "salt", Some("Salt")),
        (&soup, "1", "bay leaf", None),
        (&chili, "2 1/4 cups", "chicken broth", Some("Chicken broth")),
        (&chili, "1 medium", "onion", Some("Onion")),
        (&chili, "3 cloves", "garlic", Some("Garlic")),
        (&chili, "2 tsp", "olive oil", Some("Olive oil")),
    ] {
        let mut line = json!({"meal": meal, "amount": amount, "thing": thing});
        if let Some(i) = ingredient {
            line["ingredient"] = json!({"name": i});
        }
        add(pod, "recipe_lines", line).await;
    }
    (soup, chili)
}

/// `name → (total_text, recipes, done)` of the live groceries.
async fn groceries(pod: &Pod) -> Vec<(String, String, String, bool)> {
    rows(pod, "groceries")
        .await
        .into_iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap_or("").to_owned(),
                e["total_text"].as_str().unwrap_or("").to_owned(),
                e["recipes"].as_str().unwrap_or("").to_owned(),
                e["done"] == true,
            )
        })
        .collect()
}

fn g(name: &str, total: &str, recipes: &str, done: bool) -> (String, String, String, bool) {
    (name.into(), total.into(), recipes.into(), done)
}

#[tokio::test]
async fn the_grocery_list_accumulates_recipes_checks_off_and_undoes() {
    let pod = pod();
    let (soup, chili) = define_groceries(&pod).await;
    // A layout's button: action.mutation.optimistic to the action's endpoint.
    let button = json!({"type": "core://Button", "label": "Add to groceries",
        "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
            "field": "adding", "value_expr": "true",
            "endpoint": "/v1/collections/groceries/actions/add_recipe",
            "payload": {"meal": "$item.id", "servings": "$form?.servings", "client_id": "$nonce"}}});
    let list = |button: Value| {
        json!({"type": "core://Workspace", "title": "Meals", "slots": [
            {"slot_id": "list", "provider_uri": "core://custom", "content":
                {"type": "core://List", "query_key": "collection:meals", "item_template":
                    {"type": "core://Row", "children": [
                        {"type": "core://Text", "style": "Body", "value_expr": "$item.name"}, button]}}},
            {"slot_id": "groceries", "provider_uri": "core://custom", "content":
                {"type": "core://List", "query_key": "collection:groceries", "item_template":
                    {"type": "core://Text", "style": "Body", "value_expr": "$item.name · $item.total_text ($item.recipes)"}}}]})
    };
    let feeds = json!([["collections", "meals"], ["collections", "groceries"]]);
    ok(
        &pod,
        "create_app",
        json!({"title": "Meals", "feeds": feeds, "layout": list(button.clone())}),
    )
    .await;
    // The pod checks the action and its payload.
    let mut bad = button.clone();
    bad["on_click"]["payload"]["people"] = json!("4");
    let v = refused(
        &pod,
        "create_app",
        json!({"title": "Bad", "feeds": feeds, "layout": list(bad)}),
    )
    .await;
    assert!(v.to_string().contains("not a parameter"), "{v}");
    let mut bad = button.clone();
    bad["on_click"]["endpoint"] = json!("/v1/collections/groceries/actions/nope");
    refused(
        &pod,
        "create_app",
        json!({"title": "Bad", "feeds": feeds, "layout": list(bad)}),
    )
    .await;

    // 1. The soup, as the button sends it (no servings: as written).
    let (status, v) = post(
        &pod,
        "groceries",
        "actions/add_recipe",
        json!({"meal": soup, "servings": "", "client_id": "tap-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["rows"].clone(), v["created"].clone(), v["added"].clone()),
        (json!(5), json!(5), json!(5)),
        "the bay leaf has no ingredient: {v}"
    );
    assert_eq!(v["unparsed"][0]["value"], "to taste", "{v}");
    // A retried tap is applied once.
    let (_, again) = post(
        &pod,
        "groceries",
        "actions/add_recipe",
        json!({"meal": soup, "servings": "", "client_id": "tap-1"}),
    )
    .await;
    assert_eq!(
        (again["repeated"].clone(), again["edit"].clone()),
        (json!(true), v["edit"].clone())
    );
    assert_eq!(rows(&pod, "grocery_entries").await.len(), 5);

    // 2. The chili over MCP, the meal named by its unique name.
    let v = ok(
        &pod,
        "run_action",
        json!({"collection": "groceries", "action": "add_recipe",
               "params": {"meal": {"name": "weeknight chili"}, "servings": 6}}),
    )
    .await;
    assert_eq!(
        (v["created"].clone(), v["added"].clone()),
        (json!(0), json!(4)),
        "{v}"
    );
    // A manual item still works.
    add(&pod, "groceries", json!({"name": "Paper towels"})).await;
    let both = "Harvest Soup, Weeknight Chili";
    assert_eq!(
        groceries(&pod).await,
        vec![
            g("Chicken broth", "6 1/4 cups", both, false),
            g("Onion", "1 large + 1 medium", both, false),
            g("Garlic", "5 cloves", both, false),
            g("Olive oil", "1 2/3 tbsp", both, false),
            g("Salt", "", "Harvest Soup", false),
            g("Paper towels", "", "", false),
        ]
    );
    // A recipe's contribution can be traced.
    let from_chili: Vec<String> = rows(&pod, "grocery_entries")
        .await
        .into_iter()
        .filter(|e| e["recipe"] == chili)
        .map(|e| e["quantity_text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        from_chili,
        vec!["2 1/4 cups", "1 medium", "3 cloves", "2 tsp"]
    );

    // Check off the broth; the soup again, for 8 (twice the recipe).
    let broth = row(&pod, "groceries", "name", "Chicken broth").await;
    update(&pod, "groceries", json!({"id": broth["id"], "done": true})).await;
    let v = ok(
        &pod,
        "run_action",
        json!({"collection": "groceries", "action": "add_recipe",
               "params": {"meal": soup, "servings": "8"}}),
    )
    .await;
    assert_eq!(
        (v["updated"].clone(), v["reset"].clone(), v["added"].clone()),
        (json!(1), json!(2), json!(5)),
        "the checked-off broth starts again: {v}"
    );
    assert_eq!(
        groceries(&pod).await[..4],
        [
            g("Chicken broth", "8 cups", "Harvest Soup", false),
            g("Onion", "3 large + 1 medium", both, false),
            g("Garlic", "9 cloves", both, false),
            g("Olive oil", "3 2/3 tbsp", both, false),
        ]
    );
    // Undo puts it all back: the broth checked off with its two recipes.
    let undone = ok(&pod, "undo_edit", json!({"edit": v["edit"]})).await;
    assert_eq!(undone["skipped"], json!([]), "{undone}");
    assert_eq!(
        groceries(&pod).await[..4],
        [
            g("Chicken broth", "6 1/4 cups", both, true),
            g("Onion", "1 large + 1 medium", both, false),
            g("Garlic", "5 cloves", both, false),
            g("Olive oil", "1 2/3 tbsp", both, false),
        ]
    );
    // Removing a grocery takes all its contributions off the list.
    let garlic = row(&pod, "groceries", "name", "Garlic").await;
    post(&pod, "groceries", "delete", json!({"id": garlic["id"]})).await;
    assert!(row(&pod, "groceries", "name", "Garlic").await.is_null());
    // Each call is listed among the collection's edits.
    let edits = ok(
        &pod,
        "describe_collection",
        json!({"collection": "groceries"}),
    )
    .await;
    assert!(
        edits["recent_edits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["note"] == "Add a recipe to groceries"),
        "{edits}"
    );
}

#[tokio::test]
async fn actions_are_checked_when_defined_and_called() {
    let pod = pod();
    define_groceries(&pod).await;
    let base = json!({"name": "a", "source": {"collection": "recipe_lines"},
        "match": {"ingredient": "$row.ingredient"}, "create": {"name": "$row.thing"}});
    let with = |k: &str, v: Value| {
        let mut a = base.clone();
        a[k] = v;
        a
    };
    for (bad, why) in [
        (
            with("source", json!({"collection": "nope"})),
            "doesn't exist",
        ),
        (with("match", json!({})), "match names"),
        (with("create", json!({})), "has no 'name'"),
        (
            with("match", json!({"ingredient": "$param.x"})),
            "names no parameter",
        ),
        (
            with("match", json!({"ingredient": "$row.nope"})),
            "not a field",
        ),
        (
            with("match", json!({"ingredient": "$row.thing.name"})),
            "not a ref",
        ),
        (
            with("match", json!({"ingredient": "$parent.id"})),
            "only in child",
        ),
        (
            with(
                "child",
                json!({"collection": "grocery_entries", "values": {"recipe": "$row.meal"}}),
            ),
            "links each child",
        ),
        (
            with(
                "child",
                json!({"collection": "grocery_entries", "values": {
                "grocery": "$parent.id", "quantity": {"from": "$row.amount", "scaled": true}}}),
            ),
            "has no scale",
        ),
        (
            with("reset_when", json!([{"field": "done", "equals": true}])),
            "has a child",
        ),
    ] {
        let v = refused(
            &pod,
            "apply_change",
            json!({"collection": "groceries", "changes": [{"op": "set_action", "action": bad}]}),
        )
        .await;
        assert!(
            v["detail"].as_str().unwrap_or("").contains(why),
            "{why}: {v}"
        );
    }
    // Calls: unknown params, a missing one, a meal that doesn't exist.
    for (params, why) in [
        (json!({"meal": "rnope"}), "is not a row"),
        (json!({}), "'meal' is required"),
        (
            json!({"meal": {"name": "Harvest Soup"}, "people": 2}),
            "not a parameter",
        ),
        (
            json!({"meal": {"name": "Harvest Soup"}, "servings": "lots"}),
            "is a number",
        ),
    ] {
        let v = refused(
            &pod,
            "run_action",
            json!({"collection": "groceries", "action": "add_recipe", "params": params}),
        )
        .await;
        assert!(
            v["detail"].as_str().unwrap_or("").contains(why),
            "{why}: {v}"
        );
    }
    let (status, _) = post(&pod, "groceries", "actions/nope", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A field an action names can't be removed; the action can.
    let v = refused(
        &pod,
        "apply_change",
        json!({"collection": "recipe_lines", "changes": [{"op": "remove_field", "field": "amount"}]}),
    )
    .await;
    assert!(v["detail"].as_str().unwrap().contains("add_recipe"), "{v}");
    ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [{"op": "remove_action", "action": "add_recipe"}]}),
    )
    .await;
    // A collection an action reads can't be deleted while it does.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "groceries", "changes": [{"op": "set_action", "action": base}]}),
    )
    .await;
    let v = refused(
        &pod,
        "delete_collection",
        json!({"collection": "recipe_lines"}),
    )
    .await;
    assert_eq!(v["error"], "collection_in_use", "{v}");
}

#[tokio::test]
async fn a_delete_action_clears_the_matching_rows_in_one_undoable_edit() {
    let pod = pod();
    ok(
        &pod,
        "define_collection",
        json!({"name": "aisles", "fields": [{"name": "name", "type": "text", "unique": true}]}),
    )
    .await;
    ok(
        &pod,
        "define_collection",
        json!({"name": "groceries", "fields": [
            {"name": "name", "type": "text"},
            {"name": "done", "type": "bool", "default": false},
            {"name": "aisle", "type": "ref", "collection": "aisles", "optional": true}]}),
    )
    .await;
    ok(
        &pod,
        "apply_change",
        json!({"collection": "aisles", "changes": [
            {"op": "add_field", "name": "items", "type": "number", "default": 0,
             "compute": {"kind": "rollup", "collection": "groceries", "via": "aisle", "op": "count"}}]}),
    )
    .await;
    let set = |a: Value| json!({"collection": "groceries", "changes": [{"op": "set_action", "action": a}]});
    // Refusals: its own collection only with delete, delete only of its own,
    // and nothing to upsert.
    for (bad, why) in [
        (
            json!({"name": "a", "source": {"collection": "groceries"},
                   "match": {"name": "$row.name"}}),
            "only a delete action",
        ),
        (
            json!({"name": "a", "source": {"collection": "aisles"}, "delete": true}),
            "its own collection",
        ),
        (
            json!({"name": "a", "source": {"collection": "groceries"}, "delete": true,
                   "match": {"name": "$row.name"}}),
            "has no match",
        ),
        (
            json!({"name": "a", "source": {"collection": "groceries"}, "delete": true,
                   "child": {"collection": "aisles", "values": {"name": "x"}}}),
            "has no child",
        ),
        (
            json!({"name": "a", "source": {"collection": "groceries"}, "delete": "yes"}),
            "true or false",
        ),
    ] {
        let v = refused(&pod, "apply_change", set(bad)).await;
        assert!(
            v["detail"].as_str().unwrap_or("").contains(why),
            "{why}: {v}"
        );
    }
    let clear = json!({"name": "clear_bag", "title": "Clear the bag",
        "source": {"collection": "groceries", "where": [{"field": "done", "equals": true}]},
        "delete": true});
    ok(&pod, "apply_change", set(clear.clone())).await;
    // With a param in where too.
    ok(
        &pod,
        "apply_change",
        set(json!({"name": "clear_aisle", "params": [{"name": "aisle", "type": "ref", "collection": "aisles"}],
            "source": {"collection": "groceries", "where": [{"field": "aisle", "equals": "$param.aisle"}]},
            "delete": true})),
    )
    .await;
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "groceries"}),
    )
    .await;
    let described = &d["definition"]["actions"][0];
    assert_eq!(described["delete"], true, "{d}");
    assert_eq!(described["source"], clear["source"], "{d}");
    assert!(described.get("match").is_none(), "{d}");
    assert_eq!(
        described["endpoint"],
        "/v1/collections/groceries/actions/clear_bag"
    );

    let produce = add(&pod, "aisles", json!({"name": "Produce"})).await;
    let dairy = add(&pod, "aisles", json!({"name": "Dairy"})).await;
    for (name, done, aisle) in [
        ("Pears", true, &produce),
        ("Leeks", false, &produce),
        ("Oat milk", true, &dairy),
        ("Feta", false, &dairy),
    ] {
        add(
            &pod,
            "groceries",
            json!({"name": name, "done": done, "aisle": aisle}),
        )
        .await;
    }
    let names = || async {
        rows(&pod, "groceries")
            .await
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let items = |id: String| {
        let pod = pod.clone();
        async move { row(&pod, "aisles", "id", &id).await["items"].clone() }
    };
    assert_eq!(items(produce.clone()).await, json!(2));

    // From a layout: the action endpoint, with a client_id.
    let (status, v) = post(
        &pod,
        "groceries",
        "actions/clear_bag",
        json!({"client_id": "tap-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["rows"].clone(), v["deleted"].clone()),
        (json!(2), json!(2)),
        "{v}"
    );
    assert_eq!(names().await, ["Leeks", "Feta"]);
    // The rollup reading groceries followed.
    assert_eq!(items(produce.clone()).await, json!(1));
    assert_eq!(items(dairy.clone()).await, json!(1));
    // A retry of the same tap is applied once.
    let (status, again) = post(
        &pod,
        "groceries",
        "actions/clear_bag",
        json!({"client_id": "tap-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        (again["repeated"].clone(), again["edit"].clone()),
        (json!(true), v["edit"].clone())
    );
    // Nothing left to clear: no edit.
    let none = ok(
        &pod,
        "run_action",
        json!({"collection": "groceries", "action": "clear_bag"}),
    )
    .await;
    assert_eq!(
        (none["deleted"].clone(), none["edit"].clone()),
        (json!(0), Value::Null),
        "{none}"
    );

    // Undo brings both back, and the rollup with them.
    let u = ok(&pod, "undo_edit", json!({"edit": v["edit"]})).await;
    assert_eq!(u["restored"], 2, "{u}");
    assert_eq!(names().await, ["Pears", "Leeks", "Oat milk", "Feta"]);
    assert_eq!(items(produce.clone()).await, json!(2));

    // A param in where: only that aisle's rows, done or not.
    let r = ok(
        &pod,
        "run_action",
        json!({"collection": "groceries", "action": "clear_aisle", "params": {"aisle": {"name": "Dairy"}}}),
    )
    .await;
    assert_eq!(r["deleted"], 2, "{r}");
    assert_eq!(names().await, ["Pears", "Leeks"]);
    assert_eq!(items(dairy.clone()).await, json!(0));
}

#[tokio::test]
async fn a_delete_action_keeps_to_the_row_cap() {
    let adapter = CollectionsAdapter::with_caps(Caps {
        edit_rows: 2,
        ..Caps::default()
    });
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    ok(
        &pod,
        "define_collection",
        json!({"name": "chores", "fields": [{"name": "title", "type": "text"}],
            "actions": [{"name": "clear_all", "source": {"collection": "chores"}, "delete": true}]}),
    )
    .await;
    for t in ["Sweep", "Water ferns", "Fold towels"] {
        ok(
            &pod,
            "add_row",
            json!({"collection": "chores", "values": {"title": t}}),
        )
        .await;
    }
    let v = refused(
        &pod,
        "run_action",
        json!({"collection": "chores", "action": "clear_all"}),
    )
    .await;
    assert_eq!(v["error"], "too_many_rows", "{v}");
}
