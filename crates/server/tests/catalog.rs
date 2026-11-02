//! Layout catalog (D26): list / get / versions / activate / restore, the
//! workspace revision, SSE on switches and restores, auth, seeding, and the
//! agent committing to the active (possibly non-default) layout.

use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};
use server::AppState;
use server::agent::{self, LayoutProvider, Selection};
use server::layouts;
use server::pixelfed::routes::session_hash;
use tokio::task::JoinHandle;

struct Pod {
    url: String,
    state: Arc<AppState>,
    handle: JoinHandle<()>,
}

impl Drop for Pod {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn spawn(state: AppState) -> Pod {
    let state = Arc::new(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = server::app(state.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Pod { url, state, handle }
}

async fn open_pod() -> Pod {
    spawn(AppState::in_memory().with_agent_open(true)).await
}

/// A pod that needs sessions, with the valid session `tok`.
async fn closed_pod() -> Pod {
    let pod = spawn(AppState::in_memory()).await;
    pod.state
        .store()
        .create_session(&session_hash("tok"))
        .unwrap();
    pod
}

struct Resp {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }
}

async fn send(req: reqwest::RequestBuilder, bearer: Option<&str>) -> Resp {
    let req = match bearer {
        Some(t) => req.bearer_auth(t),
        None => req,
    };
    let resp = req.send().await.unwrap();
    Resp {
        status: resp.status(),
        headers: resp.headers().clone(),
        body: resp.json().await.unwrap_or(Value::Null),
    }
}

async fn get(pod: &Pod, path: &str, bearer: Option<&str>) -> Resp {
    send(
        reqwest::Client::new().get(format!("{}{path}", pod.url)),
        bearer,
    )
    .await
}

async fn post(pod: &Pod, path: &str, body: Value, bearer: Option<&str>) -> Resp {
    send(
        reqwest::Client::new()
            .post(format!("{}{path}", pod.url))
            .json(&body),
        bearer,
    )
    .await
}

async fn prompt(pod: &Pod, ws: &str, text: &str) -> Resp {
    post(
        pod,
        "/v1/agent/modify_layout",
        json!({ "prompt": text, "workspace_id": ws }),
        None,
    )
    .await
}

fn ids(body: &Value) -> Vec<&str> {
    body["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect()
}

fn seed_json(id: &str) -> Value {
    let seed = layouts::catalog().iter().find(|s| s.id == id).unwrap();
    serde_json::from_str(&seed.json).unwrap()
}

fn blob_count(pod: &Pod) -> i64 {
    pod.state.store().layout_storage().unwrap().0
}

/// Reads SSE messages, skipping keepalives.
struct SseReader {
    resp: reqwest::Response,
    buf: String,
}

impl SseReader {
    async fn connect(pod: &Pod, ws: &str) -> Self {
        let resp = reqwest::Client::new()
            .get(format!("{}/v1/workspaces/{ws}/events", pod.url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        Self {
            resp,
            buf: String::new(),
        }
    }

    async fn next_event(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(i) = self.buf.find("\n\n") {
                    let msg = self.buf[..i].to_owned();
                    self.buf.drain(..i + 2);
                    if let Some(data) = msg.strip_prefix("event: layout\ndata: ") {
                        return serde_json::from_str(data).unwrap();
                    }
                    continue;
                }
                let chunk = self.resp.chunk().await.unwrap().expect("stream ended");
                self.buf.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("no SSE event within 5 s")
    }
}

fn event(ws: &str, revision: i64, layout_id: &str, layout_version: i64) -> Value {
    json!({ "workspace_id": ws, "version": revision, "layout_id": layout_id,
            "layout_version": layout_version })
}

#[tokio::test]
async fn list_and_get_layouts() {
    let pod = open_pod().await;
    let r = get(&pod, "/v1/layouts?workspace_id=pixelfed_demo", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        ids(&r.body),
        [
            "pixelfed_demo.classic",
            "pixelfed_demo.compact",
            "pixelfed_demo.editorial",
            "pixelfed_demo.minimal"
        ]
    );
    assert_eq!(r.body["workspace_id"], "pixelfed_demo");
    assert_eq!(r.body["workspace_revision"], 1);
    assert_eq!(
        r.body["workspaces"],
        json!([{ "workspace_id": "pixelfed_demo", "revision": 1,
                 "layout_id": "pixelfed_demo.classic", "layout_version": 1 }])
    );
    let classic = &r.body["layouts"][0];
    assert_eq!(classic["title"], "Classic feed");
    assert_eq!(classic["preview_hint"], "feed-classic");
    assert_eq!(classic["builtin"], true);
    assert_eq!(classic["current_version"], 1);
    assert_eq!(classic["active"], true);
    assert_eq!(classic["active_version"], 1);
    assert!(classic["updated_at"].as_i64().unwrap() > 0);
    let editorial = &r.body["layouts"][2];
    assert_eq!(editorial["title"], "Editorial journal");
    assert_eq!(editorial["active"], false);
    assert!(editorial.get("active_version").is_none());

    // Everything (open mode shows pixelfed_home too).
    let all = get(&pod, "/v1/layouts", None).await;
    assert_eq!(ids(&all.body).len(), 10);
    assert_eq!(all.body["workspaces"].as_array().unwrap().len(), 3);
    assert!(all.body.get("workspace_revision").is_none());

    let one = get(&pod, "/v1/layouts/home.big_counter", None).await;
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.body["title"], "Big counter");
    assert_eq!(one.body["workspace_id"], "home");
    assert_eq!(one.body["workspace_revision"], 1);

    // Errors.
    let r = get(&pod, "/v1/layouts/nope.nope", None).await;
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("layout_not_found"))
    );
    let r = get(&pod, "/v1/layouts/a%2Fb", None).await;
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_layout_id"))
    );
    let r = get(&pod, "/v1/layouts?workspace_id=nope", None).await;
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("workspace_not_found"))
    );
    let r = get(&pod, "/v1/layouts?workspace_id=a.b", None).await;
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_workspace_id"))
    );
}

