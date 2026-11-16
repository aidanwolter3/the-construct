//! Sharing between two pods of one server (decision `sharing`), through the
//! real host and MCP: one pod publishes a layout over its collection (the
//! collection becomes an input, a literal row reference is refused), a
//! second pod can't read the first's objects by hash, adopts the
//! publication bound to its own collection with a field mapping, draws it
//! with its own data, follows a new version, and keeps drawing it after
//! the owner revokes it.

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{Env, client, env};

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

fn encode(v: &Value) -> String {
    v.to_string()
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn ok(env: &Env, access: &str, name: &str, args: Value) -> Value {
    let (err, v) = env.tool(access, name, args).await;
    assert!(!err, "{name}: {v}");
    v
}

async fn refused(env: &Env, access: &str, name: &str, args: Value) -> Value {
    let (err, v) = env.tool(access, name, args).await;
    assert!(err, "{name} should be refused: {v}");
    v
}

/// A chores list: each row's title and a switch writing `done`.
fn chores(spacing: i64) -> Value {
    json!({"type": "core://Page", "title": "Chores", "root": {"type": "core://Column",
        "spacing_dp": spacing, "children": [
        {"type": "core://List", "query_key": "collection:chores", "item_template":
            {"type": "core://Row", "children": [
                {"type": "core://Text", "style": "Body", "value_expr": "$item.title"},
                {"type": "core://IconButton", "icon": "check", "content_description": "Done",
                    "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                        "field": "done", "value_expr": "!entity.done",
                        "endpoint": "/v1/collections/chores/update",
                        "payload": {"id": "$item.id", "done": "$value"}}}]}}]}})
}

