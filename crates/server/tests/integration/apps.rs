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
/// (it sends this build's features).
async fn get(pod: &Pod, path: &str) -> (StatusCode, Option<String>, Value) {
    get_as(pod, path, Some(&crate::workspace_features())).await
}

/// The features header of a build from before `title_scrolls` (decision
/// `pinned-app-title`): it can't draw the newest feed layout.
fn before_title_scrolls() -> String {
    protocol::FEATURES
        .iter()
        .filter(|f| **f != protocol::feature::TITLE_SCROLLS && **f != protocol::feature::PAGE)
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}

/// A `pixelfed` layout from JSON (`slots` as given).
fn feed_layout(title: &str, slots: Value) -> UiNode {
    serde_json::from_value(json!({
        "type": "core://Workspace", "id": "pixelfed", "title": title, "slots": slots,
    }))
    .unwrap()
}

/// Give `pixelfed` a history whose versions differ in what clients can
/// draw (a fresh pod has only the newest file, decision
/// `retired-layout-files`): 1 the newest file (feature `title_scrolls`),
/// 2 a plain workspace any client draws, 3 a removed node type (retired for
/// every client, D37), 4 the newest file again.
fn feed_history(pod: &Pod) {
    let store = pod.state.store();
    // Only the newest file, as version 1 (the build may hold older files
    // with the same layout, e.g. 0011 before 0012 wrote it with components).
    store
        .with_conn(|c| {
            c.execute_batch(
                "DELETE FROM app_versions WHERE app_id = 'pixelfed'
                     AND version < (SELECT MAX(version) FROM app_versions WHERE app_id = 'pixelfed');
                 UPDATE app_versions SET version = 1 WHERE app_id = 'pixelfed';",
            )
        })
        .unwrap();
    let plain = feed_layout("Plain", json!([]));
    let legacy = feed_layout(
        "Legacy",
        json!([{"slot_id": "main", "provider_uri": "core://x",
                "content": {"type": "pixelfed://FeedList"}}]),
    );
    assert_eq!(
        layouts::append_version(store, "pixelfed", &plain, Some("Plain")).unwrap(),
        2
    );
    assert_eq!(
        layouts::append_version(store, "pixelfed", &legacy, Some("Legacy")).unwrap(),
        3
    );
    assert_eq!(deploy(pod, "pixelfed", "Four"), 4);
}

