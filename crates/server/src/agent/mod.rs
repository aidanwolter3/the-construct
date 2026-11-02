//! Generative layout agent (M7, D25): `POST /v1/agent/modify_layout`.
//!
//! A [`LayoutProvider`] turns a prompt plus the current layout into an RFC 6902
//! patch; the handler applies it to a copy, validates the result and, only if
//! it is valid, commits it as a new layout version and broadcasts it. Nothing
//! is committed on any failure.

pub mod anthropic;
pub mod rules;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use protocol::{ActionIntent, UiNode};
use serde_json::{Value, json};

use crate::{AppState, json_error, layouts, parse_json_body};

/// Default `workspace_id` of a prompt.
pub const DEFAULT_WORKSPACE: &str = crate::pixelfed::layout::WORKSPACE_ID;
/// Validation limits.
pub const MAX_SLOTS: usize = 50;
pub const MAX_LAYOUT_BYTES: usize = 256 * 1024;
pub const MAX_PROMPT_CHARS: usize = 4_000;
/// Deepest JSON nesting (objects + arrays) a layout may have. The built-in
/// layouts are about 9 deep; clients parse and render recursively (serde's
/// default limit is 128), so a deep tree must never reach them.
pub const MAX_LAYOUT_DEPTH: usize = 32;
/// Most operations in one patch, and its largest JSON size.
pub const MAX_PATCH_OPS: usize = 200;
pub const MAX_PATCH_BYTES: usize = 256 * 1024;
/// `MediaCarousel.aspect_ratio` range (width / height).
pub const ASPECT_RATIO_RANGE: std::ops::RangeInclusive<f32> = 0.2..=5.0;
/// Largest `Container.padding_dp`.
pub const MAX_PADDING_DP: u16 = 64;

/// What a provider produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Generated {
    /// RFC 6902 operations (JSON objects), paths relative to the workspace root.
    pub ops: Vec<Value>,
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The rules provider does not understand the prompt (or it does not
    /// apply to this layout: `detail`). 422 `unsupported_prompt`.
    Unsupported { detail: Option<String> },
    /// "reset layout": not a patch. 422 `use_rollback`.
    UseRollback,
    /// Missing / rejected credentials. 503 `provider_not_configured`.
    NotConfigured(String),
    /// The provider call failed (HTTP error, refusal, truncation, malformed
    /// output). 502 `provider_error`.
    Failed(String),
}

pub type GenerateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Generated, ProviderError>> + Send + 'a>>;

/// Turns a prompt into a patch over the current layout.
pub trait LayoutProvider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> Option<&str>;
    fn generate<'a>(
        &'a self,
        prompt: &'a str,
        layout: &'a Value,
        registry: &'a Value,
    ) -> GenerateFuture<'a>;
}

/// The configured provider, or why there is none.
#[derive(Clone)]
pub enum Selection {
    Ready(Arc<dyn LayoutProvider>),
    NotConfigured { provider: String, detail: String },
}

impl Selection {
    pub fn rules() -> Self {
        Self::Ready(Arc::new(rules::RulesProvider))
    }

    pub fn provider(p: impl LayoutProvider + 'static) -> Self {
        Self::Ready(Arc::new(p))
    }

