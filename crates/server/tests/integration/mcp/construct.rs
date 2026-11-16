//! The content-addressed construct (decision `content-addressed-construct`)
//! through the real host and MCP: a pod with several layouts and `@me/`
//! components is drawn by name, edited (one component edit re-seals what
//! follows it, and a client holding the old objects is sent only the
//! changed ones), and refuses cycles; and bindings between layouts.

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
        .tool(access, "create_layout", json!({"title": "Coming up", "feeds": [["collections", "dishes"]],
            "layout": {"type": "core://Page", "title": "Coming up", "root": {"type": "core://Column",
                "padding_h_dp": 16, "children": [
                    {"type": "core://Use", "component": "core:section_header", "args": {"text": "Next"}},
                    {"type": "core://List", "query_key": "collection:dishes", "item_template":
                        {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}]}}}))
        .await;
    assert!(!err, "{c}");
    let c = c["id"].as_str().unwrap().to_owned();
    let (err, b) = env
        .tool(access, "create_layout", json!({"title": "Recipes",
            "layout": {"type": "core://Workspace", "title": "Recipes", "slots": [
                {"slot_id": "main", "provider_uri": "core://custom", "content": {"type": "core://Column", "children": [
                    {"type": "core://Use", "component": "pod:tile", "args": {"label": "All recipes"}, "children": [
                        {"type": "core://Text", "style": "Secondary", "value_expr": "From your book"}]},
                    {"type": "core://Layout", "layout": c, "presentation": "Inline"},
                    {"type": "core://List", "query_key": "collection:dishes", "item_template":
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
async fn a_pods_construct_is_edited_and_resealed() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let (a, b, c) = build(&env, &access).await;

    // Every pod is on the construct (issue #300): its root, and each
    // layout by name.
    let (s, root) = get(&env, "/v1/construct", &pod_code).await;
    assert_eq!(s, StatusCode::OK, "{root}");
    let root = root["root"].as_str().unwrap().to_owned();
    assert!(root.starts_with("e1-"), "{root}");
    let mut before = Vec::new();
    for id in ["home", &a, &b, &c] {
        before.push(env.drawn(id, &pod_code).await);
    }
    assert!(before[1]["workspace"].to_string().contains("Recipes"));
    assert_eq!(
        before[3]["queries"]["collection:dishes"]
            .as_array()
            .map(Vec::len),
        Some(8),
        "{}",
        before[3]
    );

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
    let a_now = env.drawn(&a, &pod_code).await;
    assert!(
        a_now["workspace"].to_string().contains("› Recipes"),
        "{a_now}"
    );
    let c_now = env.drawn(&c, &pod_code).await;
    assert_eq!(
        c_now["workspace"], before[3]["workspace"],
        "C doesn't use the tile"
    );

    // The old system's tools are gone.
    for gone in [
        "set_hidden",
        "switch_construct",
        "rebase_context",
        "dismiss_update",
    ] {
        let (err, h) = env.tool(&access, gone, json!({"app": a})).await;
        assert!(err, "{h}");
        assert_eq!(h["error"], "unknown_tool", "{gone}");
    }

    // A cycle is refused.
    let (err, cyc) = env
        .tool(&access, "update_layout", json!({"app": c, "layout": {"type": "core://Page", "title": "C",
            "root": {"type": "core://Button", "label": "Meals",
                "on_click": {"intent": "action.navigation.open", "layout": a, "presentation": "Push"}}}}))
        .await;
    assert!(err, "{cyc}");
    assert_eq!(cyc["error"], "cycle", "{cyc}");
}

/// Bindings between layouts (issue #301): one layout reads its list from an
/// input, home embeds it twice bound to two collections, and each embed
/// draws its own; an unbound use and a binding outside home are refused,
/// and a layout home doesn't reach is found by name.
#[tokio::test]
async fn a_layout_embedded_with_bind_draws_what_its_use_site_binds() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let ok = |name: &'static str, args: Value| {
        let (env, access) = (&env, &access);
        async move {
            let (err, v) = env.tool(access, name, args).await;
            assert!(!err, "{name}: {v}");
            v
        }
    };
    for (c, title) in [("books", "The Glass Orchard"), ("films", "Harbor Lights")] {
        ok(
            "define_collection",
            json!({"name": c, "fields": [{"name": "text", "type": "text"}]}),
        )
        .await;
        ok(
            "add_row",
            json!({"collection": c, "values": {"text": title}}),
        )
        .await;
    }
    // A shelf lists whatever its use site binds to `items`.
    let shelf = ok("create_layout", json!({"title": "Shelf",
        "feeds": [["collections", "books"], ["collections", "films"]],
        "layout": {"type": "core://Page", "title": "Shelf", "root": {"type": "core://List",
            "query_key": "$in.items",
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.text"}}}}))
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let home = |books: Value| {
        json!({"app": "home", "layout": {"type": "core://Page", "title": "Home", "root": {
            "type": "core://Column", "children": [
                {"type": "core://Layout", "ref": {"name": shelf, "as": "shelf-books"}, "bind": books},
                {"type": "core://Layout", "ref": {"name": shelf, "as": "shelf-films"},
                    "bind": {"items": "collection:films"}}]}}})
    };

    // Unbound: refused, naming the input.
    let (err, e) = env.tool(&access, "update_layout", home(json!({}))).await;
    assert!(err, "{e}");
    assert_eq!(e["error"], "unbound_input", "{e}");
    assert!(e["detail"].as_str().unwrap().contains("items"), "{e}");

    ok("update_layout", home(json!({"items": "collection:books"}))).await;

    // Another layout binds only by passing its own inputs on.
    let (err, e) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "Nested",
            "layout": {"type": "core://Page", "title": "Nested", "root": {"type": "core://Layout",
                "ref": shelf, "bind": {"items": "collection:books"}}}}),
        )
        .await;
    assert!(err, "{e}");
    assert_eq!(e["error"], "bind_outside_home", "{e}");
    let nested = ok(
        "create_layout",
        json!({"title": "Nested",
        "layout": {"type": "core://Page", "title": "Nested", "root": {"type": "core://Layout",
            "ref": shelf, "bind": {"items": "$in.items"}}}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Each embed's data is what its binding reads, and nothing else.
    let (s, root) = get(&env, "/v1/construct", &pod_code).await;
    assert_eq!(s, StatusCode::OK, "{root}");
    let root = root["root"].as_str().unwrap().to_owned();
    let (_, all) = post(
        &env,
        "/v1/objects",
        &pod_code,
        json!({"have": [], "want": [root]}),
    )
    .await;
    let objects = all["objects"].as_object().unwrap();
    let edges: Vec<Value> = objects[&root]["node"]["root"]["children"]
        .as_array()
        .unwrap()
        .clone();
    let el = edges[0]["ref"]["el"].as_str().unwrap().to_owned();
    assert_eq!(
        edges[1]["ref"]["el"].as_str(),
        Some(el.as_str()),
        "one element, used twice"
    );
    let data = |items: &str| {
        let bind = json!({ "items": items }).to_string();
        let path = format!(
            "/v1/construct/data/{el}?bind={}",
            bind.bytes()
                .map(|b| match b {
                    b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' =>
                        (b as char).to_string(),
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        );
        let env = &env;
        let pod_code = &pod_code;
        async move { get(env, &path, pod_code).await }
    };
    let (s, books) = data("collection:books").await;
    assert_eq!(s, StatusCode::OK, "{books}");
    assert!(books.to_string().contains("The Glass Orchard"), "{books}");
    assert!(!books.to_string().contains("Harbor Lights"), "{books}");
    assert_eq!(
        books["queries"]["collection:books"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{books}"
    );
    let (_, films) = data("collection:films").await;
    assert!(films.to_string().contains("Harbor Lights"), "{films}");
    assert!(!films.to_string().contains("The Glass Orchard"), "{films}");
    // A binding reaches only home's feeds.
    let (_, other) = data("collection:secret").await;
    assert_eq!(
        other["queries"].as_object().map(|q| q.len()),
        Some(0),
        "{other}"
    );

    // A layout home doesn't reach (a notification's) is found by name.
    let (s, by_name) = get(&env, &format!("/v1/construct/names/{nested}"), &pod_code).await;
    assert_eq!(s, StatusCode::OK, "{by_name}");
    assert!(
        by_name["el"].as_str().unwrap().starts_with("e1-"),
        "{by_name}"
    );
    let (s, _) = get(&env, "/v1/construct/names/c-nothing", &pod_code).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // A new input the use sites don't bind is refused at the layout's write.
    let (err, e) = env
        .tool(&access, "update_layout", json!({"app": shelf, "layout": {"type": "core://Page",
            "title": "$in.heading", "root": {"type": "core://List", "query_key": "$in.items",
                "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.text"}}}}))
        .await;
    assert!(err, "{e}");
    assert_eq!(e["error"], "unbound_input", "{e}");
}

/// Issue #310: Collections' `apply_change` edits the
/// construct's layouts in the same step as the collection, and a change is
/// checked against the construct's layouts as clients draw them (a bound
/// one too), never the old tables.
#[tokio::test]
async fn a_change_set_edits_and_checks_the_constructs_layouts() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let ok = |name: &'static str, args: Value| {
        let (env, access) = (&env, &access);
        async move {
            let (err, v) = env.tool(access, name, args).await;
            assert!(!err, "{name}: {v}");
            v
        }
    };
    let refused = |name: &'static str, args: Value| {
        let (env, access) = (&env, &access);
        async move {
            let (err, v) = env.tool(access, name, args).await;
            assert!(err, "{name}: {v}");
            v
        }
    };
    ok(
        "define_collection",
        json!({"name": "books", "fields": [{"name": "title", "type": "text"},
            {"name": "author", "type": "text"}]}),
    )
    .await;
    ok(
        "add_row",
        json!({"collection": "books", "values": {"title": "The Glass Orchard", "author": "Ida Vell"}}),
    )
    .await;
    let page = |query: &str, row: &str| {
        json!({"type": "core://Page", "title": "Books", "root": {"type": "core://List",
            "query_key": query,
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": row}}})
    };
    let id = |v: Value| v["id"].as_str().unwrap().to_owned();
    // One layout reads the collection itself; another reads what home binds.
    let list = id(ok(
        "create_layout",
        json!({"title": "Books", "feeds": [["collections", "books"]],
            "layout": page("collection:books", "$item.title")}),
    )
    .await);
    let shelf = id(ok(
        "create_layout",
        json!({"title": "Shelf", "layout": page("$in.items", "$item.title by $item.author")}),
    )
    .await);
    ok(
        "update_layout",
        json!({"app": "home", "layout": {"type": "core://Page", "title": "Home",
        "root": {"type": "core://Column", "children": [
            {"type": "core://Layout", "ref": list},
            {"type": "core://Layout", "ref": shelf, "bind": {"items": "collection:books"}}]}}}),
    )
    .await;
    let version = |l: &str| {
        let (env, access, l) = (&env, &access, l.to_owned());
        async move {
            let (err, v) = env.tool(access, "get_layout", json!({"app": l})).await;
            assert!(!err, "{v}");
            v["version"].as_i64().unwrap()
        }
    };
    let (list_v, shelf_v) = (version(&list).await, version(&shelf).await);

    // Removing a field the bound shelf shows is refused, naming it.
    let e = refused(
        "apply_change",
        json!({"collection": "books", "changes": [{"op": "remove_field", "field": "author"}]}),
    )
    .await;
    assert_eq!(e["error"], "breaks_layout", "{e}");
    assert_eq!(e["layout"], shelf, "{e}");
    assert_eq!(e["in_change_set"], false, "{e}");
    assert!(
        e["detail"].as_str().unwrap().contains("$item.author"),
        "{e}"
    );
    // The collection reports the construct's layouts as its users.
    let d = ok("describe_collection", json!({"collection": "books"})).await;
    assert_eq!(d["latest_version"], 1, "{d}");
    let users: Vec<&str> = d["used_by"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["layout"].as_str())
        .collect();
    assert!(
        users.contains(&list.as_str()) && users.contains(&shelf.as_str()),
        "{d}"
    );

    // With the shelf changed in the same step, it applies: the construct's
    // layout moves (its history seq + 1), the collection with it.
    let r = ok(
        "apply_change",
        json!({"collection": "books", "note": "No authors",
        "changes": [{"op": "remove_field", "field": "author"}],
        "apps": [{"app": shelf, "base_version": shelf_v, "json_patch": [{"op": "replace",
            "path": "/root/item_template/value_expr", "value": "$item.title"}]}]}),
    )
    .await;
    assert_eq!(r["version"], 2, "{r}");
    assert_eq!(r["apps"][0]["app"], shelf, "{r}");
    assert_eq!(r["apps"][0]["version"], shelf_v + 1, "{r}");
    let (_, got) = env.tool(&access, "get_layout", json!({"app": shelf})).await;
    assert_eq!(got["version"], shelf_v + 1, "{got}");
    assert_eq!(
        got["layout"]["root"]["item_template"]["value_expr"], "$item.title",
        "{got}"
    );
    assert_eq!(version(&list).await, list_v, "untouched");

    // A base that isn't the name's seq refuses the whole step.
    let e = refused(
        "apply_change",
        json!({"collection": "books",
        "changes": [{"op": "add_field", "name": "read", "type": "bool", "default": false}],
        "apps": [{"app": list, "base_version": list_v + 5, "json_patch": [{"op": "replace",
            "path": "/title", "value": "My books"}]}]}),
    )
    .await;
    assert_eq!(e["error"], "stale_base", "{e}");
    assert_eq!(e["latest_version"], list_v, "{e}");
    let d = ok("describe_collection", json!({"collection": "books"})).await;
    assert_eq!(d["latest_version"], 2, "nothing applied: {d}");
    // An edit the construct refuses refuses the change too.
    let e = refused(
        "apply_change",
        json!({"collection": "books",
        "changes": [{"op": "add_field", "name": "read", "type": "bool", "default": false}],
        "apps": [{"app": "c-nothingabc", "layout": page("collection:books", "$item.title")}]}),
    )
    .await;
    assert_eq!(e["error"], "layout_not_found", "{e}");
    let d = ok("describe_collection", json!({"collection": "books"})).await;
    assert_eq!(d["latest_version"], 2, "nothing applied: {d}");
}
