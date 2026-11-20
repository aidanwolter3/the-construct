//! Data sources (decision `custom-apps-and-data-sources`): the catalog's
//! checks of manifests and core layout feeds, the route check of layout files,
//! a layout's data composed of several sources' feeds (home's), and data
//! events by source on the construct's stream.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::store::open_session_on;
use pod_core::{
    AppState, AuthPolicy, BoxFuture, Catalog, CoreLayoutInfo, FeedSpec, Hydration, PathSpec, Pod,
    RequestCtx, Session, SignInProvider, Source, SourceManifest,
};
use protocol::{DataBundle, EntityRecord, WorkspaceSpec};
use serde_json::Value;
use tower::ServiceExt;

/// A layout file for app `id` whose one button calls `endpoint` and whose
/// screen loads `data`.
fn layout(id: &'static str, endpoint: &str, data: &str) -> LayoutSource {
    let text = format!(
        r#"{{"note": "First", "layout": {{"type": "core://Workspace", "id": "{id}",
          "title": "T",
          "screens": [{{"id": "s", "route": "{id}://things/{{id}}", "title": "S", "data": "{data}",
            "content": {{"type": "core://Text", "style": "Body", "value_expr": "hi"}}}}],
          "slots": [{{"slot_id": "main", "provider_uri": "core://test",
          "content": {{"type": "core://Button", "label": "Go", "on_click": {{
            "intent": "action.mutation.optimistic", "entity_uri": "{id}://x", "field": "n",
            "value_expr": "1", "rollback_value": 0, "endpoint": "{endpoint}",
            "payload": {{}}}}}}}}]}}}}"#
    );
    LayoutSource {
        dir: "layouts",
        app_id: id,
        name: "0001",
        text: Box::leak(text.into_boxed_str()),
    }
}

fn leak<T>(v: Vec<T>) -> &'static [T] {
    Box::leak(v.into_boxed_slice())
}

/// What a test source's feed answers.
#[derive(Clone, Copy)]
enum Answer {
    /// One entity `<scheme>://1` and the query `<scheme>:all`.
    Data,
    /// The sign-in is gone upstream.
    Stale,
}

/// A configurable source: one feed `all`, `POST /v1/<name>/act`, `GET
/// /v1/<name>/screen?id={id}`, scheme `<name>`.
struct Src {
    name: &'static str,
    sign_in: Option<&'static str>,
    provides: Option<&'static str>,
    apps: Vec<CoreLayoutInfo>,
    files: &'static [LayoutSource],
    answer: Answer,
    schemes: &'static [&'static str],
    keys: &'static [&'static str],
}

impl Src {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            sign_in: None,
            provides: None,
            apps: Vec::new(),
            files: &[],
            answer: Answer::Data,
            schemes: leak(vec![name]),
            keys: leak(vec![&*Box::leak(format!("{name}:all").into_boxed_str())]),
        }
    }

    /// Serve app `id` (public) whose root lists `feeds`, with a layout that
    /// uses this source's routes.
    fn app(mut self, id: &'static str, feeds: &[(&'static str, &'static str)]) -> Self {
        self.apps.push(CoreLayoutInfo {
            id,
            title: "T",
            icon: "photo",
            dev: false,
            auth: AuthPolicy::Public,
            feeds: leak(feeds.to_vec()),
        });
        let (act, screen) = (
            format!("/v1/{}/act", self.name),
            format!("/v1/{}/screen?id=$route.id", self.name),
        );
        let mut files = self.files.to_vec();
        files.push(layout(id, &act, &screen));
        self.files = leak(files);
        self
    }
}

impl Source for Src {
    fn name(&self) -> &'static str {
        self.name
    }
    fn core_layouts(&self) -> Vec<CoreLayoutInfo> {
        self.apps.clone()
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        self.files
    }
    fn sign_ins(&self) -> Vec<SignInProvider> {
        self.provides
            .map(|id| SignInProvider { id, title: "Test" })
            .into_iter()
            .collect()
    }
    fn manifest(&self) -> SourceManifest {
        let pattern = |p: String| &*Box::leak(p.into_boxed_str());
        SourceManifest {
            sign_in: self.sign_in,
            entity_schemes: self.schemes,
            feeds: leak(vec![FeedSpec {
                name: "all",
                query_keys: self.keys,
                doc: "",
            }]),
            data_paths: leak(vec![PathSpec {
                pattern: pattern(format!("/v1/{}/screen?id={{id}}", self.name)),
                query_keys: &[],
                doc: "",
            }]),
            actions: leak(vec![PathSpec {
                pattern: pattern(format!("/v1/{}/act", self.name)),
                query_keys: &[],
                doc: "",
            }]),
            ..SourceManifest::new("T")
        }
    }
    fn feed<'a>(&'a self, _: &'a Pod, _: &'a str, _: &'a RequestCtx) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            match self.answer {
                Answer::Data => {
                    let mut data = DataBundle::default();
                    let uri = format!("{}://1", self.name);
                    let mut e = EntityRecord::default();
                    e.set("from", self.name);
                    data.entities.insert(uri.clone(), e);
                    data.queries.insert(format!("{}:all", self.name), vec![uri]);
                    Hydration::Data(data)
                }
                Answer::Stale => Hydration::StaleSession(WorkspaceSpec {
                    version: "1.0.0".into(),
                    workspace: protocol::UiNode::Unknown(Value::Null),
                    entities: Default::default(),
                    queries: Default::default(),
                }),
            }
        })
    }
}

