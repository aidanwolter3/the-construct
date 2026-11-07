//! The Events app (decision `collection-fillers`) on a real pod against a
//! fake website: a watched site with an alert notifies for its matches on
//! the app's `matches` channel, a new matching show notifies once, the app
//! serves matches, sites with health and alerts, a site's screen and an
//! event's screen, the events fields are locked, and a pod saved before the
//! app was renamed moves to the new id.

use std::sync::{Arc, Mutex};

use app_collections::CollectionsAdapter;
use app_collections::fill;
use app_events::EventsAdapter;
use app_events::events::{COLLECTION, prepare};
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
LOCATION:The Back Room\r\nDESCRIPTION:{description}\r\nEND:VEVENT\r\n"
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
        Arc::new(EventsAdapter::new()),
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
    let (status, b) = call(pod, "GET", "/v1/notifications?app=events", None).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    b["notifications"].as_array().unwrap().clone()
}

/// Add the club's site and a Brass Lanterns alert for it, and wait for the
/// site's first fill. Answers the site's filler id.
async fn watch_club(pod: &Pod, base: &str) -> String {
    let (err, e) = tool(
        pod,
        "apply_change",
        json!({"collection": "events", "changes": [
            {"op": "set_filler", "filler": {"kind": "fetch", "title": "Improv Club",
                "url": format!("{base}/cal.ics"), "extract": "events", "zone": "America/Denver",
                "map": {"source_id": "id", "title": "title", "start": "start", "end": "end",
                    "venue": "venue", "description": "description", "url": "url"},
                "key": "source_id", "upcoming": "start", "every_minutes": 360}}]}),
    )
    .await;
    assert!(!err, "{e}");
    let d = describe(pod).await;
    let filler = d["definition"]["fillers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    settle(pod, &filler).await;
    let (err, e) = tool(
        pod,
        "apply_change",
        json!({"collection": "events", "changes": [
            {"op": "add_alert", "name": "Brass Lanterns", "any": ["Brass Lanterns"],
             "none": ["class"], "filler": filler}]}),
    )
    .await;
    assert!(!err, "{e}");
    filler
}

/// Wait until `filler` has filled once and no fill is running.
async fn settle(pod: &Pod, filler: &str) {
    for _ in 0..500 {
        let st = pod
            .store()
            .with_conn(|c| fill::state(c, COLLECTION, filler))
            .unwrap();
        if st.last_fill_at.is_some() && !fill::is_busy(pod, COLLECTION) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the site never filled");
}

#[tokio::test]
async fn events_shows_matches_and_notifies() {
    let site = Arc::new(Site::default());
    *site.ics.lock().unwrap() = calendar(&[
        event(
            "bc1",
            "Friday Improv",
            "20991106T193000",
            "Lineup: Brass Lanterns",
        ),
        event(
            "cl1",
            "Improv class",
            "20991107T100000",
            "Brass Lanterns teach",
        ),
    ]);
    let base = serve(site.clone()).await;
    let pod = pod();
    // A fresh pod: an empty events collection the app shows.
    prepare(pod.store()).unwrap();
    let (status, ws) = call(&pod, "GET", "/v1/workspaces/events", None).await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["workspace"]["title"], "Events", "{ws}");
    assert!(listed(&ws, "collection:events").is_empty());

    // A watched site and an alert: the match it already lists notifies.
    let filler = watch_club(&pod, &base).await;
    app_collections::alerts::run(&pod, None).await;
    let sent = notifications(&pod).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        (sent[0]["title"].clone(), sent[0]["channel"].clone()),
        (json!("Friday Improv"), json!("matches"))
    );
    let (_, ws) = call(&pod, "GET", "/v1/workspaces/events", None).await;
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
            json!("Brass Lanterns")
        )
    );
    let sites = listed(&ws, "collection:events:fillers");
    assert_eq!(
        (sites[0]["title"].clone(), sites[0]["alerts_text"].clone()),
        (json!("Improv Club"), json!("Watching for Brass Lanterns"))
    );

    // A new Brass Lanterns show notifies once, on the app's channel.
    *site.ics.lock().unwrap() = calendar(&[
        event(
            "bc1",
            "Friday Improv",
            "20991106T193000",
            "Lineup: Brass Lanterns",
        ),
        event(
            "bc2",
            "Saturday Improv",
            "20991114T193000",
            "Brass Lanterns again",
        ),
    ]);
    fill::fill(&pod, COLLECTION, &filler, false).await;
    let sent = notifications(&pod).await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    let saturday = sent
        .iter()
        .find(|n| n["title"] == "Saturday Improv")
        .expect("the new show notified");
    assert_eq!(
        saturday["body"],
        "Sat Nov 14 · 7:30 PM · The Back Room — Brass Lanterns"
    );
    assert_eq!(saturday["channel"], "matches");
    app_collections::alerts::run(&pod, None).await;
    assert_eq!(notifications(&pod).await.len(), 2);

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
    let id = saturday["target_uri"]
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
        err && e["detail"]
            .as_str()
            .unwrap()
            .contains("belongs to the events app"),
        "{e}"
    );
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "events", "changes": [{"op": "add_field", "name": "notes", "type": "text", "optional": true}]}),
    )
    .await;
    assert!(!err, "{e}");
}

