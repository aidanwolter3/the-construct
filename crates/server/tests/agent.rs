//! Layout agent (M7): `POST /v1/agent/modify_layout`, the rules provider, the
//! Anthropic provider against a mock Messages API, and the registry.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::HeaderMap;
use protocol::{UiNode, WorkspaceSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use server::AppState;
use server::agent::anthropic::{AnthropicConfig, AnthropicProvider, patch_schema};
use server::agent::{self, LayoutProvider, ProviderError, Selection};
use server::layouts;
use server::pixelfed::routes::session_hash;
use tokio::task::JoinHandle;

const DESIGN_PROMPT: &str = "Hide post captions and put media at the very top";

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

async fn prompt_raw(
    pod: &Pod,
    body: Value,
    bearer: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut req = reqwest::Client::new()
        .post(format!("{}/v1/agent/modify_layout", pod.url))
        .json(&body);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
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

async fn prompt(pod: &Pod, text: &str) -> (StatusCode, Value) {
    let (s, _, b) = prompt_raw(pod, json!({ "prompt": text }), None).await;
    (s, b)
}

async fn prompt_ws(pod: &Pod, ws: &str, text: &str) -> (StatusCode, Value) {
    let (s, _, b) = prompt_raw(pod, json!({ "prompt": text, "workspace_id": ws }), None).await;
    (s, b)
}

fn version_count(pod: &Pod, ws: &str) -> usize {
    pod.state.store().layout_versions(ws).unwrap().len()
}

fn latest(pod: &Pod, ws: &str) -> (i64, UiNode) {
    layouts::latest(pod.state.store(), ws).unwrap().unwrap()
}

/// The feed's PostCard template as JSON.
fn card(workspace: &Value) -> &Value {
    &workspace["slots"][0]["content"]["item_template"]
}

fn kinds(card: &Value) -> Vec<&str> {
    ["author_slot", "media_slot", "action_slot", "caption_slot"]
        .iter()
        .map(|s| card[*s]["type"].as_str().unwrap())
        .collect()
}

fn blank_caption() -> Value {
    json!({ "type": "core://Text", "value_expr": "", "style": "Caption" })
}

/// A 200 body must be a WorkspaceSpec; returns the workspace JSON.
fn spec_workspace(body: &Value) -> Value {
    let spec: WorkspaceSpec = serde_json::from_value(body.clone()).expect("WorkspaceSpec");
    assert!(spec.entities.is_empty());
    serde_json::to_value(spec.workspace).unwrap()
}

// ---------- rules provider through the endpoint ----------

#[tokio::test]
async fn design_prompt_end_to_end() {
    let pod = open_pod().await;
    // No Pixelfed session exists: the agent edits the signed-in feed layout,
    // which is versioned independently of the upstream.
    let (status, version, body) = prompt_raw(&pod, json!({ "prompt": DESIGN_PROMPT }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(version.as_deref(), Some("2"));
    assert_eq!(body["layout_version"], 2);
    assert_eq!(body["provider"], "rules");
    assert_eq!(body["model"], Value::Null);
    assert_eq!(body["version"], "1.0.0");
    let ws = spec_workspace(&body);
    assert_eq!(ws["id"], "pixelfed_home");
    let c = card(&ws);
    assert_eq!(c["caption_slot"], blank_caption());
    assert_eq!(
        kinds(c),
        [
            "pixelfed://MediaCarousel",
            "pixelfed://AuthorHeader",
            "pixelfed://ActionBar",
            "core://Text"
        ]
    );
    // One patch, replace ops with explicit values.
    let patch = body["patch"].as_array().unwrap();
    assert_eq!(patch.len(), 3, "{patch:?}");
    assert!(
        patch
            .iter()
            .all(|op| op["op"] == "replace" && op.get("value").is_some())
    );
    // Committed with prompt/provider/patch.
    let (v, node) = latest(&pod, "pixelfed_home");
    assert_eq!(v, 2);
    assert_eq!(serde_json::to_value(&node).unwrap(), ws);
    let rows = pod.state.store().layout_versions("pixelfed_home").unwrap();
    assert_eq!(rows[0].prompt.as_deref(), Some(DESIGN_PROMPT));
    assert_eq!(rows[0].provider.as_deref(), Some("rules"));
    // Repeating it changes nothing (idempotent rules): no new version.
    let (status, body) = prompt(&pod, DESIGN_PROMPT).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["unchanged"], true);
    assert_eq!(body["layout_version"], 2);
    assert_eq!(version_count(&pod, "pixelfed_home"), 2);
}

#[tokio::test]
async fn each_rule_alone() {
    let pod = open_pod().await;
    let (s, b) = prompt(&pod, "hide captions").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(card(&b["workspace"])["caption_slot"], blank_caption());
    assert_eq!(kinds(card(&b["workspace"]))[0], "pixelfed://AuthorHeader");

    let (s, b) = prompt(&pod, "Show captions").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        card(&b["workspace"])["caption_slot"],
        json!({ "type": "core://Text", "value_expr": "entity.caption", "style": "Body" })
    );
    assert_eq!(
        b["workspace"],
        serde_json::to_value(layouts::builtin("pixelfed_home").unwrap()).unwrap()
    );

    let (s, b) = prompt(&pod, "Photos first").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        kinds(card(&b["workspace"])),
        [
            "pixelfed://MediaCarousel",
            "pixelfed://AuthorHeader",
            "pixelfed://ActionBar",
            "core://Text"
        ]
    );
    assert_eq!(b["patch"].as_array().unwrap().len(), 2, "a swap");
    assert_eq!(b["layout_version"], 4);

    // Back to v1, then the action bar.
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/workspaces/pixelfed_home/rollback", pod.url))
        .json(&json!({ "version": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (s, b) = prompt(&pod, "Move the action bar above the photo").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        kinds(card(&b["workspace"])),
        [
            "pixelfed://AuthorHeader",
            "pixelfed://ActionBar",
            "pixelfed://MediaCarousel",
            "core://Text"
        ]
    );
    // Already above: idempotent.
    let (s, b) = prompt(&pod, "move the action bar above the photo").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["unchanged"], true);
}

#[tokio::test]
async fn rules_compose_in_prompt_order_on_the_current_layout() {
    let pod = open_pod().await;
    // Media first, then the action bar goes right above the (moved) photo.
    let (s, b) = prompt(&pod, "photos first and move the action bar above the photo").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        kinds(card(&b["workspace"])),
        [
            "pixelfed://ActionBar",
            "pixelfed://MediaCarousel",
            "pixelfed://AuthorHeader",
            "core://Text"
        ]
    );
    // Hide captions + weather in one patch.
    let (s, b) = prompt(
        &pod,
        "Add a weather widget at the top, and hide the captions",
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let ws = spec_workspace(&b);
    assert_eq!(ws["slots"][0]["slot_id"], "weather");
    assert_eq!(ws["slots"][1]["slot_id"], "feed");
    assert_eq!(
        ws["slots"][1]["content"]["item_template"]["caption_slot"],
        blank_caption()
    );
    assert_eq!(b["patch"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn weather_widget_is_added_once() {
    let pod = open_pod().await;
    let (s, b) = prompt(&pod, "add a weather widget").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        b["workspace"]["slots"][0],
        json!({ "slot_id": "weather", "provider_uri": "core://placeholder",
                "content": { "type": "core://Container", "orientation": "Vertical", "padding_dp": 16,
                    "children": [{ "type": "core://Text", "value_expr": "Weather widget (not available yet)", "style": "Caption" }] } })
    );
    // The padded Container passes validation (it was committed) and round-trips as a UiNode.
    spec_workspace(&b);
    assert_eq!(
        b["patch"],
        json!([{ "op": "add", "path": "/slots/0", "value": b["workspace"]["slots"][0] }])
    );
    let (s, b) = prompt(&pod, "Add a weather widget at the top").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["unchanged"], true);
    assert_eq!(b["patch"], json!([]));
    assert_eq!(b["workspace"]["slots"].as_array().unwrap().len(), 2);
    assert_eq!(version_count(&pod, "pixelfed_home"), 2);
    // Works on other versioned workspaces too.
    let (s, b) = prompt_ws(&pod, "home", "add a weather widget").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["workspace"]["id"], "home");
}

#[tokio::test]
async fn unsupported_prompts_are_422_and_commit_nothing() {
    let pod = open_pod().await;
    let (s, b) = prompt(&pod, "make it purple").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["error"], "unsupported_prompt");
    let supported = b["supported"].as_array().unwrap();
    assert!(supported.iter().any(|e| e == "Hide post captions"), "{b}");
    assert_eq!(supported.len(), agent::rules::SUPPORTED.len());
    // A known rule with nothing to act on in this layout.
    let (s, b) = prompt_ws(&pod, "home", "hide captions").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["error"], "unsupported_prompt");
    assert!(b["detail"].as_str().unwrap().contains("PostCard"), "{b}");
    // Reset is a rollback, not a patch.
    let (s, b) = prompt(&pod, "Reset layout").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["error"], "use_rollback");
    assert_eq!(b["rollback"], "/v1/workspaces/pixelfed_home/rollback");
    assert!(version_count(&pod, "pixelfed_home") <= 1);
    assert!(version_count(&pod, "home") <= 1);
}

