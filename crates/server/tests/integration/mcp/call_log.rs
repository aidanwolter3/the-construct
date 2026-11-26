//! The call log and undo this morning (decision `mcp-call-log`), through the
//! real host: every call a credential makes is logged under its name with
//! whether it was allowed or refused, a client can't name itself, and the
//! owner's undo puts the screens back to the checkpoint before a background
//! agent's first group and undoes its row edits, once.

use serde_json::{Value, json};

use super::{Env, client, env};

async fn ok(env: &Env, key: &str, tool: &str, args: Value) -> Value {
    let (err, out) = env.tool(key, tool, args).await;
    assert!(!err, "{tool}: {out}");
    out
}

async fn scoped(env: &Env, label: &str, grant: Value) -> String {
    let host = env.host.clone();
    let label = label.to_owned();
    tokio::task::spawn_blocking(move || {
        let g = pod_core::grant::Grant::from_json(&grant).unwrap();
        host.issue_mcp_scoped("mine", &label, pod_core::mcp::Access::Edit, Some(&g))
            .unwrap()
    })
    .await
    .unwrap()
}

fn recipe_layout() -> Value {
    json!({"type": "core://Page", "title": "Recipe", "root": {"type": "core://Column", "children": [
        {"type": "core://Text", "style": "Title", "value_expr": "Recipe"},
        {"type": "core://List", "query_key": "collection:dishes",
         "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}
    ]}})
}

async fn texts(env: &Env, key: &str, id: &str) -> String {
    ok(env, key, "get_layout", json!({ "id": id })).await["layout"].to_string()
}

/// The live rows of needs (an export lists deleted ones too, marked).
async fn rows(env: &Env, key: &str) -> String {
    let out = ok(
        env,
        key,
        "export_collection",
        json!({ "collection": "needs" }),
    )
    .await;
    let live: Vec<&Value> = out["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["deleted_at"].is_null())
        .collect();
    json!(live).to_string()
}

#[tokio::test]
async fn every_call_is_logged_and_undo_reverts_a_mornings_work() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
    ok(
        &env,
        &owner,
        "define_collection",
        json!({"name": "needs", "fields": [{"name": "title", "type": "text"}]}),
    )
    .await;
    let id = ok(
        &env,
        &owner,
        "create_layout",
        json!({"title": "Recipe", "icon": "recipe", "feeds": [["collections", "dishes"]],
               "layout": recipe_layout()}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = scoped(
        &env,
        "Anticipate build",
        json!({
            "tools": ["add_row", "get_layout", "stage_edit_group", "apply_edit_group",
                      "call_log", "undo_background"],
            "rows": ["needs"],
        }),
    )
    .await;

    // A morning's work: a row, a group on the screen, and three refusals.
    ok(
        &env,
        &job,
        "add_row",
        json!({ "collection": "needs", "values": { "title": "Shop for the dinner" } }),
    )
    .await;
    let v = ok(&env, &job, "get_layout", json!({ "id": id })).await["version"].clone();
    let staged = ok(
        &env,
        &job,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": [{ "op": "add", "path": "/root/children/1",
                  "value": { "type": "core://Text", "style": "Body", "value_expr": "Tip: cook it the day before" } }] }] }),
    )
    .await;
    ok(
        &env,
        &job,
        "apply_edit_group",
        json!({ "id": staged["id"] }),
    )
    .await;
    assert!(texts(&env, &owner, &id).await.contains("Tip:"));
    for _ in 0..3 {
        let (err, out) = env
            .tool(
                &job,
                "set_theme",
                json!({ "theme": { "primary": "#336699" } }),
            )
            .await;
        assert!(err, "{out}");
        let (err, out) = env
            .tool(
                &job,
                "add_row",
                json!({ "collection": "dishes", "values": { "title": "x" } }),
            )
            .await;
        assert!(err && out["error"] == "grant_refuses", "{out}");
    }
    // A client naming itself is ignored: the host strips the header.
    let resp = client()
        .post(format!("{}/v1/mcp", env.url))
        .bearer_auth(&job)
        .header(
            pod_core::call_log::LABEL_HEADER,
            pod_core::call_log::label_header_value("Someone else"),
        )
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "get_layout", "arguments": {"id": id}}}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // The owner reads the log: the job by its name, granted, with its
    // refusals; the client's own name is nowhere.
    let log = ok(&env, &owner, "call_log", json!({ "since": 0 })).await;
    let creds = log["credentials"].as_array().unwrap();
    let build = creds
        .iter()
        .find(|c| c["credential"] == "Anticipate build")
        .unwrap_or_else(|| panic!("{log}"));
    assert_eq!(build["granted"], true, "{log}");
    assert!(build["refused"].as_i64().unwrap() >= 3, "{log}");
    assert!(!log.to_string().contains("Someone else"), "{log}");
    let refused = ok(
        &env,
        &owner,
        "call_log",
        json!({ "since": 0, "credential": "Anticipate build", "refused": true }),
    )
    .await;
    let calls = refused["calls"].as_array().unwrap();
    assert!(
        calls.iter().all(|c| c["outcome"] == "refused")
            && calls
                .iter()
                .any(|c| c["tool"] == "add_row" && c["target"] == "collection dishes"),
        "{refused}"
    );
    // The job can't undo, even with the tool in its grant: only the owner.
    let (err, out) = env.tool(&job, "undo_background", json!({})).await;
    assert!(err && out["error"] == "grant_refuses", "{out}");

    // The owner's own row isn't the job's, so undo leaves it.
    ok(
        &env,
        &owner,
        "add_row",
        json!({ "collection": "needs", "values": { "title": "My own need" } }),
    )
    .await;
    let dry = ok(
        &env,
        &owner,
        "undo_background",
        json!({ "since": 0, "dry_run": true }),
    )
    .await;
    assert_eq!(dry["edits"], 1, "{dry}");
    assert!(dry["checkpoint"].is_string(), "{dry}");
    assert!(rows(&env, &owner).await.contains("Shop for the dinner"));

    let undone = ok(&env, &owner, "undo_background", json!({ "since": 0 })).await;
    assert_eq!(undone["ok"], true, "{undone}");
    assert!(!texts(&env, &owner, &id).await.contains("Tip:"));
    let now = rows(&env, &owner).await;
    assert!(
        !now.contains("Shop for the dinner") && now.contains("My own need"),
        "{now}"
    );

    // A second undo finds nothing new to do.
    let again = ok(&env, &owner, "undo_background", json!({ "since": 0 })).await;
    assert!(
        again["checkpoint"].is_null() && again["edits"] == 0,
        "{again}"
    );
}