#[tokio::test]
async fn reads_follow_the_workspace_get_policy() {
    let pod = closed_pod().await;
    // Anonymous: public workspaces only.
    let anon = get(&pod, "/v1/layouts", None).await;
    assert_eq!(anon.status, StatusCode::OK);
    assert_eq!(ids(&anon.body).len(), 6);
    assert!(
        ids(&anon.body)
            .iter()
            .all(|id| !id.starts_with("pixelfed_home."))
    );
    assert!(
        anon.body["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["workspace_id"] != "pixelfed_home")
    );
    assert_eq!(
        get(&pod, "/v1/layouts/home.counter", None).await.status,
        StatusCode::OK
    );
    for path in [
        "/v1/layouts/pixelfed_home.classic",
        "/v1/layouts/pixelfed_home.classic/versions",
        "/v1/layouts/pixelfed_home.classic/versions/1",
        "/v1/layouts?workspace_id=pixelfed_home",
    ] {
        assert_eq!(
            get(&pod, path, None).await.status,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        assert_eq!(
            get(&pod, path, Some("stale")).await.status,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        assert_eq!(
            get(&pod, path, Some("tok")).await.status,
            StatusCode::OK,
            "{path}"
        );
    }
    // A stale bearer on the full list is 401 (the client learns it is stale).
    assert_eq!(
        get(&pod, "/v1/layouts", Some("stale")).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ids(&get(&pod, "/v1/layouts", Some("tok")).await.body).len(),
        10
    );

    // Writes need a session.
    for (path, body) in [
        ("/v1/layouts/home.big_counter/activate", json!({})),
        ("/v1/layouts/home.counter/restore", json!({ "version": 1 })),
    ] {
        for bearer in [None, Some("stale")] {
            let r = post(&pod, path, body.clone(), bearer).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path} {bearer:?}");
            assert_eq!(r.body["error"], "unauthorized");
        }
    }
    assert_eq!(
        pod.state.store().active("home").unwrap().unwrap().revision,
        1,
        "nothing changed"
    );
    let r = post(
        &pod,
        "/v1/layouts/home.big_counter/activate",
        json!({}),
        Some("tok"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["revision"], 2);
}

#[tokio::test]
async fn versions_list_and_one_version() {
    let pod = open_pod().await;
    let r = get(&pod, "/v1/layouts/home.counter/versions", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let v = &r.body["versions"][0];
    assert_eq!(r.body["versions"].as_array().unwrap().len(), 1);
    assert_eq!(v["version"], 1);
    assert_eq!(v["source"], "seed");
    assert_eq!(v["hash"].as_str().unwrap().len(), 64);
    assert!(v["bytes"].as_i64().unwrap() > 500);
    assert!(v.get("prompt").is_none() && v.get("provider").is_none());
    assert!(r.body.get("next_before").is_none());

    let one = get(&pod, "/v1/layouts/home.counter/versions/1", None).await;
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.body["layout_id"], "home.counter");
    assert_eq!(one.body["workspace_id"], "home");
    assert_eq!(one.body["source"], "seed");
    assert_eq!(
        one.body["layout"],
        serde_json::to_value(layouts::builtin("home").unwrap()).unwrap()
    );

    // Two agent edits, then paging.
    for t in ["hide captions", "add a weather widget"] {
        assert_eq!(
            prompt(&pod, "pixelfed_demo", t).await.status,
            StatusCode::OK
        );
    }
    let r = get(
        &pod,
        "/v1/layouts/pixelfed_demo.classic/versions?limit=2",
        None,
    )
    .await;
    let got: Vec<i64> = r.body["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(got, [3, 2]);
    assert_eq!(r.body["versions"][0]["source"], "agent");
    assert_eq!(r.body["versions"][0]["prompt"], "add a weather widget");
    assert_eq!(r.body["versions"][0]["provider"], "rules");
    assert_eq!(r.body["next_before"], 2);
    let r = get(
        &pod,
        "/v1/layouts/pixelfed_demo.classic/versions?limit=2&before=2",
        None,
    )
    .await;
    assert_eq!(r.body["versions"].as_array().unwrap().len(), 1);
    assert!(r.body.get("next_before").is_none());

    for (path, status) in [
        ("/v1/layouts/home.counter/versions/9", StatusCode::NOT_FOUND),
        (
            "/v1/layouts/home.counter/versions/x",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/layouts/home.counter/versions/0",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/layouts/home.counter/versions?limit=0",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/layouts/home.counter/versions?before=x",
            StatusCode::BAD_REQUEST,
        ),
        ("/v1/layouts/nope.x/versions", StatusCode::NOT_FOUND),
    ] {
        assert_eq!(get(&pod, path, None).await.status, status, "{path}");
    }
}

#[tokio::test]
async fn activate_switches_the_served_layout_and_broadcasts() {
    let pod = open_pod().await;
    let mut sse = SseReader::connect(&pod, "pixelfed_demo").await;
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 1, "pixelfed_demo.classic", 1)
    );

    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.editorial/activate",
        json!({}),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body,
        json!({ "workspace_id": "pixelfed_demo", "revision": 2,
                "layout_id": "pixelfed_demo.editorial", "layout_version": 1, "unchanged": false })
    );
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 2, "pixelfed_demo.editorial", 1)
    );

    let ws = get(&pod, "/v1/workspaces/pixelfed_demo", None).await;
    assert_eq!(ws.header(layouts::VERSION_HEADER), Some("2"));
    assert_eq!(
        ws.header(layouts::LAYOUT_ID_HEADER),
        Some("pixelfed_demo.editorial")
    );
    assert_eq!(ws.header(layouts::LAYOUT_ID_VERSION_HEADER), Some("1"));
    assert_eq!(ws.body["workspace"], seed_json("pixelfed_demo.editorial"));
    assert!(
        !ws.body["entities"].as_object().unwrap().is_empty(),
        "entities still served"
    );

    // Activating the active layout again changes nothing (no event).
    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.editorial/activate",
        json!(null),
        None,
    )
    .await;
    assert_eq!(
        (r.body["revision"].as_i64(), r.body["unchanged"].as_bool()),
        (Some(2), Some(true))
    );

    // Back to classic with an explicit version equal to its latest: no restore.
    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.classic/activate",
        json!({ "version": 1 }),
        None,
    )
    .await;
    assert_eq!(r.body["revision"], 3);
    assert!(r.body.get("restored_version").is_none());
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 3, "pixelfed_demo.classic", 1)
    );
    // Legacy per-workspace endpoints follow the active layout.
    let versions = get(&pod, "/v1/workspaces/pixelfed_demo/versions", None).await;
    assert_eq!(versions.body.as_array().unwrap().len(), 1);

    // Errors.
    for (path, body, status) in [
        (
            "/v1/layouts/pixelfed_demo.editorial/activate",
            json!({ "version": 7 }),
            StatusCode::NOT_FOUND,
        ),
        (
            "/v1/layouts/pixelfed_demo.editorial/activate",
            json!({ "version": 0 }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/layouts/nope.x/activate",
            json!({}),
            StatusCode::NOT_FOUND,
        ),
        (
            "/v1/layouts/a%20b/activate",
            json!({}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(post(&pod, path, body, None).await.status, status, "{path}");
    }
    assert_eq!(
        pod.state
            .store()
            .active("pixelfed_demo")
            .unwrap()
            .unwrap()
            .revision,
        3
    );
}

#[tokio::test]
async fn restore_is_append_only_deduped_and_broadcast_when_active() {
    let pod = open_pod().await;
    let mut sse = SseReader::connect(&pod, "pixelfed_demo").await;
    sse.next_event().await;
    let r = prompt(&pod, "pixelfed_demo", "hide captions").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 2, "pixelfed_demo.classic", 2)
    );
    let blobs = blob_count(&pod);

    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.classic/restore",
        json!({ "version": 1 }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body,
        json!({ "layout_id": "pixelfed_demo.classic", "workspace_id": "pixelfed_demo",
                "version": 3, "restored_from": 1, "unchanged": false, "active": true, "revision": 3 })
    );
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 3, "pixelfed_demo.classic", 3)
    );
    assert_eq!(blob_count(&pod), blobs, "a restore adds no blob");
    let ws = get(&pod, "/v1/workspaces/pixelfed_demo", None).await;
    assert_eq!(ws.body["workspace"], seed_json("pixelfed_demo.classic"));
    let v = get(&pod, "/v1/layouts/pixelfed_demo.classic/versions", None).await;
    assert_eq!(v.body["versions"][0]["source"], "restore");
    assert_eq!(v.body["versions"][0]["prompt"], "restore v1");
    assert_eq!(v.body["versions"][0]["hash"], v.body["versions"][2]["hash"]);

    // Restoring what is already current: no-op.
    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.classic/restore",
        json!({ "version": 1 }),
        None,
    )
    .await;
    assert_eq!(
        (r.body["unchanged"].as_bool(), r.body["version"].as_i64()),
        (Some(true), Some(3))
    );
    // The restore is itself undoable: restore v2 (the agent edit).
    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.classic/restore",
        json!({ "version": 2 }),
        None,
    )
    .await;
    assert_eq!(
        (r.body["version"].as_i64(), r.body["revision"].as_i64()),
        (Some(4), Some(4))
    );
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 4, "pixelfed_demo.classic", 4)
    );

    // An inactive layout: new version, no revision, no event.
    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.minimal/activate",
        json!({}),
        None,
    )
    .await;
    assert_eq!(r.body["revision"], 5);
    assert_eq!(sse.next_event().await["version"], 5);
    assert_eq!(
        prompt(&pod, "pixelfed_demo", "hide captions").await.status,
        StatusCode::OK
    );
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 6, "pixelfed_demo.minimal", 2)
    );
    post(
        &pod,
        "/v1/layouts/pixelfed_demo.classic/activate",
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 7, "pixelfed_demo.classic", 4)
    );
    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.minimal/restore",
        json!({ "version": 1 }),
        None,
    )
    .await;
    assert_eq!(
        r.body,
        json!({ "layout_id": "pixelfed_demo.minimal", "workspace_id": "pixelfed_demo",
                "version": 3, "restored_from": 1, "unchanged": false, "active": false })
    );
    // The next event is the next visible change, not the inactive restore.
    post(
        &pod,
        "/v1/layouts/pixelfed_demo.minimal/activate",
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        sse.next_event().await,
        event("pixelfed_demo", 8, "pixelfed_demo.minimal", 3)
    );

    for (body, status) in [
        (json!({}), StatusCode::BAD_REQUEST),
        (json!({ "version": "1" }), StatusCode::BAD_REQUEST),
        (json!({ "version": 99 }), StatusCode::NOT_FOUND),
    ] {
        let r = post(
            &pod,
            "/v1/layouts/pixelfed_demo.minimal/restore",
            body.clone(),
            None,
        )
        .await;
        assert_eq!(r.status, status, "{body}");
    }
}

