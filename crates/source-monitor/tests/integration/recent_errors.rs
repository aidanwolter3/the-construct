//! The pod's recent failed requests (decision `recent-server-errors`) on
//! real pods: a 502 from a source's route shows in the `errors` feed with
//! the route's pattern, its error code and the device's name, and in the
//! `server_errors` check's detail; the path's token and the query are never
//! kept, 401 and 404 are left out, and another pod never sees them.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use pod_core::layouts::LayoutSource;
use pod_core::session::session_hash;
use pod_core::{
    AppState, BoxFuture, CoreLayoutInfo, Hydration, Pod, RequestCtx, Source, SourceManifest,
};
use serde_json::{Value, json};
use source_monitor::{ENABLED_SETTING, MonitorAdapter, MonitorConfig};
use tower::ServiceExt;

use super::alerts::call;

const PHONE: &str = "phone0000device01";
const SECRET: &str = "cpod_k7Qx9Secret";

/// A source whose routes fail the way an upstream outage does.
struct Flaky;

fn answer(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

impl Source for Flaky {
    fn name(&self) -> &'static str {
        "flaky"
    }
    fn core_layouts(&self) -> Vec<CoreLayoutInfo> {
        Vec::new()
    }
    fn layout_files(&self) -> &'static [LayoutSource] {
        &[]
    }
    fn manifest(&self) -> SourceManifest {
        SourceManifest::new("Flaky")
    }
    fn router(&self) -> Router<Pod> {
        Router::new()
            .route(
                "/v1/flaky/{token}/react",
                post(|| async {
                    answer(
                        StatusCode::BAD_GATEWAY,
                        json!({"error": "taste_unreachable",
                            "detail": "connection refused by taste.example"}),
                    )
                }),
            )
            .route(
                "/v1/flaky/conflict",
                post(|| async {
                    answer(
                        StatusCode::CONFLICT,
                        json!({"error": "not a code: free text from upstream"}),
                    )
                }),
            )
            .route(
                "/v1/flaky/stale",
                post(|| async { answer(StatusCode::UNAUTHORIZED, json!({"error": "stale"})) }),
            )
            .route(
                "/v1/flaky/gone",
                get(|| async { answer(StatusCode::NOT_FOUND, json!({"error": "not_found"})) }),
            )
    }
    fn feed<'a>(&'a self, _: &'a Pod, f: &'a str, _: &'a RequestCtx) -> BoxFuture<'a, Hydration> {
        Box::pin(async move { pod_core::unknown_feed("flaky", f) })
    }
}

fn flaky_pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(MonitorAdapter::new(MonitorConfig::default())),
        source_shell::adapter(),
        Arc::new(Flaky),
    ]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod.store().set_pod_setting(ENABLED_SETTING, "1").unwrap();
    pod
}

/// A request from the phone's app.
async fn from_phone(pod: &Pod, method: &str, uri: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer phone")
        .header(protocol::DEVICE_HEADER, PHONE)
        .header(
            protocol::BUILD_HEADER,
            "android 0.6.0+20261118153000.c41d9e2",
        )
        .header(protocol::DEVICE_NAME_HEADER, "Pixel 10 Pro")
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"token":"{SECRET}"}}"#)))
        .unwrap();
    let resp = pod_core::app(pod.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    // The answer still reaches the caller whole.
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    if status == StatusCode::BAD_GATEWAY {
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["detail"], "connection refused by taste.example");
    }
    status
}

/// `sample_data` of a monitor feed: the raw text and the parsed data.
async fn feed(pod: &Pod, name: &str) -> (String, Value) {
    let (status, body) = call(
        pod,
        "POST",
        "/v1/mcp",
        "pod",
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "sample_data",
                "arguments": {"source": "monitor", "feed": name, "limit": 100}}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["result"]["isError"], true, "{body}");
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let v = serde_json::from_str(&text).unwrap();
    (text, v)
}

/// The entities of a feed's data, under the wrapper `sample_data` may put
/// around them.
fn entities(data: &Value) -> &Value {
    let e = &data["entities"];
    if e["untrusted"].is_object() {
        &e["untrusted"]["content"]
    } else {
        e
    }
}

#[tokio::test]
async fn a_502_is_named_by_route_code_and_device_in_its_own_pod_only() {
    let pod = flaky_pod();
    let other = flaky_pod();

    for _ in 0..3 {
        let s = from_phone(
            &pod,
            "POST",
            &format!("/v1/flaky/{SECRET}/react?code=hunter2"),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
    }
    assert_eq!(
        from_phone(&pod, "POST", "/v1/flaky/conflict").await,
        StatusCode::CONFLICT
    );
    // Routine answers are left out.
    assert_eq!(
        from_phone(&pod, "POST", "/v1/flaky/stale").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        from_phone(&pod, "GET", "/v1/flaky/gone").await,
        StatusCode::NOT_FOUND
    );

    let (text, data) = feed(&pod, "errors").await;
    let order = data["queries"]["monitor:recent_errors"]
        .as_array()
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(order.len(), 4, "{text}");
    let all = entities(&data);
    // Newest first: the 409, whose free-text error is no code.
    let conflict = &all[order[0].as_str().unwrap()];
    assert_eq!(conflict["route"], "/v1/flaky/conflict", "{text}");
    assert_eq!(conflict["status"], 409);
    assert_eq!(conflict["code"], "");
    assert_eq!(conflict["server_error"], false);
    let failed = &all[order[1].as_str().unwrap()];
    assert_eq!(failed["method"], "POST");
    assert_eq!(failed["route"], "/v1/flaky/{token}/react", "{text}");
    assert_eq!(failed["status"], 502);
    assert_eq!(failed["code"], "taste_unreachable");
    assert_eq!(failed["device_id"], PHONE);
    assert_eq!(
        failed["title"],
        "POST /v1/flaky/{token}/react → 502 taste_unreachable"
    );
    assert!(failed["time"].as_i64().unwrap() > 0);
    // Never the path's token, the query or the body.
    for secret in [SECRET, "hunter2", "connection refused", "stale"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }

    // The check names them, with the device's name once its ping is written.
    let mut detail = String::new();
    for _ in 0..200 {
        let (_, data) = feed(&pod, "overview").await;
        detail = entities(&data)["monitor://checks/server_errors"]["detail"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if detail.contains("Pixel 10 Pro") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        detail.ends_with(
            " · in this pod: POST /v1/flaky/{token}/react → 502 taste_unreachable ×3 \
             (Pixel 10 Pro)"
        ),
        "{detail}"
    );
    let (_, data) = feed(&pod, "errors").await;
    let failed = &entities(&data)[data["queries"]["monitor:recent_errors"][1]
        .as_str()
        .unwrap()];
    assert_eq!(failed["device"], "Pixel 10 Pro");

    // Another pod of the same server sees none of them.
    let (text, data) = feed(&other, "errors").await;
    assert_eq!(
        data["queries"]["monitor:recent_errors"],
        json!([]),
        "{text}"
    );
    let (_, data) = feed(&other, "overview").await;
    let detail = entities(&data)["monitor://checks/server_errors"]["detail"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!detail.contains("in this pod"), "{detail}");
}

#[tokio::test]
async fn only_the_newest_fifty_are_kept() {
    let pod = flaky_pod();
    for _ in 0..(pod_core::recent_errors::KEEP + 5) {
        from_phone(&pod, "POST", "/v1/flaky/conflict").await;
    }
    // (`sample_data` shows 20 at most: read the list itself.)
    assert_eq!(
        pod_core::recent_errors::recent(&pod).len(),
        pod_core::recent_errors::KEEP
    );
}
