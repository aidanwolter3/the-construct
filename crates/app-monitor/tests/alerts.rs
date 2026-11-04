//! Monitoring alerts (decision `server-monitoring-alerts`) on a real pod: an
//! app on the source gets a "Server warnings" channel only once the pod may
//! read the monitoring data, and a check that turns to warning notifies once
//! through the D52 pipeline.

use std::sync::Arc;

use app_monitor::{ALERT_CHANNEL, ENABLED_SETTING, MonitorAdapter, MonitorConfig};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod, metrics};
use serde_json::{Value, json};
use tower::ServiceExt;

fn pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(MonitorAdapter::new(
        MonitorConfig::default(),
    ))]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

async fn call(
    pod: &Pod,
    method: &str,
    path: &str,
    bearer: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header("authorization", format!("Bearer {bearer}"));
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

/// A custom app on the monitoring source, made over MCP; its id.
async fn health_app(pod: &Pod) -> String {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/mcp",
        "pod",
        Some(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "create_app",
            "arguments": {"title": "Server health", "feeds": [["monitor", "overview"]],
                "layout": {"type": "core://Workspace", "title": "Server health", "slots": [
                    {"slot_id": "checks", "provider_uri": "monitor://overview", "content": {
                        "type": "core://List", "query_key": "monitor:checks",
                        "item_template": {"type": "core://Text", "style": "Body",
                            "value_expr": "$item.title"}}}]}}}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = &body["result"];
    assert_ne!(r["isError"], true, "{body}");
    let app: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    app["id"].as_str().unwrap().to_owned()
}

async fn warning_channels(pod: &Pod) -> Vec<Value> {
    let (status, body) = call(pod, "GET", "/v1/notifications/channels", "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["channels"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["id"] == ALERT_CHANNEL)
        .cloned()
        .collect()
}

async fn notifications(pod: &Pod) -> Vec<Value> {
    let (status, body) = call(pod, "GET", "/v1/notifications", "phone", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["notifications"].as_array().unwrap().clone()
}

#[tokio::test]
async fn a_warning_notifies_the_monitoring_app_once() {
    let pod = pod();
    let app = health_app(&pod).await;
    // The pod polls only for a registered device.
    let (status, _) = call(
        &pod,
        "PUT",
        "/v1/devices/phone-device-1",
        "phone",
        Some(json!({"platform": "android"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Monitoring off: no channel.
    assert!(warning_channels(&pod).await.is_empty());

    pod.store().set_pod_setting(ENABLED_SETTING, "1").unwrap();
    let channels = warning_channels(&pod).await;
    assert_eq!(channels.len(), 1, "{channels:?}");
    assert_eq!(channels[0]["app_id"], app.as_str());
    assert_eq!(channels[0]["enabled"], true);

    // Someone tries many tokens: the check warns and notifies.
    metrics::add(metrics::AUTH_FAILURES, &[("kind", "unknown_token")], 50);
    pod.poll_due().await;
    let got = notifications(&pod).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        got[0]["title"],
        "Server: Wrong pod codes and unknown tokens"
    );
    assert!(
        got[0]["body"]
            .as_str()
            .unwrap()
            .contains("in the last hour"),
        "{got:?}"
    );

    // Still warning at the next look: nothing new.
    pod.poll_due().await;
    assert_eq!(notifications(&pod).await.len(), 1);
}
