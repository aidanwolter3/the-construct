//! The Events app (decision `collection-fillers`) on a real pod against a
//! fake website: a pod's Watcher sites and rules move into the `events`
//! collection once (a site per filler, a rule per alert), the first fill
//! records the matches the Watcher already announced without notifying
//! again, a new matching show notifies once on the app's `matches` channel,
//! the app serves matches, sites with health and alerts, a site's screen and
//! an event's screen, the events fields are locked, and the Watcher's old
//! routes answer that they are gone.

use std::sync::{Arc, Mutex};

use app_collections::CollectionsAdapter;
use app_collections::fill;
use app_watcher::WatcherAdapter;
use app_watcher::events::{COLLECTION, prepare};
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
use pod_core::{AppState, Pod};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The calendar the fake club serves.
#[derive(Default)]
struct Site {
    ics: Mutex<String>,
}

fn event(uid: &str, title: &str, start: &str, description: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{title}\r\nDTSTART;TZID=America/Denver:{start}\r\n\
LOCATION:The End\r\nDESCRIPTION:{description}\r\nEND:VEVENT\r\n"
    )
}

fn calendar(events: &[String]) -> String {
    format!("BEGIN:VCALENDAR\r\n{}END:VCALENDAR\r\n", events.concat())
}

async fn ics(State(site): State<Arc<Site>>) -> Response {
    site.ics.lock().unwrap().clone().into_response()
}