#[tokio::test]
async fn invalid_demo_rule_is_rejected_and_versions_unchanged() {
    let pod = open_pod().await;
    let (s, _) = prompt(&pod, "hide captions").await;
    assert_eq!(s, StatusCode::OK);
    let before = pod.state.store().layout_versions("pixelfed_home").unwrap();
    let mut events = pod.state.subscribe_layouts();
    // Composed with a valid rule: the whole patch is rejected.
    let (s, b) = prompt(&pod, "show captions and remove the action bar").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert_eq!(b["error"], "invalid_patch");
    assert_eq!(b["provider"], "rules");
    assert!(b["detail"].as_str().unwrap().contains("action_slot"), "{b}");
    let patch = b["patch"].as_array().unwrap();
    assert_eq!(
        patch.last().unwrap(),
        &json!({ "op": "remove", "path": "/slots/0/content/item_template/action_slot" })
    );
    assert_eq!(
        pod.state.store().layout_versions("pixelfed_home").unwrap(),
        before
    );
    assert!(events.try_recv().is_err(), "nothing broadcast");
    let (v, node) = latest(&pod, "pixelfed_home");
    assert_eq!(v, 2);
    assert_eq!(
        card(&serde_json::to_value(node).unwrap())["caption_slot"],
        blank_caption()
    );
}

