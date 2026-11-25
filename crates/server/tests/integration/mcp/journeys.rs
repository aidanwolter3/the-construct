//! Journey walks and dry runs (decision `journeys-and-dry-runs`), through
//! the real host: a walk types and taps through the owner's screens, its
//! writes are dry runs (the rows they would write join the walk and
//! `expect-write` checks them, and nothing is written), a step that can't
//! be done fails as broken and is stored, and a staged edit group's screens
//! are walked before they go live.

use serde_json::{Value, json};

use super::{Env, env};

async fn ok(env: &Env, key: &str, tool: &str, args: Value) -> Value {
    let (err, out) = env.tool(key, tool, args).await;
    assert!(!err, "{tool}: {out}");
    out
}

fn save_button() -> Value {
    json!({ "type": "core://Button", "label": "Save", "on_click": { "intent": "action.mutation.create",
        "query_key": "collection:notes", "entity_uri": "collections://notes/$nonce", "entity": {},
        "endpoint": "/v1/collections/notes/create", "payload": { "title": "$form.title" } } })
}

fn form(with_save: bool) -> Value {
    let mut children = vec![
        json!({ "type": "core://Text", "style": "Title", "value_expr": "New note" }),
        json!({ "type": "core://TextField", "field": "title", "placeholder": "Title" }),
    ];
    if with_save {
        children.push(save_button());
    }
    json!({ "type": "core://Page", "title": "Note", "root": { "type": "core://Column", "children": children } })
}

async fn setup(env: &Env, key: &str) -> (String, String) {
    ok(
        env,
        key,
        "define_collection",
        json!({"name": "notes", "fields": [{"name": "title", "type": "text"}]}),
    )
    .await;
    let mk = |with: bool| json!({"title": "Note", "icon": "dashboard", "feeds": [["collections", "notes"]], "layout": form(with)});
    let good = ok(env, key, "create_layout", mk(true)).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bad = ok(env, key, "create_layout", mk(false)).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (good, bad)
}

fn journey(layout: &str) -> String {
    format!(
        "open-layout {layout}; type title Test dish; click Save; expect-write notes {{\"title\": \"Test dish\"}}"
    )
}

#[tokio::test]
async fn a_walk_dry_runs_its_writes_and_finds_a_missing_step() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
    let (good, bad) = setup(&env, &owner).await;

    let walked = ok(
        &env,
        &owner,
        "walk_journey",
        json!({ "script": journey(&good), "journey": "add-a-note" }),
    )
    .await;
    assert_eq!(walked["ok"], true, "{walked}");
    assert_eq!(walked["writes"][0]["status"], "dry_run", "{walked}");
    // Nothing was written.
    let export = ok(
        &env,
        &owner,
        "export_collection",
        json!({ "collection": "notes" }),
    )
    .await;
    assert_eq!(export["rows"].as_array().unwrap().len(), 0, "{export}");

    // Without the button the journey breaks, on that screen.
    let broken = ok(
        &env,
        &owner,
        "walk_journey",
        json!({ "script": journey(&bad) }),
    )
    .await;
    assert_eq!(broken["ok"], false, "{broken}");
    assert_eq!(broken["class"], "broken", "{broken}");
    assert_eq!(broken["failing_layout"], json!(bad), "{broken}");

    // The fix: the same button, staged against that walk, walked staged,
    // then applied.
    let v = ok(&env, &owner, "get_layout", json!({ "id": bad })).await["version"].clone();
    let staged = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Notes can be saved", "walk_id": broken["walk_id"],
                "changes": [{ "id": bad, "base_version": v,
                              "json_patch": [{ "op": "add", "path": "/root/children/-", "value": save_button() }] }] }),
    )
    .await;
    assert_eq!(staged["report"]["additive"], true, "{staged}");
    let group = staged["id"].as_str().unwrap().to_owned();
    let still = ok(
        &env,
        &owner,
        "walk_journey",
        json!({ "script": journey(&bad) }),
    )
    .await;
    assert_eq!(
        still["ok"], false,
        "the live screen isn't changed yet: {still}"
    );
    let fixed = ok(
        &env,
        &owner,
        "walk_journey",
        json!({ "script": journey(&bad), "group": group }),
    )
    .await;
    assert_eq!(fixed["ok"], true, "{fixed}");
    ok(&env, &owner, "apply_edit_group", json!({ "id": group })).await;
    let live = ok(
        &env,
        &owner,
        "walk_journey",
        json!({ "script": journey(&bad) }),
    )
    .await;
    assert_eq!(live["ok"], true, "{live}");
}

#[tokio::test]
async fn a_fix_may_not_call_an_endpoint_no_screen_calls() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
    let (_, bad) = setup(&env, &owner).await;
    let broken = ok(
        &env,
        &owner,
        "walk_journey",
        json!({ "script": journey(&bad) }),
    )
    .await;
    let v = ok(&env, &owner, "get_layout", json!({ "id": bad })).await["version"].clone();
    let mut other = save_button();
    other["on_click"]["endpoint"] = json!("/v1/collections/dishes/create");
    let staged = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Something else", "walk_id": broken["walk_id"],
                "changes": [{ "id": bad, "base_version": v,
                              "json_patch": [{ "op": "add", "path": "/root/children/-", "value": other }] }] }),
    )
    .await;
    assert_eq!(staged["report"]["additive"], false, "{staged}");
}

#[tokio::test]
async fn a_later_write_reads_what_an_earlier_one_would_have_written() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
    ok(
        &env,
        &owner,
        "define_collection",
        json!({"name": "lines", "fields": [{"name": "thing", "type": "text"}]}),
    )
    .await;
    ok(&env, &owner, "define_collection",
        json!({"name": "groceries", "fields": [{"name": "name", "type": "text"}],
               "actions": [{"name": "add_all", "title": "Add the lines",
                            "source": {"collection": "lines", "where": [{"field": "thing", "empty": false}]},
                            "match": {"name": "$row.thing"}, "create": {"name": "$row.thing"}}]})).await;
    let layout = json!({ "type": "core://Page", "title": "Recipe", "root": { "type": "core://Column", "children": [
        { "type": "core://TextField", "field": "thing", "placeholder": "Ingredient" },
        { "type": "core://Button", "label": "Add line", "on_click": { "intent": "action.mutation.create",
          "query_key": "collection:lines", "entity_uri": "collections://lines/$nonce", "entity": {},
          "endpoint": "/v1/collections/lines/create", "payload": { "thing": "$form.thing" } } },
        { "type": "core://Button", "label": "Add to Groceries", "on_click": { "intent": "action.mutation.optimistic",
          "endpoint": "/v1/collections/groceries/actions/add_all", "entity_uri": "local://walk",
          "field": "adding", "value_expr": "true", "payload": { "client_id": "g-$nonce" } } }
    ]}});
    let id = ok(&env, &owner, "create_layout",
        json!({"title": "Recipe", "icon": "recipe",
               "feeds": [["collections", "lines"], ["collections", "groceries"]], "layout": layout}))
        .await["id"].as_str().unwrap().to_owned();
    let walked = ok(&env, &owner, "walk_journey", json!({ "script": format!(
        "open-layout {id}; type thing Cranberries; click Add line; click Add to Groceries; expect-write groceries {{\"name\": \"Cranberries\"}}") }))
        .await;
    assert_eq!(walked["ok"], true, "{walked}");
    for c in ["lines", "groceries"] {
        let export = ok(
            &env,
            &owner,
            "export_collection",
            json!({ "collection": c }),
        )
        .await;
        assert_eq!(export["rows"].as_array().unwrap().len(), 0, "{c}: {export}");
    }
}
