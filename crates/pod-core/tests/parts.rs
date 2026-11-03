//! Built-in parts (decision `layout-parts`): layout files declaring parts,
//! custom layouts referencing them (`core://Ref`), expansion when serving,
//! `ref_update` versions after a deploy, pinned refs, placeholders, and copies
//! with their "has updates" notice.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::store::Store;
use pod_core::{
    AppAdapter, AppInfo, AppState, AuthPolicy, BoxFuture, FeedSpec, Hydration, PathSpec, Pod,
    RequestCtx, SourceManifest,
};
use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The built-in app `a_home`'s layout file `n`: a slot `feed` whose card says
/// `card`, a screen `item`, and a declared part `card`.
fn file(n: u32, card: &str, with_card_part: bool) -> LayoutSource {
    let parts = if with_card_part {
        format!(
            r#", "parts": {{"card": {{"type": "core://Text", "style": "Body", "value_expr": "{card}"}}}}"#
        )
    } else {
        String::new()
    };
    let text = format!(
        r#"{{"note": "v{n}", "schema": {schema}, "layout": {{"type": "core://Workspace", "id": "a_home", "title": "A",
          "screens": [{{"id": "item", "route": "a://items/{{id}}", "title": "Item {n}", "data": "/v1/a/screen?id=$route.id",
            "content": {{"type": "core://Text", "style": "Body", "value_expr": "item {n}"}}}}],
          "slots": [{{"slot_id": "feed", "provider_uri": "core://a",
            "content": {{"type": "core://List", "query_key": "a:all",
              "item_template": {{"type": "core://Button", "label": "{card}", "on_click": {{
                "intent": "action.mutation.optimistic", "entity_uri": "$item.uri", "field": "liked",
                "value_expr": "true", "rollback_value": false, "endpoint": "/v1/a/act", "payload": {{}}}}}}}}}}]}}{parts}}}"#,
        schema = protocol::SCHEMA_VERSION
    );
    LayoutSource {
        app_id: "a_home",
        name: Box::leak(format!("{n:04}").into_boxed_str()),
        text: Box::leak(text.into_boxed_str()),
    }
}

static A_MANIFEST: SourceManifest = SourceManifest {
    title: "Source A",
    sign_in: None,
    entity_schemes: &["a"],
    feeds: &[FeedSpec {
        name: "all",
        query_keys: &["a:all"],
        doc: "",
    }],
    data_paths: &[PathSpec {
        pattern: "/v1/a/screen?id={id}",
        query_keys: &[],
        doc: "",
    }],
    actions: &[PathSpec {
        pattern: "/v1/a/act",
        query_keys: &[],
        doc: "",
    }],
    entities: &[],
    dev: false,
};

static B_MANIFEST: SourceManifest = SourceManifest {
    title: "Source B",
    sign_in: None,
    entity_schemes: &["b"],
    feeds: &[FeedSpec {
        name: "all",
        query_keys: &["b:all"],
        doc: "",
    }],
    data_paths: &[],
    actions: &[],
    entities: &[],
    dev: false,
};

struct Src {
    name: &'static str,
    files: &'static [LayoutSource],
}

impl AppAdapter for Src {
    fn name(&self) -> &'static str {
        self.name
    }
    fn apps(&self) -> Vec<AppInfo> {
        if self.name != "a" {
            return Vec::new();
        }
        vec![AppInfo {
            id: "a_home",
            title: "A",
            icon: "photo",
            dev: false,
            auth: AuthPolicy::Public,
            feeds: &[("a", "all")],
        }]
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        self.files
    }
    fn manifest(&self) -> SourceManifest {
        if self.name == "a" {
            A_MANIFEST
        } else {
            B_MANIFEST
        }
    }
    fn feed<'a>(&'a self, _: &'a Pod, _: &'a str, _: &'a RequestCtx) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            let mut data = DataBundle::default();
            let uri = format!("{}://1", self.name);
            let mut e = EntityRecord::default();
            e.set("id", "1");
            data.entities.insert(uri.clone(), e);
            data.queries.insert(format!("{}:all", self.name), vec![uri]);
            Hydration::Data(data)
        })
    }
}

/// A pod over the database at `db` whose built-in app has `files`.
fn pod(db: &std::path::Path, files: Vec<LayoutSource>) -> Pod {
    let files: &'static [LayoutSource] = Box::leak(files.into_boxed_slice());
    let store = Store::open(db).unwrap();
    let pod = Arc::new(AppState::new(
        store,
        vec![
            Arc::new(Src { name: "a", files }),
            Arc::new(Src {
                name: "b",
                files: &[],
            }),
        ],
    ));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod
}

async fn call(pod: &Pod, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", "Bearer pod")
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        );
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = pod_core::app(pod.clone())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn r(part: &str, version: Option<i64>) -> Value {
    let mut v = json!({"type": "core://Ref", "app": "a_home", "part": part});
    if let Some(n) = version {
        v["version"] = json!(n);
    }
    v
}

