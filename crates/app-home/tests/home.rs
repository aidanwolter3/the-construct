//! The home app on a real pod: the feed (apps, pins, requests), pinning and
//! unpinning, a request's screen data, and the builder end to end against a
//! fake Messages API that scripts describe_vocabulary → define_collection →
//! create_app → end_turn. Also: no model, temporary pods, the tool
//! allowlist, a restart, and the layout file's features.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_home::builder::{BUILD_SETTING, NO_MODEL, TEMPORARY_POD};
use app_home::{BuilderConfig, HomeAdapter};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppAdapter, AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

/// What the fake model API saw, and the answers it gives in order.
struct Model {
    calls: Mutex<Vec<(HeaderMap, Value)>>,
    script: Mutex<Vec<Value>>,
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "message", "stop_reason": "tool_use", "content": [
        {"type": "text", "text": "Working on it."},
        {"type": "tool_use", "id": id, "name": name, "input": input}]})
}

fn end_turn(text: &str) -> Value {
    json!({"type": "message", "stop_reason": "end_turn",
           "content": [{"type": "text", "text": text}]})
}

async fn messages(
    State(m): State<Arc<Model>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    m.calls.lock().unwrap().push((headers, body));
    let mut script = m.script.lock().unwrap();
    Json(if script.is_empty() {
        end_turn("Done.")
    } else {
        script.remove(0)
    })
}

async fn fake_model(script: Vec<Value>) -> (String, Arc<Model>) {
    let state = Arc::new(Model {
        calls: Mutex::new(Vec::new()),
        script: Mutex::new(script),
    });
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/v1/messages"), state)
}

fn pod_with(builder: Option<BuilderConfig>) -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(HomeAdapter::new(builder)) as Arc<dyn AppAdapter>,
        Arc::new(app_collections::CollectionsAdapter::new()),
    ]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

fn config(url: &str) -> BuilderConfig {
    BuilderConfig {
        api_key: "test-key".into(),
        model: "test-model".into(),
        url: url.into(),
    }
}