fn err(adapters: Vec<Src>) -> String {
    let adapters: Vec<Arc<dyn Source>> = adapters
        .into_iter()
        .map(|a| Arc::new(a) as Arc<dyn Source>)
        .collect();
    match Catalog::new(adapters) {
        Ok(_) => "ok".into(),
        Err(e) => e,
    }
}

#[test]
fn the_catalog_checks_sources_and_feeds() {
    assert_eq!(err(vec![Src::new("a").app("a", &[("a", "all")])]), "ok");
    assert_eq!(
        err(vec![Src::new("a").app("a", &[("b", "all")])]),
        "app 'a' lists feed 'b/all', but there is no source 'b'"
    );
    assert_eq!(
        err(vec![Src::new("a").app("a", &[("a", "home")])]),
        "app 'a' lists feed 'a/home', which source 'a' does not declare"
    );
    assert_eq!(
        err(vec![Src::new("a").app("a", &[("a", "all"), ("a", "all")])]),
        "app 'a' lists feed 'a/all' twice"
    );
    let mut b = Src::new("b");
    b.schemes = &["a"];
    assert_eq!(
        err(vec![Src::new("a"), b]),
        "sources 'a' and 'b' both claim the entity scheme 'a'"
    );
    let mut b = Src::new("b");
    b.keys = &["{x}:all"];
    assert_eq!(
        err(vec![Src::new("a"), b]),
        "query key '{x}:all' of source 'b' overlaps 'a:all' of source 'a'"
    );
    let mut core = Src::new("a");
    core.schemes = &["core"];
    assert_eq!(
        err(vec![core]),
        "source 'a': the entity scheme 'core' is reserved"
    );
    let mut signed = Src::new("a");
    signed.sign_in = Some("nobody");
    assert_eq!(
        err(vec![signed]),
        "source 'a' needs sign-in 'nobody', which no adapter provides"
    );
    // Composing two sources is fine.
    assert_eq!(
        err(vec![
            Src::new("a").app("ab", &[("a", "all"), ("b", "all")]),
            Src::new("b")
        ]),
        "ok"
    );
}

#[test]
fn a_layout_file_may_only_use_its_sources_routes() {
    let start = |adapters: Vec<Src>| {
        let adapters = adapters
            .into_iter()
            .map(|a| Arc::new(a) as Arc<dyn Source>)
            .collect();
        AppState::try_new(pod_core::store::Store::open_in_memory().unwrap(), adapters).map(|_| ())
    };
    // Its own adapter's routes, and those of the sources its feeds name.
    assert_eq!(start(vec![Src::new("a").app("a", &[])]), Ok(()));
    let mut a = Src::new("a");
    a.apps.push(CoreLayoutInfo {
        id: "x",
        title: "T",
        icon: "photo",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[("b", "all")],
    });
    a.files = leak(vec![layout("x", "/v1/b/act", "/v1/b/screen?id=$route.id")]);
    assert_eq!(start(vec![a, Src::new("b")]), Ok(()));
    // Another source's action, without a feed of it: refused, naming what
    // is offered.
    let mut a = Src::new("a");
    a.apps.push(CoreLayoutInfo {
        id: "x",
        title: "T",
        icon: "photo",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[],
    });
    a.files = leak(vec![layout("x", "/v1/b/act", "/v1/a/screen?id=$route.id")]);
    let e = start(vec![a, Src::new("b")]).unwrap_err();
    assert_eq!(
        e,
        "layouts/x/0001.json: invalid layout: a mutation's endpoint '/v1/b/act' is not an \
         action of its data sources (a); they offer: /v1/a/act"
    );
    // A screen data path no source declares.
    let mut a = Src::new("a");
    a.apps.push(CoreLayoutInfo {
        id: "x",
        title: "T",
        icon: "photo",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[],
    });
    a.files = leak(vec![layout(
        "x",
        "/v1/a/act",
        "/v1/a/screen?slug=$route.id",
    )]);
    let e = start(vec![a]).unwrap_err();
    assert!(
        e.contains("screen 's' data '/v1/a/screen?slug=$route.id' is not a data path"),
        "{e}"
    );
}

