//! Custom apps (decision `custom-apps-and-data-sources`): the write API and
//! who may use it, validation against the sources' manifests, composition
//! with signed-out and failing sources, history, hide and delete, caps.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::custom::Caps;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::store::open_session_on;
use pod_core::{
    AppAdapter, AppInfo, AppState, AuthPolicy, BoxFuture, FeedSpec, Hydration, PathSpec, Pod,
    RequestCtx, SignInProvider, SourceManifest,
};
use protocol::{DataBundle, EntityRecord, WorkspaceSpec};
use serde_json::{Value, json};
use tower::ServiceExt;

const OK: u8 = 0;
const FAIL: u8 = 1;
const STALE: u8 = 2;

/// Source `a`: public, one built-in app `a_home`. Source `b`: needs sign-in
/// `test`; its feed answers per `mode`.
struct Src {
    name: &'static str,
    mode: Arc<AtomicU8>,
}

static A_FILES: &[LayoutSource] = &[LayoutSource {
    app_id: "a_home",
    name: "0001",
    text: r#"{"note": "First", "schema": 11, "layout": {"type": "core://Workspace", "id": "a_home",
        "title": "A", "slots": [{"slot_id": "main", "provider_uri": "core://test",
        "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]}}"#,
}];

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
        query_keys: &["a:item:{id}"],
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
    sign_in: Some("test"),
    entity_schemes: &["b"],
    feeds: &[FeedSpec {
        name: "all",
        query_keys: &["b:all"],
        doc: "",
    }],
    data_paths: &[],
    actions: &[PathSpec {
        pattern: "/v1/b/act",
        query_keys: &[],
        doc: "",
    }],
    entities: &[],
    dev: false,
};

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
        if self.name == "a" { A_FILES } else { &[] }
    }
    fn sign_ins(&self) -> Vec<SignInProvider> {
        if self.name == "b" {
            vec![SignInProvider {
                id: "test",
                title: "Test",
            }]
        } else {
            Vec::new()
        }
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
            match self.mode.load(Ordering::SeqCst) {
                FAIL => Hydration::Error(pod_core::json_error(
                    StatusCode::BAD_GATEWAY,
                    json!({ "error": "upstream_down" }),
                )),
                STALE => Hydration::StaleSession(WorkspaceSpec {
                    version: "1.0.0".into(),
                    workspace: protocol::UiNode::Unknown(Value::Null),
                    entities: Default::default(),
                    queries: Default::default(),
                }),
                _ => {
                    let mut data = DataBundle::default();
                    let uri = format!("{}://1", self.name);
                    let mut e = EntityRecord::default();
                    e.set("from", self.name);
                    data.entities.insert(uri.clone(), e);
                    data.queries.insert(format!("{}:all", self.name), vec![uri]);
                    Hydration::Data(data)
                }
            }
        })
    }
}

struct Env {
    pod: Pod,
    b_mode: Arc<AtomicU8>,
}

fn env_with(caps: Caps) -> Env {
    let b_mode = Arc::new(AtomicU8::new(OK));
    let pod: Pod = Arc::new(
        AppState::in_memory(vec![
            Arc::new(Src {
                name: "a",
                mode: Arc::new(AtomicU8::new(OK)),
            }),
            Arc::new(Src {
                name: "b",
                mode: b_mode.clone(),
            }),
        ])
        .with_custom_app_caps(caps),
    );
    let store = pod.store();
    // The pod token's session, a plain session, one signed in to `b`.
    store.create_pinned_session(&session_hash("pod")).unwrap();
    store.create_session(&session_hash("plain")).unwrap();
    store
        .transaction(|tx| open_session_on(tx, &session_hash("signed"), "test", 86_400))
        .unwrap();
    Env { pod, b_mode }
}

fn env() -> Env {
    env_with(Caps::default())
}

