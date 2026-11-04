//! Fillers and alerts (decision `collection-fillers`) on a real pod against a
//! local fake website: a fetch recipe fills election results (replace by
//! key, conditional requests, hand edits of unfilled fields kept, filled
//! fields and rows locked), each product row is filled from its own page
//! (jsonld, prices normalized), a header sends a sealed secret, previews show
//! sample items and rows, two event sites fill one collection side by side
//! (events extractor, zones, past events dropped, details from each event's
//! page, an llm site read by a fake model) and alerts notify once per match,
//! an agent filler (a fake model) upserts rows within its daily budget, and
//! temporary pods can't fill.

use std::sync::{Arc, Mutex};

use app_collections::agent::{AgentAnswer, AgentRequest, FillAgent, PageRequest};
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

/// An improv club's calendar: a past show, a Brass Lanterns show, a class.
async fn club_ics() -> Response {
    "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\nUID:old\r\nSUMMARY:Old show\r\nDTSTART:20200101T193000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:bc1\r\nSUMMARY:Friday Improv\r\nDTSTART;TZID=America/Denver:20991106T193000\r\n\
LOCATION:The Back Room\r\nDESCRIPTION:Lineup: Brass Lanterns\\, Teacup Riot\r\nURL:/shows/bc1\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:cl1\r\nSUMMARY:Improv class (Brass Lanterns teach)\r\nDTSTART:20991107T100000\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n"
        .into_response()
}

/// A show's own page with the full lineup.
async fn show_page() -> Response {
    r#"<script type="application/ld+json">{"@type":"Event","name":"Friday Improv","startDate":"2099-11-06T19:30:00-07:00",
       "description":"Full lineup: Brass Lanterns, Teacup Riot and friends."}</script>"#
        .into_response()
}

/// A page with no structure (read by the llm extractor).
async fn plain_page() -> Response {
    "<html><body><h1>Shows</h1><p>Sat Nov 7 2099, 8pm: Brass Lanterns live</p></body></html>"
        .into_response()
}

async fn serve(site: Arc<Site>) -> String {
    let app = Router::new()
        .route("/results.json", get(results))
        .route("/p/{id}", get(product))
        .route("/private.json", get(private_feed))
        .route("/club.ics", get(club_ics))
        .route("/shows/{id}", get(show_page))
        .route("/plain", get(plain_page))
        .with_state(site);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A fake model: answers `rows` (an agent) or `page_rows` (a page), and
/// records what it was asked.
#[derive(Debug, Default)]
struct FakeAgent {
    rows: Mutex<Vec<Value>>,
    page_rows: Mutex<Vec<Value>>,
    seen_current: Mutex<Vec<usize>>,
    pages: Mutex<Vec<String>>,
}

fn objects(v: &[Value]) -> Vec<Map<String, Value>> {
    v.iter().map(|r| r.as_object().unwrap().clone()).collect()
}

impl FillAgent for FakeAgent {
    fn fill<'a>(&'a self, req: AgentRequest<'a>) -> BoxFuture<'a, Result<AgentAnswer, String>> {
        Box::pin(async move {
            self.seen_current.lock().unwrap().push(req.current.len());
            Ok(AgentAnswer {
                rows: objects(&self.rows.lock().unwrap()),
                searches: 2,
                fetches: 1,
            })
        })
    }

    fn read_page<'a>(
        &'a self,
        req: PageRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            self.pages.lock().unwrap().push(req.page_text.to_owned());
            Ok(objects(&self.page_rows.lock().unwrap()))
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

/// The bundle of `path`.
async fn get_data(pod: &Pod, path: &str) -> Value {
    let (status, b) = call(pod, "GET", path, None).await;
    assert_eq!(status, StatusCode::OK, "{path}: {b}");
    b
}

/// The entities of `query` in `bundle`.
fn listed(b: &Value, query: &str) -> Vec<Value> {
    b["queries"][query]
        .as_array()
        .unwrap_or_else(|| panic!("no query {query}: {b}"))
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()].clone())
        .collect()
}

