//! Client schema versions (docs/DECISIONS.md D34, D36, D37). A client sends
//! the newest layout vocabulary it can draw in [`SCHEMA_HEADER`]. Every layout
//! version declares the schema it was written for (`"schema"` in its layout
//! file); the pod serves an app's newest version the client can draw
//! ([`compatibility`]).
//!
//! - 1: everything before screens (no header means 1), including the removed
//!   `pixelfed://` widgets, which this build reads as [`UiNode::Unknown`].
//! - 2: `Workspace.screens`, `core://List`, `core://Image`,
//!   `core://TextField`, `action.data.load`, `action.mutation.create`.
//!   Additive: schema 2 clients still draw schema 1 layouts.
//! - 3: `core://Row`, `Column`, `Spacer`, `Divider`, `Conditional`,
//!   `Carousel`, `IconButton`, `Chip`, `Badge`, and the `Label` / `Secondary`
//!   text styles. Breaking (D37): the `pixelfed://` widgets are gone, so
//!   schema 3 clients draw only layouts written for schema 3.
//! - 4: `core://Tappable` (any node made tappable, D42). Additive: schema 4
//!   clients still draw schema 3 layouts.
//! - 5: `core://List` `filter` (client-side search as you type) and
//!   `group_by` (headers drawn by the list, D43). Additive: schema 5 clients
//!   still draw schema 3 and 4 layouts.
//!
//! # A breaking schema change
//!
//! Bump [`SCHEMA_VERSION`], then add `(new version, oldest layout schema it
//! still draws)` to [`RETIRED`]. That marks every older layout version
//! incompatible for clients of the new schema (greyed out in their version
//! picker, never served to them) without touching the layout files. Apps whose
//! old layouts would still draw need a new version file declaring the new
//! schema.

use crate::{ActionIntent, TextStyle, UiNode};

/// Request header carrying the client's schema version.
pub const SCHEMA_HEADER: &str = "x-construct-schema";
/// The schema version this build draws.
pub const SCHEMA_VERSION: u32 = 5;

/// Breaking changes, oldest first: `(client schema, oldest layout schema it
/// draws)`. A client of schema `c` draws layouts written for schemas
/// `oldest_layout_schema(c)..=c`. Schema 3 removed the `pixelfed://` widgets
/// (D37).
pub const RETIRED: &[(u32, u32)] = &[(3, 3)];

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

fn intent_schema(i: &ActionIntent) -> u32 {
    match i {
        ActionIntent::LoadData { .. } | ActionIntent::Create { .. } => 2,
        _ => 1,
    }
}

fn node_schema(node: &UiNode) -> u32 {
    match node {
        UiNode::Workspace { screens, .. } if !screens.is_empty() => 2,
        UiNode::List {
            filter, group_by, ..
        } if filter.is_some() || group_by.is_some() => 5,
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
        UiNode::Tappable { .. } => 4,
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
        let tappable = node(
            json!({"type": "core://List", "query_key": "q", "item_template":
            {"type": "core://Tappable",
             "on_click": {"intent": "action.navigation.open", "target_uri": "x://$item.id",
                          "presentation": "Push"},
             "content": {"type": "core://Text", "value_expr": "$item.title", "style": "Body"}}}),
        );
        assert_eq!(required_schema(&tappable), 4);
        // Its intent is checked like a button's, and its content is a child.
        let t = &tappable.children()[0];
        assert_eq!(t.own_intents().len(), 1);
        assert_eq!(t.children().len(), 1);
        // A filtered or grouped list is schema 5 (D43); the group header is a child.
        let filtered = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Text", "value_expr": "$item.title", "style": "Body"},
            "filter": {"value": "$form.q", "fields": ["title"]}}));
        assert_eq!(required_schema(&filtered), 5);
        let grouped = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"},
            "group_by": {"field": "category", "header":
                {"type": "core://Tappable", "on_click": {"intent": "action.navigation.back"},
                 "content": {"type": "core://Divider"}}}}));
        assert_eq!(required_schema(&grouped), 5);
        assert_eq!(grouped.children().len(), 2);
        assert_eq!(grouped.walk().len(), 4);
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
                "core://Tappable" => {
                    json!({"type": t, "on_click": {"intent": "action.navigation.back"},
                                            "content": {"type": "core://Divider"}})
                }
                other => panic!("no sample for {other}"),
            };
            let n: UiNode = serde_json::from_value(v).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert!(!matches!(n, UiNode::Unknown(_)), "{t}");
            assert_eq!(n.type_tag(), t);
        }
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
        assert_eq!(compatibility(3, 2), UpdateRequired);
        assert_eq!(UpdateRequired.reason(), Some("update_required"));
        assert_eq!(Compatible.reason(), None);
        // Schema 3 (D37) retired everything older: the pixelfed:// widgets are gone.
        assert_eq!(oldest_layout_schema(2), 1);
        assert_eq!(oldest_layout_schema(3), 3);
        assert_eq!(compatibility(2, 3), Retired);
        assert_eq!(compatibility(1, 3), Retired);
        assert_eq!(compatibility(3, 3), Compatible);
        assert_eq!(Retired.reason(), Some("retired"));
        // Schema 4 (D42) is additive: it still draws schema 3 layouts.
        assert_eq!(oldest_layout_schema(4), 3);
        assert_eq!(compatibility(3, 4), Compatible);
        assert_eq!(compatibility(4, 3), UpdateRequired);
        assert_eq!(compatibility(2, 4), Retired);
        // Schema 5 (D43) is additive too.
        assert_eq!(oldest_layout_schema(5), 3);
        assert_eq!(compatibility(3, 5), Compatible);
        assert_eq!(compatibility(4, 5), Compatible);
        assert_eq!(compatibility(5, 4), UpdateRequired);
    }
}
