//! Jacks from the app's Settings (decision `jacks-in-settings`): the jacks
//! screens' data, and plugging and unplugging with a session, where a
//! secret goes in once and is never shown back.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

fn pod() -> Pod {
    let state = AppState::in_memory(vec![]);
    state.store().create_session(&session_hash("ana")).unwrap();
    Arc::new(state)
}

async fn call(
    pod: &Pod,
    method: &str,
    path: &str,
    body: Option<Value>,
    bearer: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if bearer {
        req = req.header("authorization", "Bearer ana");
    }
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

async fn search(pod: &Pod) -> Value {
    let (s, v) = call(pod, "GET", "/v1/jacks/screen?id=web_search", None, true).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["entities"]["jacks://web_search"].clone()
}

#[tokio::test]
async fn jacks_need_a_session() {
    let pod = pod();
    for (m, p) in [
        ("GET", "/v1/jacks/screen"),
        ("POST", "/v1/jacks/plug"),
        ("POST", "/v1/jacks/unplug"),
    ] {
        let (s, _) = call(&pod, m, p, Some(json!({"id": "web"})), false).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {p}");
    }
}

#[tokio::test]
async fn the_screen_lists_every_jack_flattened() {
    let pod = pod();
    let (s, v) = call(&pod, "GET", "/v1/jacks/screen", None, true).await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<&str> = v["queries"]["jacks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap())
        .collect();
    assert_eq!(ids, ["jacks://model", "jacks://web", "jacks://web_search"]);
    let model = &v["entities"]["jacks://model"];
    assert_eq!(model["is_switch"], true);
    assert_eq!(model["can_unplug"], true);
    assert_eq!(model["state_label"], "Plugged in");
    let ws = &v["entities"]["jacks://web_search"];
    assert_eq!(ws["is_config"], true);
    assert_eq!(ws["can_plug"], true);
    assert_eq!(ws["has_api_key"], true);
    assert_eq!(ws["api_key_set"], false);
    let (s, _) = call(&pod, "GET", "/v1/jacks/screen?id=nope", None, true).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_key_goes_in_once_and_is_never_shown_back() {
    let pod = pod();
    // Brave without a key is refused (the app rolls its change back).
    let form = |provider: &str, key: &str| json!({"id": "web_search", "config": {"provider": provider, "api_key": key, "base_url": ""}});
    let (s, v) = call(
        &pod,
        "POST",
        "/v1/jacks/plug",
        Some(form("brave", "")),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["detail"], "brave needs api_key");

    let (s, v) = call(
        &pod,
        "POST",
        "/v1/jacks/plug",
        Some(form("brave", "BSA-key-0001")),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(!v.to_string().contains("BSA-key-0001"), "{v}");
    let e = search(&pod).await;
    assert!(!e.to_string().contains("BSA-key-0001"), "{e}");
    assert_eq!(e["state_label"], "Plugged in · brave");
    assert_eq!(e["provider"], "brave");
    assert_eq!(e["api_key_set"], true);
    let cfg = pod_core::jacks::config(pod.store(), "web_search").unwrap();
    assert_eq!(cfg["api_key"], "BSA-key-0001");

    // The form sent again blank keeps the provider and the key.
    let (s, _) = call(&pod, "POST", "/v1/jacks/plug", Some(form("", "")), true).await;
    assert_eq!(s, StatusCode::OK);
    let cfg = pod_core::jacks::config(pod.store(), "web_search").unwrap();
    assert_eq!(
        (cfg["provider"].as_str(), cfg["api_key"].as_str()),
        (Some("brave"), Some("BSA-key-0001"))
    );

    // Unplugging forgets it.
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/jacks/unplug",
        Some(json!({"id": "web_search"})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(pod_core::jacks::config(pod.store(), "web_search").is_none());
    let e = search(&pod).await;
    assert_eq!(
        (e["state"].as_str(), e["can_plug"].as_bool()),
        (Some("unplugged"), Some(true))
    );
}

#[tokio::test]
async fn switches_plug_and_unplug_from_the_app() {
    let pod = pod();
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/jacks/unplug",
        Some(json!({"id": "web"})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(!pod_core::jacks::core_plugged(pod.store(), "web"));
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/jacks/plug",
        Some(json!({"id": "web"})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(pod_core::jacks::core_plugged(pod.store(), "web"));
}