    /// From `CANVAS_LLM_PROVIDER`, `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`,
    /// `CANVAS_LLM_MODEL`, `CANVAS_LLM_FALLBACKS`.
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// [`Self::from_env`] over any variable lookup (tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let var = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let choice = var("CANVAS_LLM_PROVIDER")
            .unwrap_or_else(|| "auto".to_owned())
            .to_ascii_lowercase();
        let key = var("ANTHROPIC_API_KEY");
        let anthropic = |key: String| {
            let mut config = anthropic::AnthropicConfig::new(key);
            if let Some(url) = var("ANTHROPIC_BASE_URL") {
                config.base_url = url;
            }
            if let Some(model) = var("CANVAS_LLM_MODEL") {
                config.model = model;
            }
            config.fallbacks = !var("CANVAS_LLM_FALLBACKS")
                .is_some_and(|v| v == "0" || v.eq_ignore_ascii_case("false"));
            Self::provider(anthropic::AnthropicProvider::new(config))
        };
        match (choice.as_str(), key) {
            ("rules", _) | ("auto", None) => Self::rules(),
            ("anthropic" | "auto", Some(key)) => anthropic(key),
            ("anthropic", None) => Self::NotConfigured {
                provider: anthropic::NAME.to_owned(),
                detail: "ANTHROPIC_API_KEY is not set".to_owned(),
            },
            (other, _) => Self::NotConfigured {
                provider: other.to_owned(),
                detail: format!("unknown CANVAS_LLM_PROVIDER '{other}' (auto|anthropic|rules)"),
            },
        }
    }

    /// `(provider, model)` for logs.
    pub fn describe(&self) -> (String, Option<String>) {
        match self {
            Self::Ready(p) => (p.name().to_owned(), p.model().map(str::to_owned)),
            Self::NotConfigured { provider, .. } => (provider.clone(), None),
        }
    }
}

static REGISTRY: LazyLock<Value> = LazyLock::new(|| {
    serde_json::to_value(schemars::schema_for!(UiNode)).expect("registry schema serializes")
});

/// The component registry: the JSON Schema of [`UiNode`] (with
/// [`ActionIntent`] and every nested type under `$defs`), derived from the
/// protocol types so it cannot drift from what clients deserialize.
pub fn registry() -> &'static Value {
    &REGISTRY
}

/// Applies `ops` to a copy of `layout` and validates the result (M7 contract
/// step 4). `Err` carries a human-readable reason.
pub fn apply_and_validate(layout: &Value, ops: &[Value]) -> Result<UiNode, String> {
    if ops.len() > MAX_PATCH_OPS {
        return Err(format!("{} operations (max {MAX_PATCH_OPS})", ops.len()));
    }
    let patch_size = serde_json::to_vec(ops).map_or(usize::MAX, |b| b.len());
    if patch_size > MAX_PATCH_BYTES {
        return Err(format!("patch is {patch_size} bytes (max {MAX_PATCH_BYTES})"));
    }
    let patch: json_patch::Patch = serde_json::from_value(Value::Array(ops.to_vec()))
        .map_err(|e| format!("not an RFC 6902 patch: {e}"))?;
    let mut doc = layout.clone();
    // One op at a time on a private copy, bounding the document after each:
    // `copy` can double it per op, so checking only the end result would let
    // a short patch exhaust memory first.
    for (i, op) in patch.iter().enumerate() {
        json_patch::patch_unsafe(&mut doc, std::slice::from_ref(op))
            .map_err(|e| format!("patch does not apply: operation {i}: {e}"))?;
        check_bounds(&doc).map_err(|e| format!("after operation {i}: {e}"))?;
    }
    validate(layout, &doc)
}

/// Nesting depth of objects/arrays (a scalar root is 0). Iterative, so a
/// deep document cannot overflow the stack here.
fn json_depth(v: &Value) -> usize {
    let mut max = 0;
    let mut stack = vec![(v, 1usize)];
    while let Some((v, d)) = stack.pop() {
        let children: Box<dyn Iterator<Item = &Value>> = match v {
            Value::Array(a) => Box::new(a.iter()),
            Value::Object(m) => Box::new(m.values()),
            _ => continue,
        };
        max = max.max(d);
        if d > MAX_LAYOUT_DEPTH {
            break; // deep enough to reject; don't walk the rest
        }
        stack.extend(children.map(|c| (c, d + 1)));
    }
    max
}

/// Depth (first: everything after it recurses) and size limits.
fn check_bounds(doc: &Value) -> Result<(), String> {
    if json_depth(doc) > MAX_LAYOUT_DEPTH {
        return Err(format!(
            "layout nests deeper than {MAX_LAYOUT_DEPTH} levels"
        ));
    }
    let size = serde_json::to_vec(doc).map_or(usize::MAX, |b| b.len());
    if size > MAX_LAYOUT_BYTES {
        return Err(format!("layout is {size} bytes (max {MAX_LAYOUT_BYTES})"));
    }
    Ok(())
}