#[tokio::test]
async fn agent_and_rollback_work_on_a_non_default_active_layout() {
    let pod = open_pod().await;
    post(
        &pod,
        "/v1/layouts/pixelfed_demo.editorial/activate",
        json!({}),
        None,
    )
    .await;
    let r = prompt(&pod, "pixelfed_demo", "hide captions").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["layout_version"], 3, "the workspace revision (D24)");
    assert_eq!(r.body["layout_id"], "pixelfed_demo.editorial");
    assert_eq!(r.body["layout_id_version"], 2);
    assert_eq!(r.header(layouts::VERSION_HEADER), Some("3"));
    assert_eq!(
        r.header(layouts::LAYOUT_ID_HEADER),
        Some("pixelfed_demo.editorial")
    );
    // The editorial shape is kept (media first, no action bar), caption blanked.
    let card = &r.body["workspace"]["slots"][0]["content"]["item_template"];
    assert_eq!(card["author_slot"]["type"], "pixelfed://MediaCarousel");
    assert_eq!(card["caption_slot"]["value_expr"], "");
    // Classic is untouched.
    let classic = get(&pod, "/v1/layouts/pixelfed_demo.classic", None).await;
    assert_eq!(classic.body["current_version"], 1);
    // Legacy history/rollback act on the active layout.
    let v = get(&pod, "/v1/workspaces/pixelfed_demo/versions", None).await;
    assert_eq!(v.body.as_array().unwrap().len(), 2);
    let r = post(
        &pod,
        "/v1/workspaces/pixelfed_demo/rollback",
        json!({ "version": 1 }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["version"], 3);
    assert_eq!(r.body["revision"], 4);
    assert_eq!(r.body["layout_id"], "pixelfed_demo.editorial");
    assert_eq!(r.header(layouts::VERSION_HEADER), Some("4"));
    assert_eq!(r.body["workspace"], seed_json("pixelfed_demo.editorial"));
}

