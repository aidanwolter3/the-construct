//! Deterministic, offline layout provider (M7 contract "Rules provider").
//!
//! Case-insensitive phrase matching. Every rule walks the CURRENT layout JSON
//! (no hard-coded indices) and emits RFC 6902 `replace` / `add` / `remove`
//! ops with explicit values. Rules compose: the ones named in a prompt run in
//! the order they appear, each on the result of the previous ones, and their
//! ops are concatenated into one patch. A rule whose target is already in the
//! requested state emits nothing (idempotent).

use serde_json::{Value, json};

use super::{GenerateFuture, Generated, LayoutProvider, ProviderError};

/// Example prompts reported with `unsupported_prompt`.
pub const SUPPORTED: [&str; 8] = [
    "Hide post captions",
    "Show captions",
    "Put media at the very top",
    "Photos first",
    "Move the action bar above the photo",
    "Add a weather widget at the top",
    "Reset layout",
    "Remove the action bar (rejected on purpose: demonstrates validation)",
];

pub const NAME: &str = "rules";

const POST_CARD: &str = "pixelfed://PostCard";
const MEDIA: &str = "pixelfed://MediaCarousel";
const ACTION_BAR: &str = "pixelfed://ActionBar";
const TEXT: &str = "core://Text";
/// PostCard slots in render order (the renderer draws whatever node each holds).
const SLOTS: [&str; 4] = ["author_slot", "media_slot", "action_slot", "caption_slot"];
const WEATHER_SLOT: &str = "weather";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    HideCaptions,
    ShowCaptions,
    MediaTop,
    ActionAbovePhoto,
    AddWeather,
    Reset,
    RemoveActionBar,
}

impl Rule {
    const ALL: [Rule; 7] = [
        Rule::HideCaptions,
        Rule::ShowCaptions,
        Rule::MediaTop,
        Rule::ActionAbovePhoto,
        Rule::AddWeather,
        Rule::Reset,
        Rule::RemoveActionBar,
    ];

    /// Phrases (already normalized: lowercase, single spaces) that select the rule.
    fn phrases(self) -> &'static [&'static str] {
        match self {
            Rule::HideCaptions => &[
                "hide captions",
                "hide caption",
                "hide post captions",
                "hide the captions",
                "hide the post captions",
            ],
            Rule::ShowCaptions => &[
                "show captions",
                "show caption",
                "show post captions",
                "show the captions",
                "show the post captions",
                "restore captions",
                "restore the captions",
            ],
            Rule::MediaTop => &[
                "media at the top",
                "media at the very top",
                "photos first",
                "photo first",
                "photos at the top",
                "photos at the very top",
            ],
            Rule::ActionAbovePhoto => &[
                "action bar above the photo",
                "action bar above the photos",
                "action bar above the media",
                "actions above the photo",
            ],
            Rule::AddWeather => &[
                "add a weather widget",
                "add weather widget",
                "add the weather widget",
            ],
            Rule::Reset => &["reset layout", "reset the layout"],
            Rule::RemoveActionBar => &["remove the action bar", "remove action bar"],
        }
    }
}

