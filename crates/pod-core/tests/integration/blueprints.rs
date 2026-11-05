//! Blueprints (decision `blueprints-and-home`): built-in apps are listed
//! only as blueprints, every pod has a `home` app that references the home
//! blueprint, a whole blueprint can be referenced as a layout's root, and a
//! pod made before blueprints keeps an app for each built-in it showed.

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
    AppAdapter, AppInfo, AppState, AuthPolicy, BoxFuture, FeedSpec, HOME_APP, HOME_BLUEPRINT,
    Hydration, Pod, RequestCtx, SourceManifest, StaticAdapter,
};
use protocol::{DataBundle, EntityRecord, WorkspaceSpec};
use serde_json::{Value, json};
use tower::ServiceExt;

const A_FILE: LayoutSource = LayoutSource {
    dir: "layouts",
    app_id: "a_home",
    name: "0001",
    text: r#"{"note": "A", "layout": {"type": "core://Workspace", "id": "a_home", "title": "A",
      "slots": [{"slot_id": "feed", "provider_uri": "core://a",
        "content": {"type": "core://List", "query_key": "a:all",
          "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.id"}}}]}}"#,
};

const SHELL_FILE: LayoutSource = LayoutSource {
    dir: "layouts",
    app_id: HOME_BLUEPRINT,
    name: "0001",
    text: r#"{"note": "Dock", "layout": {"type": "core://Workspace", "id": "shell", "title": "Home",
      "slots": [{"slot_id": "dock", "provider_uri": "local://apps",
        "content": {"type": "core://Dock", "field": "tab", "query_key": "local:apps",
          "item": {"value": "$item.id", "label": "$item.title",
                   "content": {"type": "core://AppFrame", "app": "$item.id"}},
          "after": [{"value": "more", "label": "More", "icon": "more",
            "content": {"type": "core://Text", "style": "Body", "value_expr": "${local://inbox#label}"}}]}}]}}"#,
};

static A_MANIFEST: SourceManifest = SourceManifest {
    title: "Source A",
    sign_in: None,
    entity_schemes: &["a"],
    feeds: &[FeedSpec {
        name: "all",
        query_keys: &["a:all"],
        doc: "",
    }],
    data_paths: &[],
    actions: &[],
    entities: &[],
    dev: false,
};

struct A;

impl AppAdapter for A {
    fn name(&self) -> &'static str {
        "a"
    }
    fn apps(&self) -> Vec<AppInfo> {
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
        &[A_FILE]
    }
    fn manifest(&self) -> SourceManifest {
        A_MANIFEST
    }
    fn feed<'a>(&'a self, _: &'a Pod, _: &'a str, _: &'a RequestCtx) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            let mut data = DataBundle::default();
            let mut e = EntityRecord::default();
            e.set("id", "1");
            data.entities.insert("a://1".into(), e);
            data.queries.insert("a:all".into(), vec!["a://1".into()]);
            Hydration::Data(data)
        })
    }
}

fn shell() -> Arc<dyn AppAdapter> {
    let empty = WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: protocol::UiNode::Spacer { size_dp: None },
        entities: Default::default(),
        queries: Default::default(),
    };
    let info = AppInfo {
        id: HOME_BLUEPRINT,
        title: "Home",
        icon: "dashboard",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[],
    };
    Arc::new(StaticAdapter::new(
        HOME_BLUEPRINT,
        info,
        &[SHELL_FILE],
        &empty,
    ))
}

fn open(db: &std::path::Path) -> Pod {
    let pod = Arc::new(AppState::new(
        Store::open(db).unwrap(),
        vec![Arc::new(A), shell()],
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
        .header(protocol::FEATURES_HEADER, protocol::features_header());
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

async fn app_ids(pod: &Pod, path: &str) -> Vec<String> {
    let (_, apps) = call(pod, "GET", path, None).await;
    apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_new_pod_has_only_its_home_app() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    assert_eq!(app_ids(&pod, "/v1/apps").await, [HOME_APP]);
    assert_eq!(
        app_ids(&pod, "/v1/apps?blueprints=1").await,
        ["a_home", HOME_BLUEPRINT, HOME_APP]
    );
    // Home serves the whole home blueprint under its own id.
    let (status, ws) = call(&pod, "GET", "/v1/workspaces/home", None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["workspace"]["id"], HOME_APP);
    assert_eq!(
        ws["workspace"]["slots"][0]["content"]["type"],
        "core://Dock"
    );
    let (_, history) = call(&pod, "GET", "/v1/apps/home/versions", None).await;
    let features = history["versions"][0]["features"].to_string();
    assert!(
        features.contains("dock") && features.contains("local_data"),
        "{history}"
    );
    // Blueprints still serve by id (notifications and links name them).
    let (status, _) = call(&pod, "GET", "/v1/workspaces/a_home", None).await;
    assert_eq!(status, StatusCode::OK);
    // Home can't be deleted, but it can be changed.
    let (status, b) = call(&pod, "DELETE", "/v1/custom-apps/home", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"], "home_app");
    // Reopening adds nothing.
    drop(pod);
    let pod = open(&dir.path().join("server.db"));
    assert_eq!(app_ids(&pod, "/v1/apps").await, [HOME_APP]);
}

#[tokio::test]
async fn an_app_can_be_a_whole_blueprint_by_reference() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    let whole = json!({"type": "core://Ref", "app": "a_home", "part": "layout"});
    let (status, b) = call(
        &pod,
        "POST",
        "/v1/custom-apps",
        Some(json!({"title": "Mine", "icon": "photo", "feeds": [["a", "all"]], "layout": whole})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let id = b["id"].as_str().unwrap().to_owned();
    let (_, ws) = call(&pod, "GET", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(ws["workspace"]["id"], id.as_str());
    assert_eq!(ws["queries"]["a:all"], json!(["a://1"]));
    // Only at the root.
    let nested = json!({"type": "core://Workspace", "id": "x", "title": "M",
        "slots": [{"slot_id": "s", "provider_uri": "core://c", "content": whole}]});
    let (status, b) = call(
        &pod,
        "POST",
        "/v1/custom-apps",
        Some(
            json!({"title": "Nested", "icon": "photo", "feeds": [["a", "all"]], "layout": nested}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert!(
        b.to_string().contains("only be referenced at the root"),
        "{b}"
    );
}

#[tokio::test]
async fn an_older_pod_keeps_an_app_for_each_built_in_it_showed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.db");
    // What the `blueprints` migration leaves on a pod with layout history.
    open(&db)
        .store()
        .set_pod_setting(pod_core::custom::CONVERT_SETTING, "1")
        .unwrap();
    let pod = open(&db);
    let (_, apps) = call(&pod, "GET", "/v1/apps", None).await;
    let rows = apps["apps"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{apps}");
    assert_eq!(rows[0]["id"], HOME_APP);
    assert_eq!(rows[1]["title"], "A");
    let id = rows[1]["id"].as_str().unwrap().to_owned();
    let (_, ws) = call(&pod, "GET", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(ws["workspace"]["id"], id.as_str());
    assert_eq!(ws["queries"]["a:all"], json!(["a://1"]), "{ws}");
    assert_eq!(
        pod.store()
            .pod_setting(pod_core::custom::CONVERT_SETTING)
            .unwrap(),
        None
    );
    drop(pod);
    let pod = open(&db);
    assert_eq!(app_ids(&pod, "/v1/apps").await.len(), 2, "converted once");
}
