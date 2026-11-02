//! Client schema versions (docs/DECISIONS.md D34, D36). A client sends the
//! newest layout vocabulary it can draw in [`SCHEMA_HEADER`]. Every layout
//! version declares the schema it was written for (`"schema"` in its layout
//! file); the pod serves an app's newest version the client can draw
//! ([`compatibility`]).
//!
//! - 1: everything before screens (no header means 1).
//! - 2: `Workspace.screens`, `core://List`, `core://Image`,
//!   `core://TextField`, `action.data.load`, `action.mutation.create`.
//!   Additive: schema 2 clients still draw schema 1 layouts.
//!
//! # A breaking schema change
//!
//! Bump [`SCHEMA_VERSION`], then add `(new version, oldest layout schema it
//! still draws)` to [`RETIRED`]. That marks every older layout version
//! incompatible for clients of the new schema (greyed out in their version
//! picker, never served to them) without touching the layout files.

use crate::{ActionIntent, UiNode};

/// Request header carrying the client's schema version.
pub const SCHEMA_HEADER: &str = "x-construct-schema";
/// The schema version this build draws.
pub const SCHEMA_VERSION: u32 = 2;

/// Breaking changes, oldest first: `(client schema, oldest layout schema it
/// draws)`. A client of schema `c` draws layouts written for schemas
/// `oldest_layout_schema(c)..=c`. Empty: every change so far was additive.
pub const RETIRED: &[(u32, u32)] = &[];

/// The oldest layout schema a client of schema `client` still draws (1 unless
/// a [`RETIRED`] entry at or below `client` raised it).
pub const fn oldest_layout_schema(client: u32) -> u32 {
    let mut oldest = 1;
    let mut i = 0;
    while i < RETIRED.len() {
        if RETIRED[i].0 <= client && RETIRED[i].1 > oldest {
            oldest = RETIRED[i].1;
        }
        i += 1;
    }
    oldest
}

/// Whether a client can draw a layout version, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    /// Written for a newer schema than the client's: the app needs an update.
    UpdateRequired,
    /// Written for a schema the client no longer draws ([`RETIRED`]).
    Retired,
}

impl Compatibility {
    /// The wire name (`"incompatible"` in the apps API), `None` when
    /// compatible.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Compatibility::Compatible => None,
            Compatibility::UpdateRequired => Some("update_required"),
            Compatibility::Retired => Some("retired"),
        }
    }
}

/// Whether a client of schema `client` can draw a layout written for schema
/// `layout`.
pub fn compatibility(layout: u32, client: u32) -> Compatibility {
    if layout > client {
        Compatibility::UpdateRequired
    } else if layout < oldest_layout_schema(client) {
        Compatibility::Retired
    } else {
        Compatibility::Compatible
    }
}

/// The lowest schema version that can draw `node` (and its screens).
pub fn required_schema(node: &UiNode) -> u32 {
    let mut need = 1;
    visit(node, &mut need);
    need
}

fn intent_schema(i: &ActionIntent) -> u32 {
    match i {
        ActionIntent::LoadData { .. } | ActionIntent::Create { .. } => 2,
        _ => 1,
    }
}

fn visit(node: &UiNode, need: &mut u32) {
    fn intent(need: &mut u32, i: &ActionIntent) {
        *need = (*need).max(intent_schema(i));
    }
    match node {
        UiNode::Workspace { slots, screens, .. } => {
            if !screens.is_empty() {
                *need = (*need).max(2);
            }
            for s in slots {
                visit(&s.content, need);
            }
            for s in screens {
                visit(&s.content, need);
                if let Some(f) = &s.footer {
                    visit(f, need);
                }
            }
        }
        UiNode::Container { children, .. } => children.iter().for_each(|c| visit(c, need)),
        UiNode::Button { on_click, .. } => intent(need, on_click),
        UiNode::Text { .. } | UiNode::MediaCarousel { .. } => {}
        UiNode::List { item_template, .. } => {
            *need = (*need).max(2);
            visit(item_template, need);
        }
        UiNode::Image { .. } => *need = (*need).max(2),
        UiNode::TextField { on_submit, .. } => {
            *need = (*need).max(2);
            if let Some(i) = on_submit {
                intent(need, i);
            }
        }
        UiNode::PostCard {
            author_slot,
            media_slot,
            action_slot,
            caption_slot,
            ..
        } => {
            for s in [author_slot, media_slot, action_slot, caption_slot] {
                visit(s, need);
            }
        }
        UiNode::AuthorHeader { on_click, .. } => {
            if let Some(i) = on_click {
                intent(need, i);
            }
        }
        UiNode::ActionBar {
            like_intent,
            bookmark_intent,
            comment_intent,
        } => {
            for i in [like_intent, bookmark_intent, comment_intent] {
                intent(need, i);
            }
        }
        UiNode::FeedList { item_template, .. } => visit(item_template, need),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(v: serde_json::Value) -> UiNode {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn old_vocabulary_is_schema_1_and_new_is_2() {
        let old = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "core://Button", "label": "Go",
             "on_click": {"intent": "action.navigation.open", "target_uri": "x://y", "presentation": "Push"}}}]}),
        );
        assert_eq!(required_schema(&old), 1);
        let list = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "core://List", "query_key": "q",
             "item_template": {"type": "core://Text", "value_expr": "$item.text", "style": "Body"}}}]}),
        );
        assert_eq!(required_schema(&list), 2);
        let screens = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [],
            "screens": [{"id": "s", "route": "x://{id}", "title": "S",
                         "content": {"type": "core://Text", "value_expr": "hi", "style": "Body"}}]}),
        );
        assert_eq!(required_schema(&screens), 2);
        let create = node(
            json!({"type": "core://Button", "label": "Add", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://tmp/$nonce",
            "entity": {}, "endpoint": "/v1/x", "payload": {}}}),
        );
        assert_eq!(required_schema(&create), 2);
    }

    #[test]
    fn retired_table_is_ordered_and_sane() {
        let mut last = (0, 1);
        for &(client, oldest) in RETIRED {
            assert!(client > last.0, "RETIRED is ordered by client schema");
            assert!(oldest > last.1, "each entry retires more than the last");
            assert!(oldest <= client, "a client draws its own schema");
            assert!(client <= SCHEMA_VERSION);
            last = (client, oldest);
        }
        assert!(oldest_layout_schema(SCHEMA_VERSION) <= SCHEMA_VERSION);
    }

    #[test]
    fn compatibility_rules() {
        use Compatibility::*;
        assert_eq!(compatibility(1, 2), Compatible);
        assert_eq!(compatibility(2, 2), Compatible);
        assert_eq!(compatibility(2, 1), UpdateRequired);
        assert_eq!(UpdateRequired.reason(), Some("update_required"));
        assert_eq!(Compatible.reason(), None);
        // Today nothing is retired: every schema-1 layout still draws.
        assert_eq!(oldest_layout_schema(SCHEMA_VERSION), 1);
    }
}