/// Validates a patched layout against the original root.
pub fn validate(original: &Value, doc: &Value) -> Result<UiNode, String> {
    check_bounds(doc)?;
    let node: UiNode = serde_json::from_value(doc.clone())
        .map_err(|e| format!("layout does not deserialize as a UiNode: {e}"))?;
    let UiNode::Workspace { id, slots, .. } = &node else {
        return Err("root must stay a core://Workspace".to_owned());
    };
    let original_id = original["id"].as_str().unwrap_or_default();
    if id != original_id {
        return Err(format!(
            "workspace id changed from '{original_id}' to '{id}'"
        ));
    }
    if slots.is_empty() {
        return Err("workspace must keep at least one slot".to_owned());
    }
    if slots.len() > MAX_SLOTS {
        return Err(format!("{} slots (max {MAX_SLOTS})", slots.len()));
    }
    let mut ids = std::collections::HashSet::new();
    if let Some(dup) = slots.iter().find(|s| !ids.insert(s.slot_id.as_str())) {
        return Err(format!("duplicate slot_id '{}'", dup.slot_id));
    }
    check_values(&node)?;
    // Intents that reach the network or start a sign-in may only be copies of
    // ones the current layout already has (the model is told the same): a
    // patch must not invent endpoints, entity targets or instance URLs.
    let original_node = serde_json::from_value::<UiNode>(original.clone()).ok();
    let mut existing = Vec::new();
    if let Some(original) = &original_node {
        collect_intents(original, &mut existing);
    }
    let mut intents = Vec::new();
    collect_intents(&node, &mut intents);
    for intent in intents {
        if let ActionIntent::OptimisticMutate { endpoint, .. } = intent
            && !endpoint.starts_with("/v1/")
        {
            return Err(format!(
                "mutation endpoint '{endpoint}' must be a pod path starting with /v1/"
            ));
        }
        let restricted = matches!(
            intent,
            ActionIntent::OptimisticMutate { .. }
                | ActionIntent::AuthLogin { .. }
                | ActionIntent::Navigate { .. }
        );
        if restricted && !existing.contains(&intent) {
            let json = serde_json::to_string(intent).unwrap_or_default();
            return Err(format!(
                "new action intent {json}: mutation, navigation and sign-in intents may only be \
                 copies of intents already in the layout"
            ));
        }
    }
    Ok(node)
}

/// Numeric fields the renderers cannot draw sensibly (or at all: a
/// non-finite `f32` would even serialize as `null` and make the stored
/// layout unreadable).
fn check_values(node: &UiNode) -> Result<(), String> {
    match node {
        UiNode::Workspace { slots, .. } => slots.iter().try_for_each(|s| check_values(&s.content)),
        UiNode::Container {
            padding_dp,
            children,
            ..
        } => {
            if *padding_dp > MAX_PADDING_DP {
                return Err(format!("padding_dp {padding_dp} (max {MAX_PADDING_DP})"));
            }
            children.iter().try_for_each(check_values)
        }
        UiNode::MediaCarousel { aspect_ratio, .. } => {
            if !ASPECT_RATIO_RANGE.contains(aspect_ratio) {
                return Err(format!(
                    "aspect_ratio {aspect_ratio} outside {}..={}",
                    ASPECT_RATIO_RANGE.start(),
                    ASPECT_RATIO_RANGE.end()
                ));
            }
            Ok(())
        }
        UiNode::PostCard {
            author_slot,
            media_slot,
            action_slot,
            caption_slot,
            ..
        } => [author_slot, media_slot, action_slot, caption_slot]
            .into_iter()
            .try_for_each(|s| check_values(s)),
        UiNode::FeedList { item_template, .. } => check_values(item_template),
        UiNode::Text { .. }
        | UiNode::Button { .. }
        | UiNode::AuthorHeader { .. }
        | UiNode::ActionBar { .. } => Ok(()),
    }
}