/// The objects a pod's construct reaches, by hash.
async fn objects(env: &Env, pod: &str) -> serde_json::Map<String, Value> {
    let (s, root) = get(env, "/v1/construct", pod).await;
    assert_eq!(s, StatusCode::OK, "{root}");
    let (s, all) = post(
        env,
        "/v1/objects",
        pod,
        json!({"have": [], "want": [root["root"]]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{all}");
    all["objects"].as_object().unwrap().clone()
}

#[tokio::test]
async fn a_second_pod_adopts_a_published_layout_and_keeps_it_after_a_revoke() {
    let env = env().await;
    let (ann, ben, cam) = (
        env.pod("ann").await,
        env.pod("ben").await,
        env.pod("cam").await,
    );
    let (a, b, c) = (
        env.connect(&ann).await,
        env.connect(&ben).await,
        env.connect(&cam).await,
    );

    // Ann's chores, Ben's tasks: the same shape, a field named differently.
    ok(
        &env,
        &a,
        "define_collection",
        json!({"name": "chores", "fields": [
        {"name": "title", "type": "text"}, {"name": "done", "type": "bool", "optional": true}]}),
    )
    .await;
    ok(
        &env,
        &a,
        "add_row",
        json!({"collection": "chores", "values": {"title": "Water the ferns"}}),
    )
    .await;
    ok(
        &env,
        &b,
        "define_collection",
        json!({"name": "tasks", "fields": [
        {"name": "name", "type": "text"}, {"name": "done", "type": "bool", "optional": true}]}),
    )
    .await;
    ok(
        &env,
        &b,
        "add_row",
        json!({"collection": "tasks", "values": {"name": "Fix the gate"}}),
    )
    .await;

    // A third pod sees nothing of the first's until it has a link.
    let e = refused(
        &env,
        &c,
        "describe_publication",
        json!({"link": "share-nothing"}),
    )
    .await;
    assert_eq!(e["error"], "unknown_link", "{e}");

    let list = ok(
        &env,
        &a,
        "create_layout",
        json!({"title": "Chores",
        "feeds": [["collections", "chores"]], "layout": chores(8)}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &env,
        &a,
        "update_layout",
        json!({"app": "home", "layout": {"type": "core://Page",
        "title": "Home", "root": {"type": "core://Column", "children": [
            {"type": "core://Layout", "ref": list}]}}}),
    )
    .await;

    // Publishing needs a handle, unique on the server.
    let e = refused(&env, &a, "publish", json!({"name": list})).await;
    assert_eq!(e["error"], "no_handle", "{e}");
    ok(&env, &a, "set_handle", json!({"handle": "ann"})).await;
    let e = refused(&env, &b, "set_handle", json!({"handle": "ann"})).await;
    assert_eq!(e["error"], "handle_taken", "{e}");
    assert_eq!(
        ok(&env, &a, "set_handle", json!({})).await["handle"],
        "@ann"
    );

    // Listed sharing waits for moderation.
    let e = refused(
        &env,
        &a,
        "publish",
        json!({"name": list, "visibility": "public"}),
    )
    .await;
    assert_eq!(e["error"], "visibility_not_available", "{e}");

    // A literal reference to one row is refused at its pointer.
    let row = ok(
        &env,
        &a,
        "create_layout",
        json!({"title": "First chore",
        "feeds": [["collections", "chores"]],
        "layout": {"type": "core://Page", "title": "First chore", "root": {"type": "core://Text",
            "style": "Body", "value_expr": "${collections://chores/1#title}"}}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let e = refused(&env, &a, "publish", json!({"name": row})).await;
    assert_eq!(e["error"], "literal_data_reference", "{e}");
    assert_eq!(e["at"], "/node/root/value_expr", "{e}");

    // The review: nothing shared yet, the collection is an input with the
    // fields it reads and writes, and the literal text is flagged.
    let review = ok(
        &env,
        &a,
        "publish",
        json!({"name": list, "keep_shared": true}),
    )
    .await;
    assert_eq!(review["published"], false, "{review}");
    assert_eq!(review["name"], "@ann/chores", "{review}");
    let inputs = review["interface"]["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 1, "{review}");
    assert_eq!(inputs[0]["name"], "chores");
    assert_eq!(inputs[0]["schema"]["tool"], "define_collection");
    let fields: Vec<(String, bool, bool)> = inputs[0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap().to_owned(),
                f["read"] == true,
                f["written"] == true,
            )
        })
        .collect();
    assert_eq!(
        fields,
        vec![("done".into(), true, true), ("title".into(), true, false)]
    );
    assert!(
        review["flags"].to_string().contains("Chores"),
        "the title is flagged: {review}"
    );
    let flat = review.to_string();
    assert!(!flat.contains("Water the ferns"), "no data: {review}");
    let published = ok(
        &env,
        &a,
        "publish",
        json!({"name": list, "keep_shared": true,
        "confirm": review["review_id"]}),
    )
    .await;
    assert_eq!(published["published"], true, "{published}");
    let link = published["link"].as_str().unwrap().to_owned();
    assert!(link.starts_with("share-"), "{published}");

    // Ann's objects stay hers: Ben can't fetch them by hash.
    let ann_objects = objects(&env, &ann).await;
    for h in ann_objects.keys() {
        let (s, _) = get(&env, &format!("/v1/objects/{h}"), &ben).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{h}");
    }
    let e = refused(
        &env,
        &b,
        "describe_publication",
        json!({"link": "share-nothing"}),
    )
    .await;
    assert_eq!(e["error"], "unknown_link", "{e}");
    let d = ok(&env, &b, "describe_publication", json!({"link": link})).await;
    assert_eq!(d["interface"]["inputs"][0]["name"], "chores", "{d}");
    assert!(d.get("publishes").is_none(), "{d}");

    // Unbound: a proposal, nothing adopted.
    let e = refused(&env, &b, "adopt", json!({"link": link})).await;
    assert_eq!(e["error"], "not_ready", "{e}");
    assert_eq!(e["proposed"]["chores"]["collection"], "tasks", "{e}");
    // The field Ben calls something else must be mapped.
    let e = refused(
        &env,
        &b,
        "adopt",
        json!({"link": link,
        "bindings": {"chores": {"collection": "tasks"}}}),
    )
    .await;
    assert_eq!(e["problems"][0]["problem"], "missing_field", "{e}");
    let adopted = ok(
        &env,
        &b,
        "adopt",
        json!({"link": link,
        "bindings": {"chores": {"collection": "tasks", "fields": {"title": "name"}}}}),
    )
    .await;
    assert_eq!(adopted["name"], "@ann/chores", "{adopted}");
    assert_eq!(adopted["bind"]["chores"], "tasks", "{adopted}");
    assert_eq!(adopted["bind"]["chores__title"], "name", "{adopted}");
    let el = adopted["el"].as_str().unwrap().to_owned();
    ok(
        &env,
        &b,
        "update_layout",
        json!({"app": "home", "layout": {"type": "core://Page",
        "title": "Home", "root": {"type": "core://Column", "children": [adopted["use"].clone()]}}}),
    )
    .await;

    // Ben's construct reaches the shared element, unchanged (its hash),
    // and it draws Ben's rows.
    let ben_objects = objects(&env, &ben).await;
    assert!(ben_objects.contains_key(&el), "{ben_objects:?}");
    let data = |bind: Value| {
        let path = format!("/v1/construct/data/{el}?bind={}", encode(&bind));
        let (env, ben) = (&env, &ben);
        async move { get(env, &path, ben).await }
    };
    let (s, mine) = data(adopted["bind"].clone()).await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert!(mine.to_string().contains("Fix the gate"), "{mine}");
    assert!(!mine.to_string().contains("Water the ferns"), "{mine}");

    // A new version without new text goes out by itself (keep_shared), and
    // Ben's follow takes it.
    ok(
        &env,
        &a,
        "update_layout",
        json!({"app": list, "layout": chores(12)}),
    )
    .await;
    let pubs = ok(&env, &a, "list_publications", json!({})).await;
    assert_eq!(pubs["publications"][0]["version"], 2, "{pubs}");
    let listed = ok(&env, &b, "list_adoptions", json!({})).await;
    assert_eq!(listed["adoptions"][0]["up_to_date"], false, "{listed}");
    let up = ok(&env, &b, "update_adoption", json!({"name": "@ann/chores"})).await;
    assert_eq!(up["version"], 2, "{up}");
    let el = up["el"].as_str().unwrap().to_owned();
    assert!(objects(&env, &ben).await.contains_key(&el));

    // Revoked: the link stops working; Ben keeps drawing his copy.
    ok(
        &env,
        &a,
        "revoke_publication",
        json!({"name": "@ann/chores"}),
    )
    .await;
    let e = refused(&env, &b, "describe_publication", json!({"link": link})).await;
    assert_eq!(e["error"], "revoked", "{e}");
    let listed = ok(&env, &b, "list_adoptions", json!({})).await;
    assert_eq!(listed["adoptions"][0]["revoked"], true, "{listed}");
    let up = ok(&env, &b, "update_adoption", json!({"name": "@ann/chores"})).await;
    assert_eq!(up["revoked"], true, "{up}");
    let after = objects(&env, &ben).await;
    assert!(after.contains_key(&el), "{after:?}");
    let (s, still) = get(&env, &format!("/v1/objects/{el}"), &ben).await;
    assert_eq!(s, StatusCode::OK, "{still}");
    let (s, mine) = data(adopted["bind"].clone()).await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert!(mine.to_string().contains("Fix the gate"), "{mine}");
}