async fn call(
    pod: &Pod,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(protocol::FEATURES_HEADER, protocol::features_header());
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
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

/// A Kitchen-style layout: a list of `a`'s items with a button that calls
/// `a`'s action, a list of `b`'s, a hint bound to `core://sources/b`, and a
/// screen that loads `a`'s data path.
fn kitchen() -> Value {
    json!({
        "type": "core://Workspace", "id": "ignored", "title": "Kitchen",
        "screens": [{"id": "item", "route": "a://items/{id}", "title": "Item",
            "data": "/v1/a/screen?id=$route.id",
            "content": {"type": "core://List", "query_key": "a:item:$route.id",
                "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.from"}}}],
        "slots": [
            {"slot_id": "a", "provider_uri": "core://custom",
             "content": {"type": "core://List", "query_key": "a:all",
                "item_template": {"type": "core://Button", "label": "Like", "on_click": {
                    "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": "liked", "value_expr": "true", "rollback_value": false,
                    "endpoint": "/v1/a/act", "payload": {"id": "$item.id"}}}}},
            {"slot_id": "b", "provider_uri": "core://custom",
             "content": {"type": "core://Column", "children": [
                {"type": "core://Text", "style": "Caption",
                 "value_expr": "Signed in: ${core://sources/b#signed_in}"},
                {"type": "core://List", "query_key": "b:all",
                 "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.from"}}]}}
        ]
    })
}

async fn create(env: &Env, feeds: Value, layout: Value) -> (StatusCode, Value) {
    call(
        &env.pod,
        "POST",
        "/v1/custom-apps",
        Some("pod"),
        Some(json!({"title": "Kitchen", "icon": "recipe", "feeds": feeds,
                    "layout": layout, "note": "Recipes next to posts"})),
    )
    .await
}

fn ab() -> Value {
    json!([["a", "all"], ["b", "all"]])
}

