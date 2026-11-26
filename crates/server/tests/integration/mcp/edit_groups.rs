//! Edit groups and checkpoints (decision `edit-groups`), through the real
//! host: a staged group changes nothing live, apply makes it live with a
//! checkpoint first, revert undoes it (or says conflict when someone changed
//! the screen since), a scoped credential's grant holds the additive and
//! protected rules at apply, and a checkpoint restores the screens.

use serde_json::{Value, json};

use super::{Env, env};

fn recipe_layout() -> Value {
    json!({"type": "core://Page", "title": "Recipe", "root": {"type": "core://Column", "children": [
        {"type": "core://Text", "style": "Title", "value_expr": "Recipe"},
        {"type": "core://List", "query_key": "collection:dishes",
         "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}
    ]}})
}

async fn ok(env: &Env, key: &str, tool: &str, args: Value) -> Value {
    let (err, out) = env.tool(key, tool, args).await;
    assert!(!err, "{tool}: {out}");
    out
}

async fn version(env: &Env, key: &str, id: &str) -> i64 {
    ok(env, key, "get_layout", json!({ "id": id })).await["version"]
        .as_i64()
        .unwrap()
}

async fn texts(env: &Env, key: &str, id: &str) -> String {
    ok(env, key, "get_layout", json!({ "id": id })).await["layout"].to_string()
}

fn add_text(text: &str) -> Value {
    json!([{ "op": "add", "path": "/root/children/1",
             "value": { "type": "core://Text", "style": "Body", "value_expr": text } }])
}

async fn scoped(env: &Env, grant: Value) -> String {
    let host = env.host.clone();
    tokio::task::spawn_blocking(move || {
        let g = pod_core::grant::Grant::from_json(&grant).unwrap();
        host.issue_mcp_scoped("mine", "Anticipate", pod_core::mcp::Access::Edit, Some(&g))
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn stage_apply_revert_and_restore() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
    let created = ok(
        &env,
        &owner,
        "create_layout",
        json!({"title": "Recipe", "icon": "recipe", "feeds": [["collections", "dishes"]],
               "layout": recipe_layout()}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let v0 = version(&env, &owner, &id).await;

    // Staged: nothing live, an additive report.
    let staged = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v0,
                "json_patch": add_text("Tip: cook it the day before") }] }),
    )
    .await;
    assert_eq!(staged["report"]["additive"], true, "{staged}");
    let group = staged["id"].as_str().unwrap().to_owned();
    assert_eq!(version(&env, &owner, &id).await, v0);
    assert!(!texts(&env, &owner, &id).await.contains("Tip:"));

    // Applied: live, with a checkpoint.
    let applied = ok(&env, &owner, "apply_edit_group", json!({ "id": group })).await;
    assert_eq!(applied["state"], "applied", "{applied}");
    assert!(texts(&env, &owner, &id).await.contains("Tip:"));
    let ckpts = ok(&env, &owner, "list_checkpoints", json!({})).await;
    let ckpt = ckpts["checkpoints"][0]["id"].as_str().unwrap().to_owned();
    // Applying twice does nothing more.
    let again = ok(&env, &owner, "apply_edit_group", json!({ "id": group })).await;
    assert_eq!(again["already"], true, "{again}");

    // Reverted: back as it was.
    let reverted = ok(&env, &owner, "revert_edit_group", json!({ "id": group })).await;
    assert_eq!(reverted["state"], "reverted", "{reverted}");
    assert!(!texts(&env, &owner, &id).await.contains("Tip:"));

    // A revert after the owner changed the screen is a conflict.
    let v = version(&env, &owner, &id).await;
    let g2 = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip: rest the dough") }] }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(&env, &owner, "apply_edit_group", json!({ "id": g2 })).await;
    let v = version(&env, &owner, &id).await;
    ok(
        &env,
        &owner,
        "update_layout",
        json!({ "id": id, "base_version": v, "json_patch": add_text("My own note") }),
    )
    .await;
    let conflict = ok(&env, &owner, "revert_edit_group", json!({ "id": g2 })).await;
    assert_eq!(conflict["state"], "conflict", "{conflict}");
    assert!(texts(&env, &owner, &id).await.contains("My own note"));

    // The first checkpoint brings it back as it was then, saying first that
    // it undoes the owner's own note.
    let dry = ok(
        &env,
        &owner,
        "restore_checkpoint",
        json!({ "id": ckpt, "dry_run": true }),
    )
    .await;
    let would = &dry["would"];
    assert!(
        would["moves"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == &json!(id)),
        "{dry}"
    );
    assert!(
        would["undoes_your_edits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == json!(id)),
        "{dry}"
    );
    let restored = ok(&env, &owner, "restore_checkpoint", json!({ "id": ckpt })).await;
    assert_eq!(restored["ok"], true, "{restored}");
    let now = texts(&env, &owner, &id).await;
    assert!(
        !now.contains("My own note") && !now.contains("Tip:"),
        "{now}"
    );
}

#[tokio::test]
async fn a_grant_applies_only_additive_unprotected_groups() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
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
    let tools = json!([
        "get_layout",
        "stage_edit_group",
        "apply_edit_group",
        "revert_edit_group",
        "list_edit_groups"
    ]);
    let job = scoped(&env, json!({ "tools": tools })).await;

    // Dropping the list loses what the screen showed: staged, not applied.
    let v = version(&env, &owner, &id).await;
    let staged = ok(
        &env,
        &job,
        "stage_edit_group",
        json!({ "title": "Drop the list", "changes": [{ "id": id, "base_version": v,
                "json_patch": [{ "op": "remove", "path": "/root/children/1" }] }] }),
    )
    .await;
    assert_eq!(staged["report"]["safe"], false, "{staged}");
    let (err, out) = env
        .tool(&job, "apply_edit_group", json!({ "id": staged["id"] }))
        .await;
    assert!(err && out["error"] == "not_safe", "{out}");

    // A restyle (a bigger title) is safe, though not additive: applied, then
    // undone so the rest of the test starts from the same screen.
    let staged = ok(
        &env,
        &job,
        "stage_edit_group",
        json!({ "title": "A bigger title", "changes": [{ "id": id, "base_version": v,
                "json_patch": [{ "op": "replace", "path": "/root/children/0/style", "value": "Display" }] }] }),
    )
    .await;
    assert_eq!(staged["report"]["safe"], true, "{staged}");
    assert_eq!(staged["report"]["additive"], false, "{staged}");
    let out = ok(
        &env,
        &job,
        "apply_edit_group",
        json!({ "id": staged["id"] }),
    )
    .await;
    assert_eq!(out["state"], "applied", "{out}");
    ok(
        &env,
        &job,
        "revert_edit_group",
        json!({ "id": staged["id"] }),
    )
    .await;
    let v = version(&env, &owner, &id).await;

    // A protected layout isn't changed even additively.
    let guarded = scoped(&env, json!({ "tools": tools, "protected_names": [id] })).await;
    let staged = ok(
        &env,
        &guarded,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip") }] }),
    )
    .await;
    let (err, out) = env
        .tool(&guarded, "apply_edit_group", json!({ "id": staged["id"] }))
        .await;
    assert!(err && out["error"] == "protected", "{out}");

    // Additive and unprotected: applied.
    let staged = ok(
        &env,
        &job,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip") }] }),
    )
    .await;
    let out = ok(
        &env,
        &job,
        "apply_edit_group",
        json!({ "id": staged["id"] }),
    )
    .await;
    assert_eq!(out["state"], "applied", "{out}");

    // A group staged before the screens moved is stale.
    let v = version(&env, &owner, &id).await;
    let stale = ok(
        &env,
        &job,
        "stage_edit_group",
        json!({ "title": "Another tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip 2") }] }),
    )
    .await;
    ok(
        &env,
        &owner,
        "update_layout",
        json!({ "id": id, "base_version": v, "json_patch": add_text("Owner") }),
    )
    .await;
    let (err, out) = env
        .tool(&job, "apply_edit_group", json!({ "id": stale["id"] }))
        .await;
    assert!(
        err && (out["error"] == "stale_stage" || out["error"] == "stale_base"),
        "{out}"
    );
}