#[tokio::test]
async fn successful_prompt_broadcasts_a_layout_event() {
    let pod = open_pod().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/v1/workspaces/pixelfed_home/events", pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut resp = resp;
    let mut buf = String::new();
    let mut next = async || -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(i) = buf.find("\n\n") {
                    let msg = buf[..i].to_owned();
                    buf.drain(..i + 2);
                    if msg != ": keepalive" {
                        return msg;
                    }
                    continue;
                }
                let chunk = resp.chunk().await.unwrap().expect("stream ended");
                buf.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("no SSE message")
    };
    let msg = |v: i64| {
        format!(
            "event: layout\ndata: {}",
            json!({ "workspace_id": "pixelfed_home", "version": v,
                    "layout_id": "pixelfed_home.classic", "layout_version": v })
        )
    };
    assert_eq!(next().await, msg(1));
    let (s, _) = prompt(&pod, DESIGN_PROMPT).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(next().await, msg(2));
}

#[tokio::test]
async fn auth_and_request_validation() {
    let pod = spawn(AppState::in_memory()).await;
    pod.state
        .store()
        .create_session(&session_hash("tok"))
        .unwrap();
    for bearer in [None, Some("wrong")] {
        let (s, _, b) = prompt_raw(&pod, json!({ "prompt": DESIGN_PROMPT }), bearer).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{bearer:?}");
        assert_eq!(b["error"], "unauthorized");
        assert_eq!(
            b["detail"],
            "a valid pod session is required (or CANVAS_AGENT_OPEN=1 for local demos)"
        );
    }
    // Same body on the history routes.
    let resp = reqwest::Client::new()
        .get(format!("{}/v1/workspaces/home/versions", pod.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let b: Value = resp.json().await.unwrap();
    assert!(
        b["detail"].as_str().unwrap().contains("CANVAS_AGENT_OPEN"),
        "{b}"
    );
    assert_eq!(version_count(&pod, "pixelfed_home"), 0);
    let (s, _, b) = prompt_raw(&pod, json!({ "prompt": DESIGN_PROMPT }), Some("tok")).await;
    assert_eq!(s, StatusCode::OK, "{b}");

    for bad in [
        json!({}),
        json!({ "prompt": "  " }),
        json!({ "prompt": 3 }),
        json!({ "prompt": "x", "workspace_id": 1 }),
        json!({ "prompt": "x".repeat(4001) }),
    ] {
        let (s, _, b) = prompt_raw(&pod, bad.clone(), Some("tok")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(b["error"], "invalid_request");
    }
    let (s, _, b) = prompt_raw(
        &pod,
        json!({ "prompt": "hide captions", "workspace_id": "nope" }),
        Some("tok"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(b["error"], "workspace_not_found");
}

#[tokio::test]
async fn malformed_workspace_ids_are_400_with_a_truncated_echo() {
    let pod = open_pod().await;
    let huge = "w".repeat(10_000);
    let check = |s: StatusCode, b: &Value, what: &str| {
        assert_eq!(s, StatusCode::BAD_REQUEST, "{what}: {b}");
        assert_eq!(b["error"], "invalid_workspace_id", "{what}");
        assert!(
            b["id"].as_str().unwrap().len() <= 67,
            "{what}: echo is truncated"
        );
    };
    let (s, b) = prompt_ws(&pod, &huge, "hide captions").await;
    check(s, &b, "agent");
    let (s, b) = prompt_ws(&pod, "bad id!", "hide captions").await;
    check(s, &b, "agent charset");
    let client = reqwest::Client::new();
    for (method, path) in [
        ("GET", format!("/v1/workspaces/{huge}")),
        ("GET", format!("/v1/workspaces/{huge}/versions")),
        ("POST", format!("/v1/workspaces/{huge}/rollback")),
        ("GET", format!("/v1/workspaces/{huge}/events")),
        ("GET", "/v1/workspaces/a.b/versions".to_owned()),
    ] {
        let url = format!("{}{path}", pod.url);
        let req = if method == "GET" {
            client.get(url)
        } else {
            client.post(url).json(&json!({ "version": 1 }))
        };
        let resp = req.send().await.unwrap();
        let s = resp.status();
        let b: Value = resp.json().await.unwrap();
        check(s, &b, &path[..40.min(path.len())]);
    }
    // A well-formed unknown id is still a 404.
    let (s, b) = prompt_ws(&pod, "no-such_ws", "hide captions").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(b["id"], "no-such_ws");
}

// ---------- provider selection ----------

fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
}

#[test]
fn provider_selection_from_env() {
    let d = |vars: &[(&str, &str)]| Selection::from_lookup(lookup(vars)).describe();
    assert_eq!(d(&[]), ("rules".into(), None));
    assert_eq!(
        d(&[("CANVAS_LLM_PROVIDER", "rules"), ("ANTHROPIC_API_KEY", "k")]),
        ("rules".into(), None)
    );
    assert_eq!(
        d(&[("ANTHROPIC_API_KEY", "k")]),
        (
            "anthropic".into(),
            Some(agent::anthropic::DEFAULT_MODEL.into())
        )
    );
    assert_eq!(
        d(&[
            ("CANVAS_LLM_PROVIDER", "Anthropic"),
            ("ANTHROPIC_API_KEY", "k"),
            ("CANVAS_LLM_MODEL", "m-1")
        ]),
        ("anthropic".into(), Some("m-1".into()))
    );
    assert!(matches!(
        Selection::from_lookup(lookup(&[("CANVAS_LLM_PROVIDER", "anthropic"), ("ANTHROPIC_API_KEY", " ")])),
        Selection::NotConfigured { detail, .. } if detail == "ANTHROPIC_API_KEY is not set"
    ));
    assert!(matches!(
        Selection::from_lookup(lookup(&[("CANVAS_LLM_PROVIDER", "gpt")])),
        Selection::NotConfigured { detail, .. } if detail.contains("unknown")
    ));
}

#[tokio::test]
async fn anthropic_selected_without_key_is_503() {
    let sel = Selection::from_lookup(lookup(&[("CANVAS_LLM_PROVIDER", "anthropic")]));
    let pod = spawn(AppState::in_memory().with_agent_open(true).with_agent(sel)).await;
    let (s, b) = prompt(&pod, DESIGN_PROMPT).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        b,
        json!({ "error": "provider_not_configured", "provider": "anthropic", "detail": "ANTHROPIC_API_KEY is not set" })
    );
    assert_eq!(version_count(&pod, "pixelfed_home"), 0);
}

// ---------- registry ----------

#[test]
fn registry_contains_every_ui_node_variant() {
    // Exhaustive: adding a UiNode variant fails to compile until it is listed.
    fn tag(node: &UiNode) -> &'static str {
        match node {
            UiNode::Workspace { .. } => "core://Workspace",
            UiNode::Container { .. } => "core://Container",
            UiNode::Text { .. } => "core://Text",
            UiNode::Button { .. } => "core://Button",
            UiNode::PostCard { .. } => "pixelfed://PostCard",
            UiNode::MediaCarousel { .. } => "pixelfed://MediaCarousel",
            UiNode::AuthorHeader { .. } => "pixelfed://AuthorHeader",
            UiNode::ActionBar { .. } => "pixelfed://ActionBar",
            UiNode::FeedList { .. } => "pixelfed://FeedList",
        }
    }
    let _ = tag;
    let tags = [
        "core://Workspace",
        "core://Container",
        "core://Text",
        "core://Button",
        "pixelfed://PostCard",
        "pixelfed://MediaCarousel",
        "pixelfed://AuthorHeader",
        "pixelfed://ActionBar",
        "pixelfed://FeedList",
    ];
    let registry = agent::registry();
    let text = registry.to_string();
    for t in tags {
        assert!(
            text.contains(&format!("\"{t}\"")),
            "{t} missing from the registry"
        );
    }
    let variants = registry["oneOf"].as_array().expect("UiNode is a oneOf");
    assert_eq!(variants.len(), tags.len());
    // Intents are described too.
    for i in [
        "action.navigation.open",
        "action.mutation.optimistic",
        "action.agent.prompt",
    ] {
        assert!(text.contains(i), "{i}");
    }
}

// ---------- validation ----------

#[test]
fn validation_rules() {
    let layout = serde_json::to_value(layouts::builtin("pixelfed_home").unwrap()).unwrap();
    let ok = |ops: Value| agent::apply_and_validate(&layout, ops.as_array().unwrap());
    assert!(ok(json!([{ "op": "replace", "path": "/title", "value": "Feed" }])).is_ok());
    let like = "/slots/0/content/item_template/action_slot/like_intent/endpoint";
    let err = ok(json!([{ "op": "replace", "path": like, "value": "https://evil.example/like" }]))
        .unwrap_err();
    assert!(err.contains("/v1/"), "{err}");
    let err = ok(json!([{ "op": "replace", "path": "/id", "value": "other" }])).unwrap_err();
    assert!(err.contains("id"), "{err}");
    let err = ok(json!([{ "op": "replace", "path": "/slots", "value": [] }])).unwrap_err();
    assert!(err.contains("at least one slot"), "{err}");
    let slot = layout["slots"][0].clone();
    let err =
        ok(json!([{ "op": "replace", "path": "/slots", "value": vec![slot; 51] }])).unwrap_err();
    assert!(err.contains("51 slots"), "{err}");
    let err = ok(json!([{ "op": "replace", "path": "", "value": { "type": "core://Text", "value_expr": "", "style": "Body" } }])).unwrap_err();
    assert!(err.contains("core://Workspace"), "{err}");
    let err = ok(json!([{ "op": "replace", "path": "/title", "value": "x".repeat(300_000) }]))
        .unwrap_err();
    assert!(err.contains("bytes"), "{err}");
    let err = ok(json!([{ "op": "replace", "path": "/slots/0/content/item_template/caption_slot/type", "value": "core://Marquee" }])).unwrap_err();
    assert!(err.contains("UiNode"), "{err}");
    let err = ok(json!([{ "op": "frobnicate", "path": "/title" }])).unwrap_err();
    assert!(err.contains("RFC 6902"), "{err}");
    let err = ok(json!([{ "op": "replace", "path": "/nope/0", "value": 1 }])).unwrap_err();
    assert!(err.contains("does not apply"), "{err}");
}

// ---------- Anthropic provider (mock Messages API) ----------

type Recorded = Arc<Mutex<Vec<(HeaderMap, Value)>>>;
/// A scripted reply: status, extra headers, JSON body.
type Reply = (u16, Vec<(&'static str, &'static str)>, Value);
type Script = Arc<Mutex<VecDeque<Reply>>>;

struct MockApi {
    url: String,
    requests: Recorded,
    handle: JoinHandle<()>,
}

impl Drop for MockApi {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn mock_api(responses: Vec<Reply>) -> MockApi {
    use axum::response::IntoResponse;
    let requests: Recorded = Arc::default();
    let script: Script = Arc::new(Mutex::new(responses.into()));
    let (rec, scr) = (requests.clone(), script.clone());
    let router = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let (rec, scr) = (rec.clone(), scr.clone());
            async move {
                rec.lock()
                    .unwrap()
                    .push((headers, serde_json::from_slice(&body).unwrap()));
                let (status, hdrs, body) =
                    scr.lock().unwrap().pop_front().expect("unexpected request");
                let mut resp = (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    axum::Json(body),
                )
                    .into_response();
                for (k, v) in hdrs {
                    resp.headers_mut().insert(k, v.parse().unwrap());
                }
                resp
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    MockApi {
        url,
        requests,
        handle,
    }
}

fn provider(api: &MockApi, fallbacks: bool) -> AnthropicProvider {
    let mut config = AnthropicConfig::new("test-key");
    config.base_url = format!("{}/", api.url);
    config.fallbacks = fallbacks;
    config.retry_base = Duration::from_millis(10);
    AnthropicProvider::new(config)
}

fn message(content: Value, stop_reason: &str, stop_details: Value) -> Value {
    json!({
        "id": "msg_01", "type": "message", "role": "assistant",
        "model": agent::anthropic::DEFAULT_MODEL,
        "content": content, "stop_reason": stop_reason, "stop_details": stop_details,
        "usage": { "input_tokens": 1200, "output_tokens": 80 }
    })
}

/// A recorded-style answer: an (omitted) thinking block, then the JSON text.
fn hide_captions_answer() -> Value {
    let out = json!({
        "explanation": "Blank the caption.",
        "operations": [
            { "op": "replace", "path": "/slots/0/content/item_template/caption_slot", "from": "",
              "value_json": blank_caption().to_string() },
            { "op": "move", "path": "/title", "from": "/title", "value_json": "" }
        ]
    });
    message(
        json!([
            { "type": "thinking", "thinking": "", "signature": "EqQBCkYIBxgCKkB" },
            { "type": "text", "text": out.to_string() }
        ]),
        "end_turn",
        Value::Null,
    )
}

fn layout() -> Value {
    serde_json::to_value(layouts::builtin("pixelfed_home").unwrap()).unwrap()
}

#[tokio::test]
async fn anthropic_request_shape() {
    let api = mock_api(vec![(200, vec![], hide_captions_answer())]).await;
    let p = provider(&api, true);
    let got = p
        .generate("Hide post captions", &layout(), agent::registry())
        .await
        .unwrap();
    assert_eq!(got.explanation.as_deref(), Some("Blank the caption."));
    assert_eq!(
        got.ops,
        vec![
            json!({ "op": "replace", "path": "/slots/0/content/item_template/caption_slot", "value": blank_caption() }),
            json!({ "op": "move", "path": "/title", "from": "/title" }),
        ]
    );
    let reqs = api.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let (h, body) = &reqs[0];
    assert_eq!(h["x-api-key"], "test-key");
    assert_eq!(h["anthropic-version"], "2026-10-21");
    assert_eq!(h["content-type"], "application/json");
    assert_eq!(h["anthropic-beta"], "server-side-fallback-2026-07-01");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["max_tokens"], 16000);
    assert_eq!(body["output_config"]["effort"], "medium");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["output_config"]["format"]["schema"], patch_schema());
    assert_eq!(body["fallbacks"], "default");
    for absent in [
        "thinking",
        "temperature",
        "top_p",
        "top_k",
        "tool_choice",
        "tools",
    ] {
        assert!(body.get(absent).is_none(), "{absent} must not be sent");
    }
    assert_eq!(
        body["messages"],
        json!([{ "role": "user", "content": "Hide post captions" }])
    );
    // System: stable instructions + registry first, then the current layout.
    let system = body["system"].as_array().unwrap();
    assert_eq!(system.len(), 2);
    let stable = system[0]["text"].as_str().unwrap();
    assert!(stable.contains("pixelfed://FeedList") && stable.contains("RFC 6902"));
    assert!(
        !stable.contains("timeline:home"),
        "layout is not in the stable part"
    );
    let current = system[1]["text"].as_str().unwrap();
    assert!(current.contains(&serde_json::to_string_pretty(&layout()).unwrap()));
    // The schema the server sends is exactly the contract's PATCH_SCHEMA.
    let contract: Value = serde_json::from_str(r#"{"type":"object","properties":{"explanation":{"type":"string"},"operations":{"type":"array","items":{"type":"object","properties":{"op":{"type":"string","enum":["add","remove","replace","move","copy","test"]},"path":{"type":"string"},"from":{"type":"string"},"value_json":{"type":"string"}},"required":["op","path","from","value_json"],"additionalProperties":false}}},"required":["explanation","operations"],"additionalProperties":false}"#).unwrap();
    assert_eq!(patch_schema(), contract);
}

#[tokio::test]
async fn anthropic_fallbacks_can_be_turned_off() {
    let api = mock_api(vec![(200, vec![], hide_captions_answer())]).await;
    provider(&api, false)
        .generate("x", &layout(), agent::registry())
        .await
        .unwrap();
    let reqs = api.requests.lock().unwrap();
    assert!(reqs[0].0.get("anthropic-beta").is_none());
    assert!(reqs[0].1.get("fallbacks").is_none());
}

#[tokio::test]
async fn anthropic_refusal_and_max_tokens() {
    let refusal = message(
        json!([]),
        "refusal",
        json!({ "type": "refusal", "category": "cyber", "explanation": null }),
    );
    let truncated = message(
        json!([{ "type": "text", "text": "{\"explan" }]),
        "max_tokens",
        Value::Null,
    );
    let api = mock_api(vec![(200, vec![], refusal), (200, vec![], truncated)]).await;
    let p = provider(&api, true);
    match p.generate("x", &layout(), agent::registry()).await {
        Err(ProviderError::Failed(d)) => {
            assert!(d.contains("refusal") && d.contains("cyber"), "{d}")
        }
        other => panic!("{other:?}"),
    }
    match p.generate("x", &layout(), agent::registry()).await {
        Err(ProviderError::Failed(d)) => assert!(d.contains("truncated"), "{d}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn anthropic_retries_429_then_succeeds() {
    let overloaded =
        json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "slow down" } });
    let api = mock_api(vec![
        (429, vec![("retry-after", "0")], overloaded.clone()),
        (
            529,
            vec![],
            json!({ "type": "error", "error": { "type": "overloaded_error", "message": "busy" } }),
        ),
        (200, vec![], hide_captions_answer()),
    ])
    .await;
    let got = provider(&api, true)
        .generate("x", &layout(), agent::registry())
        .await
        .unwrap();
    assert_eq!(got.ops.len(), 2);
    assert_eq!(api.requests.lock().unwrap().len(), 3);

    // Retries are bounded: 1 + 2 attempts, then provider_error.
    let api = mock_api(vec![
        (429, vec![("retry-after", "0")], overloaded.clone()),
        (429, vec![("retry-after", "0")], overloaded.clone()),
        (429, vec![("retry-after", "0")], overloaded),
    ])
    .await;
    match provider(&api, true)
        .generate("x", &layout(), agent::registry())
        .await
    {
        Err(ProviderError::Failed(d)) => {
            assert!(d.contains("429") && d.contains("slow down"), "{d}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(api.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn anthropic_http_errors() {
    let api = mock_api(vec![
        (401, vec![], json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } })),
        (400, vec![], json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "bad field" } })),
    ])
    .await;
    let p = provider(&api, true);
    match p.generate("x", &layout(), agent::registry()).await {
        Err(ProviderError::NotConfigured(d)) => {
            assert!(
                d.contains("ANTHROPIC_API_KEY")
                    && d.contains("401")
                    && d.contains("invalid x-api-key"),
                "{d}"
            )
        }
        other => panic!("{other:?}"),
    }
    match p.generate("x", &layout(), agent::registry()).await {
        Err(ProviderError::Failed(d)) => assert_eq!(d, "HTTP 400: bad field"),
        other => panic!("{other:?}"),
    }
    assert_eq!(api.requests.lock().unwrap().len(), 2, "4xx is not retried");
}

