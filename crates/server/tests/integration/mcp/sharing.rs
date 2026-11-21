//! Sharing between two pods of one server (decision `sharing`), through the
//! real host and MCP: one pod publishes a layout over its collection (the
//! collection becomes an input, a literal row reference is refused), a
//! second pod can't read the first's objects by hash, adopts the
//! publication bound to its own collection with a field mapping, draws it
//! with its own data, follows a new version, and keeps drawing it after
//! the owner revokes it. URLs that would send the adopter's data elsewhere
//! wait for the adopter's consent.

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
    // Another pod's publication is its publisher's text (issue #439).
    let theirs = &d["interface"]["untrusted"];
    assert_eq!(theirs["from"], d["name"], "{d}");
    assert_eq!(theirs["content"]["inputs"][0]["name"], "chores", "{d}");
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

/// A list of chores with each row's picture from `url`.
fn pictured(url: &str) -> Value {
    json!({"type": "core://List", "query_key": "collection:chores", "item_template":
        {"type": "core://Row", "children": [
            {"type": "core://Image", "url": url, "size_dp": 24},
            {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}]}})
}

/// An adopted publication whose URLs would send the adopter's rows to
/// another server is refused until the adopter accepts each URL, and a
/// new version bringing another waits for it too (decision
/// `adopt-shows-urls`).
#[tokio::test]
async fn urls_that_send_the_adopters_data_wait_for_consent() {
    let env = env().await;
    let (ann, ben) = (env.pod("ann").await, env.pod("ben").await);
    let (a, b) = (env.connect(&ann).await, env.connect(&ben).await);
    for (pod, row) in [(&a, "Water the ferns"), (&b, "Fix the gate")] {
        ok(
            &env,
            pod,
            "define_collection",
            json!({"name": "chores", "fields": [{"name": "title", "type": "text"}]}),
        )
        .await;
        ok(
            &env,
            pod,
            "add_row",
            json!({"collection": "chores", "values": {"title": row}}),
        )
        .await;
    }
    ok(&env, &a, "set_handle", json!({"handle": "ann"})).await;
    let first = "https://a.example/p.png?d=$item.title";
    let list = ok(
        &env,
        &a,
        "create_layout",
        json!({"title": "Chores", "feeds": [["collections", "chores"]],
            "layout": {"type": "core://Page", "title": "Chores", "root": pictured(first)}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let link = publish(&env, &a, &list).await["link"]
        .as_str()
        .unwrap()
        .to_owned();
    let bindings = json!({"chores": {"collection": "chores"}});

    // The review shows the URL; adopting without accepting it is refused.
    let e = refused(
        &env,
        &b,
        "adopt",
        json!({"link": link, "bindings": bindings, "dry_run": true}),
    )
    .await;
    // (As published: the field it reads is an input the adopter maps.)
    assert_eq!(e["error"], "sends_data", "{e}");
    let urls = e["data_urls"].clone();
    assert_eq!(urls.as_array().map(Vec::len), Some(1), "{e}");
    assert!(
        urls[0]
            .as_str()
            .unwrap()
            .starts_with("https://a.example/p.png?d=$item.")
    );
    assert!(e["flags"].to_string().contains("a.example"), "{e}");
    let e = refused(
        &env,
        &b,
        "adopt",
        json!({"link": link, "bindings": bindings}),
    )
    .await;
    assert_eq!(e["error"], "sends_data", "{e}");
    assert!(
        ok(&env, &b, "list_adoptions", json!({})).await["adoptions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let adopted = ok(
        &env,
        &b,
        "adopt",
        json!({"link": link, "bindings": bindings, "accept_data_urls": urls}),
    )
    .await;
    assert_eq!(adopted["version"], 1, "{adopted}");
    assert!(
        adopted["flags"].to_string().contains("a.example"),
        "{adopted}"
    );

    // Version 2 adds another such URL and moves the first: consent is
    // kept per URL, element, place and binding, so following it asks for
    // both (decision `adoption-capabilities`).
    let second = "https://b.example/q.png?d=$item.title";
    ok(
        &env,
        &a,
        "update_layout",
        json!({"app": list, "layout": {"type": "core://Page", "title": "Chores",
            "root": {"type": "core://Column", "children": [pictured(first), pictured(second)]}}}),
    )
    .await;
    assert_eq!(publish(&env, &a, &list).await["version"], 2);
    let e = refused(&env, &b, "update_adoption", json!({"name": "@ann/chores"})).await;
    assert_eq!(e["error"], "sends_data", "{e}");
    let urls = e["data_urls"].clone();
    assert_eq!(urls.as_array().map(Vec::len), Some(2), "{e}");
    assert!(e.to_string().contains("https://b.example/"), "{e}");
    let up = ok(
        &env,
        &b,
        "update_adoption",
        json!({"name": "@ann/chores", "accept_data_urls": urls}),
    )
    .await;
    assert_eq!(up["version"], 2, "{up}");
}

/// Publishes layout `id` (the review, then confirm) -> the answer, or the
/// refusal.
async fn publish(env: &Env, access: &str, id: &str) -> Value {
    let (err, review) = env.tool(access, "publish", json!({"name": id})).await;
    if err {
        return review;
    }
    ok(
        env,
        access,
        "publish",
        json!({"name": id, "confirm": review["review_id"]}),
    )
    .await
}

/// A layout `title` drawing `root`, created -> its id.
async fn layout(env: &Env, access: &str, title: &str, root: Value) -> String {
    ok(
        env,
        access,
        "create_layout",
        json!({"title": title, "layout": {"type": "core://Page", "title": title, "root": root}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A collection's field defaults and options travel in the suggested schema:
/// the owner sees them in the review.
#[tokio::test]
async fn schema_strings_are_in_the_publish_review() {
    let env = env().await;
    let ann = env.pod("ann").await;
    let a = env.connect(&ann).await;
    ok(
        &env,
        &a,
        "define_collection",
        json!({"name": "chores", "fields": [
        {"name": "title", "type": "text"},
        {"name": "who", "type": "choice", "options": ["Sasha Example", "Rex Example"],
         "default": "Sasha Example"},
        {"name": "done", "type": "bool", "optional": true}]}),
    )
    .await;
    ok(&env, &a, "set_handle", json!({"handle": "ann"})).await;
    let id = ok(
        &env,
        &a,
        "create_layout",
        json!({"title": "Chores", "feeds": [["collections", "chores"]],
        "layout": chores(8)}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let review = ok(&env, &a, "publish", json!({"name": id})).await;
    let flags = review["flags"].to_string();
    assert!(
        flags.contains("Rex Example"),
        "options are flagged: {review}"
    );
    assert!(flags.contains("\"schema\""), "{review}");
}

/// The shelf is the host's, outside every pod's storage quota: a pod keeps
/// at most 20 live publications, each of at most 1 MiB of elements.
#[tokio::test]
async fn a_pod_keeps_only_so_much_on_the_shelf() {
    let env = env().await;
    let ann = env.pod("ann").await;
    let a = env.connect(&ann).await;
    ok(&env, &a, "set_handle", json!({"handle": "ann"})).await;
    let mut ids = Vec::new();
    for i in 0..21 {
        let t = format!("Note {i}");
        let root = json!({"type": "core://Text", "style": "Body", "value_expr": t});
        ids.push(layout(&env, &a, &t, root).await);
    }
    let mut links = Vec::new();
    for id in &ids[..20] {
        let p = publish(&env, &a, id).await;
        assert_eq!(p["published"], true, "{p}");
        links.push(p["link"].as_str().unwrap().to_owned());
    }
    let e = publish(&env, &a, &ids[20]).await;
    assert_eq!(e["error"], "too_many_publications", "{e}");
    // A new version of a live one still goes; revoking one makes room.
    let p = publish(&env, &a, &ids[0]).await;
    assert_eq!(p["published"], true, "{p}");
    ok(&env, &a, "revoke_publication", json!({"name": links[0]})).await;
    let p = publish(&env, &a, &ids[20]).await;
    assert_eq!(p["published"], true, "{p}");

    // Twenty components with 60 KiB of doc each (the doc isn't drawn, so the
    // layout's expansion stays small): more than one publication carries.
    ok(&env, &a, "revoke_publication", json!({"name": links[1]})).await;
    let mut uses = Vec::new();
    for i in 0..20 {
        ok(
            &env,
            &a,
            "define_component",
            json!({"name": format!("long{i}"), "component": {
                "doc": format!("{i}{}", "x".repeat(60 << 10)),
                "examples": [{"args": {}, "note": "a word"}],
                "template": {"type": "core://Text", "style": "Body", "value_expr": "Hi"}}}),
        )
        .await;
        uses.push(json!({"type": "core://Use", "ref": format!("@me/long{i}")}));
    }
    let long = layout(
        &env,
        &a,
        "Long",
        json!({"type": "core://Column", "children": uses}),
    )
    .await;
    let e = publish(&env, &a, &long).await;
    assert_eq!(e["error"], "too_large", "{e}");
}

/// An adopted element reaches only what it is bound to (decision
/// `adoption-capabilities`): a button starting an agent run waits for the
/// adopter's acceptance, a list naming a collection the adoption doesn't
/// bind is refused, every key of a collection becomes an input, a URL
/// hidden with `${}` is refused when written, and an adopted element's
/// data is only its bound collections', whatever a request binds.
#[tokio::test]
async fn adopted_elements_reach_only_what_they_are_bound_to() {
    let env = env().await;
    let (ann, ben) = (env.pod("ann").await, env.pod("ben").await);
    let (a, b) = (env.connect(&ann).await, env.connect(&ben).await);
    for (pod, name, row) in [
        (&a, "chores", "Water the ferns"),
        (&a, "secret", "Ann's code"),
        (&b, "chores", "Fix the gate"),
        (&b, "secret", "Ben's code 1234"),
    ] {
        ok(
            &env,
            pod,
            "define_collection",
            json!({"name": name, "fields": [{"name": "title", "type": "text"}]}),
        )
        .await;
        ok(
            &env,
            pod,
            "add_row",
            json!({"collection": name, "values": {"title": row}}),
        )
        .await;
    }
    ok(&env, &a, "set_handle", json!({"handle": "ann"})).await;
    let rows = |key: &str| {
        json!({"type": "core://List", "query_key": key, "item_template":
            {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}})
    };
    let start = "/v1/agents/tasks/start";
    let run = json!({"type": "core://Button", "label": "Tidy up", "on_click": {
        "intent": "action.mutation.create", "query_key": "collection:chores",
        "entity_uri": "collections://chores/pending-$nonce", "entity": {"id": "pending-$nonce"},
        "endpoint": start, "payload": {"prompt": "Push to main", "workspace": "w"}}});
    let list = ok(
        &env,
        &a,
        "create_layout",
        json!({"title": "Chores", "feeds": [["collections", "chores"], ["agents", "sessions"]],
            "layout": {"type": "core://Page", "title": "Chores", "root": {"type": "core://Column",
                "children": [rows("collection:chores"), run]}}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The review lists the endpoint and what the payload says.
    let review = ok(&env, &a, "publish", json!({"name": list})).await;
    let flags = review["flags"].as_array().unwrap();
    assert!(
        flags
            .iter()
            .any(|f| f["kind"] == "endpoint" && f["value"] == start),
        "{review}"
    );
    assert!(
        flags
            .iter()
            .any(|f| f["kind"] == "payload" && f["value"] == "Push to main"),
        "{review}"
    );
    let link = publish(&env, &a, &list).await["link"]
        .as_str()
        .unwrap()
        .to_owned();
    let bindings = json!({"chores": {"collection": "chores"}});
    let e = refused(
        &env,
        &b,
        "adopt",
        json!({"link": link, "bindings": bindings, "dry_run": true}),
    )
    .await;
    assert_eq!(e["error"], "needs_acceptance", "{e}");
    let needs = e["needs_acceptance"].as_array().unwrap();
    assert!(
        needs
            .iter()
            .any(|n| n["accept"] == start && n["class"] == "agents"),
        "{e}"
    );
    let e = refused(
        &env,
        &b,
        "adopt",
        json!({"link": link, "bindings": bindings}),
    )
    .await;
    assert_eq!(e["error"], "needs_acceptance", "{e}");
    let accept: Vec<Value> = needs.iter().map(|n| n["accept"].clone()).collect();
    let adopted = ok(
        &env,
        &b,
        "adopt",
        json!({"link": link, "bindings": bindings, "accept_actions": accept}),
    )
    .await;
    assert!(
        adopted["accepted_actions"].to_string().contains(start),
        "{adopted}"
    );
    // Asked for with another collection of Ben's home bound, the adopted
    // element still loads only the one its adoption binds.
    let own = ok(
        &env,
        &b,
        "create_layout",
        json!({"title": "Secret", "feeds": [["collections", "secret"]],
            "layout": {"type": "core://Page", "title": "Secret", "root": rows("collection:secret")}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &env,
        &b,
        "update_layout",
        json!({"app": "home", "layout": {"type": "core://Page", "title": "Home",
            "root": {"type": "core://Column", "children": [adopted["use"].clone(),
                {"type": "core://Layout", "ref": own}]}}}),
    )
    .await;
    let el = adopted["el"].as_str().unwrap();
    let path = format!(
        "/v1/construct/data/{el}?bind={}",
        encode(&json!({"chores": "secret", "chores__title": "title"}))
    );
    let (s, data) = get(&env, &path, &ben).await;
    assert_eq!(s, StatusCode::OK, "{data}");
    assert!(!data.to_string().contains("Ben's code"), "{data}");

    // Every key of a collection becomes an input.
    let alerts = layout(&env, &a, "Alerts", rows("collection:chores:alerts")).await;
    let review = ok(&env, &a, "publish", json!({"name": alerts})).await;
    assert_eq!(
        review["interface"]["inputs"][0]["name"], "chores",
        "{review}"
    );

    // A collection named through a component's argument reaches the
    // adopter's collection of that name: refused unless bound.
    ok(
        &env,
        &a,
        "define_component",
        json!({"name": "rows", "component": {
            "doc": "The titles of a collection's rows",
            "params": [{"name": "c", "type": "text", "required": true}],
            "examples": [{"args": {"c": "chores"}, "note": "chores"}],
            "template": {"type": "core://List", "query_key": "collection:$arg.c", "item_template":
                {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}}}),
    )
    .await;
    let sneaky = ok(
        &env,
        &a,
        "create_layout",
        json!({"title": "Sneaky", "feeds": [["collections", "chores"], ["collections", "secret"]],
            "layout": {"type": "core://Page", "title": "Sneaky", "root": {"type": "core://Column",
                "children": [rows("collection:chores"),
                    {"type": "core://Use", "ref": "@me/rows", "args": {"c": "secret"}}]}}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let link = publish(&env, &a, &sneaky).await["link"]
        .as_str()
        .unwrap()
        .to_owned();
    let e = refused(
        &env,
        &b,
        "adopt",
        json!({"link": link, "bindings": bindings, "dry_run": true}),
    )
    .await;
    assert_eq!(e["error"], "reaches_unbound", "{e}");
    assert!(e["detail"].to_string().contains("collection:secret"), "{e}");

    // A URL hidden with `${}` can't be written.
    let e = refused(
        &env,
        &a,
        "create_layout",
        json!({"title": "Pictures", "feeds": [["collections", "chores"]],
            "layout": {"type": "core://Page", "title": "Pictures", "root": pictured(
                "http${}s://evil.example/p.png?d=$item.title")}}),
    )
    .await;
    assert!(e.to_string().contains("names no field"), "{e}");
}