/// Signals when generation starts, then waits to be released.
struct GatedProvider {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl LayoutProvider for GatedProvider {
    fn name(&self) -> &str {
        "gated"
    }
    fn model(&self) -> Option<&str> {
        None
    }
    fn generate<'a>(
        &'a self,
        _prompt: &'a str,
        _layout: &'a Value,
        _registry: &'a Value,
    ) -> agent::GenerateFuture<'a> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            Ok(agent::Generated {
                ops: vec![json!({ "op": "replace", "path": "/title", "value": "Agent" })],
                explanation: None,
            })
        })
    }
}

#[tokio::test]
async fn a_switch_during_generation_reapplies_on_the_new_active_layout() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let pod = spawn(
        AppState::in_memory()
            .with_agent_open(true)
            .with_agent(Selection::provider(GatedProvider {
                started: started.clone(),
                release: release.clone(),
            })),
    )
    .await;
    let url = pod.url.clone();
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{url}/v1/agent/modify_layout"))
            .json(&json!({ "prompt": "rename", "workspace_id": "pixelfed_demo" }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    });
    started.notified().await;
    let r = post(
        &pod,
        "/v1/layouts/pixelfed_demo.compact/activate",
        json!({}),
        None,
    )
    .await;
    assert_eq!(r.body["revision"], 2);
    release.notify_one();
    let body = request.await.unwrap();
    assert_eq!(body["layout_version"], 3, "{body}");
    assert_eq!(body["layout_id"], "pixelfed_demo.compact");
    assert_eq!(body["layout_id_version"], 2);
    assert_eq!(body["workspace"]["title"], "Agent");
    let mut expected = seed_json("pixelfed_demo.compact");
    expected["title"] = json!("Agent");
    assert_eq!(
        body["workspace"], expected,
        "applied to compact, not to classic"
    );
    let classic = get(&pod, "/v1/layouts/pixelfed_demo.classic", None).await;
    assert_eq!(
        classic.body["current_version"], 1,
        "the old active layout was not edited"
    );
}

