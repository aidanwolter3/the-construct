//! MCP access levels (decision `mcp-access-levels`, issue #438), through the
//! real host: the level chosen on the consent page (or with the CLI) is kept
//! on the credential, each tool checks it before it runs, the owner changes
//! it in Settings' Connected apps, a client can't claim a level with the
//! host's header, and rows Claude writes below outward access count as
//! Claude's, not the owner's.

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{Env, client, env};

/// `true` when `out` is the refusal for too little access, needing `needs`.
fn too_low(out: &Value, needs: &str) -> bool {
    out["error"] == "access_too_low" && out["needs"] == needs
}

async fn set_access(env: &Env, bearer: &str, id: &str, access: &str) -> (StatusCode, Value) {
    let resp = client()
        .post(format!("{}/v1/connected-apps/access", env.url))
        .bearer_auth(bearer)
        .json(&json!({ "id": id, "access": access }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// The Connected apps entity of the only app connected to the pod.
async fn the_app(env: &Env, pod_code: &str) -> Value {
    let (status, screen) = env.get("/v1/connected-apps/screen", pod_code).await;
    assert_eq!(status, StatusCode::OK, "{screen}");
    let apps: Vec<&Value> = screen["entities"].as_object().unwrap().values().collect();
    assert_eq!(apps.len(), 1, "{screen}");
    apps[0].clone()
}

#[tokio::test]
async fn read_access_looks_only_until_the_owner_raises_it() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let key = env.connect_with(&pod_code, "read").await;

    // Claude is told its level.
    let (_, init) = env.rpc(Some(&key), "initialize", json!({})).await;
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("This connection's access: read."),
        "{instructions}"
    );

    // Reads work; a write is refused before it runs.
    let (err, out) = env.tool(&key, "list_layouts", json!({})).await;
    assert!(!err, "{out}");
    let (err, out) = env
        .tool(
            &key,
            "set_theme",
            json!({ "theme": { "primary": "#336699" } }),
        )
        .await;
    assert!(err && too_low(&out, "edit"), "{out}");
    assert_eq!(out["has"], "read");
    let (err, out) = env.tool(&key, "set_theme", json!({})).await;
    assert!(err && too_low(&out, "edit"), "{out}");

    // Settings shows the level; the MCP credential can't change it.
    let app = the_app(&env, &pod_code).await;
    assert_eq!(app["access"], "read");
    assert!(
        app["access_label"]
            .as_str()
            .unwrap()
            .starts_with("Read only.")
    );
    let id = app["id"].as_str().unwrap().to_owned();
    let (status, _) = set_access(&env, &key, &id, "outward").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, out) = set_access(&env, &pod_code, &id, "everything").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    let (status, out) = set_access(&env, &pod_code, "0123456789abcdef", "edit").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{out}");

    // The owner raises it: the next call has it.
    let (status, out) = set_access(&env, &pod_code, &id, "edit").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["access"], "edit");
    assert_eq!(the_app(&env, &pod_code).await["access"], "edit");
    let (err, out) = env
        .tool(
            &key,
            "set_theme",
            json!({ "theme": { "primary": "#336699" } }),
        )
        .await;
    assert!(!err, "{out}");
}