#[tokio::test]
async fn anthropic_malformed_output_is_a_provider_error_through_the_endpoint() {
    let not_json = message(
        json!([{ "type": "text", "text": "Sure! Here is the patch:" }]),
        "end_turn",
        Value::Null,
    );
    let bad_value = message(
        json!([{ "type": "text", "text": json!({ "explanation": "", "operations": [
            { "op": "add", "path": "/slots/0", "from": "", "value_json": "{not json" }] }).to_string() }]),
        "end_turn",
        Value::Null,
    );
    let api = mock_api(vec![(200, vec![], not_json), (200, vec![], bad_value)]).await;
    let pod = spawn(
        AppState::in_memory()
            .with_agent_open(true)
            .with_agent(Selection::provider(provider(&api, true))),
    )
    .await;
    for want in ["not valid JSON", "value_json is not valid JSON"] {
        let (s, b) = prompt(&pod, "hide captions").await;
        assert_eq!(s, StatusCode::BAD_GATEWAY, "{b}");
        assert_eq!(b["error"], "provider_error");
        assert_eq!(b["provider"], "anthropic");
        assert!(b["detail"].as_str().unwrap().contains(want), "{b}");
    }
    assert_eq!(
        version_count(&pod, "pixelfed_home"),
        1,
        "only the seeded v1"
    );
}

