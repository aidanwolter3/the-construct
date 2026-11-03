//! App catalog calls (`list_apps`, `app_versions`, `app_version`; D27, D32)
//! and the snapshot's `layout_json` against the real pod (`server::app`,
//! in-process on a local port).

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use client_core::catalog::{LAYOUTS_NOT_A_POD, LAYOUTS_OFFLINE};
use client_core::{ConstructEngine, EngineConfig, LayoutApiError, SIGN_INS_UNSUPPORTED};
use protocol::UiNode;
use serde_json::{Value, json};
use server::{AppState, layouts};

const TIMEOUT: Duration = Duration::from_secs(10);
/// The dev app these tests version (hidden from `/v1/apps` by default).
const APP: &str = "pixelfed_demo";

struct Pod {
    state: Arc<AppState>,
    base: String,
    _rt: tokio::runtime::Runtime,
}

fn pod(state: AppState) -> Pod {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let state = Arc::new(state);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = server::app(Arc::clone(&state));
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    Pod {
        state,
        base,
        _rt: rt,
    }
}

fn fast_config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(100),
        backoff_base: Duration::from_millis(20),
        backoff_cap: Duration::from_millis(200),
    }
}

fn engine(base: &str, ws: &str) -> ConstructEngine {
    ConstructEngine::init_with_config("", base, ws, fast_config())
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

/// The newest `pixelfed_demo` layout file with another title.
fn retitled(title: &str) -> UiNode {
    let newest = layouts::parse_files(&server::catalog(), &server::layout_files())
        .unwrap()
        .into_iter()
        .rfind(|f| f.app_id == APP)
        .unwrap()
        .layout;
    let mut v = serde_json::to_value(newest).unwrap();
    v["title"] = json!(title);
    serde_json::from_value(v).unwrap()
}

fn latest(pod: &Pod, app: &str) -> i64 {
    layouts::latest(pod.state.store(), pod.state.catalog(), app)
        .unwrap()
        .unwrap()
        .0
}

fn versions(page: &Value) -> Vec<i64> {
    page["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect()
}

#[test]
fn list_apps_shows_the_app_and_dev_apps_on_request() {
    let p = pod(server::state_in_memory());
    let e = engine(&p.base, "pixelfed_home");
    let apps = parse(&e.list_apps().unwrap());
    let ids: Vec<&str> = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["pixelfed_home", "recipes"]);
    let app = &apps["apps"][0];
    assert_eq!(app["title"], "Pixelfed");
    assert_eq!(app["latest_version"], latest(&p, "pixelfed_home"));
    assert!(app["updated_at"].as_str().unwrap().ends_with('Z'));

    let dev = pod(server::state_in_memory().with_dev_apps(true));
    let e = engine(&dev.base, APP);
    let apps = parse(&e.list_apps().unwrap());
    let ids: Vec<&str> = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["pixelfed_home", "recipes", "pixelfed_demo"]);
}

#[test]
fn app_versions_pages_newest_first() {
    let p = pod(server::state_in_memory());
    let first = latest(&p, APP);
    for i in 0..4 {
        layouts::append_version(
            p.state.store(),
            APP,
            &retitled(&format!("T{i}")),
            Some(&format!("edit {i}")),
        )
        .unwrap();
    }
    let top = first + 4;
    // A hidden app's endpoints still work.
    let e = engine(&p.base, "pixelfed_home");

    let page = parse(&e.app_versions(APP, None, Some(2)).unwrap());
    assert_eq!(page["app_id"], APP);
    assert_eq!(page["latest_version"], top);
    assert_eq!(versions(&page), [top, top - 1]);
    assert_eq!(page["versions"][0]["note"], "edit 3");
    assert_eq!(page["next_before"], top - 1);

    let page = parse(
        &e.app_versions(APP, page["next_before"].as_i64(), Some(2))
            .unwrap(),
    );
    assert_eq!(versions(&page), [top - 2, top - 3]);

    // Default limit: everything here fits one page, and no next_before.
    let all = parse(&e.app_versions(APP, None, None).unwrap());
    assert_eq!(versions(&all), (1..=top).rev().collect::<Vec<_>>());
    assert!(all.get("next_before").is_none());
    assert_eq!(
        all["versions"].as_array().unwrap().last().unwrap()["version"],
        1
    );

    // Out-of-range limit: the pod's 400, as a readable error.
    let err = e.app_versions(APP, None, Some(0)).unwrap_err();
    assert!(
        matches!(err, LayoutApiError::Server { status: 400, .. }),
        "{err:?}"
    );
    assert_eq!(err.reason(), "The pod rejected the request.");
}

