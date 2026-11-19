//! Devices and their builds (decision `device-versions`) on a real pod: a
//! request that names its device is recorded, the `devices` feed and the MCP
//! tools show it, and a watch for it fires once when it updates.

use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pod_core::Pod;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::alerts::{call, create, notifications, pod};

const PHONE: &str = "phone0000device01";

/// A request from the phone's app, drawing `features`.
async fn ping(pod: &Pod, features: &str) {
    let req = Request::builder()
        .uri("/v1/notifications/channels")
        .header("authorization", "Bearer phone")
        .header(protocol::DEVICE_HEADER, PHONE)
        .header(
            protocol::BUILD_HEADER,
            "android 0.6.0+20261118153000.c41d9e2",
        )
        .header(protocol::DEVICE_NAME_HEADER, "Pixel 9")
        .header(protocol::FEATURES_HEADER, features)
        .body(Body::empty())
        .unwrap();
    let resp = pod_core::app(pod.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = resp.into_body().collect().await;
}

/// An MCP tool's answer.
async fn tool(pod: &Pod, name: &str, args: Value) -> Value {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/mcp",
        "pod",
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": args}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// `list_devices` once `ok` holds (the write runs after the request).
async fn devices_when(pod: &Pod, ok: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..200 {
        let v = tool(pod, "list_devices", json!({})).await;
        if ok(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("list_devices never matched");
}

#[tokio::test]
async fn a_device_is_recorded_and_a_watch_fires_once_when_it_updates() {
    let pod = pod();
    // A layout that shows the devices: its channel gets watches' notifications.
    create(
        &pod,
        json!({"title": "Server health", "feeds": [["monitor", "devices"]],
            "layout": {"type": "core://Workspace", "title": "Server health", "slots": [
                {"slot_id": "devices", "provider_uri": "monitor://devices", "content": {
                    "type": "core://List", "query_key": "monitor:devices",
                    "item_template": {"type": "core://Text", "style": "Body",
                        "value_expr": "$item.name"}}}]}}),
    )
    .await;

    ping(&pod, "list,text").await;
    let v = devices_when(&pod, |v| {
        v["devices"].as_array().is_some_and(|d| !d.is_empty())
    })
    .await;
    let d = &v["devices"][0];
    assert_eq!(d["name"], "Pixel 9");
    assert_eq!(d["kind"], "android");
    assert_eq!(d["build"], "0.6.0+20261118153000.c41d9e2");
    assert_eq!(d["build_label"], "Nov 18 · c41d9e2");
    assert_eq!(d["features"], json!(["list", "text"]));

    // The feed: the device, today's bar, the legend.
    let (status, body) = call(
        &pod,
        "POST",
        "/v1/mcp",
        "pod",
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "sample_data", "arguments": {"source": "monitor", "feed": "devices"}}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("monitor://devices/phone0000device01"),
        "{text}"
    );
    assert!(text.contains("monitor:device_days"), "{text}");

    // An action that is no source's is refused.
    let refused = tool(
        &pod,
        "watch_device",
        json!({"device": "Pixel 9", "until": {"feature": "bars"},
            "then": {"action": {"path": "/v1/nowhere"}}}),
    )
    .await;
    assert_eq!(refused["error"], "unknown_action", "{refused}");
    // Already drawn: nothing to wait for.
    let met = tool(
        &pod,
        "watch_device",
        json!({"device": "Pixel 9", "until": {"feature": "list"}, "then": {"notify": "x"}}),
    )
    .await;
    assert_eq!(met["already_met"], true, "{met}");

    let w = tool(
        &pod,
        "watch_device",
        json!({"device": "pixel 9", "until": {"feature": "bars"},
            "then": {"notify": "Your phone draws charts now."}, "note": "chart layouts"}),
    )
    .await;
    assert_eq!(w["ok"], true, "{w}");
    let v = tool(&pod, "list_devices", json!({})).await;
    assert_eq!(v["watches"].as_array().unwrap().len(), 1);

    // The phone updates: the watch fires once.
    ping(&pod, "bars,list,text").await;
    let v = devices_when(&pod, |v| {
        v["watches"].as_array().is_some_and(|w| w.is_empty())
    })
    .await;
    assert_eq!(v["devices"][0]["features"], json!(["bars", "list", "text"]));
    let mut got = Vec::new();
    for _ in 0..200 {
        got = notifications(&pod).await;
        if !got.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["title"], "Pixel 9 updated");
    assert_eq!(got[0]["body"], "Your phone draws charts now.");

    // The user's name for it wins.
    let r = tool(
        &pod,
        "rename_device",
        json!({"device": PHONE, "name": "My phone"}),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let v = tool(&pod, "list_devices", json!({"kind": "android"})).await;
    assert_eq!(v["devices"][0]["name"], "My phone");
    let v = tool(&pod, "list_devices", json!({"kind": "web"})).await;
    assert_eq!(v["devices"], json!([]));
}
