//! The core with test adapters only (D40): the catalog's checks, a
//! layout-only app, sign-in gating, adapter routes and migrations, logout.

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
    AppAdapter, AppInfo, AppState, AuthPolicy, BoxFuture, Catalog, FeedSpec, Hydration, Pod,
    RequestCtx, SignInProvider, SourceManifest, StaticAdapter,
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

const OPEN: AppInfo = AppInfo {
    id: "open",
    title: "Open",
    icon: "photo",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[("open", "all")],
};
const GATED: AppInfo = AppInfo {
    id: "gated",
    title: "Gated",
    icon: "photo",
    dev: true,
    auth: AuthPolicy::SignIn("test"),
    feeds: &[("gated", "all")],
};

fn open_app() -> Arc<dyn AppAdapter> {
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

impl AppAdapter for Gated {
    fn name(&self) -> &'static str {
        "gated"
    }
    fn apps(&self) -> Vec<AppInfo> {
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
                query_keys: &[],
                doc: "",
            }],
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
    let err = |adapters: Vec<Arc<dyn AppAdapter>>| Catalog::new(adapters).err().unwrap();
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
        "app 'open' is served by two adapters"
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
    assert!(Catalog::empty().apps().next().is_none());
}

#[tokio::test]
async fn a_layout_only_app_is_served_and_listed() {
    let state = state();
    let (status, body, headers) = get_json(&state, "/v1/workspaces/open", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-construct-layout-version"], "1");
    assert_eq!(body["workspace"]["id"], "open");
    assert_eq!(body["entities"]["core://test/x"]["n"], 1);
    // A bearer that is not a session is ignored by a public app.
    let (status, _, _) = get_json(&state, "/v1/workspaces/open", Some("stale")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, apps, _) = get_json(&state, "/v1/apps?blueprints=1", None).await;
    let ids: Vec<&str> = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["open", "gated"]);
    let (status, _, _) = get_json(&state, "/v1/workspaces/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_app_that_requires_a_sign_in_is_served_only_with_it() {
    let state = state();
    let store = state.store();
    store
        .transaction(|tx| open_session_on(tx, &session_hash("good"), "test", 86_400))
        .unwrap();
    store.create_session(&session_hash("bare")).unwrap();
    // No bearer: the signed-out screen, 200, no layout version.
    let (status, body, headers) = get_json(&state, "/v1/workspaces/gated", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["workspace"]["id"], "gated");
    assert!(body["entities"].as_object().is_none_or(|e| e.is_empty()));
    assert!(!headers.contains_key("x-construct-layout-version"));
    // A stale bearer: 401 + the screen, so the client drops it.
    let (status, body, headers) = get_json(&state, "/v1/workspaces/gated", Some("stale")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["x-construct-session"], "invalid");
    assert_eq!(body["workspace"]["id"], "gated");
    // A valid session without the sign-in keeps its session (D46): 200 + the screen.
    let (status, body, headers) = get_json(&state, "/v1/workspaces/gated", Some("bare")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key("x-construct-session"));
    assert_eq!(body["workspace"]["id"], "gated");
    let (status, _, _) = get_json(&state, "/v1/workspaces/gated/events", Some("stale")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "stale events");
    // Signed in: the adapter's data under the layout; it saw the session.
    let (status, body, headers) = get_json(&state, "/v1/workspaces/gated", Some("good")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-construct-layout-version"], "1");
    assert_eq!(body["entities"]["gated://me"]["sign_ins"], "test");
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
    // A dev app is listed only when dev apps are shown, like `GET /v1/apps`.
    let hidden = Arc::new(AppState::in_memory(vec![open_app(), Arc::new(Gated)]));
    hidden
        .store()
        .transaction(|tx| open_session_on(tx, &session_hash("good"), "test", 86_400))
        .unwrap();
    let (_, body, _) = get_json(&hidden, "/v1/auth/sign-ins", Some("good")).await;
    assert_eq!(body["sign_ins"][0]["apps"], json!([]));
}

/// The core knows no app (D40): outside comments, no source file names one.
/// `store.rs` is the exception: migrations v2-v4 and v11 are frozen history
/// that created and backfilled the Pixelfed tables.
#[test]
fn the_core_names_no_app() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if name == "store.rs" {
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
            for app in ["pixelfed", "recipes", "collections"] {
                assert!(
                    !code.to_ascii_lowercase().contains(app),
                    "src/{name}:{}: the core must not name an app: {line}",
                    i + 1
                );
            }
        }
    }
}

/// An adapter that ships server components and nothing else.
struct Shipping(&'static [ComponentSource]);

impl AppAdapter for Shipping {
    fn name(&self) -> &'static str {
        "shipping"
    }
    fn apps(&self) -> Vec<AppInfo> {
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

/// A component file of `crates/app-test/components`.
macro_rules! component {
    ($name:literal, $text:literal) => {
        ComponentSource {
            dir: "crates/app-test/components",
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

/// Issue #135: an adapter's server components are recorded with the core's
/// as `core:<name>`; a name another crate ships stops the start.
#[test]
fn adapters_ship_server_components() {
    let state = AppState::try_new(
        pod_core::store::Store::open_in_memory().unwrap(),
        vec![Arc::new(Shipping(GREETING))],
    )
    .unwrap();
    let (version, def) = pod_core::components::latest(state.store(), "core:test_greeting")
        .unwrap()
        .expect("recorded");
    assert_eq!(version, 1);
    assert_eq!(def["template"]["value_expr"], "hi");
    let err = Catalog::new(vec![Arc::new(Shipping(TAKEN))]).err().unwrap();
    assert_eq!(
        err,
        "component 'card' is in both crates/pod-core/components and crates/app-test/components; \
         component names are unique"
    );
}