fn collect_intents<'a>(node: &'a UiNode, out: &mut Vec<&'a ActionIntent>) {
    match node {
        UiNode::Workspace { slots, .. } => {
            for s in slots {
                collect_intents(&s.content, out);
            }
        }
        UiNode::Container { children, .. } => {
            for c in children {
                collect_intents(c, out);
            }
        }
        UiNode::Text { .. } | UiNode::MediaCarousel { .. } => {}
        UiNode::Button { on_click, .. } => out.push(on_click),
        UiNode::PostCard {
            author_slot,
            media_slot,
            action_slot,
            caption_slot,
            ..
        } => {
            for s in [author_slot, media_slot, action_slot, caption_slot] {
                collect_intents(s, out);
            }
        }
        UiNode::AuthorHeader { on_click, .. } => out.extend(on_click.as_ref()),
        UiNode::ActionBar {
            like_intent,
            bookmark_intent,
            comment_intent,
        } => out.extend([like_intent, bookmark_intent, comment_intent]),
        UiNode::FeedList { item_template, .. } => collect_intents(item_template, out),
    }
}

// ---------- route ----------

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v1/agent/modify_layout", post(modify_layout))
}

fn bad_request(detail: &str) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_request", "detail": detail,
                "expected": { "prompt": "non-empty string", "workspace_id": "optional string" } }),
    )
}

