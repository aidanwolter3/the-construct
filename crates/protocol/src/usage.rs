//! Usage trails (decision `usage-trails`): how the owner uses their
//! screens, reported by their devices to their own pod (`POST
//! /v1/usage`), for the pod's `usage` jack and the Anticipate job.
//!
//! Every value is a name taken from the layout, or an enum: a layout name, a
//! route *name*, a node's identity (its authored tag, else its path in the
//! tree with list indices taken out, so every row of a list is one node), an
//! intent's kind, an endpoint's kind and collection. Never bound text,
//! values, route or param values, typed text, row ids or screenshots.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where devices send their usage.
pub const USAGE_PATH: &str = "/v1/usage";
/// The most events one batch carries.
pub const MAX_BATCH: usize = 500;
/// The longest any string of an event may be.
pub const MAX_CHARS: usize = 200;

/// One thing the owner did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UsageEvent {
    /// Unix seconds.
    pub at: i64,
    /// `screen`, `tap`, `back` or `result`.
    pub kind: String,
    /// The layout it happened on (for `screen`: the one opened).
    pub layout: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// `screen`: the layout it was opened from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// `tap`: the node's identity ([`node_identity`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// `tap`: the intent's kind (`action.navigation.open`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// `tap`: the layout it opens, or the endpoint's kind
    /// (`collections/update`, `collections/actions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// `tap`, `result`: the collection the endpoint writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// `tap`: `navigated`, `sent`, `changed` or `none`; `result`: `ok`,
    /// `failed` or `refused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// `back`: how long the screen was open, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dwell_ms: Option<u64>,
}

/// Unix seconds now, on std's clock: it panics in a web build (wasm32), so
/// the renderer uses its own (`construct_render::device_data::now_secs`).
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn cap(s: &str) -> String {
    s.chars().take(MAX_CHARS).collect()
}

/// A node's identity across rows and versions: its authored tag (an id with
/// no `/`), else its tree path with list cells' indices (`i3`) and items
/// (`[collections://…]`) taken out.
pub fn node_identity(id: &str) -> String {
    if !id.contains('/') {
        return cap(id);
    }
    // A list cell keyed by its item (`[collections://meals/x1]`) is one
    // segment, whatever its URI holds.
    let mut plain = String::new();
    let mut depth = 0;
    for ch in id.chars() {
        match ch {
            '[' => {
                if depth == 0 {
                    plain.push_str("[]");
                }
                depth += 1;
            }
            ']' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            c => plain.push(c),
        }
    }
    let parts: Vec<String> = plain
        .split('/')
        .map(|seg| {
            if seg.starts_with('[') && seg.ends_with(']') {
                "[]".to_owned()
            } else if seg.len() > 1
                && seg.starts_with('i')
                && seg[1..].bytes().all(|b| b.is_ascii_digit())
            {
                "i".to_owned()
            } else {
                seg.to_owned()
            }
        })
        .collect();
    cap(&parts.join("/"))
}

/// An endpoint's kind and the collection it writes: `/v1/collections/meals/
/// update` is (`collections/update`, `meals`); an action's kind is
/// `collections/actions`. Another source's is its first two segments.
pub fn endpoint_kind(endpoint: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = endpoint
        .split('?')
        .next()
        .unwrap_or("")
        .trim_start_matches("/v1/")
        .split('/')
        .collect();
    match parts.as_slice() {
        ["collections", c, op, ..] => (cap(&format!("collections/{op}")), Some(cap(c))),
        [a, b, ..] => (cap(&format!("{a}/{b}")), None),
        [a] => (cap(a), None),
        [] => (String::new(), None),
    }
}

impl UsageEvent {
    pub fn screen(at: i64, layout: &str, route: Option<&str>, from: &str) -> UsageEvent {
        UsageEvent {
            at,
            kind: "screen".into(),
            layout: cap(layout),
            route: route.map(cap),
            from: Some(cap(from)),
            ..UsageEvent::default()
        }
    }

