//! The `monitor` source (decision `server-monitoring`) through the real host: off until
//! the operator turns it on for an admin pod, then the server's counters
//! (here, a wrong pod code) and gauges (pods) as checks and metrics.

use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use server::PixelfedConfig;
use server::host::{Host, HostConfig, PodKind};

async fn host(dir: &std::path::Path) -> Arc<Host> {
    let factory = server::pod_factory(
        PixelfedConfig::new("http://127.0.0.1:9", None),
        false,
        86_400,
        false,
    );
    let config = HostConfig::default();
    let path = dir.to_owned();
    let host = tokio::task::spawn_blocking(move || Host::open(&path, factory, config))
        .await
        .unwrap()
        .unwrap();
    Arc::new(host)
}

/// `sample_data` of the monitor's overview over MCP with the pod token:
/// `(is_error, result)`.
async fn overview(url: &str, token: &str) -> (bool, Value) {
    let body: Value = reqwest::Client::new()
        .post(format!("{url}/v1/mcp"))
        .bearer_auth(token)
        .json(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "sample_data",
                "arguments": {"source": "monitor", "feed": "overview", "limit": 100},
            }}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = &body["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("null");
    (
        r["isError"] == true,
        serde_json::from_str(text).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_operator_turns_monitoring_on_for_a_pod() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path()).await;
    let h = host.clone();
    let (token, temp) = tokio::task::spawn_blocking(move || {
        let admin = h.create_pod("me", PodKind::Admin, None, &[]).unwrap().token;
        h.create_pod("visitor", PodKind::Temporary, None, &[])
            .unwrap();
        (admin, "visitor")
    })
    .await
    .unwrap();
    {
        let h = host.clone();
        server::metrics::set_gauge_source(move || h.monitoring_gauges());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = server::host::app(host.clone());
    let serve = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // Off by default: the server's data stays out of the pod.
    let (err, off) = overview(&url, &token).await;
    assert!(err, "{off}");
    assert_eq!(off["status"], 403, "{off}");
    assert_eq!(off["detail"]["error"], "monitoring_off", "{off}");

    // Only admin pods can be given it.
    let h = host.clone();
    let refused = tokio::task::spawn_blocking(move || {
        h.set_admin_pod_setting(temp, server::monitor::ENABLED_SETTING, "1")
    })
    .await
    .unwrap();
    assert!(refused.unwrap_err().contains("temporary"));
    let h = host.clone();
    tokio::task::spawn_blocking(move || {
        h.set_admin_pod_setting("me", server::monitor::ENABLED_SETTING, "1")
    })
    .await
    .unwrap()
    .unwrap();

    // A wrong pod code is counted.
    let resp = reqwest::Client::new()
        .post(format!("{url}/v1/pods/open"))
        .json(&json!({"token": "cpod_wrong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let (err, data) = overview(&url, &token).await;
    assert!(!err, "{data}");
    let entities = &data["entities"];
    let summary = &entities["monitor://summary/server"];
    assert!(summary["headline"].is_string(), "{data}");
    let checks = data["queries"]["monitor:checks"].as_array().unwrap();
    assert!(checks.contains(&json!("monitor://checks/sign_in_failures")));
    let failures = &entities["monitor://metrics/auth_failures-pod_code"];
    assert!(failures["last_hour"].as_u64().unwrap() >= 1, "{failures}");
    let admins = &entities["monitor://metrics/pods-admin"];
    assert_eq!(admins["total"], json!(1.0), "{data}");

    // The Prometheus page has the same counters and the gauges.
    let text = tokio::task::spawn_blocking(server::metrics::render)
        .await
        .unwrap();
    assert!(
        text.contains("construct_pods{kind=\"temporary\"} 1"),
        "{text}"
    );
    assert!(text.contains("construct_auth_failures_total{kind=\"pod_code\"}"));
    assert!(text.contains("construct_http_responses_total{status=\"401\"}"));
    serve.abort();
}