/// The grant holds on every way a group's names are written, not only a
/// first apply: a redo, an undo (one group or every group since), a
/// checkpoint restore; and only the owner keeps a trial.
#[tokio::test]
async fn a_grant_holds_on_redo_undo_keep_and_restore() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
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
    let guarded = scoped(
        &env,
        json!({ "tools": ["apply_edit_group", "revert_edit_group", "restore_checkpoint",
                          "list_edit_groups", "list_checkpoints"],
                "protected_names": [id] }),
    )
    .await;
    let refused = |out: &Value, error: &str| out["error"] == error;

    // The owner's own group on the protected layout, applied then undone.
    let v = version(&env, &owner, &id).await;
    let group = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip") }] }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(&env, &owner, "apply_edit_group", json!({ "id": group })).await;
    let ckpt = ok(&env, &owner, "list_checkpoints", json!({})).await["checkpoints"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(&env, &owner, "revert_edit_group", json!({ "id": group })).await;
    let undone = texts(&env, &owner, &id).await;

    // Redo: refused, and nothing moved.
    let (err, out) = env
        .tool(&guarded, "apply_edit_group", json!({ "id": group }))
        .await;
    assert!(err && refused(&out, "protected"), "{out}");
    assert_eq!(texts(&env, &owner, &id).await, undone);

    // Undo, by id or every group since: refused.
    ok(&env, &owner, "apply_edit_group", json!({ "id": group })).await;
    let redone = texts(&env, &owner, &id).await;
    let (err, out) = env
        .tool(&guarded, "revert_edit_group", json!({ "id": group }))
        .await;
    assert!(err && refused(&out, "protected"), "{out}");
    let (err, out) = env
        .tool(&guarded, "revert_edit_group", json!({ "since": 0 }))
        .await;
    assert!(err && refused(&out, "protected"), "{out}");
    assert_eq!(texts(&env, &owner, &id).await, redone);

    // A checkpoint restore that would move it: refused (a dry run still answers).
    let out = ok(
        &env,
        &guarded,
        "restore_checkpoint",
        json!({ "id": ckpt, "dry_run": true }),
    )
    .await;
    assert_eq!(out["dry_run"], true, "{out}");
    let (err, out) = env
        .tool(&guarded, "restore_checkpoint", json!({ "id": ckpt }))
        .await;
    assert!(err && refused(&out, "protected"), "{out}");
    assert_eq!(texts(&env, &owner, &id).await, redone);

    // Keep: the owner's alone, even on an unprotected layout.
    ok(&env, &owner, "revert_edit_group", json!({ "id": group })).await;
    let other = ok(
        &env,
        &owner,
        "create_layout",
        json!({"title": "Other", "icon": "recipe", "feeds": [["collections", "dishes"]],
               "layout": recipe_layout()}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let v = version(&env, &owner, &other).await;
    let trial = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": other, "base_version": v,
                "json_patch": add_text("Tip") }] }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &env,
        &owner,
        "apply_edit_group",
        json!({ "id": trial, "trial": true }),
    )
    .await;
    let (err, out) = env
        .tool(&guarded, "apply_edit_group", json!({ "id": trial }))
        .await;
    assert!(err && refused(&out, "owner_keeps"), "{out}");
    let groups = ok(&env, &owner, "list_edit_groups", json!({})).await;
    let state = groups["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == trial.as_str())
        .unwrap()["state"]
        .clone();
    assert_eq!(state, "trial", "{groups}");

    // Undoing its trial on an unprotected layout is still allowed.
    let out = ok(&env, &guarded, "revert_edit_group", json!({ "id": trial })).await;
    assert_eq!(out["state"], "reverted", "{out}");
}

