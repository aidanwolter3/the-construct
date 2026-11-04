//! Fillers (decision `collection-fillers`) on a real pod against a local
//! fake website: a fetch recipe fills election results (replace by key,
//! conditional requests, hand edits of unfilled fields kept, filled fields
//! and rows locked), each product row is filled from its own page
//! (jsonld, prices normalized), a header sends a sealed secret, previews
//! show sample items and rows, an agent filler (a fake model) upserts rows
//! within its daily budget, and temporary pods can't fill.

use std::sync::{Arc, Mutex};

use app_collections::agent::{AgentAnswer, AgentRequest, FillAgent};
use app_collections::fill::{self, Outcome};
use app_collections::{Caps, CollectionsAdapter};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, BoxFuture, Pod};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

/// The fake website's state: the results JSON and how often it was read.
#[derive(Default)]
struct Site {
    results: Mutex<Value>,
    hits: Mutex<usize>,
}

async fn results(State(site): State<Arc<Site>>, headers: HeaderMap) -> Response {
    *site.hits.lock().unwrap() += 1;
    let body = site.results.lock().unwrap().to_string();
    let etag = format!("\"{}\"", body.len());
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    ([(header::ETAG, etag)], body).into_response()
}

async fn product(axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    let price = if id == "1" { "$1,299.00" } else { "45" };
    format!(
        r#"<html><script type="application/ld+json">{{"@context":"https://schema.org","@graph":[
        {{"@type":"Product","name":"Item {id}","offers":{{"@type":"Offer","price":"{price}","availability":"https://schema.org/InStock"}}}}]}}</script></html>"#
    )
    .into_response()
}

async fn private_feed(headers: HeaderMap) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bearer s3cret")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    r#"{"data":[{"title":"Hidden"}]}"#.into_response()
}

async fn serve(site: Arc<Site>) -> String {
    let app = Router::new()
        .route("/results.json", get(results))
        .route("/p/{id}", get(product))
        .route("/private.json", get(private_feed))
        .with_state(site);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A fake agent: answers `rows`, and records the requests' current rows.
#[derive(Debug, Default)]
struct FakeAgent {
    rows: Mutex<Vec<Value>>,
    seen_current: Mutex<Vec<usize>>,
}

impl FillAgent for FakeAgent {
    fn fill<'a>(&'a self, req: AgentRequest<'a>) -> BoxFuture<'a, Result<AgentAnswer, String>> {
        Box::pin(async move {
            self.seen_current.lock().unwrap().push(req.current.len());
            let rows: Vec<Map<String, Value>> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.as_object().unwrap().clone())
                .collect();
            Ok(AgentAnswer {
                rows,
                searches: 2,
                fetches: 1,
            })
        })
    }
}

fn pod_with(agent: Option<Arc<FakeAgent>>) -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_private_hosts(true)
        .with_agent(agent.map(|a| a as Arc<dyn FillAgent>));
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    pod
}