/// A pod saved before the app was renamed (decision
/// `rename-apps-and-folders`): its history, notifications and `events`
/// collection move to the new id, and the old Watcher's tables go.
#[tokio::test]
async fn a_pod_from_before_the_rename_moves_to_the_new_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.db");
    let adapters = || -> Vec<Arc<dyn pod_core::AppAdapter>> {
        vec![
            Arc::new(EventsAdapter::new()),
            Arc::new(CollectionsAdapter::new().with_private_hosts(true)),
        ]
    };
    {
        let pod: Pod = Arc::new(AppState::new(
            pod_core::store::Store::open(&path).unwrap(),
            adapters(),
        ));
        prepare(pod.store()).unwrap();
        // Back to the old names, as the pod stored them.
        pod.store()
            .with_conn(|c| {
                // The pod predates 0005 (decision `plumbing-and-porcelain`).
                c.execute_batch(
                    "DELETE FROM app_versions WHERE app_id = 'events' AND file_key IN ('events/0005', 'events/0006');
                     UPDATE app_versions SET app_id = 'watcher',
                         file_key = replace(file_key, 'events/0004', 'watcher/0003')
                         WHERE app_id = 'events';
                     DELETE FROM adapter_migrations WHERE adapter = 'events';
                     DELETE FROM schema_migrations WHERE scope = 'events';
                     INSERT INTO adapter_migrations (adapter, version) VALUES ('watcher', 3);
                     CREATE TABLE watcher_targets (id TEXT PRIMARY KEY);
                     INSERT INTO pod_settings (key, value) VALUES ('watcher.imported', '1');
                     INSERT INTO pod_settings (key, value) VALUES ('apps.hidden', '[\"watcher\"]');
                     INSERT INTO notifications (app_id, channel, dedupe_key, title, created_at, inserted_at)
                         VALUES ('watcher', 'matches', 'k1', 'Friday Improv', 1, 1);
                     UPDATE collections_defs SET def_json = replace(def_json, '\"app\":\"events\"', '\"app\":\"watcher\"');",
                )
            })
            .unwrap();
    }
    let pod: Pod = Arc::new(AppState::new(
        pod_core::store::Store::open(&path).unwrap(),
        adapters(),
    ));
    prepare(pod.store()).unwrap();
    pod.store()
        .create_pinned_session(&session_hash("pod"))
        .unwrap();
    pod.store().create_session(&session_hash("phone")).unwrap();
    let (versions, keys, old_tables, old_rows): (Vec<i64>, Vec<String>, i64, i64) = pod
        .store()
        .with_conn(|c| {
            let versions = c
                .prepare(
                    "SELECT version FROM app_versions WHERE app_id = 'events' ORDER BY version",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let keys = c
                .prepare(
                    "SELECT file_key FROM app_versions WHERE app_id = 'events' ORDER BY version",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let old_tables = c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'watcher%'",
                [],
                |r| r.get(0),
            )?;
            let old_rows = c.query_row(
                "SELECT (SELECT count(*) FROM app_versions WHERE app_id = 'watcher')
                      + (SELECT count(*) FROM adapter_migrations WHERE adapter = 'watcher')
                      + (SELECT count(*) FROM schema_migrations WHERE scope = 'watcher')
                      + (SELECT count(*) FROM pod_settings WHERE key = 'watcher.imported')",
                [],
                |r| r.get(0),
            )?;
            Ok::<_, rusqlite::Error>((versions, keys, old_tables, old_rows))
        })
        .unwrap();
    assert_eq!(
        versions,
        [1, 2, 3, 4],
        "the history stays, the new files appended"
    );
    assert_eq!(
        keys,
        ["events/0003", "events/0004", "events/0005", "events/0006"]
    );
    assert_eq!((old_tables, old_rows), (0, 0));
    assert_eq!(
        pod_core::custom::hidden_apps(pod.store()).unwrap(),
        ["events"]
    );
    let sent = notifications(&pod).await;
    assert_eq!(sent[0]["title"], "Friday Improv");
    assert_eq!(describe(&pod).await["definition"]["system_app"], "events");
}

