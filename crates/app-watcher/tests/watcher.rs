//! The watcher on a real pod against a fake website and a fake model API:
//! a rule and a target added from the app's routes, a check that reads
//! JSON-LD and each new event's own page, the match notified once, a
//! conditional re-fetch, the ics and llm extractors, health, seen and
//! archive, and the MCP tools (fetching off included).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use app_watcher::check::FETCH_SETTING;
use app_watcher::when::civil_from_days;
use app_watcher::{LlmConfig, WatcherAdapter, WatcherConfig};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

/// What the fake site saw.
#[derive(Default)]
struct Site {
    listing_hits: AtomicUsize,
    not_modified: AtomicUsize,
    llm_calls: std::sync::Mutex<Vec<(HeaderMap, Value)>>,
}

/// `YYYY-MM-DD`, `days` from now (UTC).
fn day(days: i64) -> String {
    let now = pod_core::util::now_secs();
    let (y, m, d) = civil_from_days(now.div_euclid(86_400) + days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn ld(events: Value) -> String {
    format!(
        "<html><body><h1>Events</h1><script type=\"application/ld+json\">{events}</script></body></html>"
    )
}

async fn listing(State(site): State<Arc<Site>>, headers: HeaderMap) -> Response {
    site.listing_hits.fetch_add(1, Ordering::SeqCst);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some("\"v1\"")
    {
        site.not_modified.fetch_add(1, Ordering::SeqCst);
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let page = ld(json!([
        {"@context": "https://schema.org", "@type": "Event", "name": "Friday Improv",
         "startDate": format!("{}T19:30:00-07:00", day(3)), "url": "/e/1",
         "location": {"@type": "Place", "name": "The End"}},
        {"@type": "Event", "name": "Improv class: scenes", "startDate": format!("{}T18:00:00-07:00", day(4)),
         "url": "/e/2", "description": "Taught by Budget Cuts"},
        {"@type": "Event", "name": "Last week's show", "startDate": format!("{}T19:30:00-07:00", day(-7)),
         "description": "Budget Cuts"}
    ]));
    ([(header::ETAG, "\"v1\"")], axum::response::Html(page)).into_response()
}

async fn event_page() -> Response {
    axum::response::Html(ld(json!({"@type": "Event", "name": "Friday Improv",
        "startDate": format!("{}T19:30:00-07:00", day(3)),
        "description": "<p>Lineup: <b>Budget Cuts</b>, Sandwich</p>"})))
    .into_response()
}

async fn calendar() -> Response {
    let d = day(5).replace('-', "");
    let body = format!(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:show-9@improv\r\nSUMMARY:Budget Cuts Showcase\r\n\
         DTSTART:{d}T020000Z\r\nLOCATION:Main stage\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    ([(header::CONTENT_TYPE, "text/calendar")], body).into_response()
}

async fn plain() -> Response {
    axum::response::Html(format!(
        "<html><body><p>Saturday {} at 8pm: house teams Budget Cuts and Duck</p></body></html>",
        day(6)
    ))
    .into_response()
}

async fn messages(
    State(site): State<Arc<Site>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    site.llm_calls.lock().unwrap().push((headers, body));
    let events = json!({"events": [
        {"title": "House teams night", "start": format!("{}T20:00", day(6)), "end": "",
         "venue": "", "description": "Budget Cuts and Duck", "url": "", "id": ""}]});
    Json(json!({"type": "message", "stop_reason": "end_turn",
                "content": [{"type": "text", "text": events.to_string()}]}))
    .into_response()
}

async fn site() -> (String, Arc<Site>) {
    let state = Arc::new(Site::default());
    let app = Router::new()
        .route("/events", get(listing))
        .route("/e/1", get(event_page))
        .route("/e/2", get(event_page))
        .route("/cal.ics", get(calendar))
        .route("/plain", get(plain))
        .route("/v1/messages", post(messages))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

fn pod(base: &str) -> Pod {
    let adapter = WatcherAdapter::new(WatcherConfig {
        llm: Some(LlmConfig {
            api_key: "test-key".into(),
            model: "test-model".into(),
            base_url: base.into(),
        }),
        allow_private_hosts: true,
    });
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

/// The target screen's data once the target has been checked and is no
/// longer due.
async fn checked(pod: &Pod, id: &str) -> Value {
    for _ in 0..200 {
        app_watcher::check::run_due(pod).await;
        let (status, b) = call(
            pod,
            "GET",
            &format!("/v1/watcher/screens/target?id={id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{b}");
        let t = b["entities"][format!("watcher://targets/{id}")].clone();
        if t["health"] != "pending" && !is_due(pod, id) {
            return b;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("target {id} was never checked");
}

fn is_due(pod: &Pod, id: &str) -> bool {
    pod.store().with_conn(|c| {
        c.query_row(
            "SELECT next_fetch_at <= strftime('%s','now') FROM watcher_targets WHERE id = ?1",
            [id],
            |r| r.get::<_, bool>(0),
        )
        .unwrap()
    })
}

async fn workspace(pod: &Pod) -> Value {
    let (status, b) = call(pod, "GET", "/v1/workspaces/watcher", None).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b
}

fn titles(ws: &Value, query: &str) -> Vec<String> {
    ws["queries"][query]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            ws["entities"][u.as_str().unwrap()]["title"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

async fn notifications(pod: &Pod) -> Vec<Value> {
    // Deliveries run in the background after rule writes.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (status, b) = call(pod, "GET", "/v1/notifications?app=watcher", None).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["notifications"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn watches_sites_and_notifies_matches() {
    let (base, site) = site().await;
    let pod = pod(&base);

    // A rule from the app's form: keywords and exclusions as text.
    let (status, rule) = call(
        &pod,
        "POST",
        "/v1/watcher/rules/create",
        Some(json!({"keywords": "Budget Cuts", "exclude": "class, workshop", "client_id": "n1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    assert_eq!(rule["title"], "Budget Cuts");
    assert_eq!(
        rule["summary"],
        "\"Budget Cuts\" · not \"class\" or \"workshop\""
    );
    let (_, again) = call(
        &pod,
        "POST",
        "/v1/watcher/rules/create",
        Some(json!({"keywords": "Budget Cuts", "client_id": "n1"})),
    )
    .await;
    assert_eq!(
        again["id"], rule["id"],
        "an outbox retry answers the same rule"
    );

    // A website whose lineups are only on each event's own page.
    let (status, t) = call(
        &pod,
        "POST",
        "/v1/watcher/targets/create",
        Some(json!({"url": format!("{base}/events"), "details": "true", "client_id": "n2"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    let tid = t["id"].as_str().unwrap().to_owned();
    assert_eq!(t["title"], "127.0.0.1");
    let screen = checked(&pod, &tid).await;
    let target = &screen["entities"][format!("watcher://targets/{tid}")];
    assert_eq!(target["health"], "ok", "{target}");
    assert_eq!(target["upcoming_count"], 2, "the past show is dropped");
    assert_eq!(target["meta"], "127.0.0.1 · jsonld · every 6 h");

    let ws = workspace(&pod).await;
    assert_eq!(
        titles(&ws, "watcher:matches"),
        ["Friday Improv"],
        "the class is excluded"
    );
    let uri = ws["queries"]["watcher:matches"][0].as_str().unwrap();
    let ev = &ws["entities"][uri];
    assert_eq!(ev["venue"], "The End");
    assert_eq!(ev["description"], "Lineup: Budget Cuts, Sandwich");
    assert_eq!(ev["rules_text"], "Matched Budget Cuts");
    assert_eq!(ev["seen"], false);
    assert!(
        ev["meta"]
            .as_str()
            .unwrap()
            .ends_with(" · 7:30 PM · The End"),
        "{ev}"
    );
    assert_eq!(titles(&ws, "watcher:targets"), ["127.0.0.1"]);

    let n = notifications(&pod).await;
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0]["title"], "Friday Improv");
    assert_eq!(n[0]["target_uri"], uri);
    assert!(
        n[0]["body"]
            .as_str()
            .unwrap()
            .ends_with("The End — Budget Cuts"),
        "{}",
        n[0]
    );

    // Checking again sends the ETag: unchanged, nothing new.
    let hits = site.listing_hits.load(Ordering::SeqCst);
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/watcher/targets/refresh",
        Some(json!({"id": tid})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    checked(&pod, &tid).await;
    assert!(site.listing_hits.load(Ordering::SeqCst) > hits);
    assert!(site.not_modified.load(Ordering::SeqCst) >= 1);
    assert_eq!(notifications(&pod).await.len(), 1);

    // An iCalendar feed, and a page only the model can read.
    let (_, ics) = call(
        &pod,
        "POST",
        "/v1/watcher/targets/create",
        Some(json!({"url": format!("{base}/cal.ics"), "title": "Calendar", "extractor": "ics"})),
    )
    .await;
    let ics_id = ics["id"].as_str().unwrap().to_owned();
    checked(&pod, &ics_id).await;
    let (_, llm) = call(
        &pod,
        "POST",
        "/v1/watcher/targets/create",
        Some(
            json!({"url": format!("{base}/plain"), "title": "Plain", "extractor": "llm",
                    "hint": "Shows list the house teams", "utc_offset": "-07:00"}),
        ),
    )
    .await;
    let llm_id = llm["id"].as_str().unwrap().to_owned();
    checked(&pod, &llm_id).await;
    {
        let calls = site.llm_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (h, body) = &calls[0];
        assert_eq!(h["x-api-key"], "test-key");
        assert_eq!(h["anthropic-version"], "2026-10-21");
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        let prompt = body["messages"][0]["content"].as_str().unwrap();
        assert!(
            prompt.contains("house teams Budget Cuts")
                && prompt.contains("Shows list the house teams")
        );
    }
    let ws = workspace(&pod).await;
    assert_eq!(
        titles(&ws, "watcher:matches"),
        ["Friday Improv", "Budget Cuts Showcase", "House teams night"]
    );
    assert_eq!(notifications(&pod).await.len(), 3);

    // Seen, then the rule archived: no matches left.
    let id = uri.rsplit('/').next().unwrap();
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/watcher/events/seen",
        Some(json!({"id": id, "seen": "true"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, b) = call(
        &pod,
        "GET",
        &format!("/v1/watcher/screens/event?id={id}"),
        None,
    )
    .await;
    assert_eq!(b["entities"][uri]["seen"], true);
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/watcher/rules/update",
        Some(json!({"id": rule["id"], "archived": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(titles(&workspace(&pod).await, "watcher:matches").is_empty());

    // A broken page shows in the target's health.
    let (_, bad) = call(
        &pod,
        "POST",
        "/v1/watcher/targets/create",
        Some(json!({"url": format!("{base}/missing")})),
    )
    .await;
    let bad_id = bad["id"].as_str().unwrap().to_owned();
    let screen = checked(&pod, &bad_id).await;
    let t = &screen["entities"][format!("watcher://targets/{bad_id}")];
    assert_eq!(t["health"], "failing");
    assert_eq!(t["failing"], true);
    assert_eq!(t["health_text"], "Failing: the site answered 404 Not Found");

    // Refusals a form gets back (422 rolls back its optimistic change).
    for body in [
        json!({"url": "ftp://x.example/"}),
        json!({"url": format!("{base}/events"), "extractor": "magic"}),
        json!({"url": format!("{base}/events"), "interval_minutes": "5"}),
        json!({"title": "no url"}),
    ] {
        let (status, b) = call(&pod, "POST", "/v1/watcher/targets/create", Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    }
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/watcher/rules/create",
        Some(json!({"days": "someday"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_tools() {
    let (base, _site) = site().await;
    let pod = pod(&base);
    let (err, preview) = tool(
        &pod,
        "watcher_preview",
        json!({"url": format!("{base}/events")}),
    )
    .await;
    assert!(!err, "{preview}");
    assert_eq!(preview["extractor"], "jsonld");
    assert_eq!(
        (preview["found"].as_i64(), preview["upcoming"].as_i64()),
        (Some(3), Some(2))
    );
    assert_eq!(preview["events"][0]["title"], "Friday Improv");

    let (err, t) = tool(
        &pod,
        "watcher_set_target",
        json!({"url": format!("{base}/cal.ics"), "interval_minutes": 60}),
    )
    .await;
    assert!(!err, "{t}");
    let (err, r) = tool(
        &pod,
        "watcher_set_rule",
        json!({"title": "Showcases", "any": ["showcase"], "days": [], "within_days": 30}),
    )
    .await;
    assert!(!err, "{r}");
    checked(&pod, t["id"].as_str().unwrap()).await;
    let (_, status) = tool(&pod, "watcher_status", json!({})).await;
    assert_eq!(status["llm_extractor"], true);
    assert_eq!(status["targets"][0]["health"], "ok", "{status}");
    assert_eq!(status["rules"][0]["spec"]["within_days"], 30);
    assert_eq!(
        status["upcoming_matches"][0]["title"],
        "Budget Cuts Showcase"
    );

    let (err, e) = tool(&pod, "watcher_set_rule", json!({"id": "rnope"})).await;
    assert!(err && e["error"] == "rule_not_found", "{e}");
    let (_, r) = tool(&pod, "watcher_refresh", json!({})).await;
    assert_eq!(r["refreshing"], 1);

    // Temporary pods can't fetch.
    pod.store().set_pod_setting(FETCH_SETTING, "0").unwrap();
    let (err, e) = tool(
        &pod,
        "watcher_preview",
        json!({"url": format!("{base}/events")}),
    )
    .await;
    assert!(err && e["error"] == "fetch_off", "{e}");
}
