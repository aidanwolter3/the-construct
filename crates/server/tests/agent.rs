//! Layout agent (kept in the library, not routed; see `server::agent`): the
//! rules provider, the Anthropic provider against a mock Messages API, patch
//! validation and the registry. `POST /v1/agent/modify_layout` is gone (404,
//! tested in `tests/apps.rs`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::HeaderMap;
use protocol::UiNode;
use serde_json::{Value, json};
use server::agent::anthropic::{AnthropicConfig, AnthropicProvider, patch_schema};
use server::agent::{self, LayoutProvider, ProviderError, Selection};
use server::layouts;
use tokio::task::JoinHandle;

const DESIGN_PROMPT: &str = "Hide post captions and put media at the very top";

/// The rules provider on `layout`, then patch validation: `(ops, new layout)`.
async fn rules(prompt: &str, layout: &Value) -> Result<(Vec<Value>, Value), String> {
    let generated = agent::rules::RulesProvider
        .generate(prompt, layout, agent::registry())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let node = agent::apply_and_validate(layout, &generated.ops)?;
    Ok((generated.ops, serde_json::to_value(node).unwrap()))
}

fn builtin(ws: &str) -> Value {
    serde_json::to_value(layouts::builtin(ws).unwrap()).unwrap()
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

// ---------- rules provider ----------

#[tokio::test]
async fn design_prompt() {
    let (ops, ws) = rules(DESIGN_PROMPT, &builtin("pixelfed_home"))
        .await
        .unwrap();
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
    assert_eq!(ops.len(), 3, "{ops:?}");
    assert!(
        ops.iter()
            .all(|op| op["op"] == "replace" && op.get("value").is_some())
    );
    // Repeating it changes nothing (idempotent rules).
    let (ops, again) = rules(DESIGN_PROMPT, &ws).await.unwrap();
    assert!(ops.is_empty(), "{ops:?}");
    assert_eq!(again, ws);
}

#[tokio::test]
async fn each_rule_alone() {
    let (_, ws) = rules("hide captions", &builtin("pixelfed_home"))
        .await
        .unwrap();
    assert_eq!(card(&ws)["caption_slot"], blank_caption());
    assert_eq!(kinds(card(&ws))[0], "pixelfed://AuthorHeader");

    let (_, ws) = rules("Show captions", &ws).await.unwrap();
    assert_eq!(
        card(&ws)["caption_slot"],
        json!({ "type": "core://Text", "value_expr": "entity.caption", "style": "Body" })
    );
    assert_eq!(ws, builtin("pixelfed_home"));

    let (ops, ws) = rules("Photos first", &ws).await.unwrap();
    assert_eq!(
        kinds(card(&ws)),
        [
            "pixelfed://MediaCarousel",
            "pixelfed://AuthorHeader",
            "pixelfed://ActionBar",
            "core://Text"
        ]
    );
    assert_eq!(ops.len(), 2, "a swap");

    let (_, ws) = rules(
        "Move the action bar above the photo",
        &builtin("pixelfed_home"),
    )
    .await
    .unwrap();
    assert_eq!(
        kinds(card(&ws)),
        [
            "pixelfed://AuthorHeader",
            "pixelfed://ActionBar",
            "pixelfed://MediaCarousel",
            "core://Text"
        ]
    );
    // Already above: idempotent.
    let (ops, _) = rules("move the action bar above the photo", &ws)
        .await
        .unwrap();
    assert!(ops.is_empty());
}

#[tokio::test]
async fn rules_compose_in_prompt_order_on_the_current_layout() {
    // Media first, then the action bar goes right above the (moved) photo.
    let (_, ws) = rules(
        "photos first and move the action bar above the photo",
        &builtin("pixelfed_home"),
    )
    .await
    .unwrap();
    assert_eq!(
        kinds(card(&ws)),
        [
            "pixelfed://ActionBar",
            "pixelfed://MediaCarousel",
            "pixelfed://AuthorHeader",
            "core://Text"
        ]
    );
    // Hide captions + weather in one patch.
    let (ops, ws) = rules(
        "Add a weather widget at the top, and hide the captions",
        &ws,
    )
    .await
    .unwrap();
    assert_eq!(ws["slots"][0]["slot_id"], "weather");
    assert_eq!(ws["slots"][1]["slot_id"], "feed");
    assert_eq!(
        ws["slots"][1]["content"]["item_template"]["caption_slot"],
        blank_caption()
    );
    assert_eq!(ops.len(), 2);
}

#[tokio::test]
async fn weather_widget_is_added_once() {
    let (ops, ws) = rules("add a weather widget", &builtin("pixelfed_home"))
        .await
        .unwrap();
    assert_eq!(
        ws["slots"][0],
        json!({ "slot_id": "weather", "provider_uri": "core://placeholder",
                "content": { "type": "core://Container", "orientation": "Vertical", "padding_dp": 16,
                    "children": [{ "type": "core://Text", "value_expr": "Weather widget (not available yet)", "style": "Caption" }] } })
    );
    assert_eq!(
        Value::Array(ops),
        json!([{ "op": "add", "path": "/slots/0", "value": ws["slots"][0] }])
    );
    let (ops, again) = rules("Add a weather widget at the top", &ws).await.unwrap();
    assert!(ops.is_empty());
    assert_eq!(again["slots"].as_array().unwrap().len(), 2);
    // Works on the other apps too.
    let (_, home) = rules("add a weather widget", &builtin("home"))
        .await
        .unwrap();
    assert_eq!(home["id"], "home");
}

#[tokio::test]
async fn unsupported_prompts() {
    let feed = builtin("pixelfed_home");
    let generate = |p: &'static str, layout: Value| async move {
        agent::rules::RulesProvider
            .generate(p, &layout, agent::registry())
            .await
    };
    assert_eq!(
        generate("make it purple", feed.clone()).await,
        Err(ProviderError::Unsupported { detail: None })
    );
    assert!(agent::rules::SUPPORTED.contains(&"Hide post captions"));
    // A known rule with nothing to act on in this layout.
    match generate("hide captions", builtin("home")).await {
        Err(ProviderError::Unsupported { detail: Some(d) }) => {
            assert!(d.contains("PostCard"), "{d}")
        }
        other => panic!("{other:?}"),
    }
    // Reset is not a patch.
    assert_eq!(
        generate("Reset layout", feed).await,
        Err(ProviderError::UseRollback)
    );
}

