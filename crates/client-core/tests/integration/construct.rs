//! The engine on a pod switched to the content-addressed construct
//! (decision `content-addressed-construct`), against the real pod: it is
//! current when one hash matches, an edit to one component fetches only the
//! objects that changed, and on a pod that isn't switched it uses the old
//! paths.

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

/// Home links layout A and shows a tile; A shows a tile too; B uses none.
fn build(p: &Pod) -> (String, String) {
    server::components::put_pod(
        p.state.store(),
        "pod:tile",
        &tile(""),
        "Defined",
        "claude",
        None,
    )
    .unwrap();
    let state = p.state.clone();
    p.rt.block_on(async move {
        let b = server::custom::create(&state, &json!({"title": "B", "layout": {"type": "core://Page",
            "title": "B", "root": {"type": "core://Text", "style": "Body", "value_expr": "Plain"}}}),
            server::custom::Writer::Claude)
        .await
        .unwrap()
        .id;
        let a = server::custom::create(&state, &json!({"title": "A", "layout": {"type": "core://Page",
            "title": "A", "root": {"type": "core://Column", "children": [
                {"type": "core://Use", "component": "pod:tile", "args": {"label": "Inside A"}},
                {"type": "core://Button", "label": "B",
                 "on_click": {"intent": "action.navigation.open", "layout": b, "presentation": "Push"}}]}}}),
            server::custom::Writer::Claude)
        .await
        .unwrap()
        .id;
        server::custom::update_layout(&state, "home", &json!({"layout": {"type": "core://Page",
            "title": "Home", "root": {"type": "core://Column", "children": [
                {"type": "core://Use", "component": "pod:tile", "args": {"label": "Home"}},
                {"type": "core://Button", "label": "A",
                 "on_click": {"intent": "action.navigation.open", "layout": a, "presentation": "Push"}}]}}}),
            server::custom::Writer::Claude)
        .await
        .unwrap();
        (a, b)
    })
}

fn home_json(e: &ConstructEngine) -> String {
    e.snapshot().workspace_json
}

#[test]
fn a_pod_that_isnt_switched_is_drawn_from_its_workspaces() {
    let p = pod();
    build(&p);
    let e = engine(&p.base);
    wait_until("home drawn", || home_json(&e).contains("Home"));
    let status = e.construct_status();
    assert!(!status.switched, "{status:?}");
    assert!(e.snapshot().construct_json.is_empty());
}

#[test]
fn one_hash_says_the_device_is_current_and_an_edit_fetches_only_what_changed() {
    let p = pod();
    let (a, b) = build(&p);
    server::construct::switch::switch(&p.state).unwrap();
    let e = engine(&p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().switched && home_json(&e).contains("Home")
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
    server::construct::switch::switch(&p.state).unwrap();
    let dir = temp_dir("restart");
    let e = engine_at(&dir, &p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().switched && home_json(&e).contains("Home")
    });
    assert_eq!(e.construct_status().fetched.first(), Some(&4));
    let root = e.construct_status().root;
    drop(e);

    let again = engine_at(&dir, &p.base);
    wait_until("home drawn again", || {
        again.construct_status().switched && !again.construct_status().fetched.is_empty()
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
    let state = p.state.clone();
    let lone = p.rt.block_on(async move {
        server::custom::create(&state, &json!({"title": "Lone", "layout": {"type": "core://Page",
            "title": "Lone", "root": {"type": "core://Text", "style": "Body", "value_expr": "Off the tree"}}}),
            server::custom::Writer::Claude)
        .await
        .unwrap()
        .id
    });
    server::construct::switch::switch(&p.state).unwrap();
    let e = engine(&p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().switched && home_json(&e).contains("Home")
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
    server::construct::switch::switch(&p.state).unwrap();
    let state = p.state.clone();
    p.rt.block_on(async move {
        let shelf = server::construct::write::mcp(&state, "create_layout", &json!({"title": "Shelf",
            "feeds": [["recipes", "all"]],
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
                    "bind": {"label": "Books", "items": "recipes:all"}},
                {"type": "core://Layout", "ref": {"name": shelf, "as": "shelf-b"},
                    "bind": {"label": "Films", "items": "recipes:none"}}]}}}))
            .await
            .unwrap();
        assert!(r.is_ok(), "{r:?}");
    });
    let e = engine(&p.base);
    wait_until("home drawn from the construct", || {
        e.construct_status().switched && home_json(&e).contains("shelf-a")
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
        drawn("shelf-a").contains("recipes:all"),
        "{}",
        drawn("shelf-a")
    );
    assert!(
        drawn("shelf-b").contains("recipes:none"),
        "{}",
        drawn("shelf-b")
    );
    assert!(!drawn("shelf-a").contains("$in."), "{}", drawn("shelf-a"));
    // The data shelf-a's binding reads came with it.
    wait_until("shelf-a's data", || {
        e.snapshot().queries_json.contains("recipes:all")
    });
}

/// Decision `newest-query-answer-wins`: a layout cached on an earlier run
/// that home no longer reaches is never loaded again, so its cached answer
/// (which may list the same query keys as a reachable layout's) is dropped
/// after the next sync.
#[test]
fn a_cached_layout_the_root_no_longer_reaches_is_forgotten() {
    let p = pod();
    let (a, _) = build(&p);
    server::construct::switch::switch(&p.state).unwrap();
    let dir = temp_dir("unreachable");
    let first = engine_at(&dir, &p.base);
    wait_until("home drawn from the construct", || {
        first.construct_status().switched && home_json(&first).contains("Home")
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