#[tokio::test]
async fn edit_access_is_refused_outward_tools_and_the_header_cant_raise_it() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    // The consent page's default is edit.
    let key = env.connect_with(&pod_code, "").await;
    assert_eq!(the_app(&env, &pod_code).await["access"], "edit");

    for (tool, args) in [
        ("share_publicly", json!({ "layout": "home" })),
        (
            "agents_schedules",
            json!({ "op": "create", "name": "nightly", "cron": "0 3 * * *",
                    "workspace": "test-repo", "prompt": "Tidy up" }),
        ),
        ("delete_collection", json!({ "collection": "dishes" })),
        (
            "define_collection",
            json!({ "name": "deals", "fields": [{ "name": "title", "type": "text" }],
                    "fillers": [{ "url": "https://shop.example/deals", "extract": "auto" }] }),
        ),
        (
            "apply_change",
            json!({ "collection": "dishes",
                    "changes": [{ "op": "set_filler", "filler": { "url": "https://shop.example/x" } }] }),
        ),
        ("mcp_call_tool", json!({ "server": "x", "tool": "y" })),
        ("a_tool_nobody_placed", json!({})),
    ] {
        let (err, out) = env.tool(&key, tool, args).await;
        assert!(err && too_low(&out, "outward"), "{tool}: {out}");
    }
    // An agents_schedules list is a read.
    let (_, out) = env
        .tool(&key, "agents_schedules", json!({ "op": "list" }))
        .await;
    assert!(out["error"] != "access_too_low", "{out}");

    // A client can't claim more with the host's own header.
    let resp = client()
        .post(format!("{}/v1/mcp", env.url))
        .bearer_auth(&key)
        .header("x-construct-mcp-access", "outward")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "share_publicly", "arguments": {"layout": "home"}}}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let out: Value = serde_json::from_str(text).unwrap();
    assert!(too_low(&out, "outward"), "{out}");

    // With outward access, share_publicly gets past the check (to its review).
    let id = the_app(&env, &pod_code).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _) = set_access(&env, &pod_code, &id, "outward").await;
    assert_eq!(status, StatusCode::OK);
    let (_, out) = env
        .tool(&key, "share_publicly", json!({ "layout": "home" }))
        .await;
    assert!(out["error"] != "access_too_low", "{out}");
}

#[tokio::test]
async fn rows_written_below_outward_are_claudes() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let edit = env.connect_with(&pod_code, "edit").await;
    let host = env.host.clone();
    let outward = tokio::task::spawn_blocking(move || {
        host.issue_mcp("mine", "Claude Code", pod_core::mcp::Access::Outward)
            .unwrap()
    })
    .await
    .unwrap();

    let (err, out) = env
        .tool(
            &edit,
            "add_row",
            json!({ "collection": "dishes", "values": { "title": "Test Lentil Soup" } }),
        )
        .await;
    assert!(!err, "{out}");
    let (err, out) = env
        .tool(
            &outward,
            "add_row",
            json!({ "collection": "dishes", "values": { "title": "Test Rice Bowl" } }),
        )
        .await;
    assert!(!err, "{out}");
    let (err, out) = env
        .tool(
            &edit,
            "edit_rows",
            json!({ "collection": "dishes",
                    "edits": [{ "op": "update", "id": "test-garden-salad", "values": { "category": "Sides" } }] }),
        )
        .await;
    assert!(!err, "{out}");
    let (err, out) = env
        .tool(
            &edit,
            "edit_rows",
            json!({ "collection": "dishes", "confirm": out["confirm"] }),
        )
        .await;
    assert!(!err, "{out}");

    // The export marks what the edit connection wrote as Claude's.
    let (err, export) = env
        .tool(
            &outward,
            "export_collection",
            json!({ "collection": "dishes" }),
        )
        .await;
    assert!(!err, "{export}");
    let row = |title: &str| {
        export["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r["values"]["title"] == title
                    || r["values"]["title"]["untrusted"]["content"] == title
            })
            .unwrap_or_else(|| panic!("{title} in {export}"))
            .clone()
    };
    let soup = row("Test Lentil Soup");
    assert_eq!(
        soup["values"]["title"]["untrusted"]["from"], "claude",
        "{soup}"
    );
    let bowl = row("Test Rice Bowl");
    assert_eq!(bowl["values"]["title"], "Test Rice Bowl", "{bowl}");
    let salad = row("Test Garden Salad");
    assert_eq!(salad["values"]["title"], "Test Garden Salad", "{salad}");
    assert_eq!(
        salad["values"]["category"]["untrusted"]["from"], "claude",
        "{salad}"
    );
}