#[tokio::test]
async fn seeding_is_idempotent_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    {
        let pod = spawn(
            AppState::open_dir(dir.path())
                .unwrap()
                .with_agent_open(true),
        )
        .await;
        post(
            &pod,
            "/v1/layouts/home.big_counter/activate",
            json!({}),
            None,
        )
        .await;
        let r = post(
            &pod,
            "/v1/layouts/home.counter/restore",
            json!({ "version": 1 }),
            None,
        )
        .await;
        assert_eq!(r.body["unchanged"], true);
    }
    let pod = spawn(
        AppState::open_dir(dir.path())
            .unwrap()
            .with_agent_open(true),
    )
    .await;
    let all = get(&pod, "/v1/layouts", None).await;
    assert_eq!(ids(&all.body).len(), 10);
    let home = get(&pod, "/v1/layouts?workspace_id=home", None).await;
    assert_eq!(home.body["workspace_revision"], 2);
    assert_eq!(home.body["layouts"][0]["id"], "home.big_counter");
    assert_eq!(home.body["layouts"][0]["active"], true);
    for l in home.body["layouts"].as_array().unwrap() {
        assert_eq!(l["current_version"], 1, "{l}");
    }
    let ws = get(&pod, "/v1/workspaces/home", None).await;
    assert_eq!(
        ws.header(layouts::LAYOUT_ID_HEADER),
        Some("home.big_counter")
    );
    assert_eq!(ws.body["workspace"], seed_json("home.big_counter"));
    // 10 built-ins, 10 distinct blobs.
    assert_eq!(pod.state.store().layout_storage().unwrap().0, 10);
}