/// A pod over a public source `a` (which ships the home core layout) and a
/// source `b` that needs sign-in `test` (answering `b_answer`), with a
/// layout that lists both feeds (home binds them): the pod and the
/// layout's id.
async fn composed(b_answer: Answer) -> (Pod, String) {
    let mut b = Src::new("b");
    b.sign_in = Some("test");
    b.provides = Some("test");
    b.answer = b_answer;
    let a = Src::new("a").app("shell", &[]);
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(a), Arc::new(b)]));
    let id = bind(&pod, &["a", "b"]).await;
    (pod, id)
}

/// A new layout listing `<source>:all` of each source (home binds them):
/// its id. Written with the pod token ("pod").
async fn bind(pod: &Pod, sources: &[&str]) -> String {
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    let lists: Vec<Value> = sources
        .iter()
        .map(|s| {
            serde_json::json!({"type": "core://List", "query_key": format!("{s}:all"),
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.from"}})
        })
        .collect();
    let feeds: Vec<Value> = sources
        .iter()
        .map(|s| serde_json::json!([s, "all"]))
        .collect();
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "create_layout", "arguments": {"title": "Both", "feeds": feeds,
            "layout": {"type": "core://Page", "title": "Both",
                "root": {"type": "core://Column", "children": lists}}}}});
    let resp = pod_core::app(pod.clone())
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
    v["result"]["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn drawn(pod: &Pod, id: &str, bearer: Option<&str>) -> (StatusCode, Value) {
    pod_core::testing::layout_for(&pod_core::app(pod.clone()), id, bearer).await
}

#[tokio::test]
async fn a_layout_merges_its_feeds_and_skips_signed_out_sources() {
    let (pod, id) = composed(Answer::Data).await;
    pod.store()
        .transaction(|tx| open_session_on(tx, &session_hash("good"), "test", 86_400))
        .unwrap();
    pod.store().create_session(&session_hash("bare")).unwrap();
    for bearer in [None, Some("bare")] {
        let (status, body) = drawn(&pod, &id, bearer).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entities"]["a://1"]["from"], "a", "{bearer:?}");
        assert!(body["entities"].get("b://1").is_none(), "{bearer:?}");
        assert_eq!(body["queries"]["a:all"][0], "a://1");
    }
    let (status, body) = drawn(&pod, &id, Some("good")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entities"]["a://1"]["from"], "a");
    assert_eq!(body["entities"]["b://1"]["from"], "b");
    assert_eq!(body["queries"]["b:all"][0], "b://1");
}

#[tokio::test]
async fn a_stale_sign_in_of_one_feed_is_handled_like_before() {
    let (pod, id) = composed(Answer::Stale).await;
    let store = pod.store();
    store
        .transaction(|tx| open_session_on(tx, &session_hash("good"), "test", 86_400))
        .unwrap();
    // An ordinary session: 401, the client drops it.
    let (status, _) = drawn(&pod, &id, Some("good")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A pod token's session stays; only the source's sign-in goes.
    let pinned = session_hash("pinned");
    store.create_pinned_session(&pinned).unwrap();
    store.attach_sign_in(&pinned, "test").unwrap();
    let (status, body) = drawn(&pod, &id, Some("pinned")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(store.session_sign_ins(&pinned).unwrap().is_empty());
    // Signed out of b now: a's data alone.
    let (status, body) = drawn(&pod, &id, Some("pinned")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entities"]["a://1"]["from"], "a");
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
async fn data_events_reach_the_construct_stream_for_homes_sources() {
    let a = Src::new("a").app("shell", &[]);
    let b = Src::new("b");
    let c = Src::new("c");
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(a),
        Arc::new(b),
        Arc::new(c),
    ]));
    // Home binds a's and b's feeds; nothing binds c's.
    bind(&pod, &["a", "b"]).await;
    pod.store().create_session(&session_hash("alice")).unwrap();
    let mut s = pod_core::app(pod.clone())
        .oneshot(
            Request::get("/v1/construct/events")
                .header("authorization", "Bearer alice")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body();
    assert!(
        sse_text(&mut s, Duration::from_millis(200))
            .await
            .contains("event: construct")
    );
    let alice = Session {
        token_hash: session_hash("alice"),
        sign_ins: Vec::new(),
        pinned: false,
    };
    for (source, expected) in [("b", true), ("c", false)] {
        pod.publish_data(source, &alice);
        let text = sse_text(&mut s, Duration::from_millis(300)).await;
        assert_eq!(text.contains("event: data"), expected, "{source}: {text}");
        if expected {
            // The wire event is unchanged: it names home.
            assert!(text.contains(r#"{"workspace_id":"home"}"#), "{text}");
        }
    }
}