#[tokio::test]
async fn only_the_pod_token_writes_and_any_session_reads() {
    let env = env();
    let body =
        json!({"title": "K", "icon": "photo", "feeds": [["a", "all"]], "layout": kitchen_a()});
    let (status, _) = call(
        &env.pod,
        "POST",
        "/v1/custom-apps",
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, b) = call(
        &env.pod,
        "POST",
        "/v1/custom-apps",
        Some("plain"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(b["error"], "pod_token_required");
    let (status, b) = call(&env.pod, "POST", "/v1/custom-apps", Some("pod"), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let id = b["id"].as_str().unwrap();
    assert!(id.starts_with("c-") && id.len() == 12, "{id}");
    assert_eq!(b["version"], 1);
    let (status, d) = call(
        &env.pod,
        "GET",
        &format!("/v1/custom-apps/{id}"),
        Some("plain"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(d["feeds"], json!([["a", "all"]]));
    assert_eq!(d["layout"]["id"], id, "the pod sets the root id");
}

/// The Kitchen layout without `b`'s parts.
fn kitchen_a() -> Value {
    let mut k = kitchen();
    k["slots"].as_array_mut().unwrap().truncate(1);
    k
}

#[tokio::test]
async fn a_custom_app_is_listed_and_served_over_several_sources() {
    let env = env();
    let (status, b) = create(&env, ab(), kitchen()).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let id = b["id"].as_str().unwrap().to_owned();
    // Listed after the built-ins, marked custom.
    let (_, apps) = call(&env.pod, "GET", "/v1/apps", None, None).await;
    let rows = apps["apps"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["kind"], "builtin");
    assert_eq!(rows[1]["id"], id.as_str());
    assert_eq!(rows[1]["kind"], "custom");
    assert_eq!(rows[1]["title"], "Kitchen");
    assert_eq!(rows[1]["hidden"], false);
    assert_eq!(rows[1]["latest_compatible_version"], 1);
    // Signed out of b: a's data, and b's source says so.
    let path = format!("/v1/workspaces/{id}");
    let (status, ws) = call(&env.pod, "GET", &path, Some("plain"), None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["workspace"]["title"], "Kitchen");
    assert_eq!(ws["entities"]["a://1"]["from"], "a");
    assert!(ws["entities"].get("b://1").is_none());
    assert_eq!(ws["entities"]["core://sources/b"]["signed_in"], false);
    assert_eq!(ws["entities"]["core://sources/b"]["title"], "Source B");
    assert_eq!(ws["entities"]["core://sources/a"]["signed_in"], true);
    // Signed in: both.
    let (_, ws) = call(&env.pod, "GET", &path, Some("signed"), None).await;
    assert_eq!(ws["entities"]["b://1"]["from"], "b");
    assert_eq!(ws["entities"]["core://sources/b"]["signed_in"], true);
    // A failing source is recorded, the rest is served.
    env.b_mode.store(FAIL, Ordering::SeqCst);
    let (status, ws) = call(&env.pod, "GET", &path, Some("signed"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ws["entities"]["a://1"]["from"], "a");
    assert_eq!(ws["entities"]["core://sources/b"]["error"], "error 502");
    // A stale sign-in: a plain session is told (401); the pod token's
    // session loses only that sign-in.
    env.b_mode.store(STALE, Ordering::SeqCst);
    let (status, _) = call(&env.pod, "GET", &path, Some("signed"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    env.pod
        .store()
        .attach_sign_in(&session_hash("pod"), "test")
        .unwrap();
    let (status, ws) = call(&env.pod, "GET", &path, Some("pod"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ws["entities"]["core://sources/b"]["signed_in"], false);
    assert!(
        env.pod
            .store()
            .session_sign_ins(&session_hash("pod"))
            .unwrap()
            .is_empty()
    );
    // Built-in apps are unchanged: no sources entity.
    let (_, ws) = call(&env.pod, "GET", "/v1/workspaces/a_home", None, None).await;
    assert!(ws["entities"].get("core://sources/a").is_none());
}

#[tokio::test]
async fn layouts_may_only_use_their_sources() {
    let env = env();
    let refused = |layout: Value, feeds: Value| {
        let env = &env;
        async move {
            let (status, b) = create(env, feeds, layout).await;
            assert_ne!(status, StatusCode::CREATED, "{b}");
            b
        }
    };
    // b's action without b's feed.
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"]["on_click"]["endpoint"] = json!("/v1/b/act");
    let b = refused(l, json!([["a", "all"]])).await;
    assert_eq!(b["error"], "invalid_layout");
    assert_eq!(b["at"], "/slots/0/content/item_template/on_click/endpoint");
    assert_eq!(b["allowed"], json!(["/v1/a/act"]));
    // Hidden inside an action.sequence: still checked, step by step.
    let mut l = kitchen_a();
    let tap = l["slots"][0]["content"]["item_template"]["on_click"].clone();
    let mut bad = tap.clone();
    bad["endpoint"] = json!("/v1/b/act");
    l["slots"][0]["content"]["item_template"]["on_click"] =
        json!({ "intent": "action.sequence", "steps": [tap, bad] });
    let b = refused(l, json!([["a", "all"]])).await;
    assert_eq!(
        b["at"],
        "/slots/0/content/item_template/on_click/steps/1/endpoint"
    );
    // A local set on another source's entity.
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"]["on_click"] = json!({
        "intent": "action.local.set", "entity_uri": "b://things/1", "field": "seen",
        "value_expr": "true" });
    let b = refused(l, json!([["a", "all"]])).await;
    assert_eq!(
        b["at"],
        "/slots/0/content/item_template/on_click/entity_uri"
    );
    // A route no source declares, a core route.
    for endpoint in ["/v1/auth/logout", "/v1/pods/open", "/v1/a/act?x=1"] {
        let mut l = kitchen_a();
        l["slots"][0]["content"]["item_template"]["on_click"]["endpoint"] = json!(endpoint);
        let b = refused(l, json!([["a", "all"]])).await;
        assert!(
            b["detail"]
                .as_str()
                .unwrap()
                .contains("not an action of this app's data sources"),
            "{b}"
        );
    }
    // A screen data path with the wrong parameter.
    let mut l = kitchen_a();
    l["screens"][0]["data"] = json!("/v1/a/screen?slug=$route.id");
    let b = refused(l, json!([["a", "all"]])).await;
    assert_eq!(b["at"], "/screens/0/data");
    assert_eq!(b["allowed"], json!(["/v1/a/screen?id={id}"]));
    // A query key no source fills.
    let mut l = kitchen_a();
    l["slots"][0]["content"]["query_key"] = json!("timeline:home");
    let b = refused(l, json!([["a", "all"]])).await;
    assert_eq!(b["at"], "/slots/0/content/query_key");
    // Another source's entities.
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"]["on_click"]["entity_uri"] = json!("b://1");
    let b = refused(l, json!([["a", "all"]])).await;
    assert!(
        b["detail"].as_str().unwrap().contains("entity URI 'b://1'"),
        "{b}"
    );
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"] =
        json!({"type": "core://Text", "style": "Body", "value_expr": "${b://1#from}"});
    refused(l, json!([["a", "all"]])).await;
    // A sign-in button.
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"]["on_click"] = json!({"intent": "action.auth.login", "instance_url": "x", "app_path": "/v1/a/act",
               "token_path": "/v1/a/act"});
    let b = refused(l, json!([["a", "all"]])).await;
    assert!(b["detail"].as_str().unwrap().contains("Settings"), "{b}");
    // A pod path anywhere else (an image URL).
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"] =
        json!({"type": "core://Image", "url": "/v1/uploads/x"});
    refused(l, json!([["a", "all"]])).await;
    // An unknown node type or intent.
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"] = json!({"type": "core://Nope"});
    let b = refused(l, json!([["a", "all"]])).await;
    assert!(
        b["detail"].as_str().unwrap().contains("unknown node type"),
        "{b}"
    );
    // Feeds that don't exist, bad titles and icons.
    let b = refused(kitchen_a(), json!([["a", "nope"]])).await;
    assert_eq!(b["error"], "invalid_feeds");
    assert!(b["allowed"].as_array().unwrap().contains(&json!("a/all")));
    let b = refused(kitchen_a(), json!([])).await;
    assert_eq!(b["error"], "invalid_feeds");
    let (_, b) = call(
        &env.pod,
        "POST",
        "/v1/custom-apps",
        Some("pod"),
        Some(json!({"title": "K", "icon": "rocketship", "feeds": [["a", "all"]], "layout": kitchen_a()})),
    )
    .await;
    assert_eq!(b["error"], "invalid_icon");
    // The dry run says the same without writing.
    let (status, b) = call(
        &env.pod,
        "POST",
        "/v1/custom-apps/validate",
        Some("pod"),
        Some(json!({"feeds": ab(), "layout": kitchen()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    assert_eq!(b["ok"], true);
    let (_, apps) = call(&env.pod, "GET", "/v1/apps", None, None).await;
    assert_eq!(apps["apps"].as_array().unwrap().len(), 1, "nothing created");
}

#[tokio::test]
async fn a_progress_spinner_is_accepted_and_needs_its_feature() {
    let env = env();
    // A spinner beside each item while its status is "pending".
    let mut l = kitchen_a();
    l["slots"][0]["content"]["item_template"] = json!({"type": "core://Row", "children": [
        {"type": "core://Text", "style": "Body", "value_expr": "$item.from"},
        {"type": "core://Conditional", "value": "$item.status", "equals": "pending",
         "content": {"type": "core://Progress", "size_dp": 16}}]});
    let (status, b) = create(&env, json!([["a", "all"]]), l).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let id = b["id"].as_str().unwrap();
    let (status, v) = call(
        &env.pod,
        "GET",
        &format!("/v1/apps/{id}/versions"),
        Some("plain"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let features = v["versions"][0]["features"].as_array().unwrap();
    assert!(features.contains(&json!("progress")), "{v}");
    assert!(
        features.contains(&json!("list")),
        "base vocabulary is named too: {v}"
    );
}

#[tokio::test]
async fn edits_append_history_and_restore_appends_a_copy() {
    let env = env();
    let (_, b) = create(&env, ab(), kitchen()).await;
    let id = b["id"].as_str().unwrap().to_owned();
    let mut v2 = kitchen();
    v2["title"] = json!("Kitchen 2");
    let put = |body: Value| {
        let pod = env.pod.clone();
        let id = id.clone();
        async move {
            call(
                &pod,
                "PUT",
                &format!("/v1/custom-apps/{id}/layout"),
                Some("pod"),
                Some(body),
            )
            .await
        }
    };
    let (status, b) = put(json!({"layout": v2, "note": "Rename", "base_version": 1})).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    assert_eq!(b["version"], 2);
    // Based on an old version: refused, nothing written.
    let (status, b) = put(json!({"layout": kitchen(), "base_version": 1})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(b["error"], "stale_base");
    assert_eq!(b["latest_version"], 2);
    let (status, b) = call(
        &env.pod,
        "POST",
        &format!("/v1/custom-apps/{id}/restore"),
        Some("pod"),
        Some(json!({"version": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    assert_eq!(b["version"], 3);
    let (_, h) = call(
        &env.pod,
        "GET",
        &format!("/v1/apps/{id}/versions"),
        None,
        None,
    )
    .await;
    let versions = h["versions"].as_array().unwrap();
    let notes: Vec<&str> = versions
        .iter()
        .map(|v| v["note"].as_str().unwrap())
        .collect();
    assert_eq!(
        notes,
        ["Restored version 1", "Rename", "Recipes next to posts"]
    );
    assert_eq!(versions[0]["source"], "restore");
    assert_eq!(versions[1]["source"], "custom");
    assert!(versions.iter().all(|v| v["author"] == "user"));
    let (_, ws) = call(&env.pod, "GET", &format!("/v1/workspaces/{id}"), None, None).await;
    assert_eq!(ws["workspace"]["title"], "Kitchen");
    // Renaming keeps the layout; dropping a feed its layout uses is refused.
    let patch = |body: Value| {
        let pod = env.pod.clone();
        let id = id.clone();
        async move {
            call(
                &pod,
                "PATCH",
                &format!("/v1/custom-apps/{id}"),
                Some("pod"),
                Some(body),
            )
            .await
        }
    };
    let (status, _) = patch(json!({"title": "My kitchen", "icon": "photo"})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, apps) = call(&env.pod, "GET", "/v1/apps", None, None).await;
    assert_eq!(apps["apps"][1]["title"], "My kitchen");
    let (status, b) = patch(json!({"feeds": [["b", "all"]]})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
}

#[tokio::test]
async fn hiding_and_deleting_apps() {
    let env = env();
    let (_, b) = create(&env, ab(), kitchen()).await;
    let id = b["id"].as_str().unwrap().to_owned();
    // Any session may hide, a built-in too; hidden apps are listed only
    // with ?all=1.
    let (status, _) = call(
        &env.pod,
        "PUT",
        "/v1/apps/a_home/hidden",
        None,
        Some(json!({"hidden": true})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, b) = call(
        &env.pod,
        "PUT",
        "/v1/apps/a_home/hidden",
        Some("plain"),
        Some(json!({"hidden": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let (_, apps) = call(&env.pod, "GET", "/v1/apps", None, None).await;
    let ids: Vec<&str> = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [id.as_str()]);
    let (_, apps) = call(&env.pod, "GET", "/v1/apps?all=1", None, None).await;
    assert_eq!(apps["apps"][0]["hidden"], true);
    // A hidden app still serves.
    let (status, _) = call(&env.pod, "GET", "/v1/workspaces/a_home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    call(
        &env.pod,
        "PUT",
        "/v1/apps/a_home/hidden",
        Some("plain"),
        Some(json!({"hidden": false})),
    )
    .await;
    let (_, apps) = call(&env.pod, "GET", "/v1/apps", None, None).await;
    assert_eq!(apps["apps"].as_array().unwrap().len(), 2);

    // Deleting: an open events stream hears `deleted` and ends.
    let resp = pod_core::app(env.pod.clone())
        .oneshot(
            Request::get(format!("/v1/workspaces/{id}/events"))
                .header("authorization", "Bearer plain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body();
    let first = sse_text(&mut stream, Duration::from_millis(200)).await;
    assert!(first.contains("event: layout"), "{first}");
    let (status, _) = call(
        &env.pod,
        "DELETE",
        &format!("/v1/custom-apps/{id}"),
        Some("plain"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = sse_text(&mut stream, Duration::from_millis(500)).await;
    assert!(
        text.contains("event: deleted") && text.contains(&id),
        "{text}"
    );
    let (status, _) = call(&env.pod, "GET", &format!("/v1/workspaces/{id}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, apps) = call(&env.pod, "GET", "/v1/apps?all=1", None, None).await;
    assert_eq!(apps["apps"].as_array().unwrap().len(), 1);
    let (status, _) = call(
        &env.pod,
        "DELETE",
        &format!("/v1/custom-apps/{id}"),
        Some("plain"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Built-ins can't be deleted.
    let (status, _) = call(
        &env.pod,
        "DELETE",
        "/v1/custom-apps/a_home",
        Some("plain"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Purged after 30 days, with its versions.
    let later = pod_core::util::now_secs() + pod_core::custom::PURGE_AFTER_SECS + 1;
    assert_eq!(pod_core::custom::purge(env.pod.store(), later).unwrap(), 1);
    assert!(env.pod.store().app_latest_meta(&id).unwrap().is_none());
}

async fn sse_text(body: &mut Body, wait: Duration) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, body.frame()).await {
        if let Some(d) = frame.data_ref() {
            text.push_str(&String::from_utf8_lossy(d));
        }
    }
    text
}

#[tokio::test]
async fn caps_limit_apps_versions_and_writes() {
    let env = env_with(Caps {
        apps: 2,
        versions: 3,
        writes_per_minute: 100,
    });
    let (_, b) = create(&env, ab(), kitchen()).await;
    let id = b["id"].as_str().unwrap().to_owned();
    create(&env, ab(), kitchen()).await;
    let (status, b) = create(&env, ab(), kitchen()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(b["error"], "too_many_apps");
    // Versions beyond the cap: the oldest go, numbers keep counting.
    for _ in 0..4 {
        let (status, _) = call(
            &env.pod,
            "PUT",
            &format!("/v1/custom-apps/{id}/layout"),
            Some("pod"),
            Some(json!({"layout": kitchen()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (_, h) = call(
        &env.pod,
        "GET",
        &format!("/v1/apps/{id}/versions"),
        None,
        None,
    )
    .await;
    let numbers: Vec<i64> = h["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [5, 4, 3]);
    // Too large a layout.
    let mut big = kitchen();
    big["title"] = json!("x".repeat(300 * 1024));
    let (status, b) = call(
        &env.pod,
        "PUT",
        &format!("/v1/custom-apps/{id}/layout"),
        Some("pod"),
        Some(json!({"layout": big})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(b["detail"].as_str().unwrap().contains("bytes"), "{b}");

    let env = env_with(Caps {
        apps: 50,
        versions: 500,
        writes_per_minute: 2,
    });
    create(&env, ab(), kitchen()).await;
    create(&env, ab(), kitchen()).await;
    let (status, b) = create(&env, ab(), kitchen()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{b}");
}