async fn call(pod: &Pod, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(
            protocol::SCHEMA_HEADER,
            protocol::SCHEMA_VERSION.to_string(),
        )
        .header(protocol::FEATURES_HEADER, protocol::features_header())
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

/// The rows of `collection` by a field, `{key: entity}`.
async fn rows(pod: &Pod, collection: &str) -> Vec<Value> {
    let (status, b) = call(
        pod,
        "GET",
        &format!("/v1/collections/{collection}/rows"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["queries"][format!("collection:{collection}")]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()].clone())
        .collect()
}

/// Wait until the fill a define or set_filler started has ended.
async fn settle(pod: &Pod, collection: &str) {
    for _ in 0..500 {
        let c = collection.to_owned();
        let filled = pod
            .store()
            .with_conn(|conn| fill::state(conn, &c))
            .unwrap()
            .last_fill_at
            .is_some();
        if filled && !fill::is_busy(pod, collection) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("a fill of {collection} never ended");
}

#[tokio::test]
async fn a_fetch_recipe_keeps_results_current() {
    let site = Arc::new(Site::default());
    *site.results.lock().unwrap() = json!({"race": "Mayor", "candidates": [
        {"name": "Avery", "votes": 120}, {"name": "Blake", "votes": "1,050"}]});
    let base = serve(site.clone()).await;
    let pod = pod_with(None);
    // Preview without a map shows the items to write paths from.
    let p = ok(
        &pod,
        "collections_preview",
        json!({"url": format!("{base}/results.json"), "extract": "json", "items": "candidates"}),
    )
    .await;
    assert_eq!(p["items"], 2, "{p}");
    assert_eq!(p["sample_items"][0]["name"], "Avery");
    let p = ok(
        &pod,
        "collections_preview",
        json!({"url": format!("{base}/results.json"), "extract": "json", "items": "candidates",
               "map": {"candidate": "name", "votes": "votes"}}),
    )
    .await;
    assert_eq!(
        p["rows"][1]["votes"], "1,050",
        "raw before the collection coerces"
    );
    ok(
        &pod,
        "define_collection",
        json!({"name": "results", "fields": [
            {"name": "candidate", "type": "text", "optional": true},
            {"name": "votes", "type": "number", "default": 0},
            {"name": "note", "type": "text", "optional": true}],
            "order_by": "votes", "descending": true,
            "filler": {"kind": "fetch", "url": format!("{base}/results.json"), "extract": "json",
                       "items": "candidates", "map": {"candidate": "name", "votes": "votes"},
                       "key": "candidate", "on_open_minutes": 2}}),
    )
    .await;
    settle(&pod, "results").await;
    let r = rows(&pod, "results").await;
    assert_eq!(r.len(), 2);
    assert_eq!(
        (r[0]["candidate"].clone(), r[0]["votes"].clone()),
        (json!("Blake"), json!(1050))
    );
    let blake = r[0]["id"].as_str().unwrap().to_owned();
    // An unfilled field takes a hand edit; filled fields and new rows don't.
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/results/update",
        Some(json!({"id": blake, "note": "incumbent"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, e) = call(
        &pod,
        "POST",
        "/v1/collections/results/update",
        Some(json!({"id": blake, "votes": 1})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("filled_by_filler"))
    );
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/results/create",
        Some(json!({"note": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "replace owns its rows");
    let (err, e) = tool(
        &pod,
        "edit_rows",
        json!({"collection": "results",
        "edits": [{"op": "update", "id": blake, "values": {"votes": 2}}]}),
    )
    .await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("filled"),
        "{e}"
    );
    // An unchanged page: a 304, no work.
    let hits = *site.hits.lock().unwrap();
    assert_eq!(fill::fill(&pod, "results", false).await, Outcome::Unchanged);
    assert_eq!(*site.hits.lock().unwrap(), hits + 1);
    // New counts: updated by key, the hand edit kept, a dropped name removed.
    *site.results.lock().unwrap() = json!({"race": "Mayor", "candidates": [
        {"name": "Blake", "votes": 2000}, {"name": "Casey", "votes": 30}]});
    let out = ok(
        &pod,
        "collections_refresh",
        json!({"collection": "results"}),
    )
    .await;
    assert_eq!(
        (
            out["created"].clone(),
            out["updated"].clone(),
            out["removed"].clone()
        ),
        (json!(1), json!(1), json!(1)),
        "{out}"
    );
    let r = rows(&pod, "results").await;
    assert_eq!(r[0]["id"], json!(blake), "rows keep their ids by key");
    assert_eq!(
        (r[0]["votes"].clone(), r[0]["note"].clone()),
        (json!(2000), json!("incumbent"))
    );
    assert_eq!(r[1]["candidate"], "Casey");
    let st = ok(&pod, "collections_status", json!({"collection": "results"})).await;
    assert_eq!(st["fillers"][0]["state"], "ok", "{st}");
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "results"}),
    )
    .await;
    assert_eq!(d["definition"]["filler"]["map"]["votes"], "votes");
    assert_eq!(d["fill"]["kind"], "fetch");
    // A field the filler fills can't be removed while it does.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "results",
        "changes": [{"op": "remove_field", "field": "votes"}]}),
    )
    .await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("filler"),
        "{e}"
    );
    // A layout's refresh action.
    let (s, b) = call(
        &pod,
        "POST",
        "/v1/collections/results/refresh",
        Some(json!({})),
    )
    .await;
    assert_eq!((s, b["refreshing"].clone()), (StatusCode::OK, json!(true)));
    settle(&pod, "results").await;
    // Removing the filler keeps the rows and frees them.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "results", "changes": [{"op": "remove_filler"}]}),
    )
    .await;
    assert_eq!(rows(&pod, "results").await.len(), 2);
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/results/update",
        Some(json!({"id": blake, "votes": 1})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn rows_fill_from_their_own_pages_and_secrets_stay_sealed() {
    let base = serve(Arc::new(Site::default())).await;
    let pod = pod_with(None);
    ok(
        &pod,
        "define_collection",
        json!({"name": "wishlist", "fields": [
            {"name": "url", "type": "text"},
            {"name": "name", "type": "text", "optional": true},
            {"name": "price", "type": "number", "optional": true},
            {"name": "stock", "type": "choice", "options": ["InStock", "OutOfStock"], "optional": true}],
            "filler": {"kind": "fetch", "url_field": "url", "extract": "jsonld", "types": ["Product"],
                       "map": {"name": "name", "price": "offers.price", "stock": "offers.availability"},
                       "every_minutes": 360}}),
    )
    .await;
    settle(&pod, "wishlist").await;
    // People add rows by URL.
    for id in ["1", "2"] {
        let (s, b) = call(
            &pod,
            "POST",
            "/v1/collections/wishlist/create",
            Some(json!({"url": format!("{base}/p/{id}")})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/wishlist/create",
        Some(json!({"url": "x", "price": 3})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "price is filled");
    let out = fill::fill(&pod, "wishlist", false).await;
    assert!(matches!(out, Outcome::Filled { items: 2, .. }), "{out:?}");
    let r = rows(&pod, "wishlist").await;
    assert_eq!(
        (r[0]["price"].clone(), r[0]["stock"].clone()),
        (json!(1299), json!("InStock"))
    );
    assert_eq!(r[1]["name"], "Item 2");
    let p = ok(
        &pod,
        "collections_preview",
        json!({"collection": "wishlist"}),
    )
    .await;
    assert_eq!(p["rows"][0]["price"], 1299, "{p}");

    // A header with a secret: unset fails, set works, never shown back.
    ok(&pod, "define_collection", json!({"name": "hidden", "fields": [{"name": "title", "type": "text", "optional": true}],
        "filler": {"kind": "fetch", "url": format!("{base}/private.json"), "extract": "json", "items": "data",
                   "map": {"title": "title"}, "headers": {"Authorization": "Bearer {secret:feed_key}"}}})).await;
    settle(&pod, "hidden").await;
    let st = ok(&pod, "collections_status", json!({"collection": "hidden"})).await;
    assert_eq!(st["fillers"][0]["state"], "failing");
    assert!(
        st["fillers"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("feed_key"),
        "{st}"
    );
    let set = ok(
        &pod,
        "collections_set_secret",
        json!({"name": "feed_key", "value": "s3cret"}),
    )
    .await;
    assert_eq!(set["secrets"], json!(["feed_key"]));
    assert!(!set.to_string().contains("s3cret"));
    ok(&pod, "collections_refresh", json!({"collection": "hidden"})).await;
    assert_eq!(rows(&pod, "hidden").await[0]["title"], "Hidden");
    let st = ok(&pod, "collections_status", json!({})).await;
    assert!(!st.to_string().contains("s3cret"));
}

#[tokio::test]
async fn an_agent_upserts_within_its_budget() {
    let agent = Arc::new(FakeAgent::default());
    *agent.rows.lock().unwrap() = vec![
        json!({"name": "Acme 27", "price": "$329", "url": "https://shop.example/acme"}),
        json!({"name": "Bolt 27", "price": null, "url": "https://shop.example/bolt"}),
    ];
    let pod = pod_with(Some(agent.clone()));
    let (err, e) = tool(&pod, "define_collection", json!({"name": "monitors", "fields": [
            {"name": "name", "type": "text", "optional": true},
            {"name": "price", "type": "number", "optional": true},
            {"name": "url", "type": "text", "optional": true}],
        "filler": {"kind": "agent", "prompt": "best 27-inch monitors under $400", "fills_per_day": 99}})).await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("operator"),
        "{e}"
    );
    ok(&pod, "define_collection", json!({"name": "monitors", "fields": [
            {"name": "name", "type": "text", "optional": true},
            {"name": "price", "type": "number", "optional": true},
            {"name": "url", "type": "text", "optional": true},
            {"name": "bought", "type": "bool", "default": false}],
        "filler": {"kind": "agent", "prompt": "best 27-inch monitors under $400",
                   "fields": ["name", "price", "url"], "mode": "upsert", "key": "url", "fills_per_day": 2}})).await;
    settle(&pod, "monitors").await;
    let r = rows(&pod, "monitors").await;
    assert_eq!(r.len(), 2);
    assert_eq!(r[0]["price"], 329);
    // People may add their own rows and tick unfilled fields.
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/monitors/update",
        Some(json!({"id": r[0]["id"], "bought": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    *agent.rows.lock().unwrap() =
        vec![json!({"name": "Acme 27", "price": 299, "url": "https://shop.example/acme"})];
    let out = fill::fill(&pod, "monitors", true).await;
    assert!(matches!(out, Outcome::Filled { .. }), "{out:?}");
    let r = rows(&pod, "monitors").await;
    assert_eq!(r.len(), 2, "upsert keeps rows the answer leaves out");
    assert_eq!(
        (r[0]["price"].clone(), r[0]["bought"].clone()),
        (json!(299), json!(true))
    );
    assert_eq!(
        *agent.seen_current.lock().unwrap(),
        vec![0, 2],
        "it sees the current rows"
    );
    // Two fills a day.
    let out = fill::fill(&pod, "monitors", true).await;
    assert!(
        matches!(&out, Outcome::Failed(e) if e.contains("fills_per_day")),
        "{out:?}"
    );
    let st = ok(
        &pod,
        "collections_status",
        json!({"collection": "monitors"}),
    )
    .await;
    assert_eq!(st["fillers"][0]["agent_fills_today"], 2);
}

#[tokio::test]
async fn temporary_pods_dont_fill_and_no_model_means_unavailable() {
    let pod = pod_with(None);
    pod.store()
        .set_pod_setting(fill::FILL_SETTING, "0")
        .unwrap();
    let (err, e) = tool(
        &pod,
        "collections_preview",
        json!({"url": "https://example.com/x.json", "extract": "json"}),
    )
    .await;
    assert!(err && e["error"] == "fill_off", "{e}");
    ok(&pod, "define_collection", json!({"name": "news", "fields": [{"name": "title", "type": "text", "optional": true}],
        "filler": {"kind": "fetch", "url": "https://example.com/feed", "extract": "rss", "map": {"title": "title"}}})).await;
    settle(&pod, "news").await;
    let st = ok(&pod, "collections_status", json!({})).await;
    assert_eq!(st["fillers"][0]["state"], "off", "{st}");
    let pod = pod_with(None);
    ok(
        &pod,
        "define_collection",
        json!({"name": "picks", "fields": [{"name": "title", "type": "text", "optional": true}],
        "filler": {"kind": "agent", "prompt": "good books"}}),
    )
    .await;
    settle(&pod, "picks").await;
    let st = ok(&pod, "collections_status", json!({})).await;
    assert_eq!(st["fillers"][0]["state"], "unavailable", "{st}");
}
