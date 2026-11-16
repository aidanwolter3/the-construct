//! The core with test adapters only (D40): the catalog's checks, a
//! layout-only blueprint, sign-in gating of a source's data, adapter routes
//! and migrations, logout.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use http_body_util::BodyExt;
use pod_core::components::ComponentSource;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::store::{Step, open_session_on};
use pod_core::{
    AppState, AuthPolicy, BlueprintInfo, BoxFuture, Catalog, FeedSpec, Hydration, Pod, RequestCtx,
    SignInProvider, Source, SourceManifest, StaticAdapter,
};
use protocol::{DataBundle, EntityRecord, WorkspaceSpec};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A minimal valid layout file for app `$id`.
macro_rules! layout {
    ($id:literal) => {
        concat!(
            r#"{"note": "First", "schema": 1, "layout": {"type": "core://Workspace", "id": ""#,
            $id,
            r#"", "title": "T", "slots": [{"slot_id": "main", "provider_uri": "core://test",
              "content": {"type": "core://Text", "style": "Body", "value_expr": "hi"}}]}}"#
        )
    };
}

static OPEN_FILES: &[LayoutSource] = &[LayoutSource {
    dir: "layouts",
    app_id: "open",
    name: "0001",
    text: layout!("open"),
}];
static GATED_FILES: &[LayoutSource] = &[LayoutSource {
    dir: "layouts",
    app_id: "gated",
    name: "0001",
    text: layout!("gated"),
}];

const OPEN: BlueprintInfo = BlueprintInfo {
    id: "open",
    title: "Open",
    icon: "photo",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[("open", "all")],
};
const GATED: BlueprintInfo = BlueprintInfo {
    id: "gated",
    title: "Gated",
    icon: "photo",
    dev: true,
    auth: AuthPolicy::SignIn("test"),
    feeds: &[("gated", "all")],
};

fn open_app() -> Arc<dyn Source> {
    let mut data = WorkspaceSpec {
        version: "1.0.0".into(),
        workspace: protocol::UiNode::Unknown(Value::Null),
        entities: Default::default(),
        queries: Default::default(),
    };
    data.entities
        .entry("core://test/x".into())
        .or_default()
        .set("n", 1);
    Arc::new(StaticAdapter::new("open", OPEN, OPEN_FILES, &data))
}

/// An adapter with a sign-in, an app that requires it, a route and a table.
struct Gated;

static GATED_MIGRATIONS: &[Step] = &[Step::sql(
    "gated_t",
    "CREATE TABLE gated_t (n INTEGER NOT NULL);",
)];

impl Source for Gated {
    fn name(&self) -> &'static str {
        "gated"
    }
    fn blueprints(&self) -> Vec<BlueprintInfo> {
        vec![GATED]
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        GATED_FILES
    }
    fn sign_ins(&self) -> Vec<SignInProvider> {
        vec![SignInProvider {
            id: "test",
            title: "Test service",
        }]
    }
    fn migrations(&self) -> &'static [Step] {
        GATED_MIGRATIONS
    }
    fn router(&self) -> Router<Pod> {
        Router::new().route(
            "/v1/gated/ping",
            get(
                |axum::extract::State(pod): axum::extract::State<Pod>| async move {
                    Json(json!({ "found": pod.adapter::<Gated>().is_some() }))
                },
            ),
        )
    }
    fn manifest(&self) -> SourceManifest {
        SourceManifest {
            feeds: &[FeedSpec {
                name: "all",
                query_keys: &["gated:all"],
                doc: "",
            }],
            sign_in: Some("test"),
            entity_schemes: &["gated"],
            ..SourceManifest::new("T")
        }
    }
    fn feed<'a>(
        &'a self,
        _pod: &'a Pod,
        _feed: &'a str,
        req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            let mut e = EntityRecord::default();
            e.set(
                "sign_ins",
                req.session.as_ref().map(|s| s.sign_ins.join(",")),
            );
            let mut data = DataBundle::default();
            data.entities.insert("gated://me".into(), e);
            Hydration::Data(data)
        })
    }
}

fn state() -> Arc<AppState> {
    Arc::new(AppState::in_memory(vec![open_app(), Arc::new(Gated)]).with_dev_apps(true))
}