#[tokio::test]
async fn the_app_lists_and_undoes_a_group_and_only_the_owner_may() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
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
    let v = version(&env, &owner, &id).await;
    let group = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip") }] }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(&env, &owner, "apply_edit_group", json!({ "id": group })).await;

    let (status, screen) = env.get("/v1/edit-groups/screen", &pod_code).await;
    assert!(status.is_success(), "{screen}");
    let e = &screen["entities"][format!("edit_groups://{group}")];
    assert_eq!(e["can_undo"], true, "{screen}");
    assert_eq!(
        screen["queries"]["checkpoints"].as_array().unwrap().len(),
        1,
        "{screen}"
    );

    // An MCP credential can't call the app's route.
    let resp = super::client()
        .post(format!("{}/v1/edit-groups/revert", env.url))
        .bearer_auth(&owner)
        .json(&json!({ "id": group }))
        .send()
        .await
        .unwrap();
    assert!(!resp.status().is_success());
    // The owner's device can.
    let resp = super::client()
        .post(format!("{}/v1/edit-groups/revert", env.url))
        .bearer_auth(&pod_code)
        .json(&json!({ "id": group }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{:?}", resp.text().await);
    assert!(!texts(&env, &owner, &id).await.contains("\"Tip\""));
}

#[tokio::test]
async fn a_trial_swaps_back_and_forth_and_is_kept() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
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
    let v = version(&env, &owner, &id).await;
    let group = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip") }] }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = ok(
        &env,
        &owner,
        "apply_edit_group",
        json!({ "id": group, "trial": true }),
    )
    .await;
    assert_eq!(out["state"], "trial", "{out}");
    let with_tip = texts(&env, &owner, &id).await;
    assert!(with_tip.contains("\"Tip\""));

    // Undo, redo, undo, redo from the app: the same two screens each time.
    async fn post(env: &Env, code: &str, path: &str, group: &str) {
        let r = super::client()
            .post(format!("{}{path}", env.url))
            .bearer_auth(code)
            .json(&json!({ "id": group }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{path}: {:?}", r.text().await);
    }
    post(&env, &pod_code, "/v1/edit-groups/revert", &group).await;
    let without = texts(&env, &owner, &id).await;
    assert!(!without.contains("\"Tip\""));
    post(&env, &pod_code, "/v1/edit-groups/apply", &group).await;
    assert_eq!(texts(&env, &owner, &id).await, with_tip);
    post(&env, &pod_code, "/v1/edit-groups/revert", &group).await;
    assert_eq!(texts(&env, &owner, &id).await, without);
    post(&env, &pod_code, "/v1/edit-groups/apply", &group).await;
    assert_eq!(texts(&env, &owner, &id).await, with_tip);

    // The redo kept it a trial; Keep (apply again) makes it the owner's.
    let (_, screen) = env.get("/v1/edit-groups/screen", &pod_code).await;
    let e = &screen["entities"][format!("edit_groups://{group}")];
    assert_eq!(e["can_keep"], true, "{screen}");
    post(&env, &pod_code, "/v1/edit-groups/apply", &group).await;
    let (_, screen) = env.get("/v1/edit-groups/screen", &pod_code).await;
    let e = &screen["entities"][format!("edit_groups://{group}")];
    assert_eq!(e["state"], "applied", "{screen}");
    assert_eq!(e["can_undo"], true, "{screen}");
}

/// Decision `instant-swap`: the manifest a swapping client is sent lists a
/// trial's screens before and after it, both served with the root; a client
/// that doesn't swap is sent no swaps.
#[tokio::test]
async fn the_manifest_lists_a_groups_screens_for_a_swapping_client() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let owner = env.connect_with(&pod_code, "outward").await;
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
    let v = version(&env, &owner, &id).await;
    let group = ok(
        &env,
        &owner,
        "stage_edit_group",
        json!({ "title": "Show a tip", "changes": [{ "id": id, "base_version": v,
                "json_patch": add_text("Tip") }] }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &env,
        &owner,
        "apply_edit_group",
        json!({ "id": group, "trial": true }),
    )
    .await;

    async fn manifest(env: &Env, code: &str, features: &str) -> Value {
        let get = |path: String| {
            super::client()
                .get(format!("{}{path}", env.url))
                .bearer_auth(code)
                .header(protocol::FEATURES_HEADER, features)
                .header(protocol::ROOT_HEADER, protocol::ROOT_MANIFEST)
        };
        let root: Value = get("/v1/construct".into())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = root["root"].as_str().unwrap().to_owned();
        get(format!("/v1/objects/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    let swapping = manifest(&env, &pod_code, &protocol::features_header()).await;
    let pair = &swapping["swaps"][&group][&id];
    let (before, after) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
    assert_ne!(before, after, "{swapping}");

    // Both are served with the root, in one fetch from it.
    let root_id = construct_elements::object_id(&swapping);
    let r: Value = super::client()
        .post(format!("{}/v1/objects", env.url))
        .bearer_auth(&pod_code)
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .json(&json!({ "have": [], "want": [root_id] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r["objects"].get(before).is_some(), "{r}");
    assert!(!r["objects"][before].to_string().contains("\"Tip\""), "{r}");
    assert!(r["objects"][after].to_string().contains("\"Tip\""), "{r}");

    let old: Vec<&str> = protocol::FEATURES
        .iter()
        .copied()
        .filter(|f| *f != protocol::feature::CONSTRUCT_SWAP)
        .collect();
    let plain = manifest(&env, &pod_code, &old.join(",")).await;
    assert!(plain.get("swaps").is_none(), "{plain}");
}
