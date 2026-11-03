//! Layouts as code: the read-only apps / versions API, the served version
//! header, the SSE push channel, and the routes that no longer exist.

use std::sync::Arc;
use std::time::Duration;

use protocol::{UiNode, WorkspaceSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use server::AppState;
use server::layouts;
use server::session::session_hash;
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
    spawn(server::state_in_memory()).await
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// `(status, X-Construct-Layout-Version, JSON body)`, as a current client
/// (it sends this build's schema).
async fn get(pod: &Pod, path: &str) -> (StatusCode, Option<String>, Value) {
    get_as(pod, path, Some(protocol::SCHEMA_VERSION)).await
}

/// [`get`] as a client of `schema` (`None`: no header, an old client).
async fn get_as(pod: &Pod, path: &str, schema: Option<u32>) -> (StatusCode, Option<String>, Value) {
    let mut req = client().get(format!("{}{path}", pod.url));
    if let Some(n) = schema {
        req = req.header(protocol::SCHEMA_HEADER, n.to_string());
    }
    let resp = req.send().await.unwrap();
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

/// The layout of an app's newest file (what a current client is served on a
/// fresh pod).
fn newest_file(ws: &str) -> UiNode {
    layouts::parse_files(&server::catalog(), &server::layout_files())
        .unwrap()
        .into_iter()
        .rfind(|f| f.app_id == ws)
        .unwrap()
        .layout
}

/// A recognizably different layout: the newest file's, retitled.
fn retitled(ws: &str, title: &str) -> UiNode {
    let mut node = newest_file(ws);
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

/// How many layout files an app has (its latest version on a fresh pod).
fn files(app: &str) -> i64 {
    server::layout_files()
        .iter()
        .filter(|f| f.app_id == app)
        .count() as i64
}

#[tokio::test]
async fn apps_lists_dev_apps_only_when_shown() {
    let pod = pod().await;
    let (status, _, body) = get(&pod, "/v1/apps").await;
    assert_eq!(status, StatusCode::OK);
    let apps = body["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 2, "{body}");
    let a = &apps[0];
    assert_eq!(
        (&a["id"], &a["title"], &a["icon"], &a["latest_version"]),
        (
            &json!("pixelfed_home"),
            &json!("Pixelfed"),
            &json!("photo"),
            &json!(files("pixelfed_home"))
        )
    );
    assert!(is_rfc3339(&a["updated_at"]), "{a}");
    assert_eq!(
        (&apps[1]["id"], &apps[1]["title"], &apps[1]["icon"]),
        (&json!("recipes"), &json!("Recipes"), &json!("recipe"))
    );

    let dev = spawn(server::state_in_memory().with_dev_apps(true)).await;
    deploy(&dev, "pixelfed_demo", "Two");
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
            ("pixelfed_home", "Pixelfed", "photo", files("pixelfed_home")),
            ("recipes", "Recipes", "recipe", files("recipes")),
            (
                "pixelfed_demo",
                "Pixelfed demo",
                "photo",
                files("pixelfed_demo") + 1
            ),
        ]
    );
    // Hidden apps' endpoints work without the flag.
    let (status, version, _) = get_as(&pod, "/v1/workspaces/pixelfed_demo", None).await;
    assert_eq!(status, StatusCode::OK);
    // No schema header: the newest version a schema-1 client draws.
    assert_eq!(version.as_deref(), Some("1"));
    let (status, _, body) = get(&pod, "/v1/apps/pixelfed_demo/versions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["latest_version"], files("pixelfed_demo"));
}

#[tokio::test]
async fn versions_page_newest_first() {
    let pod = pod().await;
    for i in files("pixelfed_demo") + 1..=5 {
        assert_eq!(deploy(&pod, "pixelfed_demo", &format!("T{i}")), i);
    }
    let (status, _, body) = get(&pod, "/v1/apps/pixelfed_demo/versions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app_id"], "pixelfed_demo");
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
    assert_eq!(first["note"], "Classic feed");
    assert!(is_rfc3339(&first["created_at"]), "{first}");
    assert_eq!(versions[0]["source"], "manual");
    assert_eq!(versions[0]["note"], "T5");
    assert!(versions[0].get("layout").is_none(), "metadata only");

    // Paging.
    let (_, _, page) = get(&pod, "/v1/apps/pixelfed_demo/versions?limit=2").await;
    let numbers: Vec<i64> = page["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [5, 4]);
    assert_eq!(page["next_before"], 4);
    let (_, _, page) = get(&pod, "/v1/apps/pixelfed_demo/versions?limit=2&before=4").await;
    assert_eq!(page["versions"][0]["version"], 3);
    assert_eq!(page["next_before"], 2);
    let (_, _, page) = get(&pod, "/v1/apps/pixelfed_demo/versions?limit=2&before=2").await;
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
        let (status, _, body) = get(&pod, &format!("/v1/apps/pixelfed_demo/versions?{q}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body["error"], "invalid_request", "{q}");
    }
    let (status, _, _) = get(&pod, "/v1/apps/pixelfed_demo/versions?limit=100").await;
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
    assert_eq!(
        layout,
        layouts::builtin(&server::catalog(), "pixelfed_home").unwrap()
    );
    // f32 fields are not widened on the way out.
    assert!(!body["layout"].to_string().contains("0000001"), "{body}");
    let deployed = files("pixelfed_home") + 1;
    let (_, _, two) = get(&pod, &format!("/v1/apps/pixelfed_home/versions/{deployed}")).await;
    assert_eq!(two["layout"]["title"], "Two");
    // Public: no session needed even for pixelfed_home.
    let missing = deployed + 1;
    let (status, _, body) = get(&pod, &format!("/v1/apps/pixelfed_home/versions/{missing}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({ "error": "version_not_found", "app_id": "pixelfed_home", "version": missing })
    );
    for bad in ["0", "-1", "x", "1.5"] {
        let (status, _, body) = get(&pod, &format!("/v1/apps/pixelfed_demo/versions/{bad}")).await;
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
    for ws in ["pixelfed_demo"] {
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
        let file = layouts::parse_files(&server::catalog(), &server::layout_files())
            .unwrap()
            .into_iter()
            .find(|f| f.app_id == ws && f.number == 1)
            .unwrap();
        assert_eq!(spec.workspace, file.layout);
    }
    let second = deploy(&pod, "pixelfed_demo", "Second").to_string();
    let (status, version, body) = get(&pod, "/v1/workspaces/pixelfed_demo").await;
    assert_eq!(
        (status, version.as_deref()),
        (StatusCode::OK, Some(second.as_str()))
    );
    assert_eq!(body["workspace"]["title"], "Second");
    assert!(!body["entities"].as_object().unwrap().is_empty(), "{body}");
}

#[tokio::test]
async fn client_reachable_layout_writes_and_old_catalog_routes_are_gone() {
    let pod = spawn(server::state_in_memory().with_test_hooks(true)).await;
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
        "/v1/apps/pixelfed_demo/versions",
        "/v1/apps/pixelfed_demo/versions/1",
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
    let (_, version, _) = get(&pod, "/v1/workspaces/pixelfed_demo").await;
    assert_eq!(version, Some(files("pixelfed_demo").to_string()));
}

#[tokio::test]
async fn restart_on_the_same_database_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let state = server::open_dir(dir.path()).unwrap();
        assert_eq!(
            layouts::latest(state.store(), state.catalog(), "pixelfed_home")
                .unwrap()
                .unwrap()
                .0,
            files("pixelfed_home")
        );
        layouts::append_version(
            state.store(),
            "pixelfed_demo",
            &retitled("pixelfed_demo", "Two"),
            None,
        )
        .unwrap();
    }
    let state = server::open_dir(dir.path()).unwrap();
    for (ws, want) in [
        ("pixelfed_home", files("pixelfed_home")),
        ("pixelfed_demo", files("pixelfed_demo") + 1),
    ] {
        let (v, _) = layouts::latest(state.store(), state.catalog(), ws)
            .unwrap()
            .unwrap();
        assert_eq!(v, want, "{ws}");
    }
    assert!(
        layouts::latest(state.store(), state.catalog(), "nope")
            .unwrap()
            .is_none()
    );
}

/// A legacy (migrated) AI-edited version still holding the removed
/// `action.agent.prompt` intent serves as `action.unknown`, from both the
/// versions API and the workspace GET.
#[tokio::test]
async fn agent_era_versions_serve_with_unknown_intents() {
    let pod = pod().await;
    let layout = serde_json::to_string(&newest_file("pixelfed_demo")).unwrap();
    assert!(layout.contains("\"action.mutation.optimistic\""));
    let legacy = layout.replacen(
        "\"action.mutation.optimistic\"",
        "\"action.agent.prompt\",\"prompt\":\"make it bigger\"",
        1,
    );
    let v = pod
        .state
        .store()
        .append_app_version("pixelfed_demo", &legacy, "agent", Some("make it bigger"))
        .unwrap();
    assert_eq!(v, files("pixelfed_demo") + 1);
    let (status, _, body) = get(&pod, &format!("/v1/apps/pixelfed_demo/versions/{v}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body["layout"].to_string();
    assert!(text.contains("\"intent\":\"action.unknown\""), "{text}");
    assert!(!text.contains("action.agent.prompt"), "{text}");
    assert_eq!(body["source"], "agent");
    let (status, version, body) = get(&pod, "/v1/workspaces/pixelfed_demo").await;
    assert_eq!(
        (status, version.as_deref()),
        (StatusCode::OK, Some(v.to_string().as_str()))
    );
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
    let odd = store
        .append_app_version("pixelfed_demo", "{\"edited\":true}", "agent", Some("odd"))
        .unwrap();
    let (status, _, body) = get(&pod, &format!("/v1/apps/pixelfed_demo/versions/{odd}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["layout"], json!({ "edited": true }));
    assert_eq!(body["note"], "odd");
    // Not even JSON: a clear 500.
    let bad = store
        .append_app_version("pixelfed_demo", "not json", "agent", None)
        .unwrap();
    let (status, _, body) = get(&pod, &format!("/v1/apps/pixelfed_demo/versions/{bad}")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "corrupt_version", "{body}");
    assert_eq!(body["version"], bad);
}

/// A database where `pixelfed_demo/0001` was recorded with other content.
fn db_with_a_changed_file(dir: &std::path::Path) {
    let store = server::store::Store::open(&dir.join("server.db")).unwrap();
    let file = server::store::FileVersion {
        key: "pixelfed_demo/0001".into(),
        number: 1,
        path: "layouts/pixelfed_demo/0001.json".into(),
        note: "Classic feed".into(),
        json: "{}".into(),
        file_hash: "0".repeat(64),
        schema: 1,
    };
    store
        .sync_layout_files(&[("pixelfed_demo", vec![file])])
        .unwrap()
        .unwrap();
}

const CHANGED: &str = "layouts/pixelfed_demo/0001.json changed after it was deployed as version 1";

#[test]
fn a_sync_error_stops_the_start() {
    let dir = tempfile::tempdir().unwrap();
    db_with_a_changed_file(dir.path());
    let err = server::open_dir(dir.path()).err().unwrap().to_string();
    assert!(err.contains(CHANGED), "{err}");
    assert!(err.contains("To recover"), "{err}");
    // Nothing was appended for any app.
    let store = server::store::Store::open(&dir.path().join("server.db")).unwrap();
    assert!(store.app_latest_meta("pixelfed_home").unwrap().is_none());
}

/// D46: `server pods adopt` checks the database (migrations and layout sync)
/// before it becomes a pod; a sync error stops it with the error and leaves
/// the original file where it was.
#[test]
fn adopting_a_database_with_a_sync_error_fails_and_changes_nothing() {
    use std::process::Command;
    let old = tempfile::tempdir().unwrap();
    db_with_a_changed_file(old.path());
    let data = tempfile::tempdir().unwrap();
    let src = old.path().join("server.db");
    let out = Command::new(env!("CARGO_BIN_EXE_server"))
        .args(["pods", "adopt", "me", src.to_str().unwrap()])
        .env("CONSTRUCT_DATA_DIR", data.path())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains(CHANGED), "{text}");
    assert!(src.is_file(), "the original stays");
    let list = Command::new(env!("CARGO_BIN_EXE_server"))
        .args(["pods", "list"])
        .env("CONSTRUCT_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    assert!(list.stdout.is_empty(), "no pod was recorded");
    assert_eq!(
        std::fs::read_dir(data.path().join("pods")).unwrap().count(),
        0,
        "no pod files were left"
    );
}

// ---------- SSE ----------

/// Reads SSE messages (blocks separated by a blank line) from a streaming response.
struct SseReader {
    resp: reqwest::Response,
    buf: String,
}

impl SseReader {
    async fn connect(url: &str, bearer: Option<&str>) -> Self {
        Self::connect_as(url, bearer, None).await
    }

    /// With `schema`, the request carries `X-Construct-Schema` like an engine.
    async fn connect_as(url: &str, bearer: Option<&str>, schema: Option<u32>) -> Self {
        let mut req = client().get(url);
        if let Some(s) = schema {
            req = req.header(protocol::SCHEMA_HEADER, s.to_string());
        }
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
    let two = deploy(&pod, "pixelfed_demo", "Two");
    let demo_url = format!("{}/v1/workspaces/pixelfed_demo/events", pod.url);
    let mut demo = SseReader::connect(&demo_url, None).await;
    assert_eq!(demo.next_event().await, layout_msg("pixelfed_demo", two));
    let mut feed = SseReader::connect(
        &format!("{}/v1/workspaces/pixelfed_home/events", pod.url),
        None,
    )
    .await;
    let feed_n = files("pixelfed_home");
    assert_eq!(feed.next_event().await, layout_msg("pixelfed_home", feed_n));
    // Each stream only gets its own workspace's versions.
    assert_eq!(deploy(&pod, "pixelfed_home", "X"), feed_n + 1);
    assert_eq!(deploy(&pod, "pixelfed_demo", "Three"), two + 1);
    assert_eq!(
        demo.next_event().await,
        layout_msg("pixelfed_demo", two + 1)
    );
    assert_eq!(
        feed.next_event().await,
        layout_msg("pixelfed_home", feed_n + 1)
    );
    // A reconnecting client catches up with the current version.
    let mut again = SseReader::connect(&demo_url, None).await;
    assert_eq!(
        again.next_event().await,
        layout_msg("pixelfed_demo", two + 1)
    );
}

/// A subscriber that sends its schema hears only of versions it can draw: a
/// version it can't draw (newer or retired) causes no event, so no refetch.
#[tokio::test]
async fn sse_announces_the_newest_version_the_client_can_draw() {
    let pod = pod().await;
    let app = "pixelfed_home";
    let url = format!("{}/v1/workspaces/{app}/events", pod.url);
    // Files: 0001 schema 1, 0002 schema 2, 0003 schema 3 (which retires the
    // older ones for schema-3 clients), 0004 schema 8 (D48), 0005 schema 9 (photo posting, D51),
    // 0006 schema 10 (comment threads, list focus).
    let mut s1 = SseReader::connect_as(&url, None, Some(1)).await;
    let mut s2 = SseReader::connect_as(&url, None, Some(2)).await;
    let mut s7 = SseReader::connect_as(&url, None, Some(7)).await;
    let mut s8 = SseReader::connect_as(&url, None, Some(8)).await;
    let mut s9old = SseReader::connect_as(&url, None, Some(9)).await;
    let mut s9 = SseReader::connect_as(&url, None, Some(protocol::SCHEMA_VERSION)).await;
    let mut legacy = SseReader::connect(&url, None).await;
    assert_eq!(s1.next_event().await, layout_msg(app, 1));
    assert_eq!(s2.next_event().await, layout_msg(app, 2));
    assert_eq!(s7.next_event().await, layout_msg(app, 3));
    assert_eq!(s8.next_event().await, layout_msg(app, 4));
    assert_eq!(s9old.next_event().await, layout_msg(app, 5));
    assert_eq!(s9.next_event().await, layout_msg(app, 6));
    // No header: the app's latest, as before (engines from before this
    // change send no header on the events stream).
    assert_eq!(legacy.next_event().await, layout_msg(app, 6));

    // Publish a copy of the version a schema-`n` client is served.
    let publish = |schema: u32| {
        let mut layout = layouts::active_for(pod.state.store(), pod.state.catalog(), app, schema)
            .unwrap()
            .unwrap()
            .layout;
        if let UiNode::Workspace { title, .. } = &mut layout {
            title.push_str(" (again)");
        }
        let v = layouts::append_version(pod.state.store(), app, &layout, None).unwrap();
        pod.state.publish_layout(app, v);
        v
    };
    assert_eq!(publish(protocol::SCHEMA_VERSION), 7);
    assert_eq!(s9.next_event().await, layout_msg(app, 7));
    assert_eq!(legacy.next_event().await, layout_msg(app, 7));
    // A copy of 0001 (recorded as schema 1: it holds node types this build
    // doesn't know): schema 1 and 2 clients, not schema 3 (retired).
    assert_eq!(publish(1), 8);
    assert_eq!(s1.next_event().await, layout_msg(app, 8));
    assert_eq!(s2.next_event().await, layout_msg(app, 8));
    assert_eq!(legacy.next_event().await, layout_msg(app, 8));
    // The current client's next event is the next version it can draw.
    assert_eq!(publish(protocol::SCHEMA_VERSION), 9);
    assert_eq!(s9.next_event().await, layout_msg(app, 9));
}

#[tokio::test]
async fn sse_keepalive_comment() {
    let pod = spawn(server::state_in_memory().with_sse_keepalive(Duration::from_millis(100))).await;
    let url = format!("{}/v1/workspaces/pixelfed_demo/events", pod.url);
    let mut sse = SseReader::connect(&url, None).await;
    assert_eq!(
        sse.next().await,
        layout_msg("pixelfed_demo", files("pixelfed_demo"))
    );
    assert_eq!(sse.next().await, ": keepalive");
    assert_eq!(sse.next().await, ": keepalive");
}

#[tokio::test]
async fn sse_auth_matches_the_workspace_get() {
    let pod = pod().await;
    let store = pod.state.store();
    store.create_session(&session_hash("tok")).unwrap();
    store
        .attach_sign_in(&session_hash("tok"), server::pixelfed::SIGN_IN)
        .unwrap();
    // A valid session without the app's sign-in (D40).
    store.create_session(&session_hash("other")).unwrap();
    store
        .attach_sign_in(&session_hash("other"), "other")
        .unwrap();
    let url = format!("{}/v1/workspaces/pixelfed_home/events", pod.url);
    // No bearer: allowed (events carry only version numbers).
    let mut anon = SseReader::connect(&url, None).await;
    assert_eq!(
        anon.next_event().await,
        layout_msg("pixelfed_home", files("pixelfed_home"))
    );
    // Valid session: allowed.
    let mut signed_in = SseReader::connect(&url, Some("tok")).await;
    assert_eq!(
        signed_in.next_event().await,
        layout_msg("pixelfed_home", files("pixelfed_home"))
    );
    // A valid session without the `pixelfed` sign-in keeps its session
    // (D46), like GET /v1/workspaces/pixelfed_home: allowed.
    let mut other = SseReader::connect(&url, Some("other")).await;
    assert_eq!(
        other.next_event().await,
        layout_msg("pixelfed_home", files("pixelfed_home"))
    );
    // A stale bearer: 401, like the workspace GET.
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
    let url = format!("{}/v1/workspaces/pixelfed_demo/events", pod.url);
    let mut sse = SseReader::connect(&url, None).await;
    assert_eq!(
        sse.next_event().await,
        layout_msg("pixelfed_demo", files("pixelfed_demo"))
    );
    pod.state.shutdown();
    let end = tokio::time::timeout(Duration::from_secs(5), sse.resp.chunk())
        .await
        .expect("stream did not end after shutdown");
    assert!(matches!(end, Ok(None)), "{end:?}");
}

/// Each version says which schema it was written for and whether the asking
/// client (its `X-Construct-Schema`) can draw it.
#[tokio::test]
async fn versions_say_whether_the_client_can_draw_them() {
    let pod = pod().await;
    let page = |schema: Option<u32>| {
        let mut req = client().get(format!("{}/v1/apps/pixelfed_home/versions", pod.url));
        if let Some(s) = schema {
            req = req.header(protocol::SCHEMA_HEADER, s.to_string());
        }
        async move { req.send().await.unwrap().json::<Value>().await.unwrap() }
    };
    let rows = |body: &Value| -> Vec<(i64, i64, Option<String>)> {
        body["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                assert_eq!(v["compatible"], v.get("incompatible").is_none(), "{v}");
                (
                    v["version"].as_i64().unwrap(),
                    v["schema"].as_i64().unwrap(),
                    v.get("incompatible")
                        .map(|r| r.as_str().unwrap().to_owned()),
                )
            })
            .collect()
    };
    // Schema 3 removed the pixelfed:// widgets (D37): current clients draw
    // 0003 to 0006.
    let retired = Some("retired".to_owned());
    let update = Some("update_required".to_owned());
    assert_eq!(
        rows(&page(Some(protocol::SCHEMA_VERSION)).await),
        [
            (6, 10, None),
            (5, 9, None),
            (4, 8, None),
            (3, 3, None),
            (2, 2, retired.clone()),
            (1, 1, retired.clone())
        ]
    );
    // A schema 9 client draws up to 0005, not 0006 (list focus).
    assert_eq!(rows(&page(Some(9)).await)[0], (6, 10, update.clone()));
    assert_eq!(rows(&page(Some(9)).await)[1], (5, 9, None));
    // A schema 8 client draws up to 0004, not 0005 (photo posting, D51).
    assert_eq!(rows(&page(Some(8)).await)[1], (5, 9, update.clone()));
    // A schema 7 client draws 0003, not 0004 (the cards view, D48).
    assert_eq!(
        rows(&page(Some(7)).await),
        [
            (6, 10, update.clone()),
            (5, 9, update.clone()),
            (4, 8, update.clone()),
            (3, 3, None),
            (2, 2, retired.clone()),
            (1, 1, retired)
        ]
    );
    // A schema 2 client draws 0001 and 0002, not 0003.
    assert_eq!(
        rows(&page(Some(2)).await),
        [
            (6, 10, update.clone()),
            (5, 9, update.clone()),
            (4, 8, update.clone()),
            (3, 3, update.clone()),
            (2, 2, None),
            (1, 1, None)
        ]
    );
    // A client from before screens (no header) cannot draw 0002.
    let body = page(None).await;
    assert_eq!(
        rows(&body),
        [
            (6, 10, update.clone()),
            (5, 9, update.clone()),
            (4, 8, update.clone()),
            (3, 3, update.clone()),
            (2, 2, update),
            (1, 1, None)
        ]
    );
    // `latest_compatible_version`: what opening the app serves each client.
    for (schema, want) in [
        (Some(protocol::SCHEMA_VERSION), 6),
        (Some(9), 5),
        (Some(8), 4),
        (Some(7), 3),
        (Some(2), 2),
        (None, 1),
    ] {
        let body = page(schema).await;
        assert_eq!(
            (&body["latest_version"], &body["latest_compatible_version"]),
            (&json!(6), &json!(want)),
            "{schema:?}"
        );
    }
    let one = client()
        .get(format!("{}/v1/apps/pixelfed_home/versions/2", pod.url))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        (&one["schema"], &one["incompatible"]),
        (&json!(2), &json!("update_required"))
    );
    assert!(one["layout"].is_object(), "still served to look at");
}

/// `GET /v1/apps` says which version opening each app serves this client, so
/// the menu never offers a version the client can't draw as the latest.
#[tokio::test]
async fn apps_say_the_newest_version_the_client_can_draw() {
    let pod = pod().await;
    let apps = |schema: Option<u32>| {
        let mut req = client().get(format!("{}/v1/apps", pod.url));
        if let Some(s) = schema {
            req = req.header(protocol::SCHEMA_HEADER, s.to_string());
        }
        async move { req.send().await.unwrap().json::<Value>().await.unwrap() }
    };
    for (schema, want) in [
        (Some(protocol::SCHEMA_VERSION), 6),
        (Some(9), 5),
        (Some(8), 4),
        (Some(7), 3),
        (Some(2), 2),
        (None, 1),
    ] {
        let body = apps(schema).await;
        let a = &body["apps"][0];
        assert_eq!(
            (&a["latest_version"], &a["latest_compatible_version"]),
            (&json!(6), &json!(want)),
            "{schema:?}"
        );
    }
}