#[test]
fn app_version_returns_the_layout_and_errors_are_sentences() {
    let p = pod(server::state_in_memory());
    layouts::append_version(p.state.store(), APP, &retitled("Two"), Some("two")).unwrap();
    let n = latest(&p, APP);
    let e = engine(&p.base, APP);

    let v = parse(&e.app_version(APP, n).unwrap());
    assert_eq!(v["app_id"], APP);
    assert_eq!(v["version"], n);
    assert_eq!(v["note"], "two");
    assert_eq!(v["layout"]["type"], "core://Workspace");
    assert_eq!(v["layout"]["title"], "Two");
    let one = parse(&e.app_version(APP, 1).unwrap());
    assert_eq!(one["layout"]["id"], APP);

    let err = e.app_version(APP, n + 10).unwrap_err();
    assert_eq!(
        err,
        LayoutApiError::NotFound {
            reason: "That version doesn't exist.".into()
        }
    );
    let err = e.app_versions("nope", None, None).unwrap_err();
    assert_eq!(err.reason(), "That app doesn't exist.");
    assert!(matches!(err, LayoutApiError::NotFound { .. }));
    let err = e.app_version("nope", 1).unwrap_err();
    assert_eq!(err.reason(), "That app doesn't exist.");
    let err = e.app_versions("bad id!", None, None).unwrap_err();
    assert_eq!(
        err,
        LayoutApiError::Server {
            status: 400,
            reason: "That isn't a valid app id.".into()
        }
    );
}

#[test]
fn unreachable_pod_is_offline() {
    let e = engine("http://127.0.0.1:1", APP);
    for result in [
        e.list_apps(),
        e.app_versions(APP, None, None),
        e.app_version(APP, 1),
    ] {
        assert_eq!(
            result.unwrap_err(),
            LayoutApiError::Offline {
                reason: LAYOUTS_OFFLINE.into()
            }
        );
    }
}

/// A plain web server (not a pod) on a local port: every path answers
/// `status` with an HTML body.
fn web_server(status: u16) -> (String, tokio::runtime::Runtime) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let status = axum::http::StatusCode::from_u16(status).unwrap();
    let app = axum::Router::new().fallback(move || async move {
        (
            status,
            [("content-type", "text/html")],
            "<!doctype html><title>Example Domain</title>",
        )
    });
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    (base, rt)
}

#[test]
fn a_server_that_is_not_a_pod_says_so() {
    let (base, _rt) = web_server(404);
    let e = engine(&base, "pixelfed_home");
    assert_eq!(
        e.list_apps().unwrap_err(),
        LayoutApiError::NotFound {
            reason: LAYOUTS_NOT_A_POD.into()
        }
    );
    // The per-app endpoints keep their own sentence.
    assert_eq!(
        e.app_versions("pixelfed_home", None, None)
            .unwrap_err()
            .reason(),
        "That app doesn't exist."
    );

    let (base, _rt) = web_server(200);
    let e = engine(&base, "pixelfed_home");
    let err = e.list_apps().unwrap_err();
    assert_eq!(err.reason(), LAYOUTS_NOT_A_POD);
    assert!(matches!(err, LayoutApiError::Server { .. }), "{err:?}");
}

#[test]
fn sign_ins_without_a_session_needs_no_request_and_an_old_pod_says_so() {
    // No session: answered locally, even with no server at all.
    let e = engine("http://127.0.0.1:9", "pixelfed_home");
    assert_eq!(parse(&e.sign_ins().unwrap()), json!({ "sign_ins": [] }));
    // With a session, a pod without `/v1/auth/sign-ins` (404) gets its own sentence.
    let (base, _rt) = web_server(404);
    let store = Arc::new(client_core::MemoryStore::default());
    client_core::SecureStore::set(
        store.as_ref(),
        &client_core::auth::session_key(&base),
        r#"{"token":"t"}"#,
    );
    let e = ConstructEngine::init_with_options(
        "",
        &base,
        "pixelfed_home",
        client_core::EngineOptions {
            config: fast_config(),
            secure_store: Some(store as Arc<dyn client_core::SecureStore>),
            ..client_core::EngineOptions::default()
        },
    );
    assert_eq!(
        e.sign_ins().unwrap_err(),
        LayoutApiError::NotFound {
            reason: SIGN_INS_UNSUPPORTED.into()
        }
    );
}