#[tokio::test]
async fn anthropic_provider_through_the_endpoint() {
    let api = mock_api(vec![
        (200, vec![], hide_captions_answer()),
        (401, vec![], json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } })),
    ])
    .await;
    let pod = spawn(
        AppState::in_memory()
            .with_agent_open(true)
            .with_agent(Selection::provider(provider(&api, true))),
    )
    .await;
    let (s, b) = prompt(&pod, "Hide post captions").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["provider"], "anthropic");
    assert_eq!(b["model"], "claude-opus-5-5");
    assert_eq!(b["explanation"], "Blank the caption.");
    assert_eq!(card(&spec_workspace(&b))["caption_slot"], blank_caption());
    let rows = pod.state.store().layout_versions("pixelfed_home").unwrap();
    assert_eq!(rows[0].provider.as_deref(), Some("anthropic"));
    // The registry and current layout went to the API.
    let reqs = api.requests.lock().unwrap().clone();
    assert!(
        reqs[0].1["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("pixelfed://PostCard")
    );
    // A rejected key is a 503 with a clear message.
    let (s, b) = prompt(&pod, "Hide post captions").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert_eq!(b["error"], "provider_not_configured");
    assert!(
        b["detail"]
            .as_str()
            .unwrap()
            .contains("rejected ANTHROPIC_API_KEY"),
        "{b}"
    );
}