/// [`get`] as a client sending `features` (`None`: no header, a client
/// that draws nothing this pod serves).
async fn get_as(
    pod: &Pod,
    path: &str,
    features: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut req = client().get(format!("{}{path}", pod.url));
    if let Some(f) = features {
        req = req.header(protocol::FEATURES_HEADER, f);
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

/// The note of an app's newest file (version 1 on a fresh pod).
/// The note of an app's oldest file in the build: its version 1 on a fresh
/// pod.
fn first_note(ws: &str) -> String {
    layouts::parse_files(&server::catalog(), &server::layout_files())
        .unwrap()
        .into_iter()
        .find(|f| f.app_id == ws)
        .unwrap()
        .note
}

fn newest_note(ws: &str) -> String {
    layouts::parse_files(&server::catalog(), &server::layout_files())
        .unwrap()
        .into_iter()
        .rfind(|f| f.app_id == ws)
        .unwrap()
        .note
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
    let (status, _, body) = get(&pod, "/v1/apps?blueprints=1").await;
    assert_eq!(status, StatusCode::OK);
    let apps = body["apps"].as_array().unwrap();
    // The blueprints (Settings and the inbox among them, decision
    // `layouts-not-apps`), then the pod's own home (decision `blueprints-and-home`).
    assert_eq!(apps.len(), 7, "{body}");
    assert_eq!(apps[3]["id"], "shell");
    assert_eq!(apps[4]["id"], "settings");
    assert_eq!(apps[5]["id"], "inbox");
    assert_eq!(apps[6]["id"], "home");
    let a = &apps[0];
    assert_eq!(
        (&a["id"], &a["title"], &a["icon"], &a["latest_version"]),
        (
            &json!("pixelfed"),
            &json!("Pixelfed"),
            &json!("photo"),
            &json!(files("pixelfed"))
        )
    );
    assert!(is_rfc3339(&a["updated_at"]), "{a}");
    assert_eq!(
        (&apps[1]["id"], &apps[1]["title"], &apps[1]["icon"]),
        (&json!("recipes"), &json!("Recipes"), &json!("recipe"))
    );
    assert_eq!(
        (&apps[2]["id"], &apps[2]["title"], &apps[2]["icon"]),
        (&json!("events"), &json!("Events"), &json!("dashboard"))
    );

    let dev = spawn(server::state_in_memory().with_dev_apps(true)).await;
    deploy(&dev, "pixelfed_sample_no_login", "Two");
    let (_, _, body) = get(&dev, "/v1/apps?blueprints=1").await;
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
            ("pixelfed", "Pixelfed", "photo", files("pixelfed")),
            ("recipes", "Recipes", "recipe", files("recipes")),
            ("events", "Events", "dashboard", files("events")),
            (
                "pixelfed_sample_no_login",
                "Pixelfed sample",
                "photo",
                files("pixelfed_sample_no_login") + 1
            ),
            ("shell", "Home", "dashboard", files("shell")),
            ("settings", "Settings", "dashboard", files("settings")),
            ("inbox", "Notifications", "dashboard", files("inbox")),
            ("home", "Home", "dashboard", 1),
        ]
    );
    // Hidden apps' endpoints work without the flag.
    let (status, version, _) = get(&pod, "/v1/workspaces/pixelfed_sample_no_login").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        version.as_deref(),
        Some(files("pixelfed_sample_no_login").to_string().as_str())
    );
    let (status, _, body) = get(&pod, "/v1/apps/pixelfed_sample_no_login/versions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["latest_version"], files("pixelfed_sample_no_login"));
}

