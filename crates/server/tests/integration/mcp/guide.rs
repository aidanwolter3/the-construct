//! What an agent reads when it connects (decision
//! `element-roles-and-reuse`): short instructions that name the build
//! procedure and the guide's topics, and the guide itself.

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::env;

#[tokio::test]
async fn the_instructions_are_short_and_name_tools_and_topics_that_exist() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    let (status, init) = env
        .rpc(
            Some(&access),
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{init}");
    let text = init["result"]["instructions"].as_str().unwrap();
    // A client may show only the start of long instructions (one cut them
    // at 4,096 characters), so everything an agent must know fits well
    // inside that.
    assert!(
        text.chars().count() <= pod_core::mcp::MAX_INSTRUCTIONS,
        "{} characters",
        text.chars().count()
    );
    // Reuse comes first: looking at what exists is step 1.
    let find = text.find("find_elements").expect("names find_elements");
    assert!(find < 700, "find_elements at {find}");

    // Every tool it names exists.
    let (_, list) = env.rpc(Some(&access), "tools/list", json!({})).await;
    let tools: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for named in [
        "list_layouts",
        "find_elements",
        "define_component",
        "render_layout_preview",
        "note_gap",
        "get_layout",
        "restore_version",
        "list_data_sources",
        "sample_data",
        "guide",
    ] {
        assert!(text.contains(named), "the instructions name {named}");
        assert!(tools.contains(&named), "{named} is a tool");
    }

    // Every topic it lists answers, the sources' among them.
    let listed = text.rsplit("guide {topic}:").next().unwrap();
    let topics: Vec<&str> = listed
        .split(';')
        .filter_map(|t| t.trim().split(' ').next())
        .collect();
    for want in ["navigation", "components", "theme", "collections", "events"] {
        assert!(topics.contains(&want), "{topics:?}");
    }
    for t in &topics {
        let (err, g) = env.tool(&access, "guide", json!({"topic": t})).await;
        assert!(!err, "{t}: {g}");
        assert!(g["text"].as_str().unwrap().len() > 100, "{t}: {g}");
    }
    let (_, c) = env
        .tool(&access, "guide", json!({"topic": "collections"}))
        .await;
    assert!(c["text"].as_str().unwrap().contains("define_collection"));

    // `all` is every topic; an unknown one says which there are.
    let (err, all) = env.tool(&access, "guide", json!({"topic": "all"})).await;
    assert!(!err, "{all}");
    let all = all["text"].as_str().unwrap();
    assert!(all.contains("action.confirm") && all.contains("define_collection"));
    let (err, no) = env.tool(&access, "guide", json!({"topic": "nope"})).await;
    assert!(err, "{no}");
    assert_eq!(no["error"], "unknown_topic", "{no}");
    assert!(no["topics"].as_array().unwrap().len() >= 9, "{no}");
    let _: &Value = &no;
}

fn names(found: &Value) -> Vec<&str> {
    found["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn find_elements_answers_what_exists_by_what_it_is_for() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    let (err, f) = env
        .tool(&access, "find_elements", json!({"query": "empty list"}))
        .await;
    assert!(!err, "{f}");
    // The list-or-empty-note pattern first, and the note itself among them.
    assert_eq!(names(&f)[0], "@core/counted_list", "{f}");
    let first = f["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "@core/muted_note")
        .unwrap_or_else(|| panic!("{f}"));
    assert_eq!(first["role"], "component");
    assert_eq!(first["scope"], "core");
    assert_eq!(first["category"], "pattern");
    assert!(first["use_when"].as_str().unwrap().contains("empty"));
    assert!(
        first["params"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "count"),
        "{first}"
    );
    // A row is small: no template, no doc, no examples.
    assert!(first.get("node").is_none() && first.get("doc").is_none());

    let (_, f) = env
        .tool(
            &access,
            "find_elements",
            json!({"query": "photo feed", "role": "layout"}),
        )
        .await;
    assert_eq!(names(&f)[0], "@core/pixelfed", "{f}");

    let (_, rows) = env
        .tool(
            &access,
            "find_elements",
            json!({"category": "row", "scope": "core", "limit": 60}),
        )
        .await;
    assert!(names(&rows).contains(&"@core/icon_row"), "{rows}");
    assert!(!names(&rows).contains(&"@core/pill"), "{rows}");

    // The pod's own components and layouts are found too.
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "price_tag", "component": {
                "doc": "A price on a soft green tag. For a shopping row.",
                "params": [{"name": "price", "type": "text", "required": true, "doc": "The price."}],
                "template": {"type": "core://Text", "style": "Caption", "value_expr": "$arg.price"},
                "examples": [{"args": {"price": "$12"}, "note": "A price"}]}}),
        )
        .await;
    assert!(!err, "{d}");
    let (_, own) = env
        .tool(
            &access,
            "find_elements",
            json!({"query": "price", "scope": "own"}),
        )
        .await;
    assert_eq!(names(&own), ["@me/price_tag"], "{own}");
    assert_eq!(
        own["elements"][0]["use_when"],
        "A price on a soft green tag"
    );
    let (_, home) = env
        .tool(
            &access,
            "find_elements",
            json!({"query": "home", "scope": "own"}),
        )
        .await;
    assert!(names(&home).contains(&"home"), "{home}");

    // Every core component and layout says where it belongs and when to
    // use it: the catalog files keep up with what ships.
    let (_, all) = env
        .tool(
            &access,
            "find_elements",
            json!({"scope": "core", "limit": 60}),
        )
        .await;
    let listed = all["elements"].as_array().unwrap();
    assert_eq!(
        all["total"].as_u64().unwrap() as usize,
        listed.len(),
        "{all}"
    );
    assert!(listed.len() > 35, "{all}");
    for e in listed {
        assert!(
            e["category"].is_string() && e["use_when"].as_str().is_some_and(|u| u.len() > 20),
            "{e} needs an entry in its crate's catalog.json"
        );
    }
}

