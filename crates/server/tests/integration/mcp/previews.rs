//! Previews read like a device with the user's data (decision
//! `previews-read-like-a-device`).

use super::{env, preview_call, preview_details};
use serde_json::{Value, json};

/// The node of a preview answer whose text contains `text`.
fn preview_node(details: &Value, text: &str) -> Option<Value> {
    details["nodes"]
        .as_array()?
        .iter()
        .find(|n| n["text"].as_str().is_some_and(|t| t.contains(text)))
        .cloned()
}

/// A screen opened directly loads its data path, home gets the device's
/// list of layouts, a row can be tried with a made-up item and made-up
/// lists, a long screen is drawn whole, and nothing a source doesn't
/// declare is read.
#[tokio::test]
async fn previews_read_like_a_device_with_the_users_data() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let (err, created) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "Recipes", "feeds": [["recipes", "all"]],
                   "layout": {"type": "core://Ref", "app": "recipes", "part": "layout"}}),
        )
        .await;
    assert!(!err, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();

    // 1. A screen opened by its URI, with its data path loaded.
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": id, "target_uri": "recipes://recipes/test-garden-salad", "full_height": true}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(text.contains("opened screen recipe"), "{text}");
    assert!(!text.contains("not sent"), "{text}");
    let details = preview_details(&text);
    assert!(
        preview_node(&details, "romaine lettuce").is_some(),
        "the ingredients: {details}"
    );
    // Taps are summed up in a few words.
    for n in details["nodes"].as_array().unwrap() {
        if !n["tap"].is_null() {
            assert!(n["tap"].as_str().is_some_and(|t| t.len() < 300), "{n}");
        }
    }
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": id, "target_uri": "nope://x"}),
    )
    .await;
    assert!(err && text.contains("no_screen"), "{text}");
    assert!(text.contains("recipes://recipes/{id}"), "{text}");

    // 2. Home binds the device's list of layouts.
    let home = json!({"type": "core://Workspace", "title": "Home", "slots": [{"slot_id": "l",
        "content": {"type": "core://List", "query_key": "local:layouts",
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "Layout $item.title"}}}]});
    let (err, w) = env
        .tool(
            &access,
            "update_layout",
            json!({"id": "home", "layout": home}),
        )
        .await;
    assert!(!err, "{w}");
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": "home"}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(
        preview_node(&preview_details(&text), "Layout Recipes").is_some(),
        "{text}"
    );

    // 3. A row tried with a made-up item, and a list of made-up rows.
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"layout": {"type": "core://Text", "style": "Body", "value_expr": "Row: $item.title"},
               "item": {"title": "Made-up salad"}}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(
        preview_node(&preview_details(&text), "Row: Made-up salad").is_some(),
        "{text}"
    );
    let mut entities = serde_json::Map::new();
    let mut uris = Vec::new();
    for i in 0..60 {
        let uri = format!("test://rows/{i}");
        entities.insert(uri.clone(), json!({"title": format!("Made-up row {i}")}));
        uris.push(uri);
    }
    let long = json!({"type": "core://Page", "id": "d", "root": {"type": "core://Scroll", "content": {"type": "core://List", "query_key": "test:rows",
        "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}}});
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"layout": long, "entities": entities, "queries": {"test:rows": uris},
               "full_height": true, "nodes": "full"}),
    )
    .await;
    assert!(!err, "{text}");
    let details = preview_details(&text);
    let last = preview_node(&details, "Made-up row 59")
        .unwrap_or_else(|| panic!("the whole list: {text}"));
    let height = details["image"]["height"].as_f64().unwrap();
    assert!(height > 844.0 * 2.0, "drawn taller than a phone: {height}");
    let last_id = last["id"].as_str().unwrap().to_owned();

    // 4. Scrolled to a node, and the node list left out.
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"layout": long, "entities": entities, "queries": {"test:rows": uris},
               "scroll_to": last_id}),
    )
    .await;
    assert!(!err, "{text}");
    let details = preview_details(&text);
    let last = preview_node(&details, "Made-up row 59").expect("scrolled into view");
    let y = last["bounds"][1].as_f64().unwrap();
    assert!((0.0..844.0).contains(&y), "{last}");
    let preview_id = details["preview_id"].as_str().unwrap().to_owned();
    let (err, _, text) = preview_call(
        &env,
        &access,
        "dispatch_virtual_interaction",
        json!({"preview_id": preview_id, "action": "scroll", "dy": -100, "nodes": "none"}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(preview_details(&text).get("nodes").is_none(), "{text}");

    // 5. A load from a path no data source declares reads nothing.
    let load = json!({"type": "core://Page", "id": "d", "root": {"type": "core://Button", "label": "Load",
        "on_click": {"intent": "action.data.load", "path": "/v1/apps?all=1"}}});
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"layout": load}),
    )
    .await;
    assert!(!err, "{text}");
    let preview_id = preview_details(&text)["preview_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (err, _, text) = preview_call(
        &env,
        &access,
        "dispatch_virtual_interaction",
        json!({"preview_id": preview_id, "action": "tap", "text": "Load"}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(text.contains("not a data path"), "{text}");
}
