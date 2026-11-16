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