async fn describe(pod: &Pod) -> Value {
    let (err, d) = tool(pod, "describe_collection", json!({"collection": "events"})).await;
    assert!(!err, "{d}");
    d
}

/// The Events app's simulator fixtures, from a real pod: two sites (one
/// failing), alerts, matches (one seen). `UPDATE_FIXTURES=1` writes
/// `crates/construct-sim/data/events_bundle.json` (the app's workspace) and
/// `crates/construct-sim/data/events_screen_data.json` (its screens' data paths), with the
/// local site's address replaced by a stable one.
#[tokio::test]
async fn simulator_fixture() {
    let site = Arc::new(Site::default());
    *site.ics.lock().unwrap() = calendar(&[
        event(
            "bc1",
            "Friday Night Improv",
            "20991106T193000",
            "Lineup: Brass Lanterns\\, Teacup Riot\\, Open Tab",
        ),
        event(
            "bc2",
            "Brass Lanterns: Live!",
            "20991113T200000",
            "An hour of Brass Lanterns",
        ),
        event(
            "cl1",
            "Intro to Improv (class)",
            "20991107T100000",
            "Taught by Brass Lanterns",
        ),
        event("jm1", "Saturday Jam", "20991107T190000", "Open jam"),
    ]);
    let base = serve(site.clone()).await;
    let pod = pod();
    prepare(pod.store()).unwrap();
    let club = watch_club(&pod, &base).await;
    // A second site that fails.
    let (err, e) = tool(
        &pod,
        "apply_change",
        json!({"collection": "events", "changes": [
            {"op": "set_filler", "filler": {"kind": "fetch", "title": "Laugh Loft",
                "url": format!("{base}/missing.ics"), "extract": "events", "zone": "America/Denver",
                "map": {"source_id": "id", "title": "title", "start": "start", "venue": "venue"},
                "key": "source_id", "upcoming": "start", "every_minutes": 360}}]}),
    )
    .await;
    assert!(!err, "{e}");
    let d = describe(&pod).await;
    let loft = d["definition"]["fillers"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fill::fill(&pod, COLLECTION, &club, false).await;
    // set_filler started the failing site's fill in the background.
    for _ in 0..500 {
        let st = pod
            .store()
            .with_conn(|c| fill::state(c, COLLECTION, &loft))
            .unwrap();
        if st.last_fill_at.is_some() && !fill::is_busy(&pod, COLLECTION) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let (_, ws) = call(&pod, "GET", "/v1/workspaces/events", None).await;
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
    let (_, ws) = call(&pod, "GET", "/v1/workspaces/events", None).await;
    assert_eq!(listed(&ws, "collection:events:fillers")[1]["failing"], true);
    let mut screens = serde_json::Map::new();
    for f in [&club, &loft] {
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
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../construct-sim/data");
        std::fs::write(root.join("events_bundle.json"), stable(&bundle)).unwrap();
        std::fs::write(
            root.join("events_screen_data.json"),
            stable(&Value::Object(screens)),
        )
        .unwrap();
    }
}
