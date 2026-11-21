//! Core layouts on the construct (decisions `blueprints-and-home`,
//! `content-addressed-construct`): a new pod's home is the whole home
//! core layout (`@core/shell`), a layout can be a whole core layout by
//! reference, home owns the feeds and each layout loads what it binds, and
//! the layout writes are checked.

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
    AppState, AuthPolicy, BoxFuture, CoreLayoutInfo, FeedSpec, HOME_APP, HOME_CORE_LAYOUT,
    Hydration, Pod, RequestCtx, Source, SourceManifest, StaticAdapter,
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
    app_id: HOME_CORE_LAYOUT,
    name: "0001",
    text: r#"{"note": "List", "layout": {"type": "core://Workspace", "id": "shell", "title": "Home",
      "slots": [{"slot_id": "list", "provider_uri": "local://inbox",
        "content": {"type": "core://Tappable", "content": {"type": "core://Text", "style": "Body", "value_expr": "Inbox"},
            "on_click": {"intent": "action.navigation.open", "layout": "a_home", "presentation": "Push"}}}]}}"#,
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
    mcp_actions: &[],
    entities: &[],
    dev: false,
};

struct A;

impl Source for A {
    fn name(&self) -> &'static str {
        "a"
    }
    fn core_layouts(&self) -> Vec<CoreLayoutInfo> {
        vec![CoreLayoutInfo {
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

fn shell() -> Arc<dyn Source> {
    let empty = WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: protocol::UiNode::Spacer { size_dp: None },
        entities: Default::default(),
        queries: Default::default(),
    };
    let info = CoreLayoutInfo {
        id: HOME_CORE_LAYOUT,
        title: "Home",
        icon: "dashboard",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[],
    };
    Arc::new(StaticAdapter::new(
        HOME_CORE_LAYOUT,
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

/// One MCP tool call: `(is_error, result)`.
async fn tool(pod: &Pod, name: &str, args: Value) -> (bool, Value) {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/mcp",
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": args}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = &body["result"];
    (r["isError"] == true, r["structuredContent"].clone())
}

/// A layout as a device draws it.
async fn layout(pod: &Pod, id: &str) -> (StatusCode, Value) {
    pod_core::testing::layout_for(&pod_core::app(pod.clone()), id, Some("pod")).await
}

#[tokio::test]
async fn a_new_pod_starts_with_home_as_the_whole_home_core_layout() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    let (status, root) = call(&pod, "GET", "/v1/construct", None).await;
    assert_eq!(status, StatusCode::OK, "{root}");
    let (_, home) = tool(&pod, "get_layout", json!({"id": HOME_APP})).await;
    assert_eq!(home["layout"]["ref"]["name"], "@core/shell", "{home}");
    assert_eq!(home["title"], "Home");
    assert_eq!(home["feeds"], json!([]));
    // Home draws the whole home core layout under its own id.
    let (status, ws) = layout(&pod, HOME_APP).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["workspace"]["type"], "core://Page", "{ws}");
    assert_eq!(ws["workspace"]["id"], HOME_APP, "{ws}");
    // Core layouts are drawn by their key too (notifications and links name
    // them).
    let (status, _) = layout(&pod, "a_home").await;
    assert_eq!(status, StatusCode::OK);
    // Home can't be deleted.
    let (err, b) = tool(&pod, "delete_layout", json!({"id": HOME_APP})).await;
    assert!(err, "{b}");
    assert_eq!(b["error"], "home_cannot_be_deleted");
    // Reopening moves nothing.
    let names = || {
        pod_core::construct::store::names(&rusqlite_conn(&dir.path().join("server.db"))).unwrap()
    };
    let before = names();
    let seq = |n: &str| {
        pod_core::construct::store::last_seq(&rusqlite_conn(&dir.path().join("server.db")), n)
            .unwrap()
    };
    let home_seq = seq(HOME_APP);
    drop(pod);
    let pod = open(&dir.path().join("server.db"));
    assert_eq!(names(), before);
    assert_eq!(seq(HOME_APP), home_seq);
    drop(pod);
}

fn rusqlite_conn(db: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(db).unwrap()
}

#[tokio::test]
async fn a_layout_can_be_a_whole_core_layout_by_reference() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    let whole = json!({"type": "core://Use", "ref": "@core/a_home"});
    let (err, b) = tool(
        &pod,
        "create_layout",
        json!({"title": "Mine", "icon": "photo", "feeds": [["a", "all"]], "layout": whole}),
    )
    .await;
    assert!(!err, "{b}");
    let id = b["id"].as_str().unwrap().to_owned();
    let (status, ws) = layout(&pod, &id).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["queries"]["a:all"], json!(["a://1"]), "{ws}");
    // The old spelling of a use is taken; the old reference node isn't.
    let (err, b) = tool(
        &pod,
        "create_layout",
        json!({"title": "Old", "layout": {"type": "core://Use", "component": "core:a_home"}}),
    )
    .await;
    assert!(!err, "{b}");
    let (err, b) = tool(
        &pod,
        "create_layout",
        json!({"title": "Older", "layout": {"type": "core://Page", "title": "P",
            "root": {"type": format!("core://{}", "Ref"), "app": "a_home", "part": "layout"}}}),
    )
    .await;
    assert!(err, "{b}");
    assert!(b.to_string().contains("unknown node type"), "{b}");
}

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
    let (err, made) = tool(
        &pod,
        "create_layout",
        json!({"title": "A list", "feeds": [["a", "all"]], "layout": list_of("a:all")}),
    )
    .await;
    assert!(!err, "{made}");
    let first = made["id"].as_str().unwrap().to_owned();
    let (_, home) = tool(&pod, "get_layout", json!({"id": HOME_APP})).await;
    assert_eq!(home["feeds"], json!([["a", "all"]]), "{home}");
    // Another layout binds them without naming any.
    let (err, made) = tool(
        &pod,
        "create_layout",
        json!({"title": "Again", "layout": list_of("a:all")}),
    )
    .await;
    assert!(!err, "{made}");
    let (_, ws) = layout(&pod, &first).await;
    assert_eq!(ws["queries"]["a:all"], json!(["a://1"]), "{ws}");
    // Home binds no feed of its own: nothing of a's loads with it, but it
    // still learns about its sources and the pod's theme.
    let (_, ws) = layout(&pod, HOME_APP).await;
    assert!(ws["queries"].get("a:all").is_none(), "{ws}");
    assert!(ws["entities"].get("core://theme").is_some(), "{ws}");
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
        {"type": "core://Layout", "layout": "a_home", "presentation": "Sheet", "show_when": "$form.inbox"},
        {"type": "core://Button", "label": "Open", "on_click": {"intent": "action.navigation.open",
            "layout": "a_home", "presentation": "Panel", "animation": "slide"}},
        {"type": "core://Button", "label": "Leave", "on_click": {"intent": "action.confirm",
            "title": "Leave this pod?", "confirm_label": "Leave",
            "then": {"intent": "action.device", "command": "leave_pod"}}}]}));
    let (err, b) = tool(&pod, "create_layout", ok).await;
    assert!(!err, "{b}");
    for (bad, says) in [
        (
            json!({"type": "core://Layout", "layout": "not a layout"}),
            "no layout 'not a layout'",
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
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.device", "command": "open_settings"}}),
            "open the layout with action.navigation.open",
        ),
        (
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.device", "command": "hide_layout", "layout": "home"}}),
            "device_layout_controls",
        ),
        (
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.device", "command": "show_versions", "layout": "home"}}),
            "unknown device command",
        ),
        (
            json!({"type": "core://Button", "label": "x", "on_click": {"intent": "action.confirm",
                "title": " ", "confirm_label": "Leave", "then": {"intent": "action.device", "command": "leave_pod"}}}),
            "needs a title and a confirm_label",
        ),
        (
            json!({"type": "core://AppFrame", "app": "home"}),
            "core://Layout",
        ),
    ] {
        let (err, b) = tool(&pod, "create_layout", with(bad)).await;
        assert!(err, "{b}");
        assert!(b.to_string().contains(says), "{says}: {b}");
    }
}