/// Lowercase, every non-alphanumeric run -> one space, padded with spaces so
/// phrases match on word boundaries via `contains(" phrase ")`.
fn normalize(s: &str) -> String {
    let mut out = String::from(" ");
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// The rules named in `prompt`, in order of first appearance.
fn match_rules(prompt: &str) -> Vec<Rule> {
    let text = normalize(prompt);
    let mut found: Vec<(usize, Rule)> = Rule::ALL
        .iter()
        .filter_map(|&rule| {
            rule.phrases()
                .iter()
                .filter_map(|p| text.find(&format!(" {p} ")))
                .min()
                .map(|pos| (pos, rule))
        })
        .collect();
    found.sort_by_key(|(pos, _)| *pos);
    found.into_iter().map(|(_, r)| r).collect()
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// JSON pointers of every `pixelfed://PostCard` object under `v`.
fn post_cards(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some(POST_CARD) {
                out.push(path.to_owned());
            }
            for (k, child) in map {
                post_cards(child, &format!("{path}/{}", escape_pointer(k)), out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                post_cards(child, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

fn pointer_get<'a>(doc: &'a Value, path: &str) -> &'a Value {
    doc.pointer(path).unwrap_or(&Value::Null)
}

fn kind(node: &Value) -> Option<&str> {
    node.get("type").and_then(Value::as_str)
}

fn blank_caption() -> Value {
    json!({ "type": TEXT, "value_expr": "", "style": "Caption" })
}

fn body_caption() -> Value {
    json!({ "type": TEXT, "value_expr": "entity.caption", "style": "Body" })
}

/// The weather placeholder: a padded Container, so the text does not touch
/// the screen edge.
pub fn weather_content() -> Value {
    json!({
        "type": "core://Container",
        "orientation": "Vertical",
        "padding_dp": 16,
        "children": [{
            "type": TEXT,
            "value_expr": "Weather widget (not available yet)",
            "style": "Caption"
        }]
    })
}

/// The four slot nodes of the card at `card` (render order).
fn slot_nodes(doc: &Value, card: &str) -> Vec<Value> {
    let card = pointer_get(doc, card);
    SLOTS.iter().map(|s| card[*s].clone()).collect()
}

/// `replace` ops turning the card's slots into `order` (only changed slots).
fn reorder_ops(doc: &Value, card: &str, order: &[Value]) -> Vec<Value> {
    let current = slot_nodes(doc, card);
    SLOTS
        .iter()
        .zip(current.iter().zip(order))
        .filter(|(_, (old, new))| old != new)
        .map(|(slot, (_, new))| json!({ "op": "replace", "path": format!("{card}/{slot}"), "value": new }))
        .collect()
}

/// Index of the slot holding the caption: `caption_slot` if it holds a Text,
/// else the first slot that does.
fn caption_index(nodes: &[Value]) -> Option<usize> {
    if kind(&nodes[3]) == Some(TEXT) {
        return Some(3);
    }
    nodes.iter().position(|n| kind(n) == Some(TEXT))
}

/// A rule's ops on `doc`, or why it does not apply to this layout.
fn rule_ops(rule: Rule, doc: &Value) -> Result<Vec<Value>, String> {
    let mut cards = Vec::new();
    post_cards(doc, "", &mut cards);
    let no_card = || "this layout has no pixelfed://PostCard".to_owned();
    match rule {
        Rule::HideCaptions | Rule::ShowCaptions => {
            let want = if rule == Rule::HideCaptions {
                blank_caption()
            } else {
                body_caption()
            };
            let mut ops = Vec::new();
            let mut any = false;
            for card in &cards {
                let nodes = slot_nodes(doc, card);
                let Some(i) = caption_index(&nodes) else {
                    continue;
                };
                any = true;
                if nodes[i] != want {
                    ops.push(json!({ "op": "replace", "path": format!("{card}/{}", SLOTS[i]), "value": want }));
                }
            }
            if any { Ok(ops) } else { Err(no_card()) }
        }
        Rule::MediaTop | Rule::ActionAbovePhoto => {
            let mut ops = Vec::new();
            let mut any = false;
            for card in &cards {
                let mut nodes = slot_nodes(doc, card);
                let Some(m) = nodes.iter().position(|n| kind(n) == Some(MEDIA)) else {
                    continue;
                };
                let (from, to) = if rule == Rule::MediaTop {
                    any = true;
                    (m, 0)
                } else {
                    let Some(a) = nodes.iter().position(|n| kind(n) == Some(ACTION_BAR)) else {
                        continue;
                    };
                    any = true;
                    if a < m {
                        continue; // already above the photo
                    }
                    (a, m)
                };
                // Move the node to `to`, keeping the others' relative order.
                let node = nodes.remove(from);
                nodes.insert(to, node);
                ops.extend(reorder_ops(doc, card, &nodes));
            }
            if any {
                Ok(ops)
            } else if cards.is_empty() {
                Err(no_card())
            } else {
                Err(format!(
                    "no PostCard in this layout holds a {}",
                    if rule == Rule::MediaTop {
                        "MediaCarousel"
                    } else {
                        "MediaCarousel and an ActionBar"
                    }
                ))
            }
        }
        Rule::AddWeather => {
            let Some(slots) = doc.get("slots").and_then(Value::as_array) else {
                return Err("the layout root has no slots".to_owned());
            };
            if slots
                .iter()
                .any(|s| s.get("slot_id").and_then(Value::as_str) == Some(WEATHER_SLOT))
            {
                return Ok(Vec::new()); // idempotent
            }
            Ok(vec![json!({
                "op": "add",
                "path": "/slots/0",
                "value": {
                    "slot_id": WEATHER_SLOT,
                    "provider_uri": "core://placeholder",
                    "content": weather_content()
                }
            })])
        }
        Rule::RemoveActionBar => {
            // Deliberately invalid (QA demo): PostCard slots are required, so
            // the patched layout fails validation and nothing is committed.
            let ops: Vec<Value> = cards
                .iter()
                .filter_map(|card| {
                    let nodes = slot_nodes(doc, card);
                    let i = nodes.iter().position(|n| kind(n) == Some(ACTION_BAR))?;
                    Some(json!({ "op": "remove", "path": format!("{card}/{}", SLOTS[i]) }))
                })
                .collect();
            if ops.is_empty() {
                Err("this layout has no pixelfed://ActionBar".to_owned())
            } else {
                Ok(ops)
            }
        }
        Rule::Reset => unreachable!("handled before any rule runs"),
    }
}

/// The patch for `prompt` against `layout` (the workspace root node).
pub fn patch_for(prompt: &str, layout: &Value) -> Result<Vec<Value>, ProviderError> {
    let rules = match_rules(prompt);
    if rules.is_empty() {
        return Err(ProviderError::Unsupported { detail: None });
    }
    if rules.contains(&Rule::Reset) {
        return Err(ProviderError::UseRollback);
    }
    let mut doc = layout.clone();
    let mut all = Vec::new();
    for rule in rules {
        let ops = rule_ops(rule, &doc).map_err(|detail| ProviderError::Unsupported {
            detail: Some(detail),
        })?;
        if ops.is_empty() {
            continue;
        }
        // Later rules see this rule's result. An op that doesn't apply here
        // would not apply in the handler either; let the handler report it.
        if let Ok(patch) = serde_json::from_value::<json_patch::Patch>(Value::Array(ops.clone())) {
            let _ = json_patch::patch(&mut doc, &patch);
        }
        all.extend(ops);
    }
    Ok(all)
}

/// The deterministic offline provider.
#[derive(Debug, Default, Clone, Copy)]
pub struct RulesProvider;

impl LayoutProvider for RulesProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn model(&self) -> Option<&str> {
        None
    }

    fn generate<'a>(
        &'a self,
        prompt: &'a str,
        layout: &'a Value,
        _registry: &'a Value,
    ) -> GenerateFuture<'a> {
        Box::pin(async move {
            Ok(Generated {
                ops: patch_for(prompt, layout)?,
                explanation: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_is_case_insensitive_ordered_and_word_bounded() {
        assert_eq!(
            match_rules("Hide post captions and put media at the very top"),
            [Rule::HideCaptions, Rule::MediaTop]
        );
        assert_eq!(
            match_rules("PHOTOS FIRST, then hide the captions!"),
            [Rule::MediaTop, Rule::HideCaptions]
        );
        assert!(match_rules("unhide captionsx").is_empty());
        assert_eq!(match_rules("please reset   the layout"), [Rule::Reset]);
    }
}
