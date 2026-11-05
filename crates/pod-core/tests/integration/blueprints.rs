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
    text: r#"{"note": "List", "layout": {"type": "core://Workspace", "id": "shell", "title": "Home",
      "slots": [{"slot_id": "list", "provider_uri": "local://layouts",
        "content": {"type": "core://List", "query_key": "local:layouts",
          "item_template": {"type": "core://Tappable", "content": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"},
            "on_click": {"intent": "action.navigation.open", "layout": "$item.id", "presentation": "Push"}}}}]}}"#,
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
        .header(protocol::FEATURES_HEADER, crate::workspace_features());
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
        "core://List"
    );
    let (_, history) = call(&pod, "GET", "/v1/apps/home/versions", None).await;
    let features = history["versions"][0]["features"].to_string();
    assert!(
        features.contains("navigate_layout") && features.contains("local_data"),
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

// ---------- layouts, not apps (decision `layouts-not-apps`) ----------

fn list_of(query: &str) -> Value {
    json!({"type": "core://Workspace", "title": "L", "slots": [{"slot_id": "s", "provider_uri": "core://a",
        "content": {"type": "core://List", "query_key": query,
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.id"}}}]})
}

#[tokio::test]
async fn home_owns_the_feeds_and_each_layout_loads_what_it_binds() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    // Feeds named by a new layout join home's.
    let (status, made) = call(
        &pod,
        "POST",
        "/v1/custom-apps",
        Some(json!({"title": "A list", "feeds": [["a", "all"]], "layout": list_of("a:all")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let first = made["id"].as_str().unwrap().to_owned();
    let (_, home) = call(&pod, "GET", "/v1/custom-apps/home", None).await;
    assert_eq!(home["feeds"], json!([["a", "all"]]), "{home}");
    // Another layout binds them without naming any.
    let (status, made) = call(
        &pod,
        "POST",
        "/v1/custom-apps",
        Some(json!({"title": "Again", "layout": list_of("a:all")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let second = made["id"].as_str().unwrap().to_owned();
    let (_, ws) = call(&pod, "GET", &format!("/v1/workspaces/{first}"), None).await;
    assert_eq!(ws["queries"]["a:all"], json!(["a://1"]), "{ws}");
    // Home binds no feed of its own: nothing of a's loads with it, but it
    // still learns about the sources of its scope.
    let (_, ws) = call(&pod, "GET", "/v1/workspaces/home", None).await;
    assert!(ws["queries"].get("a:all").is_none(), "{ws}");
    assert_eq!(
        ws["entities"]["core://sources/a"]["signed_in"], true,
        "{ws}"
    );
    // Home can't drop a feed a layout still binds; the error names it.
    let (status, b) = call(
        &pod,
        "PATCH",
        "/v1/custom-apps/home",
        Some(json!({"feeds": []})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"], "feed_in_use");
    let named = b["layout"].as_str().unwrap();
    assert!(named == first || named == second, "{b}");
    assert!(b["detail"].as_str().unwrap().contains(named), "{b}");
}

#[tokio::test]
async fn embedded_and_presented_layouts_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    let with = |content: Value| {
        json!({"title": "E", "layout": {"type": "core://Workspace", "title": "E",
            "slots": [{"slot_id": "s", "provider_uri": "local://x", "content": content}]}})
    };
    let ok = with(json!({"type": "core://Column", "children": [
        {"type": "core://Layout", "layout": "inbox", "presentation": "Sheet", "show_when": "$form.inbox"},
        {"type": "core://Button", "label": "Open", "on_click": {"intent": "action.navigation.open",
            "layout": "settings", "presentation": "Panel", "animation": "slide"}},
        {"type": "core://Button", "label": "Leave", "on_click": {"intent": "action.confirm",
            "title": "Leave this pod?", "confirm_label": "Leave",
            "then": {"intent": "action.device", "command": "leave_pod"}}}]}));
    let (status, b) = call(&pod, "POST", "/v1/custom-apps", Some(ok)).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    for (bad, says) in [
        (
            json!({"type": "core://Layout", "layout": "not a layout"}),
            "not a layout id",
        ),
        (
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.navigation.open", "presentation": "Push"}}),
            "needs a target_uri or a layout",
        ),
        (
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.device", "command": "open_app", "app": "home"}}),
            "device_open_app",
        ),
        (
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.confirm",
                "title": " ", "confirm_label": "Leave", "then": {"intent": "action.device", "command": "leave_pod"}}}),
            "needs a title and a confirm_label",
        ),
        (
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.confirm",
                "title": "Sure?", "confirm_label": "Yes", "then": {"intent": "action.device", "command": "explode"}}}),
            "unknown device command",
        ),
        (
            json!({"type": "core://AppFrame", "app": "home"}),
            "core://Layout",
        ),
    ] {
        let (status, b) = call(&pod, "POST", "/v1/custom-apps", Some(with(bad))).await;
        assert!(status.is_client_error(), "{b}");
        assert!(b["detail"].as_str().unwrap().contains(says), "{says}: {b}");
    }
}

#[tokio::test]
async fn an_app_era_home_becomes_a_list_and_gives_its_feeds_to_home() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.db");
    let pod = open(&db);
    let (_, made) = call(
        &pod,
        "POST",
        "/v1/custom-apps",
        Some(json!({"title": "A list", "feeds": [["a", "all"]], "layout": list_of("a:all")})),
    )
    .await;
    let id = made["id"].as_str().unwrap().to_owned();
    // What a pod of the app era holds: the app's own feeds, an empty home
    // whose latest version is a dock.
    let dock = json!({"type": "core://Workspace", "id": "home", "title": "Home", "slots": [{"slot_id": "d",
        "provider_uri": "local://apps", "content": {"type": "core://Dock", "field": "tab", "query_key": "local:apps",
        "item": {"value": "$item.id", "label": "$item.title", "content": {"type": "core://AppFrame", "app": "$item.id"}}}}]});
    pod.store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE custom_apps SET feeds_json = CASE WHEN id = 'home' THEN '[]' ELSE '[[\"a\",\"all\"]]' END",
                [],
            )?;
            pod_core::store::append_authored_version_on(
                tx,
                HOME_APP,
                &dock.to_string(),
                "custom",
                Some("Dock"),
                pod_core::store::Authored {
                    author: "user",
                    meta_json: None,
                    features: "[\"dock\",\"app_frame\"]",
                },
            )?;
            Ok(())
        })
        .unwrap();
    pod.store()
        .set_pod_setting(pod_core::custom::LAYOUTS_CONVERT_SETTING, "1")
        .unwrap();
    drop(pod);
    let pod = open(&db);
    let (_, home) = call(&pod, "GET", "/v1/custom-apps/home", None).await;
    assert_eq!(home["feeds"], json!([["a", "all"]]), "{home}");
    assert_eq!(
        home["layout"]["slots"][0]["content"]["children"][1]["query_key"], "local:layouts",
        "{home}"
    );
    let (_, ws) = call(&pod, "GET", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(ws["queries"]["a:all"], json!(["a://1"]), "{ws}");
    assert_eq!(
        pod.store()
            .pod_setting(pod_core::custom::LAYOUTS_CONVERT_SETTING)
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_feed_belongs_to_the_layout_that_binds_it_else_home() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    let owners = || pod_core::custom::feed_owners(pod.store(), pod.catalog(), "a").unwrap();
    // No layout declares a feed of a: nothing to own.
    assert!(owners().is_empty());
    // A layout that binds nothing of a's, made first, with the feed: every
    // layout reports home's feeds, but none binds it, so home owns it.
    let empty = json!({"type": "core://Workspace", "title": "E", "slots": [{"slot_id": "s",
        "provider_uri": "local://x", "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]});
    let (status, made) = call(
        &pod,
        "POST",
        "/v1/custom-apps",
        Some(json!({"title": "Empty", "feeds": [["a", "all"]], "layout": empty})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(owners()["all"], HOME_APP);
    // The layouts that bind it: one of them owns it (the oldest; made in
    // the same second, they tie on age), the same one each time.
    let mut binders = Vec::new();
    for title in ["First", "Second"] {
        let (status, made) = call(
            &pod,
            "POST",
            "/v1/custom-apps",
            Some(json!({"title": title, "layout": list_of("a:all")})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        binders.push(made["id"].as_str().unwrap().to_owned());
    }
    let owner = owners()["all"].clone();
    assert!(binders.contains(&owner), "{owner} of {binders:?}");
    assert_eq!(owners()["all"], owner);
    assert_eq!(owners().len(), 1);
}
