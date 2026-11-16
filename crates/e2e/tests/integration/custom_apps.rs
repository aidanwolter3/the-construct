//! Custom apps end to end (decision `custom-apps-and-data-sources`), against the fake:
//! create a "Kitchen" app over the Pixelfed home feed and the made-up test dishes with the pod token,
//! then through the client engine: open it, see posts and dishes together, like a post
//! through the outbox (it reaches the upstream), load a dish screen, and watch a second
//! engine pick up a new version live. Deleting it ends with a 404.

use std::sync::Arc;

use client_core::MemoryStore;
use e2e::{
    Harness, TIMEOUT, entities, fresh_post, like_intent, wait_flushed, wait_signed_in, wait_until,
    wait_upstream,
};
use serde_json::{Value, json};

/// The pod token of the test (its pinned session is created in the pod directly).
const POD_TOKEN: &str = "e2e-pod-token";

fn kitchen(title: &str) -> Value {
    json!({
        "type": "core://Workspace", "id": "x", "title": title,
        "screens": [{"id": "dish", "route": "collections://dishes/{id}", "title": "Dish",
            "data": "/v1/collections/dishes/row?id=$route.id",
            "content": {"type": "core://List", "query_key": "collection:dishes:row:$route.id",
                "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.category"}}}],
        "slots": [
            {"slot_id": "posts", "provider_uri": "core://custom",
             "content": {"type": "core://Column", "children": [
                {"type": "core://Text", "style": "Caption",
                 "value_expr": "Pixelfed signed in: ${core://sources/pixelfed#signed_in}"},
                {"type": "core://List", "query_key": "timeline:home",
                 "item_template": {"type": "core://Button", "label": "Like", "on_click": {
                    "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": "is_liked", "value_expr": "!entity.is_liked", "rollback_value": false,
                    "endpoint": "/v1/pixelfed/posts/like",
                    "payload": {"id": "$item.id", "liked": "$value"}}}}]}},
            {"slot_id": "dishes", "provider_uri": "core://custom",
             "content": {"type": "core://List", "query_key": "collection:dishes",
                "item_template": {"type": "core://Button", "label": "$item.title", "on_click": {
                    "intent": "action.navigation.open", "target_uri": "collections://dishes/$item.id",
                    "presentation": "Push"}}}}
        ]
    })
}

#[test]
fn a_custom_app_shows_two_sources_and_its_actions_work() {
    let h = Harness::from_env();
    let up = &h.upstream;
    if up.is_real() {
        eprintln!("[e2e] custom apps: skipped (the fake only)");
        return;
    }
    up.reset();
    let pod_store = h.pod_state.store();
    pod_store
        .create_pinned_session(&server::session::session_hash(POD_TOKEN))
        .unwrap();
    // A made-up collection of dishes to list next to the posts.
    pod_store
        .with_conn(source_collections::test_seed::seed_test_dishes)
        .unwrap();
    let http = reqwest::blocking::Client::new();
    let pod = |method: reqwest::Method, path: &str, body: Option<Value>| -> (u16, Value) {
        let mut req = http
            .request(method, format!("{}{path}", h.pod_url))
            .bearer_auth(POD_TOKEN);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().unwrap_or(Value::Null))
    };

    // 1. Sign in to Pixelfed through the engine (the session lives in `store`).
    let store = Arc::new(MemoryStore::new());
    let home = h.engine(&store);
    wait_until("the signed-out bundle", TIMEOUT, || {
        home.snapshot()
            .workspace_json
            .contains("Sign in with Pixelfed")
    });
    home.dispatch(&h.login_intent());
    wait_signed_in(&home);
    drop(home);

    // 2. Create the app with the pod token.
    let (status, created) = pod(
        reqwest::Method::POST,
        "/v1/custom-apps",
        Some(json!({"title": "Kitchen", "icon": "recipe",
            "feeds": [["collections", "dishes"], ["pixelfed", "home"]],
            "layout": kitchen("Kitchen"), "note": "Dishes next to my feed"})),
    );
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    let (_, apps) = pod(reqwest::Method::GET, "/v1/apps", None);
    assert!(
        apps["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == id.as_str() && a["kind"] == "custom"),
        "{apps}"
    );

    // 3. An engine on it: posts and dishes together.
    let kitchen_engine = h.engine_on(&store, None, &id);
    wait_until("posts and dishes", TIMEOUT, || {
        let ents = entities(&kitchen_engine.snapshot());
        ents.as_object().is_some_and(|m| {
            m.keys().any(|k| k.starts_with("pixelfed://posts/"))
                && m.keys().any(|k| k.starts_with("collections://dishes/"))
        })
    });
    let snap = kitchen_engine.snapshot();
    assert!(snap.workspace_json.contains("Kitchen"));
    assert_eq!(
        entities(&snap)["core://sources/pixelfed"]["signed_in"],
        true
    );

    // 4. Like a post through the outbox: it reaches the upstream.
    let (post_id, _) = fresh_post(&snap);
    kitchen_engine.dispatch(&like_intent(&post_id));
    wait_flushed(&kitchen_engine, TIMEOUT);
    wait_upstream(up, &post_id, (true, false));

    // 5. A dish screen's data loads.
    kitchen_engine.dispatch(
        &json!({"intent": "action.data.load", "path": "/v1/collections/dishes/row?id=placeholder-pot-pie"})
            .to_string(),
    );
    wait_until("the dish's row", TIMEOUT, || {
        kitchen_engine
            .snapshot()
            .queries_json
            .contains("collection:dishes:row:placeholder-pot-pie")
    });

    // 6. A second engine on the app gets a new version live.
    let other = h.engine_on(&store, None, &id);
    wait_until("the second engine's first fetch", TIMEOUT, || {
        other.snapshot().workspace_json.contains("Kitchen")
    });
    let (status, b) = pod(
        reqwest::Method::PUT,
        &format!("/v1/custom-apps/{id}/layout"),
        Some(json!({"layout": kitchen("Kitchen v2"), "note": "Rename", "base_version": 1})),
    );
    assert_eq!(status, 200, "{b}");
    wait_until("the pushed version", TIMEOUT, || {
        other.snapshot().workspace_json.contains("Kitchen v2")
    });

    // 7. Delete: gone for everyone.
    let (status, _) = pod(
        reqwest::Method::DELETE,
        &format!("/v1/custom-apps/{id}"),
        None,
    );
    assert_eq!(status, 200);
    let (status, _) = pod(reqwest::Method::GET, &format!("/v1/workspaces/{id}"), None);
    assert_eq!(status, 404);
    // Undo the like on the fake for the other tests.
    kitchen_engine.dispatch(&like_intent(&post_id));
    wait_flushed(&kitchen_engine, TIMEOUT);
}