#[tokio::test]
async fn invalid_demo_rule_is_rejected_as_a_whole() {
    let (_, hidden) = rules("hide captions", &builtin("pixelfed_home"))
        .await
        .unwrap();
    // Composed with a valid rule: the whole patch is rejected.
    let generated = agent::rules::RulesProvider
        .generate(
            "show captions and remove the action bar",
            &hidden,
            agent::registry(),
        )
        .await
        .unwrap();
    assert_eq!(
        generated.ops.last().unwrap(),
        &json!({ "op": "remove", "path": "/slots/0/content/item_template/action_slot" })
    );
    let err = agent::apply_and_validate(&hidden, &generated.ops).unwrap_err();
    assert!(err.contains("action_slot"), "{err}");
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
        d(&[
            ("CONSTRUCT_LLM_PROVIDER", "rules"),
            ("ANTHROPIC_API_KEY", "k")
        ]),
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
            ("CONSTRUCT_LLM_PROVIDER", "Anthropic"),
            ("ANTHROPIC_API_KEY", "k"),
            ("CONSTRUCT_LLM_MODEL", "m-1")
        ]),
        ("anthropic".into(), Some("m-1".into()))
    );
    assert!(matches!(
        Selection::from_lookup(lookup(&[("CONSTRUCT_LLM_PROVIDER", "anthropic"), ("ANTHROPIC_API_KEY", " ")])),
        Selection::NotConfigured { detail, .. } if detail == "ANTHROPIC_API_KEY is not set"
    ));
    assert!(matches!(
        Selection::from_lookup(lookup(&[("CONSTRUCT_LLM_PROVIDER", "gpt")])),
        Selection::NotConfigured { detail, .. } if detail.contains("unknown")
    ));
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
        "action.auth.login",
        "action.navigation.back",
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
async fn anthropic_malformed_output_is_a_provider_error() {
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
    let p = provider(&api, true);
    for want in ["not valid JSON", "value_json is not valid JSON"] {
        match p
            .generate("hide captions", &layout(), agent::registry())
            .await
        {
            Err(ProviderError::Failed(d)) => assert!(d.contains(want), "{d}"),
            other => panic!("{other:?}"),
        }
    }
}

// ---------- review hardening ----------

#[test]
fn builtin_layouts_pass_validation() {
    for app in layouts::APPS {
        let ws = app.id;
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
    // Moving or copying existing intents is fine, and back buttons are allowed.
    assert!(ok(json!([{ "op": "copy", "from": action, "path": caption }])).is_ok());
    let ask = json!({ "type": "core://Button", "label": "Back",
        "on_click": { "intent": "action.navigation.back" } });
    assert!(ok(json!([{ "op": "replace", "path": caption, "value": ask }])).is_ok());
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
