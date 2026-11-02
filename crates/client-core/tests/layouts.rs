//! Layout catalog API (docs/DECISIONS.md D27) against the real pod
//! (`server::app`, in-process on a local port): list, version paging,
//! version fetch, activate / restore (with the immediate refetch and
//! `layout_json`), 401 and offline mapping.

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use client_core::catalog::{LAYOUTS_OFFLINE, LAYOUTS_SIGN_IN_TO_CHANGE};
use client_core::{
    CanvasEngine, EngineConfig, EngineOptions, LayoutApiError, MemoryStore, SecureStore,
    StateSnapshot,
};
use protocol::UiNode;
use serde_json::Value;
use server::AppState;

const TIMEOUT: Duration = Duration::from_secs(10);

struct Pod {
    state: Arc<AppState>,
    url: String,
    rt: Option<tokio::runtime::Runtime>,
}

impl Pod {
    /// `open`: `CANVAS_AGENT_OPEN=1` (writes need no session).
    fn start(open: bool) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(AppState::in_memory().with_agent_open(open));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let app = server::app(Arc::clone(&state));
        rt.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            state,
            url: format!("http://{addr}"),
            rt: Some(rt),
        }
    }

    /// Commit a copy of the active `home` layout with root title `title`
    /// (a new version of the active layout).
    fn commit_title(&self, title: &str) -> i64 {
        let store = self.state.store();
        let (_, node) = server::layouts::latest(store, "home").unwrap().unwrap();
        let mut v = serde_json::to_value(&node).unwrap();
        v["title"] = Value::from(title);
        let node: UiNode = serde_json::from_value(v).unwrap();
        server::layouts::commit(store, "home", &node, Some(title), Some("test"), None).unwrap()
    }
}

impl Drop for Pod {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
    }
}

