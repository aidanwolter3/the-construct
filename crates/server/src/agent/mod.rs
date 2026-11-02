//! Generative layout agent (M7, D25), kept in the repo but NOT routed: clients
//! cannot change layouts on the pod any more (layouts are code, see
//! [`crate::layouts`]). What remains: the [`LayoutProvider`] trait with the
//! offline [`rules`] and [`anthropic`] providers (prompt -> RFC 6902 patch),
//! [`apply_and_validate`] for patches, and [`validate_structure`], which the
//! startup sync of `layouts/` uses for developer-authored layout files. These
//! are public library items, so they compile (and their tests run) without a
//! route using them.

pub mod anthropic;
pub mod rules;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};

use protocol::{ActionIntent, UiNode};
use serde_json::Value;

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

    /// From `CONSTRUCT_LLM_PROVIDER`, `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`,
    /// `CONSTRUCT_LLM_MODEL`, `CONSTRUCT_LLM_FALLBACKS`.
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
        let choice = var("CONSTRUCT_LLM_PROVIDER")
            .unwrap_or_else(|| "auto".to_owned())
            .to_ascii_lowercase();
        let key = var("ANTHROPIC_API_KEY");
        let anthropic = |key: String| {
            let mut config = anthropic::AnthropicConfig::new(key);
            if let Some(url) = var("ANTHROPIC_BASE_URL") {
                config.base_url = url;
            }
            if let Some(model) = var("CONSTRUCT_LLM_MODEL") {
                config.model = model;
            }
            config.fallbacks = !var("CONSTRUCT_LLM_FALLBACKS")
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
                detail: format!("unknown CONSTRUCT_LLM_PROVIDER '{other}' (auto|anthropic|rules)"),
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
        return Err(format!(
            "patch is {patch_size} bytes (max {MAX_PATCH_BYTES})"
        ));
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

/// Validates a patched layout against the original root: everything
/// [`validate_structure`] checks (with the original's workspace id), plus
/// mutation, navigation and sign-in intents may only be copies of intents the
/// original already has (a patch must not invent endpoints, entity targets or
/// instance URLs; the model is told the same).
pub fn validate(original: &Value, doc: &Value) -> Result<UiNode, String> {
    let original_id = original["id"].as_str().unwrap_or_default();
    let node = validate_structure(original_id, doc)?;
    let original_node = serde_json::from_value::<UiNode>(original.clone()).ok();
    let mut existing = Vec::new();
    if let Some(original) = &original_node {
        collect_intents(original, &mut existing);
    }
    let mut intents = Vec::new();
    collect_intents(&node, &mut intents);
    for intent in intents {
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

/// Structural validation of a workspace layout, without any intent
/// restriction (used directly for developer-authored layout files): depth and
/// size bounds, deserializes as a [`UiNode`], the root is a `core://Workspace`
/// with id `expected_id`, 1..=[`MAX_SLOTS`] slots with unique `slot_id`s,
/// numeric fields in range (aspect ratio, padding), and every mutation
/// endpoint is a pod path (`/v1/...`).
pub fn validate_structure(expected_id: &str, doc: &Value) -> Result<UiNode, String> {
    check_bounds(doc)?;
    let node: UiNode = serde_json::from_value(doc.clone())
        .map_err(|e| format!("layout does not deserialize as a UiNode: {e}"))?;
    let UiNode::Workspace { id, slots, .. } = &node else {
        return Err("root must stay a core://Workspace".to_owned());
    };
    if id != expected_id {
        return Err(format!(
            "workspace id changed from '{expected_id}' to '{id}'"
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
    check_screens(&node)?;
    let mut intents = Vec::new();
    collect_intents(&node, &mut intents);
    for intent in intents {
        let path = match intent {
            ActionIntent::OptimisticMutate { endpoint, .. }
            | ActionIntent::Create { endpoint, .. } => endpoint,
            ActionIntent::LoadData { path } => path,
            _ => continue,
        };
        if !path.starts_with("/v1/") {
            return Err(format!(
                "endpoint '{path}' must be a pod path starting with /v1/"
            ));
        }
    }
    Ok(node)
}

/// Screens (docs/DECISIONS.md D34): unique ids, a route like
/// `scheme://a/{param}/b` (each `{param}` a whole segment, named
/// `[a-z_]+`, used once), `data` a pod path, and every `$route.<name>` used
/// by the screen naming one of its route's parameters.
fn check_screens(node: &UiNode) -> Result<(), String> {
    let UiNode::Workspace { screens, .. } = node else {
        return Ok(());
    };
    let mut ids = std::collections::HashSet::new();
    for screen in screens {
        let fail = |e: String| Err(format!("screen '{}': {e}", screen.id));
        if screen.id.is_empty() || !ids.insert(screen.id.as_str()) {
            return fail("ids must be unique and non-empty".into());
        }
        let params = match route_params(&screen.route) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        if let Some(data) = &screen.data
            && !data.starts_with("/v1/")
        {
            return fail(format!(
                "data '{data}' must be a pod path starting with /v1/"
            ));
        }
        let text = serde_json::to_string(screen).map_err(|e| e.to_string())?;
        for used in text.split("$route.").skip(1) {
            let name: String = used
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !params.contains(&name) {
                return fail(format!(
                    "'$route.{name}' is not a parameter of route '{}'",
                    screen.route
                ));
            }
        }
    }
    Ok(())
}

/// The `{param}` names of a screen route; `Err` if the route is malformed.
pub fn route_params(route: &str) -> Result<Vec<String>, String> {
    let Some((scheme, rest)) = route.split_once("://") else {
        return Err(format!("route '{route}' must look like scheme://path"));
    };
    if scheme.is_empty() || rest.is_empty() {
        return Err(format!("route '{route}' must look like scheme://path"));
    }
    let mut params = Vec::new();
    for seg in rest.split('/') {
        if seg.is_empty() {
            return Err(format!("route '{route}' has an empty segment"));
        }
        if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                return Err(format!(
                    "route parameter '{{{name}}}' must be named [a-z_]+"
                ));
            }
            if params.iter().any(|p| p == name) {
                return Err(format!("route parameter '{{{name}}}' is used twice"));
            }
            params.push(name.to_owned());
        } else if seg.contains('{') || seg.contains('}') {
            return Err(format!("route segment '{seg}' mixes text and a parameter"));
        }
    }
    Ok(params)
}

/// Numeric fields the renderers cannot draw sensibly (or at all: a
/// non-finite `f32` would even serialize as `null` and make the stored
/// layout unreadable).
fn check_values(node: &UiNode) -> Result<(), String> {
    match node {
        UiNode::Workspace { slots, screens, .. } => {
            slots.iter().try_for_each(|s| check_values(&s.content))?;
            screens.iter().try_for_each(|s| {
                check_values(&s.content)?;
                s.footer.as_ref().map_or(Ok(()), check_values)
            })
        }
        UiNode::List {
            item_template,
            indent,
            ..
        } => {
            if let Some(i) = indent
                && (i.step_dp > MAX_PADDING_DP || i.max.unwrap_or(4) > 8)
            {
                return Err(format!(
                    "indent step_dp {} / max {:?} too large (max {MAX_PADDING_DP} dp, 8 steps)",
                    i.step_dp, i.max
                ));
            }
            check_values(item_template)
        }
        UiNode::Image {
            size_dp,
            aspect_ratio,
            ..
        } => {
            if let Some(a) = aspect_ratio
                && !ASPECT_RATIO_RANGE.contains(a)
            {
                return Err(format!("image aspect_ratio {a} out of range"));
            }
            if size_dp.is_some_and(|s| s == 0 || s > 512) {
                return Err(format!("image size_dp {size_dp:?} (1..=512)"));
            }
            Ok(())
        }
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
        | UiNode::TextField { .. }
        | UiNode::AuthorHeader { .. }
        | UiNode::ActionBar { .. } => Ok(()),
    }
}

/// Every action intent in the tree.
pub fn intents(node: &UiNode) -> Vec<&ActionIntent> {
    let mut out = Vec::new();
    collect_intents(node, &mut out);
    out
}

fn collect_intents<'a>(node: &'a UiNode, out: &mut Vec<&'a ActionIntent>) {
    match node {
        UiNode::Workspace { slots, screens, .. } => {
            for s in slots {
                collect_intents(&s.content, out);
            }
            for s in screens {
                collect_intents(&s.content, out);
                if let Some(f) = &s.footer {
                    collect_intents(f, out);
                }
            }
        }
        UiNode::List { item_template, .. } => collect_intents(item_template, out),
        UiNode::Image { .. } => {}
        UiNode::TextField { on_submit, .. } => out.extend(on_submit.as_ref()),
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