#[test]
fn snapshot_layout_json_follows_the_pods_version() {
    let p = pod(server::state_in_memory());
    let e = engine(&p.base, APP);
    let n = latest(&p, APP);
    wait_until("first fetch", || e.snapshot().layout_json != "{}");
    assert_eq!(
        parse(&e.snapshot().layout_json),
        json!({"app_id": APP, "version": n})
    );

    let v = layouts::append_version(p.state.store(), APP, &retitled("Pushed"), None).unwrap();
    p.state.publish_layout(APP, v);
    wait_until("pushed version", || {
        parse(&e.snapshot().layout_json)["version"] == v
    });
    assert_eq!(
        parse(&e.snapshot().layout_json),
        json!({"app_id": APP, "version": v})
    );
    let ws = parse(&e.snapshot().workspace_json);
    assert_eq!(ws["title"], "Pushed");

    // The signed-out pixelfed_home screen is not versioned.
    let signed_out = engine(&p.base, "pixelfed_home");
    wait_until("signed-out screen", || {
        signed_out.snapshot().workspace_json != "null"
    });
    assert_eq!(signed_out.snapshot().layout_json, "{}");
}

/// An engine on app `ws` holding the pod session `token`.
fn engine_with_token(base: &str, ws: &str, token: &str) -> ConstructEngine {
    let store = Arc::new(client_core::MemoryStore::default());
    client_core::SecureStore::set(
        store.as_ref(),
        &client_core::auth::session_key(base),
        &json!({ "token": token }).to_string(),
    );
    ConstructEngine::init_with_options(
        "",
        base,
        ws,
        client_core::EngineOptions {
            config: fast_config(),
            secure_store: Some(store as Arc<dyn client_core::SecureStore>),
            ..client_core::EngineOptions::default()
        },
    )
}

#[test]
fn custom_apps_are_listed_hidden_and_deleted_and_the_engine_leaves_a_deleted_one() {
    let p = pod(server::state_in_memory());
    p.state
        .store()
        .create_pinned_session(&server::session::session_hash("pod-token"))
        .unwrap();
    // A custom app over the recipes, made with the pod token.
    let layout = json!({"type": "core://Workspace", "id": "x", "title": "Mine",
        "slots": [{"slot_id": "r", "provider_uri": "core://custom",
          "content": {"type": "core://List", "query_key": "recipes:all",
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}}]});
    let created: Value = reqwest::blocking::Client::new()
        .post(format!("{}/v1/custom-apps", p.base))
        .bearer_auth("pod-token")
        .json(&json!({"title": "Mine", "icon": "recipe", "feeds": [["recipes", "all"]], "layout": layout}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();

    let e = engine_with_token(&p.base, &id, "pod-token");
    wait_until("the custom app", || {
        e.snapshot().workspace_json.contains("Mine")
    });
    let apps = parse(&e.list_apps().unwrap());
    let row = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["kind"], "custom");
    assert_eq!(row["hidden"], false);

    // Hidden apps stay in the engine's list (it asks for all of them), marked.
    e.set_app_hidden("recipes", true).unwrap();
    let apps = parse(&e.list_apps().unwrap());
    let recipes = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "recipes")
        .unwrap()
        .clone();
    assert_eq!(recipes["hidden"], true);
    e.set_app_hidden("recipes", false).unwrap();

    // Deleting it: the engine on it hears `deleted` and says the app is gone
    // (give its events stream time to connect first).
    thread::sleep(Duration::from_millis(500));
    e.delete_custom_app(&id).unwrap();
    wait_until("gone", || {
        parse(&e.snapshot().layout_json)["gone"] == json!(true)
    });
    assert_eq!(
        e.delete_custom_app(&id).unwrap_err(),
        LayoutApiError::NotFound {
            reason: "That app doesn't exist.".into()
        }
    );
    // A new engine on it learns it from the 404.
    let late = engine_with_token(&p.base, &id, "pod-token");
    wait_until("gone on a fresh engine", || {
        parse(&late.snapshot().layout_json)["gone"] == json!(true)
    });
}
