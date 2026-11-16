//! The content-addressed construct (decision `content-addressed-construct`)
//! through the real host and MCP: a pod with several layouts and `pod:`
//! components is converted in a dry run with no differences, switched
//! (installed clients' endpoints answer as before), edited (one component
//! edit re-seals what follows it, and a client holding the old objects is
//! sent only the changed ones), and reverted.

use std::collections::BTreeSet;

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{Env, client, env};

/// A get with this build's features (a client that draws the plumbing).
async fn get(env: &Env, path: &str, bearer: &str) -> (StatusCode, Value) {
    let resp = client()
        .get(format!("{}{path}", env.url))
        .bearer_auth(bearer)
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn post(env: &Env, path: &str, bearer: &str, body: Value) -> (StatusCode, Value) {
    let resp = client()
        .post(format!("{}{path}", env.url))
        .bearer_auth(bearer)
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn tile(label: &str) -> Value {
    json!({"doc": "A tile that opens something.",
        "params": [{"name": "label", "type": "text", "required": true, "doc": "What it says"}],
        "children": true,
        "template": {"type": "core://Box", "background": "${local://theme#card}", "corner_dp": 14,
            "padding_h_dp": 12, "padding_v_dp": 10,
            "content": {"type": "core://Column", "spacing_dp": 4, "children": [
                {"type": "core://Text", "style": "Title", "value_expr": format!("{label}$arg.label")},
                {"type": "core://Children"}]}},
        "examples": [{"args": {"label": "Dinner"}, "note": "A tile"}]})
}

/// A pod shaped like a real one: home links and embeds layouts, layouts use
/// `pod:` and `core:` components, one links back to another.
async fn build(env: &Env, access: &str) -> (String, String, String) {
    let (err, d) = env
        .tool(
            access,
            "define_component",
            json!({"name": "tile", "component": tile("")}),
        )
        .await;
    assert!(!err, "{d}");
    let (err, c) = env
        .tool(access, "create_layout", json!({"title": "Coming up", "feeds": [["recipes", "all"]],
            "layout": {"type": "core://Page", "title": "Coming up", "root": {"type": "core://Column",
                "padding_h_dp": 16, "children": [
                    {"type": "core://Use", "component": "core:section_header", "args": {"text": "Next"}},
                    {"type": "core://List", "query_key": "recipes:all", "item_template":
                        {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}]}}}))
        .await;
    assert!(!err, "{c}");
    let c = c["id"].as_str().unwrap().to_owned();
    let (err, b) = env
        .tool(access, "create_layout", json!({"title": "Recipes",
            "layout": {"type": "core://Workspace", "title": "Recipes", "slots": [
                {"slot_id": "main", "provider_uri": "recipes://all", "content": {"type": "core://Column", "children": [
                    {"type": "core://Use", "component": "pod:tile", "args": {"label": "All recipes"}, "children": [
                        {"type": "core://Text", "style": "Secondary", "value_expr": "From your book"}]},
                    {"type": "core://Layout", "layout": c, "presentation": "Inline"},
                    {"type": "core://List", "query_key": "recipes:all", "item_template":
                        {"type": "core://Use", "component": "core:card", "children": [
                            {"type": "core://Text", "style": "Title", "value_expr": "$item.title"}]}}]}}]}}))
        .await;
    assert!(!err, "{b}");
    let b = b["id"].as_str().unwrap().to_owned();
    let (err, a) = env
        .tool(access, "create_layout", json!({"title": "Meals",
            "layout": {"type": "core://Page", "title": "Meals", "root": {"type": "core://Column", "children": [
                {"type": "core://Use", "component": "core:page_header", "args": {"title": "Meals"}},
                {"type": "core://Tappable", "content_description": "Recipes",
                    "on_click": {"intent": "action.navigation.open", "layout": b, "presentation": "Push"},
                    "content": {"type": "core://Use", "component": "pod:tile", "args": {"label": "Recipes"}}},
                {"type": "core://Button", "label": "Settings",
                    "on_click": {"intent": "action.navigation.open", "layout": "settings", "presentation": "Panel"}}]}}}))
        .await;
    assert!(!err, "{a}");
    let a = a["id"].as_str().unwrap().to_owned();
    let (err, h) = env
        .tool(access, "update_layout", json!({"app": "home",
            "layout": {"type": "core://Page", "title": "Home", "root": {"type": "core://Column", "children": [
                {"type": "core://Use", "component": "core:header_bar", "args": {"title": "Home"}, "children": [
                    {"type": "core://IconButton", "icon": "notifications", "content_description": "Notifications",
                        "on_click": {"intent": "action.navigation.open", "layout": "inbox", "presentation": "Sheet"}}]},
                {"type": "core://Layout", "layout": c},
                {"type": "core://Tappable", "content_description": "Meals",
                    "on_click": {"intent": "action.navigation.open", "layout": a, "presentation": "Push"},
                    "content": {"type": "core://Use", "component": "pod:tile", "args": {"label": "Meals"}}}]}}}))
        .await;
    assert!(!err, "{h}");
    (a, b, c)
}

fn reachable(objects: &serde_json::Map<String, Value>) -> BTreeSet<String> {
    objects.keys().cloned().collect()
}

#[tokio::test]
async fn a_pod_is_switched_after_a_clean_dry_run_and_reverted() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let (a, b, c) = build(&env, &access).await;

    // Before: the old system, and no construct.
    let (s, body) = get(&env, "/v1/construct", &pod_code).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "not_switched");
    let mut before = Vec::new();
    for id in ["home", &a, &b, &c] {
        let (s, body) = get(&env, &format!("/v1/workspaces/{id}"), &pod_code).await;
        assert_eq!(s, StatusCode::OK, "{id}: {body}");
        before.push(body);
    }

    // The dry run changes nothing and finds no difference.
    let (err, dry) = env
        .tool(&access, "switch_construct", json!({"dry_run": true}))
        .await;
    assert!(!err, "{dry}");
    assert_eq!(dry["changed"], false);
    assert_eq!(dry["layouts_compared"], 4, "{dry:#}");
    assert_eq!(dry["layouts_different"], 0, "{dry:#}");
    let (s, _) = get(&env, "/v1/construct", &pod_code).await;
    assert_eq!(s, StatusCode::CONFLICT, "a dry run switches nothing");

    // The switch: installed clients' endpoints draw the same.
    let (err, sw) = env.tool(&access, "switch_construct", json!({})).await;
    assert!(!err, "{sw}");
    assert_eq!(sw["switched"], true);
    let (s, root) = get(&env, "/v1/construct", &pod_code).await;
    assert_eq!(s, StatusCode::OK, "{root}");
    let root = root["root"].as_str().unwrap().to_owned();
    assert!(root.starts_with("e1-"), "{root}");
    for (i, id) in ["home", &a, &b, &c].into_iter().enumerate() {
        let (s, body) = get(&env, &format!("/v1/workspaces/{id}"), &pod_code).await;
        assert_eq!(s, StatusCode::OK, "{id}: {body}");
        assert_eq!(
            body["workspace"], before[i]["workspace"],
            "{id} draws the same"
        );
        assert_eq!(
            body["queries"], before[i]["queries"],
            "{id} has the same data"
        );
    }

    // The objects a device fetches: everything from the root at first.
    let (s, all) = post(
        &env,
        "/v1/objects",
        &pod_code,
        json!({"have": [], "want": [root]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{all}");
    let first = reachable(all["objects"].as_object().unwrap());
    assert!(first.contains(&root));
    assert!(first.len() > 5, "{first:?}");

    // One component edit: home is a new hash, and a device that has every
    // object it had is sent only the changed ones.
    let (err, d) = env
        .tool(
            &access,
            "update_component",
            json!({"name": "tile", "component": tile("› ")}),
        )
        .await;
    assert!(!err, "{d}");
    let (_, now) = get(&env, "/v1/construct", &pod_code).await;
    let now = now["root"].as_str().unwrap().to_owned();
    assert_ne!(now, root, "the construct is a new hash");
    let have: Vec<&String> = first.iter().collect();
    let (_, delta) = post(
        &env,
        "/v1/objects",
        &pod_code,
        json!({"have": have, "want": [now]}),
    )
    .await;
    let changed = reachable(delta["objects"].as_object().unwrap());
    // The tile, the layouts that use it (B, A, home), nothing else: C and
    // every server component and blueprint stay.
    assert_eq!(changed.len(), 4, "{changed:?}");
    let (_, a_now) = get(&env, &format!("/v1/workspaces/{a}"), &pod_code).await;
    assert!(
        a_now["workspace"].to_string().contains("› Recipes"),
        "{a_now}"
    );
    let (_, c_now) = get(&env, &format!("/v1/workspaces/{c}"), &pod_code).await;
    assert_eq!(
        c_now["workspace"], before[3]["workspace"],
        "C doesn't use the tile"
    );

    // Writes of the old system are refused; MCP writes the construct.
    let (err, h) = env
        .tool(&access, "set_hidden", json!({"app": a, "hidden": true}))
        .await;
    assert!(err, "{h}");
    assert_eq!(h["error"], "construct_switched");

    // A cycle is refused.
    let (err, cyc) = env
        .tool(&access, "update_layout", json!({"app": c, "layout": {"type": "core://Page", "title": "C",
            "root": {"type": "core://Button", "label": "Meals",
                "on_click": {"intent": "action.navigation.open", "layout": a, "presentation": "Push"}}}}))
        .await;
    assert!(err, "{cyc}");
    assert_eq!(cyc["error"], "cycle", "{cyc}");

    // Revert: the old system answers as before the switch.
    let (err, rv) = env
        .tool(&access, "switch_construct", json!({"revert": true}))
        .await;
    assert!(!err, "{rv}");
    let (s, _) = get(&env, "/v1/construct", &pod_code).await;
    assert_eq!(s, StatusCode::CONFLICT);
    for (i, id) in ["home", &a, &b, &c].into_iter().enumerate() {
        let (s, body) = get(&env, &format!("/v1/workspaces/{id}"), &pod_code).await;
        assert_eq!(s, StatusCode::OK, "{id}: {body}");
        assert_eq!(
            body["workspace"], before[i]["workspace"],
            "{id} is as it was"
        );
    }
}