#[tokio::test]
async fn the_cli_sets_the_access() {
    let env = env().await;
    env.pod("mine").await;
    let host = env.host.clone();
    let key = tokio::task::spawn_blocking(move || {
        host.issue_mcp("mine", "Claude Code", pod_core::mcp::Access::Read)
            .unwrap()
    })
    .await
    .unwrap();
    let (err, out) = env
        .tool(
            &key,
            "add_row",
            json!({ "collection": "dishes", "values": { "title": "Test Toast" } }),
        )
        .await;
    assert!(err && too_low(&out, "edit"), "{out}");
}

/// Every tool's level with no arguments (decision `mcp-access-levels`):
/// placing a new tool is a choice, so this table changes with it. A tool
/// nobody placed needs outward. (Arguments raise some: a filler or a file
/// to fetch, a schedule's op; the other tests cover those.)
#[tokio::test]
async fn every_tool_has_the_level_the_decision_gives_it() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let key = env.connect(&pod_code).await;
    let (_, list) = env.rpc(Some(&key), "tools/list", json!({})).await;
    let dir = tempfile::tempdir().unwrap();
    let pod: pod_core::Pod = std::sync::Arc::new(server::open_dir(dir.path()).unwrap());
    let mut levels = std::collections::BTreeMap::<&str, Vec<String>>::new();
    for t in list["result"]["tools"].as_array().unwrap() {
        let name = t["name"].as_str().unwrap().to_owned();
        let needs = pod_core::mcp::tool_needs(&pod, &name, &json!({}));
        levels.entry(needs.id()).or_default().push(name);
    }
    for names in levels.values_mut() {
        names.sort();
    }
    let want = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        levels["read"],
        want(&[
            "agents_schedules",
            "collections_status",
            "describe_collection",
            "describe_publication",
            "describe_vocabulary",
            "expand",
            "export_collection",
            "find_elements",
            "get_component",
            "get_layout",
            "guide",
            "list_adoptions",
            "list_collections",
            "list_components",
            "list_data_sources",
            "list_devices",
            "list_jacks",
            "list_layouts",
            "list_parts",
            "list_publications",
            "mcp_servers",
            "note_gap",
            "render_layout_preview",
            "sample_data",
            "validate_layout",
        ]),
        "{levels:?}"
    );
    assert_eq!(
        levels["outward"],
        want(&[
            "agents_link_tasks",
            "collections_preview",
            "collections_set_secret",
            "delete_collection",
            "delete_component",
            "delete_layout",
            "mcp_call_tool",
            "mcp_connect",
            "plug_jack",
            "publish",
            "set_handle",
            "watch_device",
        ]),
        "{levels:?}"
    );
    assert_eq!(
        levels["edit"],
        want(&[
            "add_row",
            "adopt",
            "apply_change",
            "collections_refresh",
            "copy_component",
            "create_layout",
            "define_collection",
            "define_component",
            "dispatch_virtual_interaction",
            "edit_rows",
            "mcp_disconnect",
            "rename_device",
            "rename_layout",
            "restore_collection",
            "restore_collection_version",
            "restore_component",
            "restore_version",
            "revoke_publication",
            "run_action",
            "set_theme",
            "set_time_zone",
            "undo_edit",
            "unplug_jack",
            "update_adoption",
            "update_component",
            "update_layout",
            "use_kit",
        ]),
        "{levels:?}"
    );
    // Public links and calendar feeds (listed when the host serves them).
    for (tool, level) in [
        ("share_publicly", "outward"),
        ("set_calendar_feed", "outward"),
        ("rotate_calendar_feed", "outward"),
        ("unshare_publicly", "edit"),
        ("revoke_calendar_feed", "edit"),
        ("list_public_links", "read"),
        ("list_calendar_feeds", "read"),
    ] {
        assert_eq!(
            pod_core::mcp::tool_needs(&pod, tool, &json!({})).id(),
            level,
            "{tool}"
        );
    }
}