/// `POST /v1/agent/modify_layout {"prompt", "workspace_id"?}`.
async fn modify_layout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = layouts::require_agent_auth(&state, &headers).await {
        return r;
    }
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let Some(prompt) = payload["prompt"]
        .as_str()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return bad_request("`prompt` must be a non-empty string");
    };
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return bad_request("`prompt` is too long (max 4000 characters)");
    }
    let workspace_id = match &payload["workspace_id"] {
        Value::Null => DEFAULT_WORKSPACE,
        Value::String(s) => s.as_str(),
        _ => return bad_request("`workspace_id` must be a string"),
    };
    if let Some(r) = layouts::reject_invalid_workspace_id(workspace_id) {
        return r;
    }
    if !layouts::is_versioned(workspace_id) {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "workspace_not_found", "id": workspace_id,
                    "versioned": layouts::VERSIONED }),
        );
    }
    let (provider_name, model) = state.agent().describe();
    let provider = match state.agent() {
        Selection::Ready(p) => p.clone(),
        Selection::NotConfigured { detail, .. } => {
            tracing::warn!(provider = %provider_name, %prompt, outcome = "provider_not_configured", %detail, "agent prompt");
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "provider_not_configured", "provider": provider_name, "detail": detail }),
            );
        }
    };
    let started = Instant::now();
    let (base_version, layout) = match layouts::latest_async(&state, workspace_id).await {
        Ok(Some(x)) => x,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                json!({ "error": "workspace_not_found", "id": workspace_id }),
            );
        }
        Err(e) => return e.into_response(),
    };
    let layout_json = match serde_json::to_value(&layout) {
        Ok(v) => v,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "internal", "detail": e.to_string() }),
            );
        }
    };
    let log = |outcome: &str, ops: usize, detail: &str| {
        tracing::info!(
            provider = %provider_name,
            model = model.as_deref().unwrap_or("-"),
            workspace = workspace_id,
            %prompt,
            ops,
            outcome,
            detail,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "agent prompt"
        );
    };

    let generated = match provider.generate(prompt, &layout_json, registry()).await {
        Ok(g) => g,
        Err(err) => {
            let (status, body, outcome) = match err {
                ProviderError::Unsupported { detail } => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({ "error": "unsupported_prompt", "provider": provider_name,
                            "detail": detail.unwrap_or_else(|| "the rules provider does not understand this prompt".to_owned()),
                            "supported": rules::SUPPORTED }),
                    "unsupported_prompt",
                ),
                ProviderError::UseRollback => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({ "error": "use_rollback", "provider": provider_name,
                            "detail": format!("resetting is not a patch: POST /v1/workspaces/{workspace_id}/rollback {{\"version\": 1}} restores the built-in layout"),
                            "rollback": format!("/v1/workspaces/{workspace_id}/rollback") }),
                    "use_rollback",
                ),
                ProviderError::NotConfigured(detail) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({ "error": "provider_not_configured", "provider": provider_name, "detail": detail }),
                    "provider_not_configured",
                ),
                ProviderError::Failed(detail) => (
                    StatusCode::BAD_GATEWAY,
                    json!({ "error": "provider_error", "provider": provider_name, "model": model, "detail": detail }),
                    "provider_error",
                ),
            };
            log(outcome, 0, body["detail"].as_str().unwrap_or_default());
            return json_error(status, body);
        }
    };
    let patch = Value::Array(generated.ops.clone());
    let op_count = generated.ops.len();

    let respond = |version: i64, workspace: &UiNode, unchanged: bool| {
        let mut body = json!({
            "version": crate::pixelfed::layout::BUNDLE_VERSION,
            "workspace": workspace,
            "entities": {},
            "layout_version": version,
            "provider": provider_name,
            "model": model,
            "patch": patch,
        });
        if let Some(e) = &generated.explanation {
            body["explanation"] = json!(e);
        }
        if unchanged {
            body["unchanged"] = json!(true);
        }
        ([layouts::version_header(version)], Json(body)).into_response()
    };

    if generated.ops.is_empty() {
        // Already in the requested state (e.g. a second "add a weather
        // widget"): no new version.
        log(
            "unchanged",
            0,
            generated.explanation.as_deref().unwrap_or_default(),
        );
        return respond(base_version, &layout, true);
    }

    // Apply to the version the patch was made for. If another version was
    // committed meanwhile (a concurrent prompt or a rollback during a slow
    // provider call), re-apply the same patch on the new latest instead of
    // overwriting that change with a layout derived from the old one.
    let (mut base_version, mut layout, mut layout_json) = (base_version, layout, layout_json);
    let first_base = base_version;
    let mut attempt = 0;
    loop {
        let new_layout = match apply_and_validate(&layout_json, &generated.ops) {
            Ok(node) => node,
            Err(detail) => {
                log("invalid_patch", op_count, &detail);
                let detail = if base_version == first_base {
                    detail
                } else {
                    format!(
                        "{detail} (the layout changed to v{base_version} while the patch for \
                         v{first_base} was generated)"
                    )
                };
                return json_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({ "error": "invalid_patch", "detail": detail, "patch": patch,
                            "provider": provider_name, "model": model }),
                );
            }
        };
        if new_layout == layout {
            log("unchanged", op_count, "patch is a no-op");
            return respond(base_version, &layout, true);
        }
        let meta = layouts::LayoutMeta {
            prompt: Some(prompt.to_owned()),
            provider: Some(provider_name.clone()),
            patch_json: Some(patch.to_string()),
        };
        match layouts::commit_on_and_publish(
            &state,
            workspace_id,
            base_version,
            new_layout.clone(),
            meta,
        )
        .await
        {
            Ok(Some(version)) => {
                log(
                    "committed",
                    op_count,
                    &format!("v{base_version} -> v{version}"),
                );
                return respond(version, &new_layout, false);
            }
            Ok(None) if attempt < MAX_COMMIT_RETRIES => attempt += 1,
            Ok(None) => {
                log("conflict", op_count, "");
                return json_error(
                    StatusCode::CONFLICT,
                    json!({ "error": "conflict", "provider": provider_name,
                            "detail": "the layout kept changing; try again" }),
                );
            }
            Err(e) => {
                log("store_error", op_count, "");
                return e.into_response();
            }
        }
        (base_version, layout) = match layouts::latest_async(&state, workspace_id).await {
            Ok(Some(x)) => x,
            Ok(None) => return internal("the workspace lost its versions"),
            Err(e) => return e.into_response(),
        };
        layout_json = match serde_json::to_value(&layout) {
            Ok(v) => v,
            Err(e) => return internal(&e.to_string()),
        };
    }
}

/// Commit attempts after the first when the layout changed underneath.
const MAX_COMMIT_RETRIES: u32 = 3;

fn internal(detail: &str) -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "error": "internal", "detail": detail }),
    )
}