/// The rows of `collection`.
async fn rows(pod: &Pod, collection: &str) -> Vec<Value> {
    let b = get_data(pod, &format!("/v1/collections/{collection}/rows")).await;
    listed(&b, &format!("collection:{collection}"))
}

/// The ids of `collection`'s fillers.
async fn filler_ids(pod: &Pod, collection: &str) -> Vec<String> {
    let d = ok(
        pod,
        "describe_collection",
        json!({"collection": collection}),
    )
    .await;
    d["definition"]["fillers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_owned())
        .collect()
}

/// Wait until the fills a define or set_filler started have ended.
async fn settle(pod: &Pod, collection: &str) {
    for _ in 0..500 {
        let c = collection.to_owned();
        let all_filled = pod
            .store()
            .with_conn(|conn| {
                let Some((_, def)) = app_collections::db::latest_def(conn, &c)? else {
                    return Ok::<bool, rusqlite::Error>(false);
                };
                let states = fill::states(conn, &def)?;
                Ok(states.values().all(|s| s.last_fill_at.is_some()))
            })
            .unwrap();
        if all_filled && !fill::is_busy(pod, collection) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the fills of {collection} never ended");
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
    ok(
        &pod,
        "define_collection",
        json!({"name": "results", "fields": [
            {"name": "candidate", "type": "text", "optional": true},
            {"name": "votes", "type": "number", "default": 0},
            {"name": "note", "type": "text", "optional": true}],
            "order_by": "votes", "descending": true,
            "fillers": [{"kind": "fetch", "url": format!("{base}/results.json"), "extract": "json",
                       "items": "candidates", "map": {"candidate": "name", "votes": "votes"},
                       "key": "candidate", "on_open_minutes": 2, "title": "Mayor"}]}),
    )
    .await;
    settle(&pod, "results").await;
    let filler = filler_ids(&pod, "results").await.remove(0);
    let r = rows(&pod, "results").await;
    assert_eq!(r.len(), 2);
    assert_eq!(
        (r[0]["candidate"].clone(), r[0]["votes"].clone()),
        (json!("Blake"), json!(1050))
    );
    assert_eq!(r[0]["filler_title"], "Mayor");
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
    let (s, _) = call(
        &pod,
        "POST",
        "/v1/collections/results/delete",
        Some(json!({"id": blake})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "its rows can't be deleted by hand");
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
    assert_eq!(
        fill::fill(&pod, "results", &filler, false).await,
        Outcome::Unchanged
    );
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
    let f = &out["fills"][0];
    assert_eq!(
        (
            f["created"].clone(),
            f["updated"].clone(),
            f["removed"].clone()
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
    // The feed lists the fillers with their health; a filler has a screen.
    let b = get_data(&pod, "/v1/collections/results/rows").await;
    let fillers = listed(&b, "collection:results:fillers");
    assert_eq!(
        (fillers[0]["title"].clone(), fillers[0]["state"].clone()),
        (json!("Mayor"), json!("ok"))
    );
    let screen = get_data(&pod, &format!("/v1/collections/results/fillers/{filler}")).await;
    assert_eq!(
        listed(&screen, &format!("collection:results:filler:{filler}:rows")).len(),
        2
    );
    let one = get_data(&pod, &format!("/v1/collections/results/row?id={blake}")).await;
    assert_eq!(
        listed(&one, &format!("collection:results:row:{blake}"))[0]["votes"],
        2000
    );
    // A field a filler fills can't be removed while it does.
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
        Some(json!({"filler": filler})),
    )
    .await;
    assert_eq!((s, b["refreshing"].clone()), (StatusCode::OK, json!(true)));
    settle(&pod, "results").await;
    // Removing the filler keeps the rows and frees them.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "results", "changes": [{"op": "remove_filler", "filler": filler}]}),
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
            "fillers": [{"kind": "fetch", "url_field": "url", "extract": "jsonld", "types": ["Product"],
                       "map": {"name": "name", "price": "offers.price", "stock": "offers.availability"},
                       "every_minutes": 360}]}),
    )
    .await;
    settle(&pod, "wishlist").await;
    let filler = filler_ids(&pod, "wishlist").await.remove(0);
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
    let out = fill::fill(&pod, "wishlist", &filler, false).await;
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
        "fillers": [{"kind": "fetch", "url": format!("{base}/private.json"), "extract": "json", "items": "data",
                   "map": {"title": "title"}, "headers": {"Authorization": "Bearer {secret:feed_key}"}}]})).await;
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
async fn event_sites_fill_one_collection_and_alerts_notify_once() {
    let base = serve(Arc::new(Site::default())).await;
    let agent = Arc::new(FakeAgent::default());
    *agent.page_rows.lock().unwrap() = vec![json!({
        "title": "Brass Lanterns live", "start": "2099-11-07T20:00"})];
    let pod = pod_with(Some(agent.clone()));
    let fields = json!([
        {"name": "source_id", "type": "text", "optional": true},
        {"name": "title", "type": "text", "optional": true},
        {"name": "start", "type": "datetime", "optional": true},
        {"name": "venue", "type": "text", "optional": true},
        {"name": "description", "type": "text", "optional": true},
        {"name": "url", "type": "text", "optional": true},
        {"name": "seen", "type": "bool", "default": false}]);
    ok(
        &pod,
        "define_collection",
        json!({"name": "events", "fields": fields, "order_by": "start",
        "fillers": [
            {"kind": "fetch", "title": "Improv Club", "url": format!("{base}/club.ics"), "extract": "events",
             "zone": "America/Denver",
             "map": {"source_id": "id", "title": "title", "start": "start", "venue": "venue", "url": "url"},
             "key": "source_id", "upcoming": "start", "every_minutes": 360,
             "details": {"link": "url", "extract": "events", "map": {"description": "description"}}},
            {"kind": "fetch", "title": "Plain Club", "url": format!("{base}/plain"), "extract": "llm",
             "fields": ["title", "start"], "hint": "shows only", "zone": "America/Denver", "upcoming": "start"}]}),
    )
    .await;
    settle(&pod, "events").await;
    let ids = filler_ids(&pod, "events").await;
    let r = rows(&pod, "events").await;
    let titles: Vec<&str> = r.iter().map(|e| e["title"].as_str().unwrap()).collect();
    assert_eq!(
        titles,
        [
            "Friday Improv",
            "Improv class (Brass Lanterns teach)",
            "Brass Lanterns live"
        ],
        "the past show is dropped; ordered by start across both sites"
    );
    let friday = &r[0];
    assert_eq!(friday["start"], "2099-11-06T19:30-07:00", "TZID resolved");
    assert_eq!(friday["start_text"], "Fri Nov 6 · 7:30 PM");
    assert_eq!(friday["start_date"], "2099-11-06");
    assert_eq!(
        friday["description"], "Full lineup: Brass Lanterns, Teacup Riot and friends.",
        "details read the show's own page"
    );
    assert_eq!(
        r[1]["start"], "2099-11-07T10:00-07:00",
        "placed in the zone"
    );
    assert_eq!(
        r[2]["start"], "2099-11-07T20:00-07:00",
        "the model's time too"
    );
    assert_eq!(r[2]["filler_id"], json!(ids[1]));
    assert!(agent.pages.lock().unwrap()[0].contains("Brass Lanterns live"));
    // An alert per site.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "events", "changes": [
            {"op": "add_alert", "any": ["Brass Lanterns"], "none": ["class"], "filler": ids[0]},
            {"op": "add_alert", "any": ["brass lanterns"], "filler": ids[1], "name": "BL (plain)"}]}),
    )
    .await;
    assert!(!err, "{e}");
    let st = ok(&pod, "collections_status", json!({"collection": "events"})).await;
    assert_eq!(
        st["fillers"][0]["alerts"][0]["name"], "Brass Lanterns",
        "{st}"
    );
    let b = get_data(&pod, "/v1/collections/events/rows").await;
    let matched = listed(&b, "collection:events:alerted");
    assert_eq!(
        matched
            .iter()
            .map(|e| e["alert_names"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Brass Lanterns", "BL (plain)"]
    );
    let screen = get_data(&pod, &format!("/v1/collections/events/fillers/{}", ids[0])).await;
    assert_eq!(
        listed(&screen, &format!("collection:events:filler:{}", ids[0]))[0]["alerts_text"],
        "Watching for Brass Lanterns"
    );
    // Notifications need an app showing the collection.
    let app = ok(
        &pod,
        "create_app",
        json!({"title": "Shows", "feeds": [["collections", "events"]],
            "layout": {"type": "core://Workspace", "id": "x", "title": "Shows", "slots": [
                {"slot_id": "list", "provider_uri": "core://custom", "content": {
                    "type": "core://List", "query_key": "collection:events:alerted",
                    "item_template": {"type": "core://Text", "style": "Body",
                        "value_expr": "$item.title"}}}]}}),
    )
    .await;
    let app_id = app["id"].as_str().unwrap().to_owned();
    app_collections::alerts::run(&pod, None).await;
    let sent: Vec<(String, String)> = pod
        .store()
        .with_conn(|c| {
            let mut stmt = c.prepare("SELECT app_id, title FROM notifications ORDER BY title")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(
        sent,
        vec![
            (app_id.clone(), "Brass Lanterns live".to_owned()),
            (app_id.clone(), "Friday Improv".to_owned())
        ]
    );
    // Once.
    app_collections::alerts::run(&pod, None).await;
    let n: i64 = pod
        .store()
        .with_conn(|c| c.query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(n, 2);
    // A filler's alerts go with it.
    ok(
        &pod,
        "apply_change",
        json!({"collection": "events", "changes": [{"op": "remove_filler", "filler": ids[1]}]}),
    )
    .await;
    let d = ok(&pod, "describe_collection", json!({"collection": "events"})).await;
    assert_eq!(d["definition"]["alerts"].as_array().unwrap().len(), 1);
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
        "fillers": [{"kind": "agent", "prompt": "best 27-inch monitors under $400", "fills_per_day": 99}]})).await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("operator"),
        "{e}"
    );
    ok(&pod, "define_collection", json!({"name": "monitors", "fields": [
            {"name": "name", "type": "text", "optional": true},
            {"name": "price", "type": "number", "optional": true},
            {"name": "url", "type": "text", "optional": true},
            {"name": "bought", "type": "bool", "default": false}],
        "fillers": [{"kind": "agent", "prompt": "best 27-inch monitors under $400",
                   "fields": ["name", "price", "url"], "mode": "upsert", "key": "url", "fills_per_day": 2}]})).await;
    settle(&pod, "monitors").await;
    let filler = filler_ids(&pod, "monitors").await.remove(0);
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
    let out = fill::fill(&pod, "monitors", &filler, true).await;
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
    let out = fill::fill(&pod, "monitors", &filler, true).await;
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
    assert_eq!(st["fillers"][0]["model_fills_today"], 2);
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
        "fillers": [{"kind": "fetch", "url": "https://example.com/feed", "extract": "rss", "map": {"title": "title"}}]})).await;
    settle(&pod, "news").await;
    let st = ok(&pod, "collections_status", json!({})).await;
    assert_eq!(st["fillers"][0]["state"], "off", "{st}");
    let pod = pod_with(None);
    ok(
        &pod,
        "define_collection",
        json!({"name": "picks", "fields": [{"name": "title", "type": "text", "optional": true}],
        "fillers": [{"kind": "agent", "prompt": "good books"}]}),
    )
    .await;
    settle(&pod, "picks").await;
    let st = ok(&pod, "collections_status", json!({})).await;
    assert_eq!(st["fillers"][0]["state"], "unavailable", "{st}");
}
