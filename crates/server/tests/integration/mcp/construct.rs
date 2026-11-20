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

/// Re-seals in a row are one entry of a name's history, a write says what
/// it makes a device fetch, and the listing says how big each layout is
/// (decision `element-roles-and-reuse`).
#[tokio::test]
async fn reseals_are_one_history_entry_and_a_write_says_what_it_costs() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let (a, _, _) = build(&env, &access).await;

    let (_, before) = env.tool(&access, "get_layout", json!({"id": "home"})).await;
    let v = before["version"].as_i64().unwrap();

    // Three edits of a component home uses: home is re-sealed three times.
    let mut last = Value::Null;
    for mark in ["a ", "b ", "c "] {
        let (err, d) = env
            .tool(
                &access,
                "update_component",
                json!({"name": "tile", "component": tile(mark)}),
            )
            .await;
        assert!(!err, "{d}");
        last = d;
    }
    // The tile, and B, A and home above it.
    assert_eq!(last["refetch"]["objects"], 4, "{last}");
    assert!(last["refetch"]["bytes"].as_i64().unwrap() > 500, "{last}");

    let (_, home) = env.tool(&access, "get_layout", json!({"id": "home"})).await;
    assert_eq!(home["version"], v + 1, "one entry for the three re-seals");
    let resealed: Vec<&Value> = home["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| {
            h["note"]
                .as_str()
                .is_some_and(|n| n.starts_with("Re-sealed"))
        })
        .collect();
    assert_eq!(resealed.len(), 1, "{home}");
    assert!(home["layout"].to_string().contains("tile"), "{home}");
    let drawn = env.drawn(&a, &pod_code).await;
    assert!(
        drawn["workspace"].to_string().contains("c Recipes"),
        "{drawn}"
    );

    // An edit of home itself is a new entry, and the next re-seal one more.
    let (err, d) = env
        .tool(
            &access,
            "update_layout",
            json!({"app": "home", "base_version": v + 1, "note": "Same again",
                "json_patch": [{"op": "add", "path": "/root/spacing_dp", "value": 4}]}),
        )
        .await;
    assert!(!err, "{d}");
    assert_eq!(d["version"], v + 2, "{d}");
    assert_eq!(d["refetch"]["objects"], 1, "{d}");
    let (err, d) = env
        .tool(
            &access,
            "update_component",
            json!({"name": "tile", "component": tile("d ")}),
        )
        .await;
    assert!(!err, "{d}");
    let (_, home) = env.tool(&access, "get_layout", json!({"id": "home"})).await;
    assert_eq!(home["version"], v + 3, "{home}");

    let (_, list) = env.tool(&access, "list_layouts", json!({})).await;
    for l in list["layouts"].as_array().unwrap() {
        assert!(l["bytes"].as_i64().unwrap() > 100, "{l}");
    }
}