    pub fn tap(
        at: i64,
        layout: &str,
        node: &str,
        intent_kind: Option<&str>,
        intent: Option<&Value>,
        effect: &str,
    ) -> UsageEvent {
        // The step that writes, inside a confirm or a sequence.
        let mut endpoint = None;
        let mut opens = None;
        if let Some(i) = intent {
            let mut stack = vec![i];
            while let Some(v) = stack.pop() {
                if endpoint.is_none() {
                    endpoint = v["endpoint"].as_str();
                }
                if opens.is_none() {
                    opens = v["layout"].as_str().filter(|l| !l.contains('$'));
                }
                if let Some(t) = v.get("then") {
                    stack.push(t);
                }
                if let Some(steps) = v["steps"].as_array() {
                    stack.extend(steps.iter());
                }
            }
        }
        let (target, collection) = match (endpoint, opens) {
            (Some(e), _) => {
                let (k, c) = endpoint_kind(e);
                (Some(k), c)
            }
            (None, Some(l)) => (Some(cap(l)), None),
            _ => (None, None),
        };
        UsageEvent {
            at,
            kind: "tap".into(),
            layout: cap(layout),
            node: Some(cap(node)),
            intent: intent_kind.map(cap),
            target,
            collection,
            effect: Some(effect.into()),
            ..UsageEvent::default()
        }
    }

    pub fn back(at: i64, layout: &str, route: Option<&str>, dwell_ms: u64) -> UsageEvent {
        UsageEvent {
            at,
            kind: "back".into(),
            layout: cap(layout),
            route: route.map(cap),
            dwell_ms: Some(dwell_ms),
            ..UsageEvent::default()
        }
    }

    /// A write's outcome, from the engine's outbox.
    pub fn result(at: i64, endpoint: &str, outcome: &str) -> UsageEvent {
        let (k, c) = endpoint_kind(endpoint);
        UsageEvent {
            at,
            kind: "result".into(),
            target: Some(k),
            collection: c,
            effect: Some(outcome.into()),
            ..UsageEvent::default()
        }
    }

    /// Whether it is one the pod keeps: a known kind, every string within
    /// [`MAX_CHARS`].
    pub fn valid(&self) -> bool {
        let short = |s: &Option<String>| s.as_ref().is_none_or(|v| v.chars().count() <= MAX_CHARS);
        matches!(self.kind.as_str(), "screen" | "tap" | "back" | "result")
            && self.layout.chars().count() <= MAX_CHARS
            && short(&self.route)
            && short(&self.from)
            && short(&self.node)
            && short(&self.intent)
            && short(&self.target)
            && short(&self.collection)
            && short(&self.effect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_drop_list_places() {
        assert_eq!(node_identity("save"), "save");
        assert_eq!(node_identity("0/1/i3/0"), "0/1/i/0");
        assert_eq!(node_identity("0/[collections://meals/x1]/2"), "0/[]/2");
    }

    #[test]
    fn endpoints_are_kinds_not_paths() {
        assert_eq!(
            endpoint_kind("/v1/collections/meals/update"),
            ("collections/update".into(), Some("meals".into()))
        );
        assert_eq!(
            endpoint_kind("/v1/collections/groceries/actions/add_recipe"),
            ("collections/actions".into(), Some("groceries".into()))
        );
        assert_eq!(endpoint_kind("/v1/agents/tasks/start").0, "agents/tasks");
    }

    #[test]
    fn a_tap_names_the_write_inside_a_confirm() {
        let i = serde_json::json!({ "intent": "action.confirm", "then": { "intent": "action.sequence", "steps": [
            { "intent": "action.mutation.optimistic", "endpoint": "/v1/collections/meals/update" } ] } });
        let e = UsageEvent::tap(1, "c-x", "0/1", Some("action.confirm"), Some(&i), "sent");
        assert_eq!(e.target.as_deref(), Some("collections/update"));
        assert_eq!(e.collection.as_deref(), Some("meals"));
        assert!(e.valid());
    }
}