async fn get_json(
    state: &Arc<AppState>,
    path: &str,
    bearer: Option<&str>,
) -> (StatusCode, Value, http::HeaderMap) {
    let mut req = Request::get(path);
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = pod_core::app(state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}

#[test]
fn the_catalog_refuses_bad_adapter_sets() {
    let err = |adapters: Vec<Arc<dyn Source>>| Catalog::new(adapters).err().unwrap();
    assert_eq!(
        err(vec![open_app(), open_app()]),
        "two adapters are named 'open'"
    );
    let other_name = Arc::new(StaticAdapter::new(
        "other",
        OPEN,
        OPEN_FILES,
        &WorkspaceSpec {
            version: "1.0.0".into(),
            workspace: protocol::UiNode::Unknown(Value::Null),
            entities: Default::default(),
            queries: Default::default(),
        },
    ));
    assert_eq!(
        err(vec![open_app(), other_name]),
        "blueprint 'open' is shipped by two adapters"
    );
    let needs_unknown = Arc::new(StaticAdapter::new(
        "gated",
        GATED,
        GATED_FILES,
        &WorkspaceSpec {
            version: "1.0.0".into(),
            workspace: protocol::UiNode::Unknown(Value::Null),
            entities: Default::default(),
            queries: Default::default(),
        },
    ));
    assert_eq!(
        err(vec![needs_unknown]),
        "app 'gated' requires sign-in 'test', which no adapter provides"
    );
    assert!(Catalog::new(vec![open_app(), Arc::new(Gated)]).is_ok());
    assert!(Catalog::empty().blueprints().next().is_none());
}

#[tokio::test]
async fn a_layout_only_blueprint_is_drawn_by_its_key() {
    let state = state();
    let router = pod_core::app(state.clone());
    let (status, body) = pod_core::testing::layout_for(&router, "open", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["workspace"]["id"], "open");
    assert_eq!(body["workspace"]["type"], "core://Page");
    // A bearer the pod doesn't know is told so.
    let (status, _) = pod_core::testing::layout_for(&router, "open", Some("stale")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = pod_core::testing::layout_for(&router, "nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// One MCP tool call with the pod token: its structured result.
async fn tool(state: &Arc<AppState>, name: &str, args: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": args}});
    let resp = pod_core::app(state.clone())
        .oneshot(
            Request::post("/v1/mcp")
                .header("authorization", "Bearer pod")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_ne!(v["result"]["isError"], true, "{v}");
    v["result"]["structuredContent"].clone()
}

static SHELL_FILES: &[LayoutSource] = &[LayoutSource {
    dir: "layouts",
    app_id: "shell",
    name: "0001",
    text: layout!("shell"),
}];

/// The home blueprint: a pod's home (and so its feeds) needs one.
fn shell() -> Arc<dyn Source> {
    let data = WorkspaceSpec {
        version: "1.0.0".into(),
        workspace: protocol::UiNode::Unknown(Value::Null),
        entities: Default::default(),
        queries: Default::default(),
    };
    let info = BlueprintInfo {
        id: "shell",
        title: "Home",
        icon: "dashboard",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[],
    };
    Arc::new(StaticAdapter::new("shell", info, SHELL_FILES, &data))
}

#[tokio::test]
async fn a_source_that_needs_a_sign_in_is_loaded_only_with_it() {
    let state = Arc::new(
        AppState::in_memory(vec![open_app(), Arc::new(Gated), shell()]).with_dev_apps(true),
    );
    let store = state.store();
    store.create_pinned_session(&session_hash("pod")).unwrap();
    store
        .transaction(|tx| open_session_on(tx, &session_hash("good"), "test", 86_400))
        .unwrap();
    store.create_session(&session_hash("bare")).unwrap();
    let made = tool(&state, "create_layout", json!({"title": "Gated", "feeds": [["gated", "all"]],
        "layout": {"type": "core://Page", "title": "G", "root": {"type": "core://List",
            "query_key": "gated:all",
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "${gated://me#sign_ins}"}}}})).await;
    let id = made["id"].as_str().unwrap().to_owned();
    let router = pod_core::app(state.clone());
    let layout = |bearer: Option<&'static str>| {
        let (router, id) = (router.clone(), id.clone());
        async move { pod_core::testing::layout_for(&router, &id, bearer).await }
    };
    // No bearer, or a session without the sign-in (D46: it keeps its
    // session): the layout without the source's data, which says so.
    for bearer in [None, Some("bare")] {
        let (status, body) = layout(bearer).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["entities"].get("gated://me").is_none(), "{body}");
        assert_eq!(
            body["entities"]["core://sources/gated"]["signed_in"], false,
            "{body}"
        );
    }
    // A stale bearer: 401, so the client drops it.
    let (status, _) = layout(Some("stale")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Signed in: the source's data; it saw the session.
    let (status, body) = layout(Some("good")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["entities"]["gated://me"]["sign_ins"], "test");
    assert_eq!(
        body["entities"]["core://sources/gated"]["signed_in"], true,
        "{body}"
    );
}

#[tokio::test]
async fn adapter_routes_lookups_and_migrations() {
    let state = state();
    let (status, body, _) = get_json(&state, "/v1/gated/ping", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["found"], true);
    assert!(state.adapter::<Gated>().is_some());
    assert!(state.adapter::<StaticAdapter>().is_some());
    assert_eq!(state.store().adapter_version("gated").unwrap(), 1);
    assert_eq!(state.store().adapter_version("open").unwrap(), 0);
}

#[tokio::test]
async fn logout_ends_the_session_and_its_sign_ins() {
    let state = state();
    let hash = session_hash("good");
    state
        .store()
        .transaction(|tx| open_session_on(tx, &hash, "test", 86_400))
        .unwrap();
    let logout = |bearer: &'static str| {
        let state = state.clone();
        async move {
            pod_core::app(state)
                .oneshot(
                    Request::post("/v1/auth/logout")
                        .header("authorization", format!("Bearer {bearer}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(logout("good").await, StatusCode::OK);
    assert!(state.store().session_sign_ins(&hash).unwrap().is_empty());
    assert_eq!(logout("good").await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sign_ins_lists_the_session_sign_ins() {
    let state = state();
    let store = state.store();
    store
        .transaction(|tx| open_session_on(tx, &session_hash("good"), "test", 86_400))
        .unwrap();
    store.create_session(&session_hash("bare")).unwrap();
    // A sign-in no adapter provides any more is left out.
    store
        .attach_sign_in(&session_hash("good"), "retired")
        .unwrap();
    let (status, body, _) = get_json(&state, "/v1/auth/sign-ins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "sign_ins": [],
                "providers": [{ "id": "test", "title": "Test service", "signed_in": false }] })
    );
    let (status, body, _) = get_json(&state, "/v1/auth/sign-ins", Some("bare")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sign_ins"], json!([]));
    let (status, _, _) = get_json(&state, "/v1/auth/sign-ins", Some("stale")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The default account lookup knows nothing: title and apps only.
    let (status, body, _) = get_json(&state, "/v1/auth/sign-ins", Some("good")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "sign_ins": [{
            "id": "test",
            "title": "Test service",
            "apps": [{ "id": "gated", "title": "Gated" }],
        }], "providers": [{ "id": "test", "title": "Test service", "signed_in": true }] })
    );
    // A dev blueprint is listed only when dev apps are shown.
    let hidden = Arc::new(AppState::in_memory(vec![open_app(), Arc::new(Gated)]));
    hidden
        .store()
        .transaction(|tx| open_session_on(tx, &session_hash("good"), "test", 86_400))
        .unwrap();
    let (_, body, _) = get_json(&hidden, "/v1/auth/sign-ins", Some("good")).await;
    assert_eq!(body["sign_ins"][0]["apps"], json!([]));
}

/// The core knows no source (D40): outside comments, no file of its names one.
/// `store.rs` is the exception: migrations v2-v4 and v11 are frozen history
/// that created and backfilled the Pixelfed tables.
#[test]
fn the_core_names_no_source() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut dirs = vec![src.clone()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    for path in files {
        let name = path
            .strip_prefix(&src)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        // The core's migration steps are frozen history (the Pixelfed
        // tables of the first builds).
        if name == "store.rs" || name == "migrations/core.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            // `std::collections` is not the Collections source.
            let code = code.replace("std::collections", "");
            for app in ["pixelfed", "collections"] {
                assert!(
                    !code.to_ascii_lowercase().contains(app),
                    "src/{name}:{}: the core must not name a source: {line}",
                    i + 1
                );
            }
        }
    }
}

/// An adapter that ships server components and nothing else.
struct Shipping(&'static [ComponentSource]);

impl Source for Shipping {
    fn name(&self) -> &'static str {
        "shipping"
    }
    fn blueprints(&self) -> Vec<BlueprintInfo> {
        Vec::new()
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        &[]
    }
    fn component_files(&self) -> &'static [ComponentSource] {
        self.0
    }
    fn manifest(&self) -> SourceManifest {
        SourceManifest::new("T")
    }
    fn feed<'a>(
        &'a self,
        _pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move { pod_core::unknown_feed("shipping", feed) })
    }
}

/// A component file of `crates/source-test/components`.
macro_rules! component {
    ($name:literal, $text:literal) => {
        ComponentSource {
            dir: "crates/source-test/components",
            name: $name,
            file: "0001",
            text: concat!(
                r#"{"note": "First", "component": {"doc": "A greeting.", "params": [],
                  "template": {"type": "core://Text", "style": "Body", "value_expr": ""#,
                $text,
                r#""}, "examples": [{"args": {}, "note": "It."}]}}"#
            ),
        }
    };
}

static GREETING: &[ComponentSource] = &[component!("test_greeting", "hi")];
static TAKEN: &[ComponentSource] = &[component!("card", "not the core's")];

/// Issue #135: an adapter's server components are named with the core's
/// (`@core/<name>`); a name another crate ships stops the start.
#[test]
fn adapters_ship_server_components() {
    let state = AppState::try_new(
        pod_core::store::Store::open_in_memory().unwrap(),
        vec![Arc::new(Shipping(GREETING))],
    )
    .unwrap();
    let el = state
        .store()
        .with_conn(|c| pod_core::construct::store::name(c, "@core/test_greeting"))
        .unwrap()
        .expect("named");
    let (def, _) = state
        .store()
        .with_conn(|c| pod_core::construct::store::object(c, &el))
        .unwrap()
        .unwrap();
    assert_eq!(def["node"]["value_expr"], "hi");
    let err = Catalog::new(vec![Arc::new(Shipping(TAKEN))]).err().unwrap();
    assert_eq!(
        err,
        "component 'card' is in both crates/pod-core/components and crates/source-test/components; \
         component names are unique"
    );
}
