//! Rendered fillers (decision `rendered-fetches`): a product page whose
//! JSON-LD only its script writes fills each row once the server's browser
//! has rendered it; without a browser, previews and fills say so. The
//! browser half is skipped (passing) on a machine without one
//! (`CONSTRUCT_BROWSER`, or Chromium on `PATH`).

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::Request,
    response::{Html, IntoResponse},
    routing::get,
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use source_collections::fill;
use source_collections::{Caps, CollectionsAdapter};
use tower::ServiceExt;
use web_extract::render::Browser;

/// A plain fetch sees no product; the script adds it.
const PAGE: &str = r#"<!doctype html><html><head><title>Wool scarf</title></head><body>
<div id="app"></div>
<script>
setTimeout(() => {
  const s = document.createElement('script');
  s.type = 'application/ld+json';
  s.textContent = JSON.stringify({"@type": "Product", "name": "Wool scarf",
    "image": [{"@type": "ImageObject", "url": "https://img.shop.example/scarf.jpg"}],
    "offers": {"@type": "Offer", "price": "48.00"}});
  document.head.appendChild(s);
  document.getElementById('app').textContent = 'Wool scarf';
}, 200);
</script></body></html>"#;

async fn serve() -> String {
    let app = Router::new().route("/p/scarf", get(|| async { Html(PAGE).into_response() }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn pod_with(browser: Option<Browser>) -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_private_hosts(true)
        .with_browser(browser);
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod
}

async fn tool(pod: &Pod, name: &str, args: Value) -> (bool, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/mcp")
        .header("authorization", "Bearer pod")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": name, "arguments": args}})
            .to_string(),
        ))
        .unwrap();
    let resp = pod_core::app(pod.clone()).oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let r = &body["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("null");
    (r["isError"] == true, serde_json::from_str(text).unwrap())
}

async fn ok(pod: &Pod, name: &str, args: Value) -> Value {
    let (err, v) = tool(pod, name, args).await;
    assert!(!err, "{name}: {v}");
    v
}

/// A Style deck: each card's product page fills its image and price.
async fn style_cards(pod: &Pod, url: &str) {
    ok(
        pod,
        "define_collection",
        json!({"name": "style_cards", "title": "Style", "fields": [
            {"name": "url", "type": "text"},
            {"name": "image_url", "type": "text", "optional": true},
            {"name": "price", "type": "number", "optional": true},
        ]}),
    )
    .await;
    ok(
        pod,
        "add_row",
        json!({"collection": "style_cards", "values": {"url": url}}),
    )
    .await;
    ok(
        pod,
        "apply_change",
        json!({"collection": "style_cards", "changes": [{"op": "set_filler", "filler": {
        "kind": "fetch", "url_field": "url", "extract": "jsonld", "types": ["Product"],
        "map": {"image_url": "image[0].url", "price": "offers.price"},
        "render": {"wait_for": "script[type='application/ld+json']"}, "refresh": "once"}}]}),
    )
    .await;
}

async fn filled(pod: &Pod) -> (Value, Value) {
    let id = ok(
        pod,
        "describe_collection",
        json!({"collection": "style_cards"}),
    )
    .await["definition"]["fillers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The fill set_filler started, then one more.
    while fill::is_busy(pod, "style_cards") {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    fill::fill(pod, "style_cards", &id, true).await;
    let status = ok(
        pod,
        "collections_status",
        json!({"collection": "style_cards"}),
    )
    .await;
    let rows = ok(
        pod,
        "export_collection",
        json!({"collection": "style_cards"}),
    )
    .await;
    (status["fillers"][0].clone(), rows)
}

#[tokio::test]
async fn without_a_browser_rendered_fillers_say_so() {
    let base = serve().await;
    let pod = pod_with(None);
    let (err, v) = tool(
        &pod,
        "collections_preview",
        json!({"url": format!("{base}/p/scarf"), "extract": "jsonld", "types": ["Product"], "render": true}),
    )
    .await;
    assert!(
        err && v["detail"].as_str().unwrap().contains("headless browser"),
        "{v}"
    );
    style_cards(&pod, &format!("{base}/p/scarf")).await;
    let (status, _) = filled(&pod).await;
    assert!(
        status["detail"]
            .as_str()
            .unwrap()
            .contains("headless browser"),
        "{status}"
    );
}

#[tokio::test]
async fn a_rendered_product_page_fills_its_card() {
    let Some(browser) = Browser::from_env(true) else {
        eprintln!("no headless browser here (CONSTRUCT_BROWSER): skipped");
        return;
    };
    let base = serve().await;
    let url = format!("{base}/p/scarf");
    let pod = pod_with(Some(browser));

    // A plain fetch finds no product; the rendered page does.
    let plain = ok(
        &pod,
        "collections_preview",
        json!({"url": url, "extract": "jsonld", "types": ["Product"]}),
    )
    .await;
    assert_eq!(plain["items"], 0, "{plain}");
    let rendered = ok(
        &pod,
        "collections_preview",
        json!({"url": url, "extract": "jsonld", "types": ["Product"],
               "map": {"name": "name", "price": "offers.price"}, "render": true}),
    )
    .await;
    assert_eq!(rendered["rendered"], true);
    assert_eq!(
        rendered["rows"]["untrusted"]["content"][0]["name"], "Wool scarf",
        "{rendered}"
    );

    style_cards(&pod, &url).await;
    let (status, rows) = filled(&pod).await;
    assert_eq!(status["state"], "ok", "{status}");
    let text = rows.to_string();
    assert!(
        text.contains("https://img.shop.example/scarf.jpg") && text.contains("48"),
        "{rows}"
    );
}