// ---------- review hardening ----------

#[test]
fn builtin_layouts_pass_validation() {
    for ws in layouts::VERSIONED {
        let layout = serde_json::to_value(layouts::builtin(ws).unwrap()).unwrap();
        agent::apply_and_validate(&layout, &[]).unwrap_or_else(|e| panic!("{ws}: {e}"));
    }
}

#[test]
fn validation_bounds_depth_size_growth_and_values() {
    let layout = layout();
    let ok = |ops: Value| agent::apply_and_validate(&layout, ops.as_array().unwrap());
    let caption = "/slots/0/content/item_template/caption_slot";

    // Deep nesting (clients parse and render recursively).
    let mut deep = json!({ "type": "core://Text", "value_expr": "", "style": "Body" });
    for _ in 0..20 {
        deep = json!({ "type": "core://Container", "orientation": "Vertical", "padding_dp": 0, "children": [deep] });
    }
    let err = ok(json!([{ "op": "replace", "path": caption, "value": deep }])).unwrap_err();
    assert!(err.contains("deeper than"), "{err}");
    let shallow = json!({ "type": "core://Container", "orientation": "Vertical", "padding_dp": 8,
        "children": [{ "type": "core://Text", "value_expr": "entity.caption", "style": "Body" }] });
    assert!(ok(json!([{ "op": "replace", "path": caption, "value": shallow }])).is_ok());

    // `copy` doubling the document is stopped after the op that crosses the
    // size limit, not after building a gigantic document.
    let bomb: Vec<Value> = (0..60)
        .map(|i| json!({ "op": "copy", "from": "", "path": format!("/x{i}") }))
        .collect();
    let err = ok(Value::Array(bomb)).unwrap_err();
    assert!(
        err.contains("after operation") && err.contains("bytes"),
        "{err}"
    );

    // Too many operations.
    let many: Vec<Value> = (0..201)
        .map(|_| json!({ "op": "test", "path": "/id", "value": "pixelfed_home" }))
        .collect();
    let err = ok(Value::Array(many)).unwrap_err();
    assert!(err.contains("201 operations"), "{err}");

    // Values a renderer cannot draw; 1e39 would even become `null` as f32.
    let media = "/slots/0/content/item_template/media_slot/aspect_ratio";
    for bad in [json!(0.0), json!(-1.0), json!(1e39), json!(100.0)] {
        let err = ok(json!([{ "op": "replace", "path": media, "value": bad }])).unwrap_err();
        assert!(err.contains("aspect_ratio"), "{bad}: {err}");
    }
    assert!(ok(json!([{ "op": "replace", "path": media, "value": 1.5 }])).is_ok());
    let mut padded = shallow.clone();
    padded["padding_dp"] = json!(60_000);
    let err = ok(json!([{ "op": "replace", "path": caption, "value": padded }])).unwrap_err();
    assert!(err.contains("padding_dp"), "{err}");

    // Slot ids stay unique.
    let slot = layout["slots"][0].clone();
    let err = ok(json!([{ "op": "add", "path": "/slots/-", "value": slot }])).unwrap_err();
    assert!(err.contains("duplicate slot_id"), "{err}");
}

