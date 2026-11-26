//! Who wrote a row's values (decision `mcp-access-levels`): Claude below
//! outward access brings a row back (a restore, an undo, a restored
//! collection) as Claude's, even when its values are the owner's, so a row
//! the owner had put away can't come back counting as theirs (and start an
//! agent run).

use std::sync::Arc;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use source_collections::{Caps, CollectionsAdapter};
use tower::ServiceExt;

fn pod() -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default());
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.set_hosted();
    pod
}

/// `tools/call` as the owner (the pod token) or, with `access`, as an MCP
/// credential the host forwards at that level.
async fn tool(pod: &Pod, access: Option<&str>, name: &str, args: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let mut req = Request::post("/v1/mcp").header("content-type", "application/json");
    req = match access {
        Some(a) => req
            .header(pod_core::CREDENTIAL_HEADER, "mcp")
            .header(pod_core::mcp::ACCESS_HEADER, a),
        None => req.header("authorization", "Bearer pod"),
    };
    let resp = pod_core::app(pod.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let r = &v["result"];
    let out: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(r["isError"] != true, "{name}: {out}");
    out
}

/// `edit_rows`, confirmed when it asks.
async fn edit(pod: &Pod, access: Option<&str>, edits: Value) -> Value {
    let out = tool(
        pod,
        access,
        "edit_rows",
        json!({"collection": "tasks", "edits": edits}),
    )
    .await;
    if out["needs_confirmation"] == true {
        tool(
            pod,
            access,
            "edit_rows",
            json!({"collection": "tasks", "confirm": out["confirm"]}),
        )
        .await
    } else {
        out
    }
}

/// The fields of row `id` whose last writer is Claude, by name.
fn claudes(pod: &Pod, collection: &str, id: &str) -> Vec<String> {
    pod.store()
        .with_conn(|c| {
            let (_, def) = source_collections::db::latest_def(c, collection)?.unwrap();
            let w = source_collections::db::row_writers(c, collection, id)?;
            Ok::<_, rusqlite::Error>(
                def.fields
                    .iter()
                    .filter(|f| {
                        w.get(&f.id).map(String::as_str)
                            == Some(source_collections::db::WRITER_CLAUDE)
                    })
                    .map(|f| f.name.clone())
                    .collect(),
            )
        })
        .unwrap()
}

async fn define(pod: &Pod) {
    tool(
        pod,
        None,
        "define_collection",
        json!({"name": "tasks", "fields": [
            {"name": "title", "type": "text"},
            {"name": "status", "type": "choice", "options": ["todo", "queued", "done"], "default": "todo"}]}),
    )
    .await;
}

#[tokio::test]
async fn a_row_claude_restores_or_undoes_back_is_claudes() {
    let pod = pod();
    define(&pod).await;
    let made = edit(
        &pod,
        None,
        json!([{"op": "create", "values": {"title": "Fix the header", "status": "queued"}}]),
    )
    .await;
    let id = made["created"][0].as_str().unwrap().to_owned();
    assert!(claudes(&pod, "tasks", &id).is_empty());

    // The owner deletes it; Claude (edit access) restores it: Claude's.
    edit(&pod, None, json!([{"op": "delete", "id": id}])).await;
    edit(&pod, Some("edit"), json!([{"op": "restore", "id": id}])).await;
    assert_eq!(claudes(&pod, "tasks", &id), ["title", "status"]);

    // The owner marks it done (theirs again); Claude undoes that: Claude's.
    edit(
        &pod,
        None,
        json!([{"op": "update", "id": id, "values": {"title": "Fix the header!", "status": "done"}}]),
    )
    .await;
    assert!(claudes(&pod, "tasks", &id).is_empty());
    let done = edit(
        &pod,
        None,
        json!([{"op": "update", "id": id, "values": {"status": "todo"}}]),
    )
    .await;
    tool(&pod, None, "undo_edit", json!({"edit": done["edit"]})).await;
    assert!(claudes(&pod, "tasks", &id).is_empty(), "the owner's undo");
    let again = edit(
        &pod,
        None,
        json!([{"op": "update", "id": id, "values": {"status": "todo"}}]),
    )
    .await;
    tool(
        &pod,
        Some("edit"),
        "undo_edit",
        json!({"edit": again["edit"]}),
    )
    .await;
    assert_eq!(claudes(&pod, "tasks", &id), ["title", "status"]);

    // With outward access Claude's writes are the owner's, as before.
    edit(
        &pod,
        Some("outward"),
        json!([{"op": "update", "id": id, "values": {"title": "Fix the header", "status": "queued"}}]),
    )
    .await;
    assert!(claudes(&pod, "tasks", &id).is_empty());

    // A collection the owner deleted, restored by Claude: every row Claude's.
    tool(
        &pod,
        None,
        "delete_collection",
        json!({"collection": "tasks"}),
    )
    .await;
    tool(
        &pod,
        Some("edit"),
        "restore_collection",
        json!({"collection": "tasks"}),
    )
    .await;
    assert_eq!(claudes(&pod, "tasks", &id), ["title", "status"]);
}