async fn call(pod: &Pod, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
        .header(protocol::FEATURES_HEADER, protocol::FEATURES.join(","))
        .header("authorization", "Bearer phone");
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

async fn home(pod: &Pod) -> Value {
    let (status, body) = call(pod, "GET", "/v1/workspaces/launcher", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn query<'a>(body: &'a Value, key: &str) -> Vec<&'a str> {
    body["queries"][key]
        .as_array()
        .unwrap_or_else(|| panic!("no {key} in {body}"))
        .iter()
        .map(|u| u.as_str().unwrap())
        .collect()
}

/// The request's screen entity once its status is no longer `queued` or
/// `building`.
async fn finished(pod: &Pod, id: &str) -> Value {
    for _ in 0..200 {
        let (status, b) = call(
            pod,
            "GET",
            &format!("/v1/home/screens/request?id={id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{b}");
        let e = &b["entities"][format!("home://requests/{id}")];
        if e["status"] == "ready" || e["status"] == "failed" {
            return e.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("request {id} never finished");
}

#[tokio::test]
async fn the_feed_lists_apps_and_pins() {
    let pod = pod_with(None);
    let body = home(&pod).await;
    // Served as the launcher's layout, with the dark home screen.
    assert_eq!(body["workspace"]["id"], "launcher");
    assert_eq!(body["workspace"]["chrome"], "hidden");
    // No other app on this pod but a custom one we make: none yet.
    assert!(query(&body, "home:apps").is_empty());
    assert!(query(&body, "home:pins").is_empty());
    assert!(query(&body, "home:requests").is_empty());

    // A custom app appears in the list, by title.
    let made = pod_core::mcp::run_tool(
        &pod,
        "create_app",
        json!({"title": "plant care", "icon": "dashboard", "feeds": [["home", "home"]],
               "layout": {"type": "core://Workspace", "id": "x", "title": "Plant care", "slots": [
                   {"slot_id": "s", "provider_uri": "core://custom",
                    "content": {"type": "core://Text", "value_expr": "Hi", "style": "Body"}}]}}),
    )
    .await;
    assert_eq!(made["isError"], false, "{made}");
    let app_id = made["structuredContent"]["id"].as_str().unwrap().to_owned();
    let body = home(&pod).await;
    let uri = format!("home://apps/{app_id}");
    assert_eq!(query(&body, "home:apps"), [uri.as_str()]);
    assert_eq!(
        body["entities"][&uri],
        json!({"id": app_id, "title": "plant care", "letter": "P", "kind": "construct"})
    );

    // Pin it and a device app; pinning again changes nothing.
    for (id, kind, target, title) in [
        ("p1", "construct", app_id.as_str(), "plant care"),
        ("p2", "device", "com.android.camera", "Camera"),
        ("p3", "device", "com.android.camera", "Camera"),
    ] {
        let (status, b) = call(
            &pod,
            "POST",
            "/v1/home/pins",
            Some(json!({"id": id, "kind": kind, "target": target, "title": title})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{b}");
    }
    let body = home(&pod).await;
    assert_eq!(
        query(&body, "home:pins"),
        ["home://pins/p1", "home://pins/p2"]
    );
    assert_eq!(
        body["entities"]["home://pins/p2"],
        json!({"id": "p2", "kind": "device", "target": "com.android.camera",
               "title": "Camera", "unpinned": false})
    );
    // Bad pins are refused.
    for bad in [
        json!({"id": "x y", "kind": "device", "target": "a", "title": "A"}),
        json!({"id": "p9", "kind": "web", "target": "a", "title": "A"}),
        json!({"id": "p9", "kind": "device", "target": "a/b", "title": "A"}),
        json!({"id": "p9", "kind": "device", "target": "a", "title": ""}),
    ] {
        let (status, _) = call(&pod, "POST", "/v1/home/pins", Some(bad.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // Unpin; unpinning again is fine.
    for _ in 0..2 {
        let (status, _) = call(
            &pod,
            "POST",
            "/v1/home/pins/remove",
            Some(json!({"id": "p2"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    // Hidden apps leave the list; a deleted app's pin leaves the home screen.
    pod_core::custom::set_hidden(&pod, &app_id, true)
        .await
        .unwrap();
    let body = home(&pod).await;
    assert!(query(&body, "home:apps").is_empty());
    assert_eq!(query(&body, "home:pins"), ["home://pins/p1"]);
    pod_core::custom::delete(&pod, &app_id).await.unwrap();
    let body = home(&pod).await;
    assert!(query(&body, "home:pins").is_empty());
}

#[tokio::test]
async fn a_request_is_built_by_claude_and_ends_ready() {
    let layout = json!({"type": "core://Workspace", "id": "x", "title": "Plant care", "slots": [
        {"slot_id": "list", "provider_uri": "core://custom", "content": {
            "type": "core://List", "query_key": "collection:plants", "empty_text": "No plants yet",
            "item_template": {"type": "core://Text", "value_expr": "$item.name", "style": "Body"}}}]});
    let (url, model) = fake_model(vec![
        tool_use("t1", "describe_vocabulary", json!({})),
        tool_use(
            "t2",
            "define_collection",
            json!({"name": "plants", "title": "Plants", "fields": [
                {"name": "name", "type": "text"},
                {"name": "watered", "type": "date", "optional": true}]}),
        ),
        // Not allowed while building: refused, the build goes on.
        tool_use("t3", "delete_app", json!({"app": "x"})),
        tool_use(
            "t4",
            "create_app",
            json!({"title": "Plant care", "icon": "dashboard",
                   "feeds": [["collections", "plants"]], "layout": layout}),
        ),
        end_turn("Plant care keeps track of your plants."),
    ])
    .await;
    let pod = pod_with(Some(config(&url)));
    let mut data = pod.subscribe_data();

    // The screen can be loaded before the create lands: an empty list.
    let (status, b) = call(&pod, "GET", "/v1/home/screens/request?id=r1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["queries"]["home:request:r1"], json!([]));

    let (status, b) = call(
        &pod,
        "POST",
        "/v1/home/requests",
        Some(json!({"id": "r1", "prompt": "Track when I water my plants"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{b}");
    // The same id again changes nothing.
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/home/requests",
        Some(json!({"id": "r1", "prompt": "Something else"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let e = finished(&pod, "r1").await;
    assert_eq!(e["status"], "ready", "{e}");
    assert_eq!(e["app_title"], "Plant care");
    assert_eq!(e["title"], "Plant care");
    assert_eq!(e["status_text"], "Plant care is ready");
    assert_eq!(e["prompt"], "Track when I water my plants");
    let app_id = e["app_id"].as_str().unwrap();
    // The app exists and opens.
    let (status, ws) = call(&pod, "GET", &format!("/v1/workspaces/{app_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["workspace"]["title"], "Plant care");
    // It is listed on the home screen; a ready request is no longer.
    let body = home(&pod).await;
    assert_eq!(
        query(&body, "home:apps"),
        [format!("home://apps/{app_id}").as_str()]
    );
    assert!(query(&body, "home:requests").is_empty());
    // Clients heard about it.
    assert_eq!(data.recv().await.unwrap().source, "home");

    let calls = model.calls.lock().unwrap();
    assert_eq!(calls.len(), 5);
    let (headers, first) = &calls[0];
    assert_eq!(headers["x-api-key"], "test-key");
    assert_eq!(first["model"], "test-model");
    assert!(
        first["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Track when I water my plants")
    );
    assert!(
        first["system"]
            .as_str()
            .unwrap()
            .contains("call create_app exactly once")
    );
    let tools: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(tools.contains(&"create_app") && tools.contains(&"define_collection"));
    assert!(!tools.contains(&"delete_app") && !tools.contains(&"set_hidden"));
    assert!(first["tools"][0]["input_schema"].is_object());
    // The refused tool came back as an error result.
    let refused = &calls[3].1["messages"];
    let last = &refused[refused.as_array().unwrap().len() - 1];
    assert_eq!(last["content"][0]["tool_use_id"], "t3");
    assert_eq!(last["content"][0]["is_error"], true);
}

#[tokio::test]
async fn a_build_without_an_app_or_a_model_fails() {
    // The model never creates an app.
    let (url, _) = fake_model(vec![end_turn("What kind of plants?")]).await;
    let pod = pod_with(Some(config(&url)));
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/home/requests",
        Some(json!({"id": "r1", "prompt": "Plants"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let e = finished(&pod, "r1").await;
    assert_eq!(e["status"], "failed");
    assert!(
        e["error"]
            .as_str()
            .unwrap()
            .contains("What kind of plants?"),
        "{e}"
    );
    // Failed requests stay on the home screen.
    let body = home(&pod).await;
    assert_eq!(query(&body, "home:requests"), ["home://requests/r1"]);

    // No model configured.
    let pod = pod_with(None);
    call(
        &pod,
        "POST",
        "/v1/home/requests",
        Some(json!({"id": "r2", "prompt": "Plants"})),
    )
    .await;
    assert_eq!(finished(&pod, "r2").await["error"], NO_MODEL);

    // Temporary pods don't build, model or not.
    let (url, model) = fake_model(vec![]).await;
    let pod = pod_with(Some(config(&url)));
    pod.store().set_pod_setting(BUILD_SETTING, "0").unwrap();
    call(
        &pod,
        "POST",
        "/v1/home/requests",
        Some(json!({"id": "r3", "prompt": "Plants"})),
    )
    .await;
    assert_eq!(finished(&pod, "r3").await["error"], TEMPORARY_POD);
    assert!(model.calls.lock().unwrap().is_empty());

    // Bad requests.
    for bad in [
        json!({"id": "r4", "prompt": ""}),
        json!({"id": "", "prompt": "x"}),
        json!({"id": "r4", "prompt": "x".repeat(2001)}),
    ] {
        let (status, _) = call(&pod, "POST", "/v1/home/requests", Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn a_restart_fails_interrupted_builds() {
    let pod = pod_with(None);
    // A request of an earlier run, and one of this run.
    let now = pod_core::util::now_secs();
    pod.store()
        .with_conn(|c| {
            app_home::db::add_request(c, "r1", "Plants", now)?;
            c.execute(
                "UPDATE home_requests SET run_id = 'old' WHERE id = 'r1'",
                [],
            )?;
            app_home::db::add_request(c, "r2", "Birds", now)
        })
        .unwrap();
    let adapter = HomeAdapter::new(None);
    adapter.resume(&pod).await;
    let (_, b) = call(&pod, "GET", "/v1/home/screens/request?id=r1", None).await;
    let e = &b["entities"]["home://requests/r1"];
    assert_eq!(e["status"], "failed");
    assert_eq!(e["error"], "Interrupted — try again.");
    let (_, b) = call(&pod, "GET", "/v1/home/screens/request?id=r2", None).await;
    assert_eq!(b["entities"]["home://requests/r2"]["status"], "queued");
}

#[test]
fn the_layout_file_needs_the_launcher_features() {
    let src = app_home::FILES.first().expect("layouts/launcher/0001.json");
    let doc: Value = serde_json::from_str(src.text).unwrap();
    let node: protocol::UiNode = serde_json::from_value(doc["layout"].clone()).unwrap();
    assert!(pod_core::validate::unknown_node_types(&node).is_empty());
    assert_eq!(
        protocol::required_features(&node),
        [
            "action_sequence",
            "app_open",
            "autofocus",
            "backdrop",
            "clock",
            "device_apps",
            "gestures",
            "hidden_chrome",
            "on_appear",
            "progress",
        ]
    );
}

#[tokio::test]
async fn old_clients_are_not_served_the_launcher() {
    let pod = pod_with(None);
    let req = Request::builder()
        .uri("/v1/workspaces/launcher")
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
        .header(protocol::FEATURES_HEADER, "progress")
        .header("authorization", "Bearer phone")
        .body(Body::empty())
        .unwrap();
    let resp = pod_core::app(pod.clone()).oneshot(req).await.unwrap();
    assert_ne!(resp.status(), StatusCode::OK);
}
