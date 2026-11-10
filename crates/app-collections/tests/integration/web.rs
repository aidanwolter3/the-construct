//! Jacks and the web module on fillers (decisions `jacks`,
//! `web-and-agent-modules`): a row whose page is blocked or empty keeps its
//! values, `query_field` rows are filled once from the pod's own search,
//! and the jack tools.

use std::sync::{Arc, Mutex};

use app_collections::agent::{AgentAnswer, AgentRequest, FillAgent, PageRequest, PickRequest};
use app_collections::fill::{self, Outcome};
use app_collections::{Caps, CollectionsAdapter};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use http_body_util::BodyExt;
use pod_core::session::session_hash;
use pod_core::{AppState, BoxFuture, Pod};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

/// The fake site: a product page that can turn blocked or empty, and a
/// SearXNG instance.
#[derive(Default)]
struct Site {
    /// `ok`, `blocked` or `empty`.
    page: Mutex<String>,
    searches: Mutex<Vec<String>>,
}

async fn page(State(site): State<Arc<Site>>) -> Response {
    match site.page.lock().unwrap().as_str() {
        "blocked" => (StatusCode::TOO_MANY_REQUESTS, "slow down").into_response(),
        "empty" => "<html><body><p>Nothing here.</p></body></html>".into_response(),
        _ => "<html><body><h1>Paper filters</h1><p>$5.95 for 350</p></body></html>".into_response(),
    }
}

async fn searx(
    State(site): State<Arc<Site>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    site.searches
        .lock()
        .unwrap()
        .push(q.get("q").cloned().unwrap_or_default());
    json!({"results": [
        {"title": "Paper Micro-Filters", "url": "https://shop.example/filters", "content": "350 count, official store"},
        {"title": "Filters at Target", "url": "https://target.example/p/filters", "content": "same day pickup"},
    ]})
    .to_string()
    .into_response()
}

async fn serve(site: Arc<Site>) -> String {
    let app = Router::new()
        .route("/page", get(page))
        .route("/search", get(searx))
        .with_state(site);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A fake model: a page holding "$5.95" reads as one row; picks answer the
/// first result.
#[derive(Debug, Default)]
struct Fake {
    pages: Mutex<usize>,
    picks: Mutex<Vec<String>>,
}

impl FillAgent for Fake {
    fn fill<'a>(&'a self, _req: AgentRequest<'a>) -> BoxFuture<'a, Result<AgentAnswer, String>> {
        Box::pin(async { Ok(AgentAnswer::default()) })
    }

    fn read_page<'a>(
        &'a self,
        req: PageRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            *self.pages.lock().unwrap() += 1;
            Ok(if req.page_text.contains("$5.95") {
                vec![json!({"price": 5.95}).as_object().unwrap().clone()]
            } else {
                Vec::new()
            })
        })
    }

    fn pick<'a>(
        &'a self,
        req: PickRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            self.picks.lock().unwrap().push(req.item.to_owned());
            let first = &req.results[0];
            Ok(vec![
                json!({"store": first.title, "link": first.url, "why": first.snippet})
                    .as_object()
                    .unwrap()
                    .clone(),
            ])
        })
    }
}

