//! The engine on the content-addressed construct (decisions
//! `content-addressed-construct`, `one-layout-system`), against the real
//! pod: it is current when one hash matches, an edit to one component
//! fetches only the objects that changed, another layout is opened and
//! followed, and leaving the pod forgets it.

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use client_core::{ConstructEngine, EngineConfig};
use serde_json::{Value, json};
use server::AppState;

const TIMEOUT: Duration = Duration::from_secs(15);

struct Pod {
    state: Arc<AppState>,
    base: String,
    rt: tokio::runtime::Runtime,
}

fn pod() -> Pod {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let state = Arc::new(server::state_in_memory());
    state
        .store()
        .with_conn(source_collections::test_seed::seed_test_dishes)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = server::app(Arc::clone(&state));
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    Pod { state, base, rt }
}

fn engine(base: &str) -> ConstructEngine {
    ConstructEngine::init_with_config(
        "",
        base,
        "home",
        EngineConfig {
            poll_interval: Duration::from_millis(100),
            backoff_base: Duration::from_millis(20),
            backoff_cap: Duration::from_millis(200),
        },
    )
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn tile(prefix: &str) -> Value {
    json!({"doc": "A labelled tile.", "params": [{"name": "label", "type": "text", "required": true}],
        "template": {"type": "core://Text", "style": "Title", "value_expr": format!("{prefix}$arg.label")},
        "examples": [{"args": {"label": "A"}}]})
}

/// An MCP layout tool on the pod; its answer.
fn tool(p: &Pod, name: &str, args: Value) -> Value {
    let state = p.state.clone();
    let name = name.to_owned();
    let r =
        p.rt.block_on(async move { server::construct::write::mcp(&state, &name, &args).await });
    match r {
        Some(Ok(v)) => v,
        other => panic!("{other:?}"),
    }
}

/// A new layout made with `create_layout`: its id.
fn create(p: &Pod, args: Value) -> String {
    tool(p, "create_layout", args)["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Home links layout A and shows a tile; A shows a tile too; B uses none.
fn build(p: &Pod) -> (String, String) {
    tool(
        p,
        "define_component",
        json!({"name": "tile", "component": tile("")}),
    );
    let b = create(
        p,
        json!({"title": "B", "layout": {"type": "core://Page",
        "title": "B", "root": {"type": "core://Text", "style": "Body", "value_expr": "Plain"}}}),
    );
    let a = create(
        p,
        json!({"title": "A", "layout": {"type": "core://Page",
        "title": "A", "root": {"type": "core://Column", "children": [
            {"type": "core://Use", "component": "pod:tile", "args": {"label": "Inside A"}},
            {"type": "core://Button", "label": "B",
             "on_click": {"intent": "action.navigation.open", "layout": b, "presentation": "Push"}}]}}}),
    );
    tool(
        p,
        "update_layout",
        json!({"app": "home", "layout": {"type": "core://Page",
        "title": "Home", "root": {"type": "core://Column", "children": [
            {"type": "core://Use", "component": "pod:tile", "args": {"label": "Home"}},
            {"type": "core://Button", "label": "A",
             "on_click": {"intent": "action.navigation.open", "layout": a, "presentation": "Push"}}]}}}),
    );
    (a, b)
}

fn home_json(e: &ConstructEngine) -> String {
    e.snapshot().workspace_json
}

#[test]
fn one_hash_says_the_device_is_current_and_an_edit_fetches_only_what_changed() {
    let p = pod();
    let (a, b) = build(&p);
    let e = engine(&p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().root.is_some() && home_json(&e).contains("Home")
    });
    let first = e.construct_status();
    let root = first.root.clone().unwrap();
    assert!(root.starts_with("e1-"), "{root}");
    let all = first.fetched[0];
    // Home, the tile, A and B: everything the root reaches.
    assert_eq!(all, 4, "every object from the root: {first:?}");
    // The edge graph the renderer cuts its stack by.
    let graph: Value = serde_json::from_str(&e.snapshot().construct_json).unwrap();
    assert_eq!(graph["root"], json!(root));
    assert!(
        graph["reach"]["home"]
            .as_array()
            .unwrap()
            .contains(&json!(a)),
        "{graph}"
    );
    assert!(
        graph["reach"][&a].as_array().unwrap().contains(&json!(b)),
        "{graph}"
    );

    // A layout opened is its element, materialized on the device.
    e.open_layout(&a);
    wait_until("A drawn", || {
        e.layout_now(&a)
            .root
            .is_some_and(|r| serde_json::to_string(&r).unwrap().contains("Inside A"))
    });

    // One component edit: the device fetches the tile and the two layouts
    // that use it (A, home), and nothing else.
    let state = p.state.clone();
    let r = p.rt.block_on(async move {
        server::construct::write::mcp(
            &state,
            "update_component",
            &json!({"name": "tile", "component": tile("› ")}),
        )
        .await
    });
    assert!(r.as_ref().is_some_and(Result::is_ok), "{r:?}");
    wait_until("the new root", || {
        let s = e.construct_status();
        s.root.as_deref() != Some(root.as_str()) && s.fetched.len() >= 2
    });
    let now = e.construct_status();
    assert_eq!(now.fetched.last(), Some(&3), "{now:?}");
    wait_until("home redrawn", || home_json(&e).contains("› Home"));
    wait_until("A redrawn", || {
        e.layout_now(&a)
            .root
            .is_some_and(|r| serde_json::to_string(&r).unwrap().contains("› Inside A"))
    });

    // Current: the same hash again changes nothing.
    let before = e.construct_status();
    let again = server::construct::seal::root_for(
        &p.state,
        &protocol::ClientCaps::from_header(&protocol::features_header()),
    )
    .unwrap();
    assert_eq!(again, before.root);
}

fn engine_at(dir: &std::path::Path, base: &str) -> ConstructEngine {
    ConstructEngine::init_with_config(
        dir.to_str().unwrap(),
        base,
        "home",
        EngineConfig {
            poll_interval: Duration::from_millis(100),
            backoff_base: Duration::from_millis(20),
            backoff_cap: Duration::from_millis(200),
        },
    )
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "construct-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Issue #301: the objects a device fetched are kept in its store, so after
/// a restart with the root unchanged nothing is fetched again.
#[test]
fn after_a_restart_no_object_is_fetched_again_when_the_root_is_unchanged() {
    let p = pod();
    build(&p);
    let dir = temp_dir("restart");
    let e = engine_at(&dir, &p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().root.is_some() && home_json(&e).contains("Home")
    });
    assert_eq!(e.construct_status().fetched.first(), Some(&4));
    let root = e.construct_status().root;
    drop(e);

    let again = engine_at(&dir, &p.base);
    wait_until("home drawn again", || {
        again.construct_status().root.is_some() && !again.construct_status().fetched.is_empty()
    });
    let s = again.construct_status();
    assert_eq!(s.root, root);
    assert!(
        s.fetched.iter().all(|n| *n == 0),
        "nothing refetched: {s:?}"
    );
    wait_until("home drawn", || home_json(&again).contains("Home"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #301: a layout home doesn't reach (a notification's) is opened by
/// name and drawn off the tree.
#[test]
fn a_layout_the_root_doesnt_reach_is_opened_by_name() {
    let p = pod();
    build(&p);
    let lone = create(
        &p,
        json!({"title": "Lone", "layout": {"type": "core://Page",
        "title": "Lone", "root": {"type": "core://Text", "style": "Body", "value_expr": "Off the tree"}}}),
    );
    let e = engine(&p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().root.is_some() && home_json(&e).contains("Home")
    });
    let graph: Value = serde_json::from_str(&e.snapshot().construct_json).unwrap();
    assert!(graph["reach"].get(&lone).is_none(), "{graph}");
    e.open_layout(&lone);
    wait_until("the lone layout drawn", || {
        e.layout_now(&lone)
            .root
            .is_some_and(|r| serde_json::to_string(&r).unwrap().contains("Off the tree"))
    });
}

/// Issue #301: one layout embedded twice, each use site binding its inputs
/// differently, draws each binding (and loads the data each binds).
#[test]
fn an_embedded_layout_draws_what_its_use_site_binds() {
    let p = pod();
    build(&p);
    let state = p.state.clone();
    p.rt.block_on(async move {
        let shelf = server::construct::write::mcp(&state, "create_layout", &json!({"title": "Shelf",
            "feeds": [["collections", "dishes"]],
            "layout": {"type": "core://Page", "title": "Shelf", "root": {"type": "core://Column", "children": [
                {"type": "core://Text", "style": "Title", "value_expr": "$in.label"},
                {"type": "core://List", "query_key": "$in.items",
                    "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}]}}}))
            .await
            .unwrap()
            .unwrap();
        let shelf = shelf["id"].as_str().unwrap().to_owned();
        let r = server::construct::write::mcp(&state, "update_layout", &json!({"app": "home",
            "layout": {"type": "core://Page", "title": "Home", "root": {"type": "core://Column", "children": [
                {"type": "core://Text", "style": "Title", "value_expr": "Home"},
                {"type": "core://Layout", "ref": {"name": shelf, "as": "shelf-a"},
                    "bind": {"label": "Books", "items": "collection:dishes"}},
                {"type": "core://Layout", "ref": {"name": shelf, "as": "shelf-b"},
                    "bind": {"label": "Films", "items": "collection:none"}}]}}}))
            .await
            .unwrap();
        assert!(r.is_ok(), "{r:?}");
    });
    let e = engine(&p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().root.is_some() && home_json(&e).contains("shelf-a")
    });
    let drawn = |key: &str| {
        e.layout_now(key)
            .root
            .map(|r| serde_json::to_string(&r).unwrap())
            .unwrap_or_default()
    };
    e.open_layout("shelf-a");
    e.open_layout("shelf-b");
    wait_until("shelf-a drawn", || drawn("shelf-a").contains("Books"));
    wait_until("shelf-b drawn", || drawn("shelf-b").contains("Films"));
    assert!(
        drawn("shelf-a").contains("collection:dishes"),
        "{}",
        drawn("shelf-a")
    );
    assert!(
        drawn("shelf-b").contains("collection:none"),
        "{}",
        drawn("shelf-b")
    );
    assert!(!drawn("shelf-a").contains("$in."), "{}", drawn("shelf-a"));
    // The data shelf-a's binding reads came with it.
    wait_until("shelf-a's data", || {
        e.snapshot().queries_json.contains("collection:dishes")
    });
}

/// An engine on home holding the pod session `token`, its cache in `dir`
/// ("": none).
fn engine_in(base: &str, token: &str, dir: &str) -> ConstructEngine {
    let store = Arc::new(client_core::MemoryStore::default());
    client_core::SecureStore::set(
        store.as_ref(),
        &client_core::auth::session_key(base),
        &json!({ "token": token }).to_string(),
    );
    ConstructEngine::init_with_options(
        dir,
        base,
        "home",
        client_core::EngineOptions {
            config: EngineConfig {
                poll_interval: Duration::from_millis(100),
                backoff_base: Duration::from_millis(20),
                backoff_cap: Duration::from_millis(200),
            },
            secure_store: Some(store as Arc<dyn client_core::SecureStore>),
            ..client_core::EngineOptions::default()
        },
    )
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

/// A page listing the test dishes, titled `title`.
fn dishes_page(title: &str) -> Value {
    json!({"type": "core://Page", "title": title, "root": {"type": "core://List",
        "query_key": "collection:dishes",
        "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}})
}

/// Home, with a link to `to` when given.
fn home_linking(p: &Pod, to: Option<&str>) {
    let children = match to {
        Some(id) => json!([{"type": "core://Button", "label": "Mine",
            "on_click": {"intent": "action.navigation.open", "layout": id, "presentation": "Push"}}]),
        None => json!([]),
    };
    tool(
        p,
        "update_layout",
        json!({"app": "home", "layout": {"type": "core://Page", "title": "Home",
        "root": {"type": "core://Column", "children": children}}}),
    );
}

/// Decision `layouts-not-apps`: one engine on home shows another layout,
/// with its data in the shared store, and follows it as the root moves: a
/// new version, and gone once home no longer reaches it.
#[test]
fn one_engine_on_home_opens_other_layouts_and_follows_them() {
    let p = pod();
    p.state
        .store()
        .create_pinned_session(&server::session::session_hash("pod-token"))
        .unwrap();
    let id = create(
        &p,
        json!({"title": "Mine", "feeds": [["collections", "dishes"]], "layout": dishes_page("Mine")}),
    );
    home_linking(&p, Some(&id));

    let e = engine_in(&p.base, "pod-token", "");
    wait_until("home", || home_json(&e).contains("Mine"));
    e.open_layout(&id);
    wait_until("the other layout", || {
        let s = e.snapshot();
        parse(&s.layouts_json)[&id]["workspace"]["title"] == "Mine"
            && parse(&s.queries_json).get("collection:dishes").is_some()
    });
    let s = e.snapshot();
    assert_eq!(
        parse(&s.loads_json)[format!("layout:{id}")]["state"],
        "ready"
    );
    // The same layout without a snapshot, as a tap opening it reads it.
    let now = e.layout_now(&id);
    let root = serde_json::to_value(now.root.expect("loaded")).unwrap();
    assert_eq!(root["title"], "Mine");
    assert!(!now.gone && now.error.is_none());
    // The construct's stream carries the new root: the layout drawn again.
    thread::sleep(Duration::from_millis(500));
    tool(
        &p,
        "update_layout",
        json!({"app": id, "layout": dishes_page("Mine, again")}),
    );
    wait_until("the new version", || {
        parse(&e.snapshot().layouts_json)[&id]["workspace"]["title"] == "Mine, again"
    });
    // Home no longer reaches it: it is gone from the device, and marked so
    // while it is on screen (off screen, it is forgotten: decision
    // `newest-query-answer-wins`).
    e.set_visible(vec![format!("layout:{id}"), "layout:home".to_owned()]);
    home_linking(&p, None);
    wait_until("gone", || {
        parse(&e.snapshot().layouts_json)[&id]["gone"] == json!(true)
    });
    assert!(e.layout_now(&id).gone);
    // Home itself is still there.
    assert_ne!(parse(&e.snapshot().layout_json)["gone"], json!(true));
    // A missing layout says so.
    e.open_layout("c-aaaaaaaaaa");
    wait_until("not found", || {
        parse(&e.snapshot().layouts_json)["c-aaaaaaaaaa"]["gone"] == json!(true)
    });
    let missing = e.layout_now("c-aaaaaaaaaa");
    assert!(missing.gone && missing.root.is_none());
}

/// Leaving the pod forgets the other layouts the engine opened, on screen
/// and in the cache, as it forgets home: they are the pod's.
#[test]
fn leaving_the_pod_forgets_the_other_layouts_it_opened() {
    use client_core::store::{Store, StoreOps};
    let p = pod();
    p.state
        .store()
        .create_pinned_session(&server::session::session_hash("pod-token"))
        .unwrap();
    let id = create(
        &p,
        json!({"title": "Notes", "feeds": [["collections", "dishes"]],
            "layout": dishes_page("Private notes")}),
    );
    home_linking(&p, Some(&id));
    let dir = tempfile::tempdir().unwrap();
    let cached = |id: &str| {
        Store::open(&dir.path().join("cache.db"))
            .unwrap()
            .load_workspace(id)
            .unwrap()
            .is_some()
    };

    let e = engine_in(&p.base, "pod-token", dir.path().to_str().unwrap());
    wait_until("home", || home_json(&e).contains("Mine"));
    e.open_layout(&id);
    wait_until("the other layout", || {
        parse(&e.snapshot().layouts_json)[&id]["workspace"]["title"] == "Private notes"
    });
    wait_until("the other layout cached", || cached(&id));

    // Stop the pod first: leaving refetches home without the session, and
    // this open test pod would answer that with the same home, written to
    // the cache again (what a pod shows a caller without a session is public:
    // `auth::forgetting_the_session_drops_a_home_fetch_still_in_flight`
    // covers that answer).
    drop(p);
    e.forget_session();
    assert_eq!(parse(&e.snapshot().layouts_json), json!({}));
    assert!(!cached(&id), "its cached copy is gone");
    assert!(!cached("home"));
}

/// Sign-ins for Settings: answered locally without a session, and a pod
/// without `/v1/auth/sign-ins` says so.
#[test]
fn sign_ins_without_a_session_needs_no_request_and_an_old_pod_says_so() {
    use client_core::{LayoutApiError, SIGN_INS_UNSUPPORTED};
    // No session: answered locally, even with no server at all.
    let e = engine("http://127.0.0.1:9");
    assert_eq!(parse(&e.sign_ins().unwrap()), json!({ "sign_ins": [] }));
    // With a session, a server without `/v1/auth/sign-ins` (404) gets its own sentence.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().fallback(|| async {
        (
            axum::http::StatusCode::NOT_FOUND,
            [("content-type", "text/html")],
            "<!doctype html><title>Example Domain</title>",
        )
    });
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    let e = engine_in(&base, "t", "");
    assert_eq!(
        e.sign_ins().unwrap_err(),
        LayoutApiError::NotFound {
            reason: SIGN_INS_UNSUPPORTED.into()
        }
    );
}

/// Decision `newest-query-answer-wins`: a layout cached on an earlier run
/// that home no longer reaches is never loaded again, so its cached answer
/// (which may list the same query keys as a reachable layout's) is dropped
/// after the next sync.
#[test]
fn a_cached_layout_the_root_no_longer_reaches_is_forgotten() {
    let p = pod();
    let (a, _) = build(&p);
    let dir = temp_dir("unreachable");
    let first = engine_at(&dir, &p.base);
    wait_until("home drawn from the construct", || {
        first.construct_status().root.is_some() && home_json(&first).contains("Home")
    });
    first.open_layout(&a);
    wait_until("A drawn", || {
        first
            .layout_now(&a)
            .root
            .is_some_and(|r| serde_json::to_string(&r).unwrap().contains("Inside A"))
    });
    drop(first);

    // Home stops linking A.
    let state = p.state.clone();
    let r = p.rt.block_on(async move {
        server::construct::write::mcp(&state, "update_layout", &json!({"app": "home",
            "layout": {"type": "core://Page", "title": "Home", "root": {"type": "core://Column",
                "children": [{"type": "core://Text", "style": "Title", "value_expr": "Home alone"}]}}}))
        .await
    });
    assert!(r.as_ref().is_some_and(Result::is_ok), "{r:?}");

    let again = engine_at(&dir, &p.base);
    wait_until("home drawn again", || {
        home_json(&again).contains("Home alone")
    });
    wait_until("A forgotten", || {
        let layouts: Value = serde_json::from_str(&again.snapshot().layouts_json).unwrap();
        layouts.get(&a).is_none()
    });
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
}
