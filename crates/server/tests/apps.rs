//! Layouts as code: the read-only apps / versions API, the served version
//! header, the SSE push channel, and the routes that no longer exist.

use std::sync::Arc;
use std::time::Duration;

use protocol::{UiNode, WorkspaceSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use server::AppState;
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

async fn pod() -> Pod {
    spawn(AppState::in_memory()).await
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// `(status, X-Construct-Layout-Version, JSON body)`.
async fn get(pod: &Pod, path: &str) -> (StatusCode, Option<String>, Value) {
    let resp = client()
        .get(format!("{}{path}", pod.url))
        .send()
        .await
        .unwrap();
    let version = resp
        .headers()
        .get(layouts::VERSION_HEADER)
        .map(|v| v.to_str().unwrap().to_owned());
    (
        resp.status(),
        version,
        resp.json().await.unwrap_or(Value::Null),
    )
}

/// A recognizably different layout: the built-in one retitled.
fn retitled(ws: &str, title: &str) -> UiNode {
    let mut node = layouts::builtin(ws).unwrap();
    if let UiNode::Workspace { title: t, id, .. } = &mut node {
        *t = title.to_owned();
        *id = ws.to_owned();
    }
    node
}

/// Append a version (as a deploy would) and broadcast it.
fn deploy(pod: &Pod, ws: &str, title: &str) -> i64 {
    let v =
        layouts::append_version(pod.state.store(), ws, &retitled(ws, title), Some(title)).unwrap();
    pod.state.publish_layout(ws, v);
    v
}

fn is_rfc3339(v: &Value) -> bool {
    v.as_str()
        .and_then(server::pixelfed::parse_rfc3339)
        .is_some_and(|t| t > 1_700_000_000)
}

#[tokio::test]
async fn apps_lists_only_pixelfed_unless_dev_apps_are_shown() {
    let pod = pod().await;
    let (status, _, body) = get(&pod, "/v1/apps").await;
    assert_eq!(status, StatusCode::OK);
    let apps = body["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 1, "{body}");
    let a = &apps[0];
    assert_eq!(
        (&a["id"], &a["title"], &a["icon"], &a["latest_version"]),
        (
            &json!("pixelfed_home"),
            &json!("Pixelfed"),
            &json!("photo"),
            &json!(2)
        )
    );
    assert!(is_rfc3339(&a["updated_at"]), "{a}");

    let dev = spawn(AppState::in_memory().with_dev_apps(true)).await;
    deploy(&dev, "home", "Two");
    let (_, _, body) = get(&dev, "/v1/apps").await;
    let got: Vec<(&str, &str, &str, i64)> = body["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["id"].as_str().unwrap(),
                a["title"].as_str().unwrap(),
                a["icon"].as_str().unwrap(),
                a["latest_version"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("pixelfed_home", "Pixelfed", "photo", 2),
            ("pixelfed_demo", "Pixelfed demo", "photo", 1),
            ("home", "Counter", "counter", 2),
        ]
    );
    // Hidden apps' endpoints work without the flag.
    let (status, version, _) = get(&pod, "/v1/workspaces/pixelfed_demo").await;
    assert_eq!((status, version.as_deref()), (StatusCode::OK, Some("1")));
    let (status, _, body) = get(&pod, "/v1/apps/home/versions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["latest_version"], 1);
}

#[tokio::test]
async fn versions_page_newest_first() {
    let pod = pod().await;
    for i in 2..=5 {
        assert_eq!(deploy(&pod, "home", &format!("T{i}")), i);
    }
    let (status, _, body) = get(&pod, "/v1/apps/home/versions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app_id"], "home");
    assert_eq!(body["latest_version"], 5);
    let versions = body["versions"].as_array().unwrap();
    let numbers: Vec<i64> = versions
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [5, 4, 3, 2, 1]);
    assert!(body.get("next_before").is_none(), "{body}");
    let first = versions.last().unwrap();
    assert_eq!(first["source"], "file");
    assert_eq!(first["note"], "Counter");
    assert!(is_rfc3339(&first["created_at"]), "{first}");
    assert_eq!(versions[0]["source"], "manual");
    assert_eq!(versions[0]["note"], "T5");
    assert!(versions[0].get("layout").is_none(), "metadata only");

    // Paging.
    let (_, _, page) = get(&pod, "/v1/apps/home/versions?limit=2").await;
    let numbers: Vec<i64> = page["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [5, 4]);
    assert_eq!(page["next_before"], 4);
    let (_, _, page) = get(&pod, "/v1/apps/home/versions?limit=2&before=4").await;
    assert_eq!(page["versions"][0]["version"], 3);
    assert_eq!(page["next_before"], 2);
    let (_, _, page) = get(&pod, "/v1/apps/home/versions?limit=2&before=2").await;
    assert_eq!(page["versions"].as_array().unwrap().len(), 1);
    assert!(page.get("next_before").is_none(), "{page}");
    assert_eq!(page["latest_version"], 5);

    // Bad queries.
    for q in [
        "limit=0",
        "limit=101",
        "limit=x",
        "before=0",
        "before=-1",
        "before=a",
    ] {
        let (status, _, body) = get(&pod, &format!("/v1/apps/home/versions?{q}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body["error"], "invalid_request", "{q}");
    }
    let (status, _, _) = get(&pod, "/v1/apps/home/versions?limit=100").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn one_version_carries_its_layout() {
    let pod = pod().await;
    deploy(&pod, "pixelfed_home", "Two");
    let (status, _, body) = get(&pod, "/v1/apps/pixelfed_home/versions/1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["app_id"], "pixelfed_home");
    assert_eq!(body["version"], 1);
    assert_eq!(body["source"], "file");
    assert_eq!(body["note"], "Classic feed");
    assert!(is_rfc3339(&body["created_at"]));
    let layout: UiNode = serde_json::from_value(body["layout"].clone()).unwrap();
    assert_eq!(layout, layouts::builtin("pixelfed_home").unwrap());
    // f32 fields are not widened on the way out.
    assert!(!body["layout"].to_string().contains("0000001"), "{body}");
    let (_, _, two) = get(&pod, "/v1/apps/pixelfed_home/versions/3").await;
    assert_eq!(two["layout"]["title"], "Two");
    // Public: no session needed even for pixelfed_home.
    let (status, _, body) = get(&pod, "/v1/apps/pixelfed_home/versions/4").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({ "error": "version_not_found", "app_id": "pixelfed_home", "version": 4 })
    );
    for bad in ["0", "-1", "x", "1.5"] {
        let (status, _, body) = get(&pod, &format!("/v1/apps/home/versions/{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(body["error"], "invalid_request");
    }
}

#[tokio::test]
async fn unknown_and_malformed_app_ids() {
    let pod = pod().await;
    for path in ["/v1/apps/nope/versions", "/v1/apps/nope/versions/1"] {
        let (status, _, body) = get(&pod, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, json!({ "error": "app_not_found", "id": "nope" }));
    }
    let long = "a".repeat(100);
    for path in [
        "/v1/apps/bad.id/versions".to_owned(),
        format!("/v1/apps/{long}/versions/1"),
    ] {
        let (status, _, body) = get(&pod, &path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["error"], "invalid_workspace_id");
        assert!(body["id"].as_str().unwrap().len() <= 67);
    }
}

#[tokio::test]
async fn workspace_get_serves_the_latest_app_version() {
    let pod = pod().await;
    for ws in ["home", "pixelfed_demo"] {
        let resp = client()
            .get(format!("{}/v1/workspaces/{ws}", pod.url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[layouts::VERSION_HEADER], "1");
        assert!(resp.headers().get("x-construct-layout-id").is_none());
        assert!(
            resp.headers()
                .get("x-construct-layout-id-version")
                .is_none()
        );
        let spec: WorkspaceSpec = resp.json().await.unwrap();
        let file = layouts::parse_files(layouts::FILES)
            .unwrap()
            .into_iter()
            .find(|f| f.app_id == ws && f.number == 1)
            .unwrap();
        assert_eq!(spec.workspace, file.layout);
    }
    deploy(&pod, "home", "Second");
    let (status, version, body) = get(&pod, "/v1/workspaces/home").await;
    assert_eq!((status, version.as_deref()), (StatusCode::OK, Some("2")));
    assert_eq!(body["workspace"]["title"], "Second");
    assert_eq!(body["entities"]["core://counters/home"]["count"], 0);
}

#[tokio::test]
async fn client_reachable_layout_writes_and_old_catalog_routes_are_gone() {
    let pod = spawn(AppState::in_memory().with_test_hooks(true)).await;
    pod.state
        .store()
        .create_session(&session_hash("tok"))
        .unwrap();
    let posts = [
        "/v1/agent/modify_layout",
        "/v1/workspaces/home/rollback",
        "/v1/workspaces/pixelfed_home/rollback",
        "/v1/layouts/home.counter/activate",
        "/v1/layouts/home.counter/restore",
        "/v1/apps",
        "/v1/apps/home/versions",
        "/v1/apps/home/versions/1",
    ];
    for path in posts {
        let resp = client()
            .post(format!("{}{path}", pod.url))
            .bearer_auth("tok")
            .json(&json!({ "prompt": "hide captions", "version": 1 }))
            .send()
            .await
            .unwrap();
        assert!(
            matches!(
                resp.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "POST {path}: {}",
            resp.status()
        );
    }
    for path in [
        "/v1/workspaces/home/versions",
        "/v1/workspaces/pixelfed_home/versions",
        "/v1/layouts",
        "/v1/layouts?workspace_id=home",
        "/v1/layouts/home.counter",
        "/v1/layouts/home.counter/versions",
        "/v1/layouts/home.counter/versions/1",
    ] {
        let resp = client()
            .get(format!("{}{path}", pod.url))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "GET {path}");
    }
    // The agent route is a plain 404 (no route), not a handler's 401/503.
    let resp = client()
        .post(format!("{}/v1/agent/modify_layout", pod.url))
        .json(&json!({ "prompt": "hide captions" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // Nothing changed.
    let (_, version, _) = get(&pod, "/v1/workspaces/home").await;
    assert_eq!(version.as_deref(), Some("1"));
}

#[tokio::test]
async fn restart_on_the_same_database_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let state = AppState::open_dir(dir.path()).unwrap();
        assert_eq!(
            layouts::latest(state.store(), "pixelfed_home")
                .unwrap()
                .unwrap()
                .0,
            2
        );
        layouts::append_version(state.store(), "home", &retitled("home", "Two"), None).unwrap();
    }
    let state = AppState::open_dir(dir.path()).unwrap();
    for (ws, want) in [("pixelfed_home", 2), ("pixelfed_demo", 1), ("home", 2)] {
        let (v, _) = layouts::latest(state.store(), ws).unwrap().unwrap();
        assert_eq!(v, want, "{ws}");
    }
    assert!(layouts::latest(state.store(), "nope").unwrap().is_none());
}

/// A legacy (migrated) AI-edited version still holding the removed
/// `action.agent.prompt` intent serves as `action.unknown`, from both the
/// versions API and the workspace GET.
#[tokio::test]
async fn agent_era_versions_serve_with_unknown_intents() {
    let pod = pod().await;
    let layout = serde_json::to_string(&layouts::builtin("home").unwrap()).unwrap();
    assert!(layout.contains("\"action.mutation.optimistic\""));
    let legacy = layout.replacen(
        "\"action.mutation.optimistic\"",
        "\"action.agent.prompt\",\"prompt\":\"make it bigger\"",
        1,
    );
    let v = pod
        .state
        .store()
        .append_app_version("home", &legacy, "agent", Some("make it bigger"))
        .unwrap();
    assert_eq!(v, 2);
    let (status, _, body) = get(&pod, "/v1/apps/home/versions/2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body["layout"].to_string();
    assert!(text.contains("\"intent\":\"action.unknown\""), "{text}");
    assert!(!text.contains("action.agent.prompt"), "{text}");
    assert_eq!(body["source"], "agent");
    let (status, version, body) = get(&pod, "/v1/workspaces/home").await;
    assert_eq!((status, version.as_deref()), (StatusCode::OK, Some("2")));
    assert!(
        body["workspace"]
            .to_string()
            .contains("\"intent\":\"action.unknown\""),
        "{body}"
    );
}

/// A legacy version whose stored text is not a UiNode this build reads is
/// served as stored by `GET /v1/apps/{id}/versions/{n}` (history to look at),
/// not a 500.
#[tokio::test]
async fn a_legacy_version_that_is_not_a_ui_node_is_served_as_stored() {
    let pod = pod().await;
    let store = pod.state.store();
    store
        .append_app_version("home", "{\"edited\":true}", "agent", Some("odd"))
        .unwrap();
    let (status, _, body) = get(&pod, "/v1/apps/home/versions/2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["layout"], json!({ "edited": true }));
    assert_eq!(body["note"], "odd");
    // Not even JSON: a clear 500.
    store
        .append_app_version("home", "not json", "agent", None)
        .unwrap();
    let (status, _, body) = get(&pod, "/v1/apps/home/versions/3").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "corrupt_version", "{body}");
    assert_eq!(body["version"], 3);
}

/// A database where `home/0001` was recorded with other content.
fn db_with_a_changed_home_file(dir: &std::path::Path) {
    let store = server::store::Store::open(&dir.join("server.db")).unwrap();
    let file = server::store::FileVersion {
        key: "home/0001".into(),
        number: 1,
        path: "layouts/home/0001.json".into(),
        note: "Counter".into(),
        json: "{}".into(),
        file_hash: "0".repeat(64),
    };
    store
        .sync_layout_files(&[("home", vec![file])])
        .unwrap()
        .unwrap();
}

const CHANGED: &str = "layouts/home/0001.json changed after it was deployed as version 1";

#[test]
fn a_sync_error_stops_the_start() {
    let dir = tempfile::tempdir().unwrap();
    db_with_a_changed_home_file(dir.path());
    let err = AppState::open_dir(dir.path()).err().unwrap().to_string();
    assert!(err.contains(CHANGED), "{err}");
    assert!(err.contains("To recover"), "{err}");
    // Nothing was appended for any app.
    let store = server::store::Store::open(&dir.path().join("server.db")).unwrap();
    assert!(store.app_latest_meta("pixelfed_home").unwrap().is_none());
}

#[test]
fn the_server_binary_exits_non_zero_on_a_sync_error() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    db_with_a_changed_home_file(dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_server"))
        .env("CONSTRUCT_DATA_DIR", dir.path())
        .env("CONSTRUCT_ADDR", "127.0.0.1:0")
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            panic!("the server started despite a changed layout file");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert_eq!(status.code(), Some(1), "{out}");
    assert!(out.contains("cannot start"), "{out}");
    assert!(out.contains(CHANGED), "{out}");
}

// ---------- SSE ----------

/// Reads SSE messages (blocks separated by a blank line) from a streaming response.
struct SseReader {
    resp: reqwest::Response,
    buf: String,
}

impl SseReader {
    async fn connect(url: &str, bearer: Option<&str>) -> Self {
        let mut req = client().get(url);
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        Self {
            resp,
            buf: String::new(),
        }
    }

    async fn next(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(i) = self.buf.find("\n\n") {
                    let msg = self.buf[..i].to_owned();
                    self.buf.drain(..i + 2);
                    return msg;
                }
                let chunk = self.resp.chunk().await.unwrap().expect("stream ended");
                self.buf.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("no SSE message within 5 s")
    }

    /// The next message that is not a keepalive comment.
    async fn next_event(&mut self) -> String {
        loop {
            let msg = self.next().await;
            if msg != ": keepalive" {
                return msg;
            }
        }
    }
}

fn layout_msg(ws: &str, version: i64) -> String {
    format!(
        "event: layout\ndata: {}",
        json!({ "workspace_id": ws, "version": version })
    )
}

#[tokio::test]
async fn sse_sends_current_version_then_new_versions_of_its_workspace() {
    let pod = pod().await;
    deploy(&pod, "home", "Two");
    let mut home =
        SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(home.next_event().await, layout_msg("home", 2));
    let mut demo = SseReader::connect(
        &format!("{}/v1/workspaces/pixelfed_demo/events", pod.url),
        None,
    )
    .await;
    assert_eq!(demo.next_event().await, layout_msg("pixelfed_demo", 1));
    // Each stream only gets its own workspace's versions.
    assert_eq!(deploy(&pod, "pixelfed_demo", "X"), 2);
    assert_eq!(deploy(&pod, "home", "Three"), 3);
    assert_eq!(home.next_event().await, layout_msg("home", 3));
    assert_eq!(demo.next_event().await, layout_msg("pixelfed_demo", 2));
    // A reconnecting client catches up with the current version.
    let mut again =
        SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(again.next_event().await, layout_msg("home", 3));
}

#[tokio::test]
async fn sse_keepalive_comment() {
    let pod = spawn(AppState::in_memory().with_sse_keepalive(Duration::from_millis(100))).await;
    let mut sse = SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(sse.next().await, layout_msg("home", 1));
    assert_eq!(sse.next().await, ": keepalive");
    assert_eq!(sse.next().await, ": keepalive");
}

#[tokio::test]
async fn sse_auth_matches_the_workspace_get() {
    let pod = pod().await;
    pod.state
        .store()
        .create_session(&session_hash("tok"))
        .unwrap();
    let url = format!("{}/v1/workspaces/pixelfed_home/events", pod.url);
    // No bearer: allowed (events carry only version numbers).
    let mut anon = SseReader::connect(&url, None).await;
    assert_eq!(anon.next_event().await, layout_msg("pixelfed_home", 2));
    // Valid session: allowed.
    let mut signed_in = SseReader::connect(&url, Some("tok")).await;
    assert_eq!(signed_in.next_event().await, layout_msg("pixelfed_home", 2));
    // Stale bearer: 401, like GET /v1/workspaces/pixelfed_home.
    let resp = client()
        .get(&url)
        .bearer_auth("stale")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Unknown workspace: 404; malformed: 400.
    let resp = client()
        .get(format!("{}/v1/workspaces/nope/events", pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = client()
        .get(format!("{}/v1/workspaces/no.pe/events", pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn shutdown_ends_open_streams() {
    let pod = pod().await;
    let mut sse = SseReader::connect(&format!("{}/v1/workspaces/home/events", pod.url), None).await;
    assert_eq!(sse.next_event().await, layout_msg("home", 1));
    pod.state.shutdown();
    let end = tokio::time::timeout(Duration::from_secs(5), sse.resp.chunk())
        .await
        .expect("stream did not end after shutdown");
    assert!(matches!(end, Ok(None)), "{end:?}");
}