#[tokio::test]
async fn a_write_points_at_what_could_be_reused() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    // What two core components draw, written out by hand.
    let mut by_hand = Vec::new();
    for (name, args) in [
        ("@core/muted_text", json!({"text": "Updated today"})),
        ("@core/pill", json!({"text": "New"})),
    ] {
        let (err, x) = env
            .tool(
                &access,
                "expand",
                json!({"node": {"type": "core://Use", "ref": name, "args": args}}),
            )
            .await;
        assert!(!err, "{x}");
        by_hand.push(x["expanded"].clone());
    }
    // And one card written three times with different words.
    let card = |title: &str, body: &str| {
        json!({"type": "core://Box", "corner_dp": 12, "padding_h_dp": 12, "padding_v_dp": 10,
            "content": {"type": "core://Column", "spacing_dp": 4, "children": [
                {"type": "core://Text", "style": "Label", "value_expr": title},
                {"type": "core://Text", "style": "Secondary", "value_expr": body},
                {"type": "core://Row", "children": [
                    {"type": "core://Text", "style": "Caption", "value_expr": "More"},
                    {"type": "core://Spacer"}]}]}})
    };
    let layout = json!({"type": "core://Page", "title": "Hints", "root": {"type": "core://Column", "children": [
        by_hand[0], by_hand[1],
        card("One", "First"), card("Two", "Second"), card("Three", "Third"),
        {"type": "core://Text", "style": "Body",
            "value_expr": "${collections://dishes/r1a2b3c4d5#title}"},
        {"type": "core://Use", "ref": "@core/muted_text", "args": {"text": "Already a component"}}]}});
    let (err, made) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "Hints", "feeds": [["collections", "dishes"]], "layout": layout}),
        )
        .await;
    assert!(!err, "the write succeeds: hints are advice: {made}");
    let warnings: Vec<&str> = made["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("{made}"))
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    let has = |needles: &[&str]| {
        warnings
            .iter()
            .any(|w| needles.iter().all(|n| w.contains(n)))
    };
    assert!(
        has(&["/root/children/0:", "@core/muted_text", "Updated today"]),
        "{warnings:#?}"
    );
    assert!(
        has(&["/root/children/1:", "@core/pill", "New"]),
        "{warnings:#?}"
    );
    assert!(
        has(&["/root/children/2:", "written 3 times", "define_component"]),
        "{warnings:#?}"
    );
    assert!(
        has(&["collections://dishes/r1a2b3c4d5", "$in."]),
        "{warnings:#?}"
    );
    // A use of a component is not advised to be one, and a single node
    // isn't a piece.
    assert!(!has(&["/root/children/6"]), "{warnings:#?}");
    assert!(warnings.len() <= 12, "{warnings:#?}");

    // Nothing to say about a layout made of components.
    let (err, clean) = env
        .tool(
            &access,
            "validate_layout",
            json!({"layout": {"type": "core://Page", "title": "Clean", "root":
                {"type": "core://Use", "ref": "@core/scroll_page", "args": {"title": "Clean"}, "children": [
                    {"type": "core://Use", "ref": "@core/muted_text", "args": {"text": "Hello"}}]}}}),
        )
        .await;
    assert!(!err, "{clean}");
    assert!(clean.get("warnings").is_none(), "{clean}");

    // What was missing is kept for the operator.
    let (err, g) = env
        .tool(
            &access,
            "note_gap",
            json!({"what": "a row with a leading checkbox\n and a trailing tag"}),
        )
        .await;
    assert!(!err, "{g}");
    assert_eq!(g["noted"], 1, "{g}");
    let (err, g) = env.tool(&access, "note_gap", json!({"what": "  "})).await;
    assert!(err, "{g}");
}