async fn serve(site: Arc<Site>) -> String {
    let app = Router::new().route("/cal.ics", get(ics)).with_state(site);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn pod() -> Pod {
    let pod: Pod = Arc::new(AppState::in_memory(vec![
        Arc::new(WatcherAdapter::new().with_private_hosts(true)),
        Arc::new(CollectionsAdapter::new().with_private_hosts(true)),
    ]));
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

fn listed(b: &Value, query: &str) -> Vec<Value> {
    b["queries"][query]
        .as_array()
        .unwrap_or_else(|| panic!("no query {query}: {b}"))
        .iter()
        .map(|u| b["entities"][u.as_str().unwrap()].clone())
        .collect()
}

async fn notifications(pod: &Pod) -> Vec<Value> {
    let (status, b) = call(pod, "GET", "/v1/notifications?app=watcher", None).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["notifications"].as_array().unwrap().clone()
}

/// Store a Watcher site and rule as the Watcher did.
fn seed_watcher(pod: &Pod, url: &str) {
    pod.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO watcher_targets (id, title, url, extractor, config_json, interval_secs,
                    archived, created_at, updated_at, time_zone)
                 VALUES ('t1', 'Improv Club', ?1, 'auto', '{}', 21600, 0, 1, 1, 'America/Denver')",
                [url],
            )?;
            c.execute(
                "INSERT INTO watcher_targets (id, title, url, extractor, config_json, interval_secs,
                    archived, created_at, updated_at)
                 VALUES ('t2', 'Gone Club', 'https://gone.example/', 'auto', '{}', 21600, 1, 2, 2)",
                [],
            )?;
            c.execute(
                "INSERT INTO watcher_rules (id, title, spec_json, archived, created_at, updated_at)
                 VALUES ('r1', 'Budget Cuts', ?1, 0, 1, 1)",
                [
                    json!({"any": ["Budget Cuts"], "none": ["class"], "targets": ["t1"]})
                        .to_string(),
                ],
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
}

#[tokio::test]
async fn the_watcher_moves_to_events_and_keeps_notifying() {
    let site = Arc::new(Site::default());
    *site.ics.lock().unwrap() = calendar(&[
        event(
            "bc1",
            "Friday Improv",
            "20991106T193000",
            "Lineup: Budget Cuts",
        ),
        event(
            "cl1",
            "Improv class",
            "20991107T100000",
            "Budget Cuts teach",
        ),
    ]);
    let base = serve(site.clone()).await;
    let pod = pod();
    // A fresh pod: an empty events collection the app shows.
    prepare(pod.store(), true).unwrap();
    let (status, ws) = call(&pod, "GET", "/v1/workspaces/watcher", None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["workspace"]["title"], "Events", "{ws}");
    assert!(listed(&ws, "collection:events").is_empty());

    // A pod that used the Watcher.
    let pod = pod_with_watcher(&base).await;
    let d = describe(&pod).await;
    let fillers = d["definition"]["fillers"].as_array().unwrap();
    assert_eq!(fillers.len(), 1, "the archived site stays behind");
    assert_eq!(fillers[0]["title"], "Improv Club");
    assert_eq!(fillers[0]["extract"], "events");
    let filler = fillers[0]["id"].as_str().unwrap().to_owned();
    let alerts = d["definition"]["alerts"].as_array().unwrap();
    assert_eq!(
        (alerts[0]["name"].clone(), alerts[0]["filler"].clone()),
        (json!("Budget Cuts"), json!(filler))
    );
    // Moving twice adds nothing.
    assert!(prepare(pod.store(), true).unwrap().is_empty());

    // The first fill: matches recorded, not announced again.
    let out = fill::fill(&pod, COLLECTION, &filler, false).await;
    assert!(
        matches!(out, fill::Outcome::Filled { items: 2, .. }),
        "{out:?}"
    );
    app_collections::alerts::run(&pod, None).await;
    assert!(notifications(&pod).await.is_empty());
    let (_, ws) = call(&pod, "GET", "/v1/workspaces/watcher", None).await;
    let matches = listed(&ws, "collection:events:alerted");
    assert_eq!(matches.len(), 1);
    assert_eq!(
        (
            matches[0]["title"].clone(),
            matches[0]["start_text"].clone(),
            matches[0]["alert_names"].clone()
        ),
        (
            json!("Friday Improv"),
            json!("Fri Nov 6 · 7:30 PM"),
            json!("Budget Cuts")
        )
    );
    let sites = listed(&ws, "collection:events:fillers");
    assert_eq!(
        (sites[0]["title"].clone(), sites[0]["alerts_text"].clone()),
        (json!("Improv Club"), json!("Watching for Budget Cuts"))
    );

    // A new Budget Cuts show notifies once, on the app's channel.
    *site.ics.lock().unwrap() = calendar(&[
        event(
            "bc1",
            "Friday Improv",
            "20991106T193000",
            "Lineup: Budget Cuts",
        ),
        event(
            "bc2",
            "Saturday Improv",
            "20991114T193000",
            "Budget Cuts again",
        ),
    ]);
    fill::fill(&pod, COLLECTION, &filler, false).await;
    let sent = notifications(&pod).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["title"], "Saturday Improv");
    assert_eq!(
        sent[0]["body"],
        "Sat Nov 14 · 7:30 PM · The End — Budget Cuts"
    );
    assert_eq!(sent[0]["channel"], "matches");
    app_collections::alerts::run(&pod, None).await;
    assert_eq!(notifications(&pod).await.len(), 1);

    // The site's screen and an event's screen; marking it seen.
    let (status, screen) = call(
        &pod,
        "GET",
        &format!("/v1/collections/events/fillers/{filler}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed(&screen, &format!("collection:events:filler:{filler}:rows")).len(),
        2
    );
    let id = sent[0]["target_uri"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let (status, one) = call(
        &pod,
        "GET",
        &format!("/v1/collections/events/row?id={id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed(&one, &format!("collection:events:row:{id}"))[0]["seen"],
        false
    );
    let (status, _) = call(
        &pod,
        "POST",
        "/v1/collections/events/update",
        Some(json!({"id": id, "seen": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The fields the app reads are locked; adding one is fine.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "events", "changes": [{"op": "rename_field", "field": "title", "to": "name"}]}),
    )
    .await;
    assert!(
        err && e["detail"].as_str().unwrap().contains("watcher"),
        "{e}"
    );
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "events", "changes": [{"op": "add_field", "name": "notes", "type": "text", "optional": true}]}),
    )
    .await;
    assert!(!err, "{e}");

    // The Watcher's old routes are gone.
    let (status, b) = call(&pod, "GET", "/v1/watcher/screens/event?id=x", None).await;
    assert_eq!(
        (status, b["error"].clone()),
        (StatusCode::GONE, json!("gone"))
    );
    let (status, _) = call(&pod, "POST", "/v1/watcher/targets/create", Some(json!({}))).await;
    assert_eq!(status, StatusCode::GONE);
}

/// A pod whose Watcher had a site at `base` and a rule, moved to Events.
async fn pod_with_watcher(base: &str) -> Pod {
    let pod = pod();
    seed_watcher(&pod, &format!("{base}/cal.ics"));
    let moved = prepare(pod.store(), true).unwrap();
    assert_eq!(moved.len(), 1);
    pod
}

async fn describe(pod: &Pod) -> Value {
    let (err, d) = tool(pod, "describe_collection", json!({"collection": "events"})).await;
    assert!(!err, "{d}");
    d
}

/// The Events app's simulator fixtures, from a real pod: two sites (one
/// failing), alerts, matches (one seen). `UPDATE_FIXTURES=1` writes
/// `kmp-app/desktopApp/src/sim/resources/events_bundle.json` (the app's workspace) and
/// `kmp-app/desktopApp/src/sim/resources/events_screen_data.json` (its screens' data paths), with the
/// local site's address replaced by a stable one.
#[tokio::test]
async fn simulator_fixture() {
    let site = Arc::new(Site::default());
    *site.ics.lock().unwrap() = calendar(&[
        event(
            "bc1",
            "Friday Night Improv",
            "20991106T193000",
            "Lineup: Budget Cuts\\, Sandwich\\, Hot Mess",
        ),
        event(
            "bc2",
            "Budget Cuts: Live!",
            "20991113T200000",
            "An hour of Budget Cuts",
        ),
        event(
            "cl1",
            "Intro to Improv (class)",
            "20991107T100000",
            "Taught by Budget Cuts",
        ),
        event("jm1", "Saturday Jam", "20991107T190000", "Open jam"),
    ]);
    let base = serve(site.clone()).await;
    let pod = pod_with_watcher(&base).await;
    let d = describe(&pod).await;
    let club = d["definition"]["fillers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A second site that fails.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "events", "changes": [
            {"op": "set_filler", "filler": {"kind": "fetch", "title": "Comedy Works",
                "url": format!("{base}/missing.ics"), "extract": "events", "zone": "America/Denver",
                "map": {"source_id": "id", "title": "title", "start": "start", "venue": "venue"},
                "key": "source_id", "upcoming": "start", "every_minutes": 360}}]}),
    )
    .await;
    assert!(!err, "{e}");
    let d = describe(&pod).await;
    let comedy = d["definition"]["fillers"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fill::fill(&pod, COLLECTION, &club, false).await;
    // set_filler started the failing site's fill in the background.
    for _ in 0..500 {
        let st = pod
            .store()
            .with_conn(|c| fill::state(c, COLLECTION, &comedy))
            .unwrap();
        if st.last_fill_at.is_some() && !fill::is_busy(&pod, COLLECTION) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let (_, ws) = call(&pod, "GET", "/v1/workspaces/watcher", None).await;
    let matches = listed(&ws, "collection:events:alerted");
    assert_eq!(matches.len(), 2, "{matches:?}");
    // One seen.
    call(
        &pod,
        "POST",
        "/v1/collections/events/update",
        Some(json!({"id": matches[1]["id"], "seen": true})),
    )
    .await;
    let (_, ws) = call(&pod, "GET", "/v1/workspaces/watcher", None).await;
    assert_eq!(listed(&ws, "collection:events:fillers")[1]["failing"], true);
    let mut screens = serde_json::Map::new();
    for f in [&club, &comedy] {
        let path = format!("/v1/collections/events/fillers/{f}");
        let (_, b) = call(&pod, "GET", &path, None).await;
        screens.insert(path, b);
    }
    for e in listed(&ws, "collection:events") {
        let path = format!(
            "/v1/collections/events/row?id={}",
            e["id"].as_str().unwrap()
        );
        let (_, b) = call(&pod, "GET", &path, None).await;
        screens.insert(path, b);
    }
    let stable = |v: &Value| {
        serde_json::to_string_pretty(v)
            .unwrap()
            .replace(&base, "https://improvclub.example")
            + "\n"
    };
    let bundle = json!({"version": "1.0.0", "workspace": ws["workspace"],
        "entities": ws["entities"], "queries": ws["queries"]});
    if std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1") {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../kmp-app/desktopApp/src/sim/resources");
        std::fs::write(root.join("events_bundle.json"), stable(&bundle)).unwrap();
        std::fs::write(
            root.join("events_screen_data.json"),
            stable(&Value::Object(screens)),
        )
        .unwrap();
    }
}