fn engine(url: &str) -> CanvasEngine {
    CanvasEngine::init_with_config("", url, "home", config())
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn layout(s: &StateSnapshot) -> Value {
    json(&s.layout_json)
}

fn title(s: &StateSnapshot) -> String {
    json(&s.workspace_json)["title"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn ready(e: &CanvasEngine) {
    wait_until("first fetch", || {
        layout(&e.snapshot())["layout_id"].is_string()
    });
}

#[test]
fn list_layouts_returns_the_workspace_catalog_and_snapshot_names_the_active_layout() {
    let pod = Pod::start(false);
    let e = engine(&pod.url);
    ready(&e);
    assert_eq!(
        layout(&e.snapshot()),
        serde_json::json!({"layout_id": "home.counter", "layout_version": 1, "revision": 1})
    );
    let list = json(&e.list_layouts().unwrap());
    assert_eq!(list["workspace_id"], "home");
    let ids: Vec<&str> = list["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"home.counter") && ids.contains(&"home.big_counter"),
        "{ids:?}"
    );
    assert!(ids.iter().all(|id| id.starts_with("home.")), "{ids:?}");
    let active: Vec<&Value> = list["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["active"] == true)
        .collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["id"], "home.counter");
}

#[test]
fn versions_page_newest_first_and_a_version_carries_its_layout() {
    let pod = Pod::start(false);
    for t in ["T2", "T3", "T4"] {
        pod.commit_title(t);
    }
    let e = engine(&pod.url);
    ready(&e);
    let page = json(&e.layout_versions("home.counter", None, Some(2)).unwrap());
    let nums: Vec<u64> = page["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_u64().unwrap())
        .collect();
    assert_eq!(nums, [4, 3]);
    assert_eq!(page["current_version"], 4);
    let before = page["next_before"].as_u64().unwrap() as u32;
    let page = json(
        &e.layout_versions("home.counter", Some(before), Some(2))
            .unwrap(),
    );
    let nums: Vec<u64> = page["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_u64().unwrap())
        .collect();
    assert_eq!(nums, [2, 1]);
    assert!(page["next_before"].is_null());

    let v3 = json(&e.layout_version("home.counter", 3).unwrap());
    assert_eq!(v3["version"], 3);
    assert_eq!(v3["layout"]["title"], "T3");
    assert_eq!(v3["layout"]["type"], "core://Workspace");

    assert_eq!(
        e.layout_version("home.counter", 99),
        Err(LayoutApiError::NotFound {
            reason: "That version doesn't exist.".into()
        })
    );
    assert!(matches!(
        e.layout_versions("home.nope", None, None),
        Err(LayoutApiError::NotFound { .. })
    ));
}

#[test]
fn activate_applies_the_new_layout_before_returning() {
    let pod = Pod::start(true);
    let e = engine(&pod.url);
    ready(&e);
    let before = e.snapshot();
    let out = json(&e.activate_layout("home.big_counter", None).unwrap());
    assert_eq!(out["layout_id"], "home.big_counter");
    assert_eq!(out["unchanged"], false);
    // Applied by the refetch inside the call, not by the SSE echo.
    let after = e.snapshot();
    assert_ne!(after.workspace_json, before.workspace_json);
    assert_eq!(layout(&after)["layout_id"], "home.big_counter");
    assert_eq!(layout(&after)["revision"], out["revision"]);
    // Entities are untouched by a layout switch.
    assert_eq!(after.entities_json, before.entities_json);

    // Activating the active layout again changes nothing.
    let again = json(&e.activate_layout("home.big_counter", None).unwrap());
    assert_eq!(again["unchanged"], true);

    // Back to the counter at an explicit version: restored first.
    let back = json(&e.activate_layout("home.counter", Some(1)).unwrap());
    assert_eq!(back["layout_id"], "home.counter");
    assert_eq!(layout(&e.snapshot())["layout_id"], "home.counter");
    assert_eq!(e.snapshot().workspace_json, before.workspace_json);
}

#[test]
fn restore_of_the_active_layout_refetches_and_bumps_its_version() {
    let pod = Pod::start(true);
    pod.commit_title("Edited");
    let e = engine(&pod.url);
    ready(&e);
    wait_until("edited layout", || title(&e.snapshot()) == "Edited");
    assert_eq!(layout(&e.snapshot())["layout_version"], 2);

    let out = json(&e.restore_layout_version("home.counter", 1).unwrap());
    assert_eq!(out["version"], 3);
    assert_eq!(out["restored_from"], 1);
    assert_eq!(out["active"], true);
    let snap = e.snapshot();
    assert_eq!(title(&snap), "Home");
    assert_eq!(layout(&snap)["layout_version"], 3);

    // Restoring content equal to the newest appends nothing.
    let same = json(&e.restore_layout_version("home.counter", 3).unwrap());
    assert_eq!(same["unchanged"], true);

    // An inactive layout's restore leaves the screen alone.
    let seq = e.snapshot().workspace_json;
    let other = json(&e.restore_layout_version("home.big_counter", 1).unwrap());
    assert_eq!(other["active"], false);
    assert_eq!(e.snapshot().workspace_json, seq);
}

#[test]
fn writes_without_a_session_get_unauthorized_and_a_stale_session_is_signed_out() {
    let pod = Pod::start(false);
    let e = engine(&pod.url);
    ready(&e);
    assert_eq!(
        e.activate_layout("home.big_counter", None),
        Err(LayoutApiError::Unauthorized {
            reason: LAYOUTS_SIGN_IN_TO_CHANGE.into()
        })
    );
    // Reads of `home` are anonymous.
    assert!(e.list_layouts().is_ok());

    // A stored session the pod no longer knows: 401 signs out (D19).
    let store = Arc::new(MemoryStore::new());
    let key = client_core::auth::session_key(&pod.url);
    store.set(&key, r#"{"token":"stale-token","username":"alice"}"#);
    let e = CanvasEngine::init_with_options(
        "",
        &pod.url,
        "home",
        EngineOptions {
            config: config(),
            secure_store: Some(store.clone()),
            ..EngineOptions::default()
        },
    );
    assert!(e.snapshot().auth_json.contains("signed_in"));
    // `home` fetches don't check the bearer, so the engine stays signed in
    // until a write is rejected.
    ready(&e);
    let err = e.restore_layout_version("home.counter", 1).unwrap_err();
    assert_eq!(err.reason(), LAYOUTS_SIGN_IN_TO_CHANGE);
    assert_eq!(e.snapshot().auth_json, r#"{"state":"signed_out"}"#);
    wait_until("store cleared", || store.get(&key).is_none());
}

#[test]
fn unreachable_pod_maps_to_offline() {
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let e = engine(&format!("http://{addr}"));
    let offline = LayoutApiError::Offline {
        reason: LAYOUTS_OFFLINE.into(),
    };
    assert_eq!(e.list_layouts(), Err(offline.clone()));
    assert_eq!(
        e.layout_versions("home.counter", None, None),
        Err(offline.clone())
    );
    assert_eq!(e.layout_version("home.counter", 1), Err(offline.clone()));
    assert_eq!(
        e.activate_layout("home.counter", None),
        Err(offline.clone())
    );
    assert_eq!(e.restore_layout_version("home.counter", 1), Err(offline));
    assert_eq!(e.snapshot().layout_json, "{}");
}