#[test]
fn validation_rejects_invented_intents_but_allows_copies() {
    let layout = layout();
    let ok = |ops: Value| agent::apply_and_validate(&layout, ops.as_array().unwrap());
    let caption = "/slots/0/content/item_template/caption_slot";
    let action = "/slots/0/content/item_template/action_slot";
    // A sign-in button to another instance (phishing) is not a copy.
    let login = json!({ "type": "core://Button", "label": "Sign in again",
        "on_click": { "intent": "action.auth.login", "instance_url": "https://evil.example" } });
    let err = ok(json!([{ "op": "replace", "path": caption, "value": login }])).unwrap_err();
    assert!(err.contains("action.auth.login"), "{err}");
    // Retargeting a pod mutation (still /v1/) is not a copy either.
    let err = ok(
        json!([{ "op": "replace", "path": format!("{action}/like_intent/endpoint"),
        "value": "/v1/workspaces/pixelfed_home/rollback" }]),
    )
    .unwrap_err();
    assert!(err.contains("copies of intents"), "{err}");
    // Moving or copying existing intents is fine, and PromptAgent buttons are allowed.
    assert!(ok(json!([{ "op": "copy", "from": action, "path": caption }])).is_ok());
    let ask = json!({ "type": "core://Button", "label": "Hide captions",
        "on_click": { "intent": "action.agent.prompt", "prompt_text": "hide captions" } });
    assert!(ok(json!([{ "op": "replace", "path": caption, "value": ask }])).is_ok());
}