/// The `dates` source: a layout lists the days around today with no
/// collection of dates (decision `element-roles-and-reuse`).
#[tokio::test]
async fn a_layout_lists_days_from_the_dates_feed() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    let (_, sources) = env.tool(&access, "list_data_sources", json!({})).await;
    let dates = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "dates")
        .unwrap_or_else(|| panic!("{sources}"));
    assert_eq!(dates["feeds"][0]["name"], "days", "{dates}");

    let (err, made) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "Week", "feeds": [["dates", "days"]],
                "layout": {"type": "core://Page", "title": "Week", "root":
                    {"type": "core://List", "query_key": "dates:next_7", "item_template":
                        {"type": "core://Use", "ref": "@core/item_row",
                            "args": {"title": "$item.label", "meta": "$item.weekday_long"}}}}}),
        )
        .await;
    assert!(!err, "{made}");
    let drawn = env.drawn(made["id"].as_str().unwrap(), &pod_code).await;
    let week = drawn["queries"]["dates:next_7"].as_array().unwrap();
    assert_eq!(week.len(), 7, "{drawn}");
    let first = &drawn["entities"][week[0].as_str().unwrap()];
    assert_eq!(first["label"], "Today", "{first}");
    assert_eq!(first["offset"], 0, "{first}");
    assert_eq!(
        drawn["queries"]["dates:this_week"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
}

/// A kit (decision `element-roles-and-reuse`): `use_kit` makes the
/// collection, home binds its feed, and the kit's core layout draws it by
/// reference with its inputs bound; a second collection gets its own.
#[tokio::test]
async fn a_kit_is_a_core_layout_over_a_collection_it_makes() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    // Kits are found like everything else, and listed by the tool.
    let (_, found) = env
        .tool(
            &access,
            "find_elements",
            json!({"query": "things to tick off"}),
        )
        .await;
    assert_eq!(names(&found)[0], "@core/checklist", "{found}");
    let (_, kits) = env.tool(&access, "use_kit", json!({})).await;
    assert_eq!(kits["kits"][0]["kit"], "checklist", "{kits}");

    let (err, used) = env
        .tool(
            &access,
            "use_kit",
            json!({"kit": "checklist", "collection": "packing", "title": "Packing list"}),
        )
        .await;
    assert!(!err, "{used}");
    assert_eq!(used["created"], true, "{used}");
    assert_eq!(used["embed"]["bind"]["collection"], "packing", "{used}");
    let (_, cols) = env.tool(&access, "list_collections", json!({})).await;
    assert!(cols.to_string().contains("\"packing\""), "{cols}");

    // Home shows it: the embed as the kit answered it.
    let (err, h) = env
        .tool(
            &access,
            "update_layout",
            json!({"app": "home", "layout": {"type": "core://Page", "title": "Home",
                "root": used["embed"]}}),
        )
        .await;
    assert!(!err, "{h}");
    let (err, row) = env
        .tool(
            &access,
            "add_row",
            json!({"collection": "packing", "values": {"title": "Passport"}}),
        )
        .await;
    assert!(!err, "{row}");

    // Home as a device draws it: the kit's layout with its inputs filled
    // in (the title, the collection's rows).
    let (err, _, text) = super::preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": "home"}),
    )
    .await;
    assert!(!err, "{text}");
    let details = super::preview_details(&text);
    let drawn: Vec<&str> = details["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["text"].as_str())
        .collect();
    for want in ["Packing list", "Passport", "To do"] {
        assert!(
            drawn.iter().any(|t| t.contains(want)),
            "{want}: {drawn:?}\n{text}"
        );
    }
    assert!(!text.contains("$in."), "{text}");

    // Using it again over the same collection makes nothing new; a
    // collection without the kit's fields is refused with what is missing.
    let (err, again) = env
        .tool(
            &access,
            "use_kit",
            json!({"kit": "checklist", "collection": "packing"}),
        )
        .await;
    assert!(!err, "{again}");
    assert_eq!(again["created"], false, "{again}");
    let (err, d) = env
        .tool(
            &access,
            "define_collection",
            json!({"name": "books", "fields": [{"name": "title", "type": "text"}]}),
        )
        .await;
    assert!(!err, "{d}");
    let (err, no) = env
        .tool(
            &access,
            "use_kit",
            json!({"kit": "checklist", "collection": "books"}),
        )
        .await;
    assert!(err, "{no}");
    assert_eq!(no["error"], "collection_mismatch", "{no}");
    assert_eq!(no["missing"], json!(["done", "notes"]), "{no}");
    let (err, no) = env.tool(&access, "use_kit", json!({"kit": "nope"})).await;
    assert!(err && no["error"] == "unknown_kit", "{no}");
}