fn pod_with(fake: Arc<Fake>) -> Pod {
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_private_hosts(true)
        .with_agent(Some(fake as Arc<dyn FillAgent>));
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
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

async fn create(pod: &Pod, collection: &str, values: Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/collections/{collection}/create"))
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header("authorization", "Bearer phone")
        .header("content-type", "application/json")
        .body(Body::from(values.to_string()))
        .unwrap();
    let resp = pod_core::app(pod.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

async fn rows(pod: &Pod, collection: &str) -> Vec<Value> {
    let req = Request::builder()
        .uri(format!("/v1/collections/{collection}/rows"))
        .header(protocol::FEATURES_HEADER, protocol::features_header())
        .header("authorization", "Bearer phone")
        .body(Body::empty())
        .unwrap();
    let resp = pod_core::app(pod.clone()).oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let b: Value = serde_json::from_slice(&bytes).unwrap();
    b["queries"][format!("collection:{collection}")]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()].clone())
        .collect()
}

async fn filler_of(pod: &Pod, collection: &str) -> String {
    let d = ok(
        pod,
        "describe_collection",
        json!({"collection": collection}),
    )
    .await;
    d["definition"]["fillers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Wait for the fill a definition change started.
async fn settle(pod: &Pod, collection: &str) {
    for _ in 0..500 {
        if !fill::is_busy(pod, collection) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if !fill::is_busy(pod, collection) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the fills of {collection} never ended");
}

#[tokio::test]
async fn a_blocked_or_empty_page_never_erases_a_rows_values() {
    let site = Arc::new(Site::default());
    *site.page.lock().unwrap() = "ok".into();
    let base = serve(site.clone()).await;
    let fake = Arc::new(Fake::default());
    let pod = pod_with(fake.clone());
    ok(
        &pod,
        "define_collection",
        json!({"name": "wishlist", "fields": [
        {"name": "url", "type": "text"},
        {"name": "price", "type": "number", "optional": true}],
        "fillers": [{"kind": "fetch", "url_field": "url", "extract": "llm", "fields": ["price"]}]}),
    )
    .await;
    settle(&pod, "wishlist").await;
    create(&pod, "wishlist", json!({"url": format!("{base}/page")})).await;
    let f = filler_of(&pod, "wishlist").await;
    let out = fill::fill(&pod, "wishlist", &f, false).await;
    assert!(matches!(out, Outcome::Filled { items: 1, .. }), "{out:?}");
    assert_eq!(rows(&pod, "wishlist").await[0]["price"], 5.95);
    // The same page again: its text didn't change, so no model call.
    fill::fill(&pod, "wishlist", &f, false).await;
    assert_eq!(*fake.pages.lock().unwrap(), 1);

    // An empty page keeps the value (on_empty keep).
    *site.page.lock().unwrap() = "empty".into();
    fill::fill(&pod, "wishlist", &f, false).await;
    assert_eq!(*fake.pages.lock().unwrap(), 2);
    assert_eq!(rows(&pod, "wishlist").await[0]["price"], 5.95);

    // A blocked page keeps it too, and the pod stays away from the host.
    *site.page.lock().unwrap() = "blocked".into();
    let out = fill::fill(&pod, "wishlist", &f, false).await;
    assert!(matches!(out, Outcome::Failed(_)), "{out:?}");
    assert_eq!(rows(&pod, "wishlist").await[0]["price"], 5.95);
    *site.page.lock().unwrap() = "ok".into();
    let out = fill::fill(&pod, "wishlist", &f, false).await;
    match out {
        Outcome::Failed(e) => assert!(e.contains("waits"), "{e}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn query_rows_are_filled_once_from_the_pods_own_search() {
    let site = Arc::new(Site::default());
    let base = serve(site.clone()).await;
    let fake = Arc::new(Fake::default());
    let pod = pod_with(fake.clone());
    ok(
        &pod,
        "define_collection",
        json!({"name": "shopping", "fields": [
        {"name": "name", "type": "text"},
        {"name": "notes", "type": "text", "optional": true},
        {"name": "store", "type": "text", "optional": true},
        {"name": "link", "type": "text", "optional": true},
        {"name": "why", "type": "text", "optional": true}],
        "fillers": [{"kind": "fetch", "query_field": "name", "extract": "llm",
                     "fields": ["store", "link", "why"], "hint": "where to buy it"}]}),
    )
    .await;
    settle(&pod, "shopping").await;
    let d = ok(
        &pod,
        "describe_collection",
        json!({"collection": "shopping"}),
    )
    .await;
    let filler = &d["definition"]["fillers"][0];
    assert_eq!(filler["query_field"], "name");
    assert_eq!(filler["refresh"], "once");
    assert_eq!(filler["on_empty"], "keep");
    create(
        &pod,
        "shopping",
        json!({"name": "AeroPress paper filters", "notes": "the 350 pack"}),
    )
    .await;
    let f = filler_of(&pod, "shopping").await;

    // Without the web_search jack it says which jack it needs.
    match fill::fill(&pod, "shopping", &f, false).await {
        Outcome::Failed(e) => assert!(e.contains("web_search"), "{e}"),
        other => panic!("{other:?}"),
    }

    // The jack's config is validated (an https base_url); the test site is
    // local http, so it is stored directly here.
    let (err, e) = tool(&pod, "plug_jack", json!({"id": "web_search", "config": {"provider": "searxng", "base_url": "http://s.example"}})).await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("https"),
        "{e}"
    );
    pod.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO jacks (id, plugged, config, secrets, updated_at) VALUES ('web_search', 1, ?1, '{}', 0)",
                [json!({"provider": "searxng", "base_url": base}).to_string()],
            )
        })
        .unwrap();

    let out = fill::fill(&pod, "shopping", &f, false).await;
    assert!(matches!(out, Outcome::Filled { items: 1, .. }), "{out:?}");
    let r = rows(&pod, "shopping").await;
    assert_eq!(r[0]["store"], "Paper Micro-Filters");
    assert_eq!(r[0]["link"], "https://shop.example/filters");
    assert_eq!(
        fake.picks.lock().unwrap()[0],
        "AeroPress paper filters (notes: the 350 pack)"
    );
    // Once: the next fill neither searches nor asks the model again.
    fill::fill(&pod, "shopping", &f, false).await;
    assert_eq!(site.searches.lock().unwrap().len(), 1);
    assert_eq!(fake.picks.lock().unwrap().len(), 1);
    // People still add rows by hand; a new row is searched.
    create(&pod, "shopping", json!({"name": "Lavender soap bar"})).await;
    fill::fill(&pod, "shopping", &f, false).await;
    assert_eq!(site.searches.lock().unwrap().len(), 2);
    assert_eq!(rows(&pod, "shopping").await.len(), 2);
}

#[tokio::test]
async fn jacks_are_listed_plugged_and_locked() {
    let pod = pod_with(Arc::new(Fake::default()));
    let l = ok(&pod, "list_jacks", json!({})).await;
    let by = |l: &Value, id: &str| {
        l["jacks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(by(&l, "model")["state"], "plugged");
    assert_eq!(by(&l, "web")["kind"], "switch");
    assert_eq!(by(&l, "web_search")["state"], "unplugged");
    let p = ok(
        &pod,
        "plug_jack",
        json!({"id": "web_search", "config": {"provider": "brave", "api_key": "BSA-secret"}}),
    )
    .await;
    assert_eq!(p["jack"]["state"], "plugged");
    let l = ok(&pod, "list_jacks", json!({})).await;
    assert!(!l.to_string().contains("BSA-secret"), "{l}");
    let key = by(&l, "web_search")["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "api_key")
        .cloned()
        .unwrap();
    assert_eq!(key["set"], true);
    ok(&pod, "unplug_jack", json!({"id": "web"})).await;
    let (err, st) = tool(
        &pod,
        "collections_preview",
        json!({"url": "https://example.com/x.json", "extract": "json"}),
    )
    .await;
    assert!(err && st["error"] == "fill_off", "{st}");
    pod.store()
        .set_pod_setting("jack.model", pod_core::jacks::LOCKED)
        .unwrap();
    let (err, e) = tool(&pod, "plug_jack", json!({"id": "model"})).await;
    assert!(err && e["error"] == "jack_locked", "{e}");
    let (err, e) = tool(&pod, "plug_jack", json!({"id": "nope"})).await;
    assert!(err && e["error"] == "unknown_jack", "{e}");
}

/// Answers a scripted conversation: a `web_search` call, then `save_rows`.
struct Script {
    bodies: Mutex<Vec<Value>>,
}

impl construct_agent::Transport for Script {
    fn post<'a>(&'a self, body: &'a Value) -> BoxFuture<'a, Result<(u16, Value), String>> {
        let n = {
            let mut b = self.bodies.lock().unwrap();
            b.push(body.clone());
            b.len()
        };
        Box::pin(async move {
            Ok((
                200,
                if n == 1 {
                    json!({"stop_reason": "tool_use", "content": [
                        {"type": "tool_use", "id": "t1", "name": "web_search", "input": {"query": "paper filters"}}]})
                } else {
                    json!({"stop_reason": "tool_use", "content": [
                        {"type": "tool_use", "id": "t2", "name": "save_rows", "input": {"rows": [{"store": "Shop", "link": "https://shop.example/filters"}]}}]})
                },
            ))
        })
    }
}

#[tokio::test]
async fn agent_fills_search_with_the_pods_own_tools_when_web_search_is_plugged() {
    let site = Arc::new(Site::default());
    let base = serve(site.clone()).await;
    let script = Arc::new(Script {
        bodies: Mutex::new(Vec::new()),
    });
    let agent = Arc::new(construct_agent::Agent::new(
        script.clone(),
        None,
        "capable".into(),
    ));
    let adapter = CollectionsAdapter::with_caps(Caps::default())
        .with_private_hosts(true)
        .with_agent(Some(
            Arc::new(app_collections::agent::PodAgent::new(agent)) as Arc<dyn FillAgent>
        ));
    let pod: Pod = Arc::new(AppState::in_memory(vec![Arc::new(adapter)]));
    pod.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO jacks (id, plugged, config, secrets, updated_at) VALUES ('web_search', 1, ?1, '{}', 0)",
                [json!({"provider": "searxng", "base_url": base}).to_string()],
            )
        })
        .unwrap();
    let fields = vec![
        app_collections::agent::AgentField {
            name: "store".into(),
            kind: app_collections::model::Kind::Text,
            options: Vec::new(),
        },
        app_collections::agent::AgentField {
            name: "link".into(),
            kind: app_collections::model::Kind::Text,
            options: Vec::new(),
        },
    ];
    let pod_agent = pod
        .adapter::<CollectionsAdapter>()
        .unwrap()
        .agent()
        .unwrap()
        .clone();
    let out = pod_agent
        .fill(AgentRequest {
            pod: &pod,
            title: "Where to buy",
            prompt: "where to buy paper filters",
            fields: &fields,
            today: "2026-11-09",
            max_searches: 2,
            max_fetches: 0,
            key: None,
            current: &[],
        })
        .await
        .unwrap();
    assert_eq!(out.rows[0]["store"], "Shop");
    assert_eq!(site.searches.lock().unwrap().as_slice(), ["paper filters"]);
    let bodies = script.bodies.lock().unwrap();
    // The pod's tool, not the API's server-side one.
    let tools = bodies[0]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|t| t["name"] == "web_search" && t.get("type").is_none())
    );
    let result = &bodies[1]["messages"][2]["content"][0];
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("https://shop.example/filters"),
        "{result}"
    );
}
