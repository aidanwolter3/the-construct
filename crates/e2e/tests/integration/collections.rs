//! A todo app on Collections (decision `collections`, issue #54) through the client
//! engine, as the shared UI drives it (it binds `$form`, `$nonce` and `$item` before
//! dispatching): Claude defines `todos` and the app over MCP; one engine adds and
//! ticks todos through the outbox and a second one sees them live; a todo added
//! offline under the old field names still arrives after Claude renames `due` and
//! adds a priority in one change; a delete hides the row everywhere.

use std::sync::Arc;

use client_core::MemoryStore;
use e2e::{Harness, TIMEOUT, entities, queries, wait_flushed, wait_until};
use serde_json::{Value, json};

const POD_TOKEN: &str = "e2e-collections-pod-token";

fn todo_app() -> Value {
    json!({"type": "core://Workspace", "id": "x", "title": "Todos", "slots": [
        {"slot_id": "list", "provider_uri": "core://custom", "content": {
            "type": "core://List", "query_key": "collection:todos",
            "where": {"field": "deleted", "equals": "false"},
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title $item.due"}}}]})
}

/// The `action.mutation.create` of the app's add field, bound as the UI would.
fn add_intent(nonce: &str, fields: Value) -> String {
    let mut entity = json!({"id": format!("pending-{nonce}"), "done": false, "deleted": false});
    let mut payload = json!({"client_id": nonce});
    for (k, v) in fields.as_object().unwrap() {
        entity[k] = v.clone();
        payload[k] = v.clone();
    }
    json!({"intent": "action.mutation.create", "query_key": "collection:todos",
        "entity_uri": format!("collections://todos/pending-{nonce}"), "entity": entity,
        "endpoint": "/v1/collections/todos/create", "payload": payload,
        "refresh_workspace": true})
    .to_string()
}

fn mutate(uri: &str, id: &str, field: &str, expr: &str, endpoint: &str) -> String {
    let mut payload = json!({"id": id});
    if field != "deleted" {
        payload[field] = json!("$value");
    }
    json!({"intent": "action.mutation.optimistic", "entity_uri": uri, "field": field,
        "value_expr": expr, "rollback_value": false,
        "endpoint": format!("/v1/collections/todos/{endpoint}"), "payload": payload})
    .to_string()
}

/// `title -> entity` of the rows an engine lists.
fn rows(engine: &client_core::ConstructEngine) -> Vec<(String, Value)> {
    let snap = engine.snapshot();
    let ents = entities(&snap);
    queries(&snap)["collection:todos"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|u| {
                    let e = ents[u.as_str().unwrap()].clone();
                    (e["title"].as_str().unwrap_or("").to_owned(), e)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_todo_app_syncs_live_and_survives_a_change_made_while_offline() {
    let h = Harness::fake();
    h.pod_state
        .store()
        .create_pinned_session(&server::session::session_hash(POD_TOKEN))
        .unwrap();
    // Claude, over MCP (straight to the pod: the engines' path goes offline below).
    let mcp = |name: &str, args: Value| -> Value {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": name, "arguments": args}});
        let pod = h.pod_state.clone();
        let text = h.block_on(async move {
            let app = server::app(pod);
            let req = axum::http::Request::post("/v1/mcp")
                .header("authorization", format!("Bearer {POD_TOKEN}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap();
            let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            String::from_utf8(bytes.to_vec()).unwrap()
        });
        let v: Value = serde_json::from_str(&text).unwrap();
        let r = &v["result"];
        assert_eq!(r["isError"], false, "{name}: {r}");
        serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    mcp(
        "define_collection",
        json!({"name": "todos", "fields": [{"name": "title", "type": "text"},
            {"name": "done", "type": "bool", "default": false},
            {"name": "due", "type": "date", "optional": true}]}),
    );
    let app = mcp(
        "create_layout",
        json!({"title": "Todos", "feeds": [["collections", "todos"]], "layout": todo_app()}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Two devices on the app.
    let phone = h.engine_on(&Arc::new(MemoryStore::new()), None, &app);
    let laptop = h.engine_on(&Arc::new(MemoryStore::new()), None, &app);
    for e in [&phone, &laptop] {
        wait_until("the app", TIMEOUT, || {
            e.snapshot().workspace_json.contains("Todos")
        });
    }
    let events = format!("/v1/workspaces/{app}/events");
    wait_until("both event streams", TIMEOUT, || {
        h.hits().iter().filter(|x| x.path == events).count() >= 2
    });

    // Add a todo on the phone: shown at once, then on the laptop.
    phone.dispatch(&add_intent("n1", json!({"title": "Milk"})));
    assert_eq!(
        rows(&phone).first().map(|(t, _)| t.as_str()),
        Some("Milk"),
        "optimistic"
    );
    wait_flushed(&phone, TIMEOUT);
    wait_until("Milk on the laptop", TIMEOUT, || {
        rows(&laptop).iter().any(|(t, _)| t == "Milk")
    });

    // Tick it on the laptop: the phone sees it done.
    let milk = rows(&laptop)[0].1.clone();
    let (id, uri) = (
        milk["id"].as_str().unwrap().to_owned(),
        format!("collections://todos/{}", milk["id"].as_str().unwrap()),
    );
    laptop.dispatch(&mutate(&uri, &id, "done", "!entity.done", "update"));
    wait_flushed(&laptop, TIMEOUT);
    wait_until("ticked on the phone", TIMEOUT, || {
        rows(&phone).first().is_some_and(|(_, e)| e["done"] == true)
    });

    // Offline, the phone adds a todo with a due date (the old field name)...
    h.set_pod_reachable(false);
    phone.dispatch(&add_intent(
        "n2",
        json!({"title": "Eggs", "due": "2026-11-06"}),
    ));
    assert!(rows(&phone).iter().any(|(t, _)| t == "Eggs"));
    // ...while Claude renames `due` and adds a priority, with the app, in one change.
    let changed = mcp(
        "apply_change",
        json!({"collection": "todos", "changes": [
            {"op": "add_field", "name": "priority", "type": "choice", "options": ["low", "high"], "default": "low"},
            {"op": "rename_field", "field": "due", "to": "deadline"}],
            "apps": [{"app": app, "json_patch": [{"op": "replace",
                "path": "/slots/0/content/item_template/value_expr",
                "value": "$item.title $item.deadline ($item.priority)"}]}]}),
    );
    assert_eq!(changed["apps"][0]["version"], 2, "{changed}");
    h.set_pod_reachable(true);
    wait_flushed(&phone, TIMEOUT);
    wait_until("Eggs on the laptop, under the new name", TIMEOUT, || {
        rows(&laptop)
            .iter()
            .any(|(t, e)| t == "Eggs" && e["deadline"] == "2026-11-06" && e["priority"] == "low")
    });
    wait_until("the new layout", TIMEOUT, || {
        laptop.snapshot().workspace_json.contains("$item.deadline")
    });

    // Delete Milk on the phone: gone from both lists.
    phone.dispatch(&mutate(&uri, &id, "deleted", "true", "delete"));
    wait_flushed(&phone, TIMEOUT);
    wait_until("Milk gone on the laptop", TIMEOUT, || {
        let r = rows(&laptop);
        r.len() == 1 && r[0].0 == "Eggs"
    });
    assert_eq!(h.posts_to("/v1/collections/todos/create"), [200, 200]);
}