#[tokio::test]
async fn a_feed_belongs_to_the_layout_that_binds_it_else_home() {
    let dir = tempfile::tempdir().unwrap();
    let pod = open(&dir.path().join("server.db"));
    let owners = || pod_core::layouts::feed_owners(pod.store(), pod.catalog(), "a").unwrap();
    // Home binds no feed of a: nothing to own.
    assert!(owners().is_empty());
    // A layout that binds nothing of a's, made first, with the feed: home
    // binds it, but no layout shows it, so home owns it.
    let empty = json!({"type": "core://Workspace", "title": "E", "slots": [{"slot_id": "s",
        "provider_uri": "local://x", "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]});
    let (err, made) = tool(
        &pod,
        "create_layout",
        json!({"title": "Empty", "feeds": [["a", "all"]], "layout": empty}),
    )
    .await;
    assert!(!err, "{made}");
    assert_eq!(owners()["all"], HOME_APP);
    // The layouts that bind it: one of them owns it (the oldest; made in
    // the same second, they tie on age), the same one each time.
    let mut binders = Vec::new();
    for title in ["First", "Second"] {
        let (err, made) = tool(
            &pod,
            "create_layout",
            json!({"title": title, "layout": list_of("a:all")}),
        )
        .await;
        assert!(!err, "{made}");
        binders.push(made["id"].as_str().unwrap().to_owned());
    }
    let owner = owners()["all"].clone();
    assert!(binders.contains(&owner), "{owner} of {binders:?}");
    assert_eq!(owners()["all"], owner);
    assert_eq!(owners().len(), 1);
}

#[tokio::test]
async fn layout_writes_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let pod = Arc::new(
        AppState::new(
            Store::open(&dir.path().join("server.db")).unwrap(),
            vec![Arc::new(A), shell()],
        )
        .with_layout_caps(pod_core::layouts::Caps {
            layouts: 2,
            writes_per_minute: 3,
        }),
    );
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    let page = json!({"type": "core://Page", "title": "P", "root": {"type": "core://Spacer"}});
    let (err, b) = tool(
        &pod,
        "create_layout",
        json!({"title": "One", "layout": page}),
    )
    .await;
    assert!(!err, "{b}");
    // Home and one more: the cap.
    let (err, b) = tool(
        &pod,
        "create_layout",
        json!({"title": "Two", "layout": page}),
    )
    .await;
    assert!(err, "{b}");
    assert_eq!(b["error"], "too_many_layouts", "{b}");
    let (err, b) = tool(&pod, "rename_layout", json!({"id": HOME_APP, "title": "H"})).await;
    assert!(!err, "{b}");
    let (err, b) = tool(&pod, "rename_layout", json!({"id": HOME_APP, "title": "I"})).await;
    assert!(err, "{b}");
    assert_eq!(b["error"], "rate_limited", "{b}");
}

// ---------- a source that no longer exists (decision `remove-recipes-source`) ----------

/// A pod whose home binds `a/all` and whose layout is the whole `a_home`
/// core layout, reopened by a build without source `a`: it opens, serves
/// home, and home binds the feeds that remain.
#[tokio::test]
async fn a_removed_source_drops_out_of_home() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.db");
    let pod = open(&db);
    let whole = json!({"type": "core://Use", "ref": "@core/a_home"});
    let (err, made) = tool(
        &pod,
        "create_layout",
        json!({"title": "Mine", "feeds": [["a", "all"]], "layout": whole}),
    )
    .await;
    assert!(!err, "{made}");
    let id = made["id"].as_str().unwrap().to_owned();
    drop(pod);
    let pod = Arc::new(
        AppState::try_new(Store::open(&db).unwrap(), vec![shell()]).expect("opens without a"),
    );
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    // Home is served, and binds the feeds that remain.
    let (status, root) = call(&pod, "GET", "/v1/construct", None).await;
    assert_eq!(status, StatusCode::OK, "{root}");
    let feeds = pod
        .store()
        .with_conn(|c| Ok::<_, rusqlite::Error>(pod_core::construct::write::home_feeds(c)))
        .unwrap();
    assert!(feeds.is_empty(), "{feeds:?}");
    // The layout that showed a's core layout is still there.
    let (status, ws) = layout(&pod, &id).await;
    assert!(status != StatusCode::INTERNAL_SERVER_ERROR, "{ws}");
    // And layouts can still be written.
    let page = json!({"type": "core://Page", "title": "P", "root": {"type": "core://Spacer"}});
    let (err, b) = tool(
        &pod,
        "create_layout",
        json!({"title": "Plain", "layout": page}),
    )
    .await;
    assert!(!err, "{b}");
    drop(pod);
    // Reopening again changes nothing.
    let pod = AppState::try_new(Store::open(&db).unwrap(), vec![shell()]).expect("reopens");
    drop(pod);
}