/// A custom layout over `a`: the built-in's item screen and feed slot by
/// reference, and its `card` part (pinned to `card_version`, if given).
fn layout_with_refs(card_version: Option<i64>) -> Value {
    json!({"type": "core://Workspace", "id": "x", "title": "Mine",
    "screens": [r("screen:item", None)],
    "slots": [
        {"slot_id": "feed", "provider_uri": "core://custom", "content": r("slot:feed", None)},
        {"slot_id": "card", "provider_uri": "core://custom", "content": r("card", card_version)}
    ]})
}

async fn create(
    pod: &Pod,
    feeds: Value,
    layout: Value,
    copied_from: Option<Value>,
) -> (StatusCode, Value) {
    let mut body = json!({"title": "Mine", "icon": "photo", "feeds": feeds, "layout": layout});
    if let Some(c) = copied_from {
        body["copied_from"] = c;
    }
    call(pod, "POST", "/v1/custom-apps", Some(body)).await
}

#[tokio::test]
async fn refs_are_checked_expanded_on_serving_and_follow_deploys() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.db");
    let v1 = file(1, "Like v1", true);
    let pod1 = pod(&db, vec![v1]);

    // Bad references are refused with a pointer.
    for (layout, want) in [
        (
            json!({"type": "core://Workspace", "id": "x", "title": "M", "screens": [],
            "slots": [{"slot_id": "s", "provider_uri": "core://c", "content": r("nope", None)}]}),
            "no such part",
        ),
        (
            json!({"type": "core://Workspace", "id": "x", "title": "M", "screens": [],
            "slots": [{"slot_id": "s", "provider_uri": "core://c", "content": r("screen:item", None)}]}),
            "goes in screens",
        ),
        (
            json!({"type": "core://Workspace", "id": "x", "title": "M", "screens": [],
            "slots": [{"slot_id": "s", "provider_uri": "core://c",
              "content": {"type": "core://Ref", "app": "c-aaaaaaaaaa", "part": "slot:feed"}}]}),
            "only built-in",
        ),
    ] {
        let (status, b) = create(&pod1, json!([["a", "all"]]), layout, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
        assert!(b["detail"].as_str().unwrap().contains(want), "{want}: {b}");
        assert_eq!(b["at"], "/slots/0/content");
    }
    // What a part brings must fit the app's sources: a's action needs a's feed.
    let (status, b) = create(&pod1, json!([["b", "all"]]), layout_with_refs(None), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert!(b["detail"].as_str().unwrap().contains("/v1/a/"), "{b}");

    let (status, b) = create(&pod1, json!([["a", "all"]]), layout_with_refs(None), None).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let id = b["id"].as_str().unwrap().to_owned();
    let (status, pinned) = create(
        &pod1,
        json!([["a", "all"]]),
        layout_with_refs(Some(1)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let pinned_id = pinned["id"].as_str().unwrap().to_owned();
    // Clients get the parts, never a ref; the stored layout keeps the refs.
    let (status, ws) = call(&pod1, "GET", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    let text = ws["workspace"].to_string();
    assert!(!text.contains("core://Ref"), "{text}");
    assert_eq!(ws["workspace"]["screens"][0]["title"], "Item 1");
    assert_eq!(
        ws["workspace"]["slots"][0]["content"]["type"],
        "core://List"
    );
    assert_eq!(
        ws["workspace"]["slots"][1]["content"]["value_expr"],
        "Like v1"
    );
    let (_, raw) = call(&pod1, "GET", &format!("/v1/custom-apps/{id}"), None).await;
    assert_eq!(raw["layout"]["slots"][0]["content"]["type"], "core://Ref");
    let (_, v1_layout) = call(&pod1, "GET", &format!("/v1/apps/{id}/versions/1"), None).await;
    assert!(
        !v1_layout["layout"].to_string().contains("core://Ref"),
        "history is served expanded"
    );
    drop(pod1);

    // A deploy: a_home v2 changes the card and the screen. The custom app
    // that follows it gets a ref_update version; the pinned one doesn't.
    let pod2 = pod(
        &db,
        vec![file(1, "Like v1", true), file(2, "Like v2", true)],
    );
    let (_, h) = call(&pod2, "GET", &format!("/v1/apps/{id}/versions"), None).await;
    let latest = &h["versions"][0];
    assert_eq!(latest["version"], 2);
    assert_eq!(latest["source"], "ref_update");
    assert_eq!(latest["note"], "A updated to v2");
    assert_eq!(latest["author"], "pod");
    let (_, ws) = call(&pod2, "GET", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(ws["workspace"]["screens"][0]["title"], "Item 2");
    assert_eq!(
        ws["workspace"]["slots"][1]["content"]["value_expr"],
        "Like v2"
    );
    let (_, h) = call(
        &pod2,
        "GET",
        &format!("/v1/apps/{pinned_id}/versions"),
        None,
    )
    .await;
    // Its card is pinned, but its screen and slot refs follow: it is updated
    // too, and its card stays as pinned.
    assert_eq!(h["versions"].as_array().unwrap().len(), 2);
    let (_, ws) = call(&pod2, "GET", &format!("/v1/workspaces/{pinned_id}"), None).await;
    assert_eq!(
        ws["workspace"]["slots"][1]["content"]["value_expr"],
        "Like v1"
    );
    assert_eq!(
        ws["workspace"]["screens"][0]["title"], "Item 2",
        "unpinned refs still follow"
    );
    drop(pod2);

    // v3 drops the declared part: the unpinned ref becomes a placeholder.
    let pod3 = pod(
        &db,
        vec![
            file(1, "Like v1", true),
            file(2, "Like v2", true),
            file(3, "Like v3", false),
        ],
    );
    let (_, ws) = call(&pod3, "GET", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(
        ws["workspace"]["slots"][1]["content"]["value_expr"],
        "This part isn't available."
    );
    assert_eq!(
        ws["workspace"]["slots"][0]["content"]["item_template"]["label"],
        "Like v3"
    );
}

#[tokio::test]
async fn a_copy_hears_about_updates_until_dismissed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.db");
    let pod1 = pod(&db, vec![file(1, "Like v1", true)]);
    // A copy of a_home's layout (no refs), saying where it came from.
    let (_, built_in) = call(&pod1, "GET", "/v1/apps/a_home/versions/1", None).await;
    let mut copy = built_in["layout"].clone();
    copy["title"] = json!("My A");
    let (status, b) = create(
        &pod1,
        json!([["a", "all"]]),
        copy.clone(),
        Some(json!({"app": "a_home", "version": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let id = b["id"].as_str().unwrap().to_owned();
    let (status, bad) = create(
        &pod1,
        json!([["a", "all"]]),
        copy,
        Some(json!({"app": "a_home", "version": 9})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"], "invalid_copied_from");
    let (_, apps) = call(&pod1, "GET", "/v1/apps", None).await;
    let row = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id.as_str())
        .unwrap()
        .clone();
    assert!(row.get("updates_from").is_none(), "nothing new yet");
    drop(pod1);

    let pod2 = pod(
        &db,
        vec![file(1, "Like v1", true), file(2, "Like v2", true)],
    );
    let (_, apps) = call(&pod2, "GET", "/v1/apps", None).await;
    let row = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        row["updates_from"],
        json!({"app": "a_home", "title": "A", "from_version": 1, "to_version": 2})
    );
    // A copy gets no ref_update (it has no refs).
    let (_, h) = call(&pod2, "GET", &format!("/v1/apps/{id}/versions"), None).await;
    assert_eq!(h["versions"].as_array().unwrap().len(), 1);
    // An edit keeps the lineage; dismissing hides the notice.
    let (status, _) = call(
        &pod2,
        "PUT",
        &format!("/v1/custom-apps/{id}/layout"),
        Some(json!({"layout": built_in["layout"], "note": "tweak"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, apps) = call(&pod2, "GET", "/v1/apps", None).await;
    let row = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        row["updates_from"]["to_version"], 2,
        "copied_from is inherited"
    );
    let (status, _) = call(
        &pod2,
        "POST",
        &format!("/v1/custom-apps/{id}/dismiss-update"),
        Some(json!({"version": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, apps) = call(&pod2, "GET", "/v1/apps", None).await;
    let row = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id.as_str())
        .unwrap()
        .clone();
    assert!(row.get("updates_from").is_none());
    // Rebasing onto v2 (a new copied_from) moves the base.
    let (status, _) = call(
        &pod2,
        "PUT",
        &format!("/v1/custom-apps/{id}/layout"),
        Some(json!({"layout": built_in["layout"], "copied_from": {"app": "a_home", "version": 2}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        pod_core::custom::copied_from(pod2.store(), &id).unwrap(),
        Some(("a_home".to_owned(), 2))
    );
}

#[test]
fn layout_files_may_declare_parts() {
    let catalog = pod_core::Catalog::new(vec![
        Arc::new(Src {
            name: "a",
            files: &[],
        }),
        Arc::new(Src {
            name: "b",
            files: &[],
        }),
    ])
    .unwrap();
    let with = pod_core::layouts::parse_file(&catalog, &file(1, "x", true)).unwrap();
    assert_eq!(with.parts.keys().collect::<Vec<_>>(), ["card"]);
    let without = pod_core::layouts::parse_file(&catalog, &file(1, "x", false)).unwrap();
    assert_ne!(
        with.file_hash, without.file_hash,
        "parts are part of the file's hash"
    );
    let mut bad = file(1, "x", true);
    bad.text = Box::leak(bad.text.replace("\"card\":", "\"Card!\":").into_boxed_str());
    let err = pod_core::layouts::parse_file(&catalog, &bad).unwrap_err();
    assert!(err.contains("part name 'Card!'"), "{err}");
}
