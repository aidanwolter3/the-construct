//! Client schema versions (docs/DECISIONS.md D34-D36). A client sends the
//! newest layout vocabulary it can draw in [`SCHEMA_HEADER`]; the pod serves an
//! app's newest layout version whose [`required_schema`] is not above it.
//!
//! - 1: everything before screens (no header means 1), including the removed
//!   `pixelfed://` widgets, which this build reads as [`UiNode::Unknown`].
//! - 2: `Workspace.screens`, `core://List`, `core://Image`,
//!   `core://TextField`, `action.data.load`, `action.mutation.create`.
//! - 3: `core://Row`, `Column`, `Spacer`, `Divider`, `Conditional`,
//!   `Carousel`, `IconButton`, `Chip`, `Badge`, and the `Label` / `Secondary`
//!   text styles.

use crate::{ActionIntent, TextStyle, UiNode};

/// Request header carrying the client's schema version.
pub const SCHEMA_HEADER: &str = "x-construct-schema";
/// The schema version this build draws.
pub const SCHEMA_VERSION: u32 = 3;

fn intent_schema(i: &ActionIntent) -> u32 {
    match i {
        ActionIntent::LoadData { .. } | ActionIntent::Create { .. } => 2,
        _ => 1,
    }
}

fn node_schema(node: &UiNode) -> u32 {
    match node {
        UiNode::Workspace { screens, .. } if !screens.is_empty() => 2,
        UiNode::List { .. } | UiNode::Image { .. } | UiNode::TextField { .. } => 2,
        UiNode::Text {
            style: TextStyle::Label | TextStyle::Secondary,
            ..
        } => 3,
        UiNode::Row { .. }
        | UiNode::Column { .. }
        | UiNode::Spacer { .. }
        | UiNode::Divider {}
        | UiNode::Conditional { .. }
        | UiNode::Carousel { .. }
        | UiNode::IconButton { .. }
        | UiNode::Chip { .. }
        | UiNode::Badge { .. } => 3,
        _ => 1,
    }
}

/// The lowest schema version that can draw `node` (and its screens).
pub fn required_schema(node: &UiNode) -> u32 {
    node.walk()
        .into_iter()
        .map(|n| {
            n.own_intents()
                .into_iter()
                .map(intent_schema)
                .fold(node_schema(n), u32::max)
        })
        .fold(1, u32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(v: serde_json::Value) -> UiNode {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn each_vocabulary_has_its_schema() {
        let old = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "core://Button", "label": "Go",
             "on_click": {"intent": "action.navigation.open", "target_uri": "x://y", "presentation": "Push"}}}]}),
        );
        assert_eq!(required_schema(&old), 1);
        // A removed widget is an unknown node: schema 1 (the clients that drew it).
        let legacy = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "pixelfed://FeedList",
             "query_key": "q", "item_template": {"type": "core://Text", "value_expr": "x", "style": "Body"}}}]}),
        );
        assert_eq!(required_schema(&legacy), 1);
        let list = node(json!({"type": "core://List", "query_key": "q",
             "item_template": {"type": "core://Text", "value_expr": "$item.text", "style": "Body"}}));
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
        let row = node(json!({"type": "core://Row", "children": [
            {"type": "core://Text", "value_expr": "a", "style": "Body"}]}));
        assert_eq!(required_schema(&row), 3);
        let label = node(json!({"type": "core://Text", "value_expr": "a", "style": "Label"}));
        assert_eq!(required_schema(&label), 3);
        let deep = node(
            json!({"type": "core://List", "query_key": "q", "item_template":
            {"type": "core://IconButton", "icon": "favorite", "content_description": "Like",
             "on_click": {"intent": "action.navigation.back"}}}),
        );
        assert_eq!(required_schema(&deep), 3);
    }

    #[test]
    fn unknown_nodes_round_trip_unchanged() {
        let raw = json!({"type": "pixelfed://ActionBar", "like_intent": {"intent": "x"}, "n": 1.5});
        let n: UiNode = serde_json::from_value(raw.clone()).unwrap();
        assert!(matches!(&n, UiNode::Unknown(_)));
        assert_eq!(n.type_tag(), "pixelfed://ActionBar");
        assert_eq!(serde_json::to_value(&n).unwrap(), raw);
        // Inside a known node too.
        let tree = json!({"type": "core://Column", "children": [raw.clone()], "spacing_dp": 4});
        let t: UiNode = serde_json::from_value(tree.clone()).unwrap();
        assert_eq!(t.walk().len(), 2);
        let back = serde_json::to_value(&t).unwrap();
        assert_eq!(back["children"][0], raw);
        // A known type must still be valid.
        assert!(serde_json::from_value::<UiNode>(json!({"type": "core://Text"})).is_err());
    }

    #[test]
    fn every_known_type_parses_and_reports_its_tag() {
        for t in crate::NODE_TYPES {
            let v = match t {
                "core://Workspace" => json!({"type": t, "id": "a", "title": "A", "slots": []}),
                "core://Container" => {
                    json!({"type": t, "orientation": "Vertical", "padding_dp": 0, "children": []})
                }
                "core://Text" => json!({"type": t, "value_expr": "x", "style": "Body"}),
                "core://Button" => {
                    json!({"type": t, "label": "x", "on_click": {"intent": "action.navigation.back"}})
                }
                "core://List" => {
                    json!({"type": t, "query_key": "q", "item_template": {"type": "core://Divider"}})
                }
                "core://Image" => json!({"type": t, "url": "u"}),
                "core://TextField" => json!({"type": t, "field": "f"}),
                "core://Row" | "core://Column" => json!({"type": t, "children": []}),
                "core://Spacer" | "core://Divider" => json!({"type": t}),
                "core://Conditional" => {
                    json!({"type": t, "value": "$item.x", "content": {"type": "core://Divider"}})
                }
                "core://Carousel" => {
                    json!({"type": t, "items": "$item.m", "item_template": {"type": "core://Divider"}})
                }
                "core://IconButton" => {
                    json!({"type": t, "icon": "send", "content_description": "x",
                                              "on_click": {"intent": "action.navigation.back"}})
                }
                "core://Chip" => json!({"type": t, "text": "x"}),
                "core://Badge" => json!({"type": t, "text": "x", "icon": "repeat"}),
                other => panic!("no sample for {other}"),
            };
            let n: UiNode = serde_json::from_value(v).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert!(!matches!(n, UiNode::Unknown(_)), "{t}");
            assert_eq!(n.type_tag(), t);
        }
    }
}