/// Generates "/title" = "Agent" once released (a slow LLM call).
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
async fn a_change_committed_during_generation_is_not_overwritten() {
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
            .json(&json!({ "prompt": "rename" }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    });
    started.notified().await;
    // Meanwhile another change lands (a rollback or a concurrent prompt).
    let (v1, mut other) = latest(&pod, "pixelfed_home");
    assert_eq!(v1, 1);
    if let UiNode::Workspace { slots, .. } = &mut other {
        slots[0].slot_id = "concurrent".into();
    }
    let v2 = layouts::commit(pod.state.store(), "pixelfed_home", &other, None, None, None).unwrap();
    assert_eq!(v2, 2);
    release.notify_one();
    let body = request.await.unwrap();
    assert_eq!(body["layout_version"], 3, "{body}");
    let ws = spec_workspace(&body);
    assert_eq!(ws["title"], "Agent");
    assert_eq!(ws["slots"][0]["slot_id"], "concurrent", "v2's change kept");
    let (v, stored) = latest(&pod, "pixelfed_home");
    assert_eq!((v, serde_json::to_value(stored).unwrap()), (3, ws));
}

#[test]
fn anthropic_config_debug_redacts_the_key() {
    let config = AnthropicConfig::new("sk-ant-secret-123");
    let printed = format!("{config:?}");
    assert!(!printed.contains("secret"), "{printed}");
    assert!(printed.contains("redacted"), "{printed}");
}

#[tokio::test]
async fn anthropic_total_timeout_bounds_retries() {
    let overloaded =
        json!({ "type": "error", "error": { "type": "overloaded_error", "message": "busy" } });
    let api = mock_api(vec![
        (529, vec![("retry-after", "5")], overloaded.clone()),
        (529, vec![("retry-after", "5")], overloaded),
    ])
    .await;
    let mut config = AnthropicConfig::new("k");
    config.base_url = api.url.clone();
    config.total_timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    match AnthropicProvider::new(config)
        .generate("x", &layout(), agent::registry())
        .await
    {
        Err(ProviderError::Failed(d)) => assert!(d.contains("retries included"), "{d}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(3));
}