#[tokio::test]
async fn versions_page_newest_first() {
    let pod = pod().await;
    for i in files("pixelfed_sample_no_login") + 1..=5 {
        assert_eq!(
            deploy(&pod, "pixelfed_sample_no_login", &format!("T{i}")),
            i
        );
    }
    let (status, _, body) = get(&pod, "/v1/apps/pixelfed_sample_no_login/versions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app_id"], "pixelfed_sample_no_login");
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
    assert_eq!(first["note"], first_note("pixelfed_sample_no_login"));
    assert!(is_rfc3339(&first["created_at"]), "{first}");
    assert_eq!(versions[0]["source"], "manual");
    assert_eq!(versions[0]["note"], "T5");
    assert!(versions[0].get("layout").is_none(), "metadata only");

    // Paging.
    let (_, _, page) = get(&pod, "/v1/apps/pixelfed_sample_no_login/versions?limit=2").await;
    let numbers: Vec<i64> = page["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [5, 4]);
    assert_eq!(page["next_before"], 4);
    let (_, _, page) = get(
        &pod,
        "/v1/apps/pixelfed_sample_no_login/versions?limit=2&before=4",
    )
    .await;
    assert_eq!(page["versions"][0]["version"], 3);
    assert_eq!(page["next_before"], 2);
    let (_, _, page) = get(
        &pod,
        "/v1/apps/pixelfed_sample_no_login/versions?limit=2&before=2",
    )
    .await;
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
        let (status, _, body) = get(
            &pod,
            &format!("/v1/apps/pixelfed_sample_no_login/versions?{q}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body["error"], "invalid_request", "{q}");
    }
    let (status, _, _) = get(&pod, "/v1/apps/pixelfed_sample_no_login/versions?limit=100").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn one_version_carries_its_layout() {
    let pod = pod().await;
    deploy(&pod, "pixelfed", "Two");
    let (status, _, body) = get(&pod, "/v1/apps/pixelfed/versions/1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["app_id"], "pixelfed");
    assert_eq!(body["version"], 1);
    assert_eq!(body["source"], "file");
    assert_eq!(body["note"], first_note("pixelfed"));
    assert!(is_rfc3339(&body["created_at"]));
    let layout: UiNode = serde_json::from_value(body["layout"].clone()).unwrap();
    assert_eq!(
        layout,
        layouts::builtin(&server::catalog(), "pixelfed").unwrap()
    );
    // f32 fields are not widened on the way out.
    assert!(!body["layout"].to_string().contains("0000001"), "{body}");
    let deployed = files("pixelfed") + 1;
    let (_, _, two) = get(&pod, &format!("/v1/apps/pixelfed/versions/{deployed}")).await;
    assert_eq!(two["layout"]["title"], "Two");
    // Public: no session needed even for pixelfed.
    let missing = deployed + 1;
    let (status, _, body) = get(&pod, &format!("/v1/apps/pixelfed/versions/{missing}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({ "error": "version_not_found", "app_id": "pixelfed", "version": missing })
    );
    for bad in ["0", "-1", "x", "1.5"] {
        let (status, _, body) = get(
            &pod,
            &format!("/v1/apps/pixelfed_sample_no_login/versions/{bad}"),
        )
        .await;
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
    for ws in ["pixelfed_sample_no_login"] {
        let resp = client()
            .get(format!("{}/v1/workspaces/{ws}", pod.url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // No features header (a tool, not a client): the latest version.
        let latest = files(ws).to_string();
        assert_eq!(resp.headers()[layouts::VERSION_HEADER], latest.as_str());
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
            .rfind(|f| f.app_id == ws)
            .unwrap();
        assert_eq!(spec.workspace, file.layout);
    }
    let second = deploy(&pod, "pixelfed_sample_no_login", "Second").to_string();
    let (status, version, body) = get(&pod, "/v1/workspaces/pixelfed_sample_no_login").await;
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
        "/v1/workspaces/pixelfed/rollback",
        "/v1/layouts/home.counter/activate",
        "/v1/layouts/home.counter/restore",
        "/v1/apps",
        "/v1/apps/pixelfed_sample_no_login/versions",
        "/v1/apps/pixelfed_sample_no_login/versions/1",
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
        "/v1/workspaces/pixelfed/versions",
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
    let (_, version, _) = get(&pod, "/v1/workspaces/pixelfed_sample_no_login").await;
    assert_eq!(version, Some(files("pixelfed_sample_no_login").to_string()));
}

#[tokio::test]
async fn restart_on_the_same_database_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let state = server::open_dir(dir.path()).unwrap();
        assert_eq!(
            layouts::latest(state.store(), state.catalog(), "pixelfed")
                .unwrap()
                .unwrap()
                .0,
            files("pixelfed")
        );
        layouts::append_version(
            state.store(),
            "pixelfed_sample_no_login",
            &retitled("pixelfed_sample_no_login", "Two"),
            None,
        )
        .unwrap();
    }
    let state = server::open_dir(dir.path()).unwrap();
    for (ws, want) in [
        ("pixelfed", files("pixelfed")),
        (
            "pixelfed_sample_no_login",
            files("pixelfed_sample_no_login") + 1,
        ),
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
    let layout = serde_json::to_string(&newest_file("pixelfed_sample_no_login")).unwrap();
    assert!(layout.contains("\"action.mutation.optimistic\""));
    let legacy = layout.replacen(
        "\"action.mutation.optimistic\"",
        "\"action.agent.prompt\",\"prompt\":\"make it bigger\"",
        1,
    );
    let v = pod
        .state
        .store()
        .append_app_version(
            "pixelfed_sample_no_login",
            &legacy,
            "agent",
            Some("make it bigger"),
        )
        .unwrap();
    assert_eq!(v, files("pixelfed_sample_no_login") + 1);
    let (status, _, body) = get(
        &pod,
        &format!("/v1/apps/pixelfed_sample_no_login/versions/{v}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body["layout"].to_string();
    assert!(text.contains("\"intent\":\"action.unknown\""), "{text}");
    assert!(!text.contains("action.agent.prompt"), "{text}");
    assert_eq!(body["source"], "agent");
    let (status, version, body) = get(&pod, "/v1/workspaces/pixelfed_sample_no_login").await;
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
        .append_app_version(
            "pixelfed_sample_no_login",
            "{\"edited\":true}",
            "agent",
            Some("odd"),
        )
        .unwrap();
    let (status, _, body) = get(
        &pod,
        &format!("/v1/apps/pixelfed_sample_no_login/versions/{odd}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["layout"], json!({ "edited": true }));
    assert_eq!(body["note"], "odd");
    // Not even JSON: a clear 500.
    let bad = store
        .append_app_version("pixelfed_sample_no_login", "not json", "agent", None)
        .unwrap();
    let (status, _, body) = get(
        &pod,
        &format!("/v1/apps/pixelfed_sample_no_login/versions/{bad}"),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "corrupt_version", "{body}");
    assert_eq!(body["version"], bad);
}

/// A database where `pixelfed_sample_no_login/0003` (its newest file) was recorded with
/// other content.
fn db_with_a_changed_file(dir: &std::path::Path) {
    let store = server::store::Store::open(&dir.join("server.db")).unwrap();
    let file = server::store::FileVersion {
        key: "pixelfed_sample_no_login/0003".into(),
        number: 3,
        path: "crates/app-pixelfed/layouts/pixelfed_sample_no_login/0003.json".into(),
        note: newest_note("pixelfed_sample_no_login"),
        json: "{}".into(),
        file_hash: "0".repeat(64),
        meta_json: None,
    };
    store
        .sync_layout_files(&[("pixelfed_sample_no_login", vec![file])])
        .unwrap()
        .unwrap();
}

const CHANGED: &str = "crates/app-pixelfed/layouts/pixelfed_sample_no_login/0003.json changed after it was deployed as version 1";

#[test]
fn a_sync_error_stops_the_start() {
    let dir = tempfile::tempdir().unwrap();
    db_with_a_changed_file(dir.path());
    let err = server::open_dir(dir.path()).err().unwrap().to_string();
    assert!(err.contains(CHANGED), "{err}");
    assert!(err.contains("To recover"), "{err}");
    // Nothing was appended for any app.
    let store = server::store::Store::open(&dir.path().join("server.db")).unwrap();
    assert!(store.app_latest_meta("pixelfed").unwrap().is_none());
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
    /// As this build.
    async fn connect(url: &str, bearer: Option<&str>) -> Self {
        Self::connect_as(url, bearer, Some(&crate::workspace_features())).await
    }

    /// With `features`, the request carries `X-Construct-Features` like an
    /// engine.
    async fn connect_as(url: &str, bearer: Option<&str>, features: Option<&str>) -> Self {
        let mut req = client().get(url);
        if let Some(f) = features {
            req = req.header(protocol::FEATURES_HEADER, f);
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
    let two = deploy(&pod, "pixelfed_sample_no_login", "Two");
    let demo_url = format!("{}/v1/workspaces/pixelfed_sample_no_login/events", pod.url);
    let mut demo = SseReader::connect(&demo_url, None).await;
    assert_eq!(
        demo.next_event().await,
        layout_msg("pixelfed_sample_no_login", two)
    );
    let mut feed =
        SseReader::connect(&format!("{}/v1/workspaces/pixelfed/events", pod.url), None).await;
    let feed_n = files("pixelfed");
    assert_eq!(feed.next_event().await, layout_msg("pixelfed", feed_n));
    // Each stream only gets its own workspace's versions.
    assert_eq!(deploy(&pod, "pixelfed", "X"), feed_n + 1);
    assert_eq!(deploy(&pod, "pixelfed_sample_no_login", "Three"), two + 1);
    assert_eq!(
        demo.next_event().await,
        layout_msg("pixelfed_sample_no_login", two + 1)
    );
    assert_eq!(feed.next_event().await, layout_msg("pixelfed", feed_n + 1));
    // A reconnecting client catches up with the current version.
    let mut again = SseReader::connect(&demo_url, None).await;
    assert_eq!(
        again.next_event().await,
        layout_msg("pixelfed_sample_no_login", two + 1)
    );
}

/// A subscriber hears only of versions it can draw: a version that uses a
/// feature it doesn't list causes no event, so no refetch.
#[tokio::test]
async fn sse_announces_the_newest_version_the_client_can_draw() {
    let pod = pod().await;
    feed_history(&pod);
    let app = "pixelfed";
    let url = format!("{}/v1/workspaces/{app}/events", pod.url);
    // Version 2 is the newest one a build without `title_scrolls` draws.
    let old = before_title_scrolls();
    let mut before = SseReader::connect_as(&url, None, Some(&old)).await;
    let mut current = SseReader::connect(&url, None).await;
    assert_eq!(before.next_event().await, layout_msg(app, 2));
    assert_eq!(current.next_event().await, layout_msg(app, 4));

    // Publish a copy of the version `client` is served.
    let publish = |client: protocol::ClientCaps| {
        let mut layout = layouts::active_for(pod.state.store(), pod.state.catalog(), app, &client)
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
    // A copy of version 2: the older client, and this build (it draws a
    // plain workspace too).
    assert_eq!(publish(protocol::ClientCaps::from_header(&old)), 5);
    assert_eq!(before.next_event().await, layout_msg(app, 5));
    assert_eq!(current.next_event().await, layout_msg(app, 5));
    // A version only this build draws: the older client hears nothing, so
    // its next event is the next plain version.
    let v = deploy(&pod, app, "Six");
    assert_eq!(v, 6);
    assert_eq!(current.next_event().await, layout_msg(app, 6));
    assert_eq!(publish(protocol::ClientCaps::from_header(&old)), 7);
    assert_eq!(before.next_event().await, layout_msg(app, 7));
}

#[tokio::test]
async fn sse_keepalive_comment() {
    let pod = spawn(server::state_in_memory().with_sse_keepalive(Duration::from_millis(100))).await;
    let url = format!("{}/v1/workspaces/pixelfed_sample_no_login/events", pod.url);
    let mut sse = SseReader::connect(&url, None).await;
    assert_eq!(
        sse.next().await,
        layout_msg(
            "pixelfed_sample_no_login",
            files("pixelfed_sample_no_login")
        )
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
    let url = format!("{}/v1/workspaces/pixelfed/events", pod.url);
    // No bearer: allowed (events carry only version numbers).
    let mut anon = SseReader::connect(&url, None).await;
    assert_eq!(
        anon.next_event().await,
        layout_msg("pixelfed", files("pixelfed"))
    );
    // Valid session: allowed.
    let mut signed_in = SseReader::connect(&url, Some("tok")).await;
    assert_eq!(
        signed_in.next_event().await,
        layout_msg("pixelfed", files("pixelfed"))
    );
    // A valid session without the `pixelfed` sign-in keeps its session
    // (D46), like GET /v1/workspaces/pixelfed: allowed.
    let mut other = SseReader::connect(&url, Some("other")).await;
    assert_eq!(
        other.next_event().await,
        layout_msg("pixelfed", files("pixelfed"))
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
    let url = format!("{}/v1/workspaces/pixelfed_sample_no_login/events", pod.url);
    let mut sse = SseReader::connect(&url, None).await;
    assert_eq!(
        sse.next_event().await,
        layout_msg(
            "pixelfed_sample_no_login",
            files("pixelfed_sample_no_login")
        )
    );
    pod.state.shutdown();
    let end = tokio::time::timeout(Duration::from_secs(5), sse.resp.chunk())
        .await
        .expect("stream did not end after shutdown");
    assert!(matches!(end, Ok(None)), "{end:?}");
}

/// Each version says which features it uses and whether the asking client
/// (its `X-Construct-Features`) can draw it.
#[tokio::test]
async fn versions_say_whether_the_client_can_draw_them() {
    let pod = pod().await;
    let page = |features: Option<String>| {
        let mut req = client().get(format!("{}/v1/apps/pixelfed/versions", pod.url));
        if let Some(f) = features {
            req = req.header(protocol::FEATURES_HEADER, f);
        }
        async move { req.send().await.unwrap().json::<Value>().await.unwrap() }
    };
    let rows = |body: &Value| -> Vec<(i64, Option<String>)> {
        body["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                assert_eq!(v["compatible"], v.get("incompatible").is_none(), "{v}");
                assert!(v.get("schema").is_none(), "{v}");
                (
                    v["version"].as_i64().unwrap(),
                    v.get("incompatible")
                        .map(|r| r.as_str().unwrap().to_owned()),
                )
            })
            .collect()
    };
    feed_history(&pod);
    let retired = Some("retired".to_owned());
    let update = Some("update_required".to_owned());
    // Version 3 holds a removed node type (D37): retired for every client.
    let current = page(Some(crate::workspace_features())).await;
    assert_eq!(
        rows(&current),
        [(4, None), (3, retired.clone()), (2, None), (1, None)]
    );
    assert_eq!(current["latest_compatible_version"], json!(4));
    let v4 = &current["versions"][0]["features"];
    assert!(
        v4.as_array().unwrap().contains(&json!("title_scrolls")),
        "{v4}"
    );
    // A build from before `title_scrolls`: only the plain version 2.
    let before = page(Some(before_title_scrolls())).await;
    assert_eq!(
        rows(&before),
        [
            (4, update.clone()),
            (3, retired.clone()),
            (2, None),
            (1, update)
        ]
    );
    assert_eq!(before["latest_compatible_version"], json!(2));
    // A client that lists nothing draws nothing; a caller that sends no
    // header at all (a tool) is shown everything.
    let nothing = page(Some(String::new())).await;
    assert!(rows(&nothing).iter().all(|r| r.1.is_some()), "{nothing}");
    assert_eq!(nothing["latest_compatible_version"], Value::Null);
    let tool = page(None).await;
    assert!(rows(&tool).iter().all(|r| r.1.is_none()), "{tool}");
    let one = client()
        .get(format!("{}/v1/apps/pixelfed/versions/3", pod.url))
        .header(protocol::FEATURES_HEADER, crate::workspace_features())
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(one["incompatible"], json!("retired"));
    assert!(one["layout"].is_object(), "still served to look at");
}

/// `GET /v1/apps` says which version opening each app serves this client, so
/// the menu never offers a version the client can't draw as the latest.
#[tokio::test]
async fn apps_say_the_newest_version_the_client_can_draw() {
    let pod = pod().await;
    feed_history(&pod);
    let apps = |features: Option<String>| {
        let mut req = client().get(format!("{}/v1/apps?blueprints=1", pod.url));
        if let Some(f) = features {
            req = req.header(protocol::FEATURES_HEADER, f);
        }
        async move { req.send().await.unwrap().json::<Value>().await.unwrap() }
    };
    for (features, want) in [
        (Some(crate::workspace_features()), json!(4)),
        (Some(before_title_scrolls()), json!(2)),
        (Some(String::new()), Value::Null),
        (None, json!(4)),
    ] {
        let body = apps(features.clone()).await;
        let a = &body["apps"][0];
        assert_eq!(
            (&a["latest_version"], &a["latest_compatible_version"]),
            (&json!(4), &want),
            "{features:?}"
        );
    }
}

/// Decision `named-vocabulary`: no built-in app's newest layout uses
/// vocabulary current clients no longer draw (it would be retired for them).
#[test]
fn newest_layouts_use_no_removed_vocabulary() {
    let mut apps: Vec<&str> = server::layout_files().iter().map(|f| f.app_id).collect();
    apps.sort_unstable();
    apps.dedup();
    assert!(apps.contains(&"pixelfed"), "{apps:?}");
    for app in apps {
        let removed: Vec<&str> = protocol::required_features(&newest_file(app))
            .into_iter()
            .filter(|f| !protocol::FEATURES.contains(f))
            .collect();
        assert!(removed.is_empty(), "{app}: {removed:?}");
    }
}

/// `pixelfed`'s Share is a sequence (feature `action_sequence`).
#[test]
fn share_is_a_create_then_back() {
    let layout = newest_file("pixelfed");
    let share = layout
        .walk()
        .into_iter()
        .flat_map(UiNode::own_intents)
        .find(|i| matches!(i, protocol::ActionIntent::Sequence { .. }))
        .expect("a sequence");
    let protocol::ActionIntent::Sequence { steps } = share else {
        unreachable!()
    };
    assert!(matches!(
        steps.as_slice(),
        [
            protocol::ActionIntent::Create { dismiss: false, .. },
            protocol::ActionIntent::NavigateBack
        ]
    ));
    assert!(
        protocol::required_features(&layout).contains(&protocol::feature::ACTION_SEQUENCE),
        "{:?}",
        protocol::required_features(&layout)
    );
}

/// Home's stream (decision `layouts-not-apps`) starts with every other
/// layout's version and every deleted layout, so an engine that was not
/// listening (in the background, offline, the server restarting) catches up
/// on what changed meanwhile.
#[tokio::test]
async fn home_stream_starts_with_every_layouts_version() {
    let pod = pod().await;
    pod.state
        .store()
        .create_pinned_session(&session_hash("pod-token"))
        .unwrap();
    let layout = |text: &str| {
        json!({"type": "core://Workspace", "id": "x", "title": "Note",
            "slots": [{"slot_id": "r", "provider_uri": "",
              "content": {"type": "core://Text", "style": "Body", "value_expr": text}}]})
    };
    let create = || async {
        let b: Value = client()
            .post(format!("{}/v1/custom-apps", pod.url))
            .bearer_auth("pod-token")
            .json(&json!({"title": "Note", "layout": layout("one")}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        b["id"].as_str().unwrap().to_owned()
    };
    let (kept, gone) = (create().await, create().await);
    // Changed and deleted while nobody listens.
    let resp = client()
        .put(format!("{}/v1/custom-apps/{kept}/layout", pod.url))
        .bearer_auth("pod-token")
        .json(&json!({"layout": layout("two")}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let resp = client()
        .delete(format!("{}/v1/custom-apps/{gone}", pod.url))
        .bearer_auth("pod-token")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let url = format!("{}/v1/workspaces/home/events", pod.url);
    let mut sse = SseReader::connect(&url, Some("pod-token")).await;
    let home = pod.state.store().app_latest_meta("home").unwrap().unwrap();
    assert_eq!(sse.next_event().await, layout_msg("home", home.version));
    let mut events = Vec::new();
    loop {
        let e = sse.next_event().await;
        let last = e.contains(&gone);
        events.push(e);
        if last {
            break;
        }
    }
    assert!(events.contains(&layout_msg(&kept, 2)), "{events:?}");
    assert!(
        events.contains(&layout_msg("inbox", files("inbox"))),
        "{events:?}"
    );
    assert_eq!(
        events.last().unwrap(),
        &format!("event: deleted\ndata: {}", json!({ "workspace_id": gone }))
    );
    assert!(!events.iter().any(|e| e.contains("\"home\"")), "{events:?}");
    // Then changes as they happen, once each.
    let resp = client()
        .put(format!("{}/v1/custom-apps/{kept}/layout", pod.url))
        .bearer_auth("pod-token")
        .json(&json!({"layout": layout("three")}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert_eq!(sse.next_event().await, layout_msg(&kept, 3));
}