/// Template directives and named slots (feature `component_templates`,
/// decision `element-roles-and-reuse`): a component repeats over a list
/// argument, a use fills named slots, and a client without the feature is
/// sealed the newest entry of the name it can read.
#[tokio::test]
async fn a_component_repeats_and_takes_slots_and_old_clients_get_the_entry_they_read() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    // First a component any client reads, then the same name with an Each.
    let plain = json!({"doc": "Steps of a recipe.",
        "params": [{"name": "steps", "type": "json", "required": true, "doc": "The steps."}],
        "template": {"type": "core://Text", "style": "Body", "value_expr": "Steps"},
        "examples": [{"args": {"steps": ["Chop", "Fry"]}, "note": "two steps"}]});
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "steps", "component": plain}),
        )
        .await;
    assert!(!err, "{d}");
    let (_, v1) = env
        .tool(&access, "get_component", json!({"name": "@me/steps"}))
        .await;
    let v1 = v1["element"].as_str().unwrap().to_owned();
    let mut each = plain.clone();
    each["template"] = json!({"type": "core://Column", "children": [
        {"type": "core://Each", "arg": "steps", "as": "step", "template":
            {"type": "core://Text", "style": "Body", "value_expr": "$arg.step_index. $arg.step"}}]});
    let (err, d) = env
        .tool(
            &access,
            "update_component",
            json!({"name": "steps", "component": each}),
        )
        .await;
    assert!(!err, "{d}");
    let (_, v2) = env
        .tool(&access, "get_component", json!({"name": "@me/steps"}))
        .await;
    let v2 = v2["element"].as_str().unwrap().to_owned();
    assert_ne!(v1, v2);

    // A page on the core scaffold, its footer and header action in slots.
    let (err, made) = env
        .tool(&access, "create_layout", json!({"title": "Pancakes",
            "layout": {"type": "core://Page", "title": "Pancakes", "root":
                {"type": "core://Use", "ref": "@core/screen_scaffold", "args": {"title": "Pancakes"},
                    "children": [{"type": "core://Use", "ref": "@me/steps", "args": {"steps": ["Whisk", "Rest", "Fry"]}}],
                    "slots": {
                        "actions": [{"type": "core://IconButton", "icon": "add", "content_description": "Add a step",
                            "on_click": {"intent": "action.navigation.back"}}],
                        "footer": [{"type": "core://Text", "style": "Caption", "value_expr": "Serves four"}]}}}}))
        .await;
    assert!(!err, "{made}");
    let id = made["id"].as_str().unwrap().to_owned();
    let drawn = env.drawn(&id, &pod_code).await["workspace"].to_string();
    for want in [
        "0. Whisk",
        "2. Fry",
        "Add a step",
        "Serves four",
        "Pancakes",
    ] {
        assert!(drawn.contains(want), "{want}: {drawn}");
    }
    for gone in ["core://Each", "core://Children", "core://Use", "\"slots\""] {
        assert!(!drawn.contains(gone), "{gone}: {drawn}");
    }

    // What each kind of client is sent for the layout: this build the
    // component with the Each, a build without the feature the entry before.
    let objects_for = |features: String| {
        let (env, id, pod_code) = (&env, id.clone(), pod_code.clone());
        async move {
            let c = client();
            let el: Value = c
                .get(format!("{}/v1/construct/names/{id}", env.url))
                .bearer_auth(&pod_code)
                .header(protocol::FEATURES_HEADER, &features)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let got: Value = c
                .post(format!("{}/v1/objects", env.url))
                .bearer_auth(&pod_code)
                .header(protocol::FEATURES_HEADER, &features)
                .json(&json!({"have": [], "want": [el["el"]]}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            reachable(got["objects"].as_object().unwrap())
        }
    };
    let now = objects_for(protocol::features_header()).await;
    assert!(now.contains(&v2) && !now.contains(&v1), "{now:?}");
    let without: Vec<&str> = protocol::FEATURES
        .iter()
        .copied()
        .filter(|f| *f != protocol::feature::COMPONENT_TEMPLATES)
        .collect();
    let old = objects_for(without.join(",")).await;
    assert!(old.contains(&v1) && !old.contains(&v2), "{old:?}");
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

/// Decision `trace-from-home`: list_layouts and get_layout say how the user
/// reaches each layout from home, and flag a layout copied from a
/// blueprint, which doesn't follow it (a home whose swipe down opens its
/// own copy of the inbox never shows a change to `@core/inbox`).
#[tokio::test]
async fn layouts_say_how_home_reaches_them_and_which_are_copies() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let (err, copy) = env
        .tool(&access, "create_layout", json!({"title": "Notifications",
            "layout": {"type": "core://Page", "title": "Notifications", "root": {"type": "core://Column",
                "children": [{"type": "core://Text", "style": "Body", "value_expr": "My inbox"}]}}}))
        .await;
    assert!(!err, "{copy}");
    let copy = copy["id"].as_str().unwrap().to_owned();
    let (err, lone) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "Drafts",
            "layout": {"type": "core://Page", "title": "Drafts", "root": {"type": "core://Text",
                "style": "Body", "value_expr": "Nothing yet"}}}),
        )
        .await;
    assert!(!err, "{lone}");
    let lone = lone["id"].as_str().unwrap().to_owned();
    let (err, h) = env
        .tool(&access, "update_layout", json!({"id": "home", "layout": {"type": "core://Page",
            "title": "Home", "root": {"type": "core://Gesture",
                "on_swipe_down": {"intent": "action.navigation.open", "ref": copy,
                    "presentation": "Overlay", "edge": "top"},
                "content": {"type": "core://Column", "children": [
                    {"type": "core://Tappable", "content_description": "Settings",
                     "on_click": {"intent": "action.navigation.open", "ref": "@core/settings",
                        "presentation": "Panel"},
                     "content": {"type": "core://Text", "style": "Body", "value_expr": "Settings"}}]}}}}))
        .await;
    assert!(!err, "{h}");

    let (err, all) = env.tool(&access, "list_layouts", json!({})).await;
    assert!(!err, "{all}");
    let row = |id: &str| {
        all["layouts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(row("home")["reached"], "home");
    assert_eq!(
        row(&copy)["reached"],
        format!("home → (on_swipe_down, Overlay from top) → {copy}")
    );
    assert_eq!(row(&copy)["copy_of"], "@core/inbox");
    assert!(
        row(&lone)["reached"]
            .as_str()
            .unwrap()
            .starts_with("not reached from home"),
        "{}",
        row(&lone)
    );
    assert!(row(&lone).get("copy_of").is_none());
    let inbox = all["blueprints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "inbox")
        .unwrap()
        .clone();
    assert_eq!(inbox["copies"], json!([copy]));
    assert!(inbox.get("reached").is_none(), "{inbox}");
    let settings = all["blueprints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "settings")
        .unwrap()
        .clone();
    assert_eq!(
        settings["reached"],
        "home → (on_click, Panel, \"Settings\") → @core/settings"
    );

    let (err, one) = env.tool(&access, "get_layout", json!({"id": copy})).await;
    assert!(!err, "{one}");
    assert_eq!(one["copy_of"], "@core/inbox");
    assert!(one["reached"].as_str().unwrap().contains("on_swipe_down"));
}

/// A client is served a component's current element, as `expand` shows
/// it: `@me/icon_row`, first a wrapper over `@core/icon_row` and then the
/// pod's own template with a `core://IfArg`, draws the pod's template. The
/// `core://IfArg` once read as a removed node type in the element's
/// recorded features, so sealing for a client passed over it for the
/// wrapper, and the client drew `@core/icon_row`.
#[tokio::test]
async fn a_pods_own_component_is_served_as_its_own() {
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
    ok(
        "define_component",
        json!({"name": "icon_row", "component": {
        "doc": "The server's row, for now.",
        "params": [{"name": "title", "type": "text", "required": true, "doc": "The title"}],
        "template": {"type": "core://Use", "ref": "@core/icon_row",
            "args": {"icon": "bookmark", "title": "$arg.title"}},
        "examples": [{"args": {"title": "Porto"}, "note": "a row"}]}}),
    )
    .await;
    ok(
        "update_component",
        json!({"name": "icon_row", "component": {
        "doc": "The pod's own row.",
        "params": [{"name": "title", "type": "text", "required": true, "doc": "The title"},
            {"name": "chevron", "type": "bool", "default": false, "doc": "A ›"}],
        "template": {"type": "core://Row", "spacing_dp": 8, "children": [
            {"type": "core://Text", "style": "Label", "value_expr": "Mine: $arg.title"},
            {"type": "core://IfArg", "arg": "chevron",
                "then": {"type": "core://Text", "style": "Title", "value_expr": "›"}}]},
        "examples": [{"args": {"title": "Porto"}, "note": "a row"}]}}),
    )
    .await;
    let use_it = json!({"type": "core://Use", "ref": "@me/icon_row", "args": {"title": "Porto"}});
    let c = ok(
        "create_layout",
        json!({"title": "Trips", "layout": {"type": "core://Page",
        "title": "Trips", "root": {"type": "core://Column", "children": [use_it.clone()]}}}),
    )
    .await;
    let c = c["id"].as_str().unwrap().to_owned();

    let expanded = ok("expand", json!({"node": use_it})).await.to_string();
    assert!(expanded.contains("Mine: Porto"), "{expanded}");
    let drawn = env.drawn(&c, &pod_code).await["workspace"].to_string();
    assert!(drawn.contains("Mine: Porto"), "{drawn}");
    assert!(
        !drawn.contains("corner_dp"),
        "the @core/ row's circle: {drawn}"
    );
}

/// Every component this build ships expands, at each of its examples, to a
/// layout a pod saves and serves: within the limits `check_node_values`
/// holds layouts to (`@core/tinted_icon`'s 999 dp corner once broke every
/// layout using it or `@core/icon_row`). And none is recorded as needing
/// removed vocabulary, which would have a client served an older element.
#[test]
fn every_shipped_components_examples_expand_to_valid_layouts() {
    let catalog = server::catalog();
    let core = pod_core::construct::core::load(&catalog).unwrap();
    let components: Vec<(&String, &String)> = core
        .names
        .iter()
        .filter_map(|(name, els)| Some((name, &els.last()?.0)))
        .filter(|(_, el)| core.objects[*el].get("params").is_some())
        .collect();
    assert!(
        components.iter().any(|(n, _)| *n == "@core/tinted_icon")
            && components.iter().any(|(n, _)| *n == "@core/icon_row"),
        "{components:?}"
    );
    let mut checked = 0;
    for (name, el) in components {
        let features = &core.features[el];
        assert!(
            !features.split(',').any(protocol::is_retired),
            "{name}: {features}"
        );
        for (i, ex) in core.objects[el]["examples"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let at = format!("{name} examples/{i}");
            let use_ = json!({"kind": "element", "node": {"type": "core://Use",
                "ref": {"name": name, "el": el, "pin": true},
                "args": ex.get("args").cloned().unwrap_or(json!({})),
                "children": ex.get("children").cloned().unwrap_or(json!([])),
                "slots": ex.get("slots").cloned().unwrap_or(json!({}))}});
            let mut objects = core.objects.clone();
            let id = construct_elements::object_id(&use_);
            objects.insert(id.clone(), std::sync::Arc::new(use_));
            let m = construct_elements::materialize(&objects, &id)
                .unwrap_or_else(|e| panic!("{at}: {e}"));
            assert!(m.missing.is_empty(), "{at}: {:?}", m.missing);
            let node: protocol::UiNode =
                serde_json::from_value(m.node).unwrap_or_else(|e| panic!("{at}: {e}"));
            if let Err(e) = pod_core::validate::check_node_values(&node) {
                panic!("{at}: {e}");
            }
            checked += 1;
        }
    }
    assert!(checked > 20, "{checked} examples");
}
