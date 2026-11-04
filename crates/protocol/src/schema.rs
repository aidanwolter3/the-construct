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
//! - 6: `core://Box` (a coloured, rounded surface), `core://ChoiceChips`
//!   (chips editing a form value), `core://List` `where` (exact-match
//!   filter), `core://Column` and `core://Row` `align`, the `Display` text style, and the `flame` / `timer` icons
//!   (D44). Additive: schema 6 clients still draw schema 3 to 5 layouts.
//! - 7: `action.auth.session` (open a session from a server path: a pod code
//!   or a temporary pod, D46). Additive.
//! - 8: `core://List` `columns`, `spacing_dp` and `padding_h_dp` (a grid),
//!   and `core://Conditional` on a form value (`$form.<field>`) or with
//!   `equals` (D48). Additive.
//! - 9: `core://MediaPicker`, `core://TextField` `lines`,
//!   `action.mutation.create` `attachments` / `position` / `dismiss`, the
//!   `$form?.<field>` binding and the `add_photo` / `add` / `refresh` icons
//!   (D49, D51). Additive.
//! - 10: `core://ChoiceChips` `remember` (the client keeps the chosen value
//!   on the device, docs/decisions/remembered-form-values.md). Additive.
//! - 11: `core://Sticky` (content pinned to the top while its parent
//!   scrolls, docs/decisions/sticky-nodes.md). Additive.
//! - 12: `core://List` `focus` (scroll to one item and tint it,
//!   docs/decisions/list-focus.md). Additive.
//!
//! # Additive changes: features, not numbers
//!
//! After 12, additive vocabulary no longer bumps [`SCHEMA_VERSION`] (one
//! shared counter made every parallel change conflict,
//! docs/decisions/layout-features.md). Each addition is a named feature, one
//! file `crates/protocol/features/<name>.md` (see its README), mapped from the
//! nodes that use it in [`node_features`] / [`intent_features`]. A client
//! sends the features it draws in [`FEATURES_HEADER`]; a layout needs the
//! features [`required_features`] finds in it, worked out by the pod (layout
//! files don't declare them). [`ClientCaps::can_draw`] checks both.
//!
//! # A breaking schema change
//!
//! Bump [`SCHEMA_VERSION`], then add `(new version, oldest layout schema it
//! still draws)` to [`RETIRED`]. That marks every older layout version
//! incompatible for clients of the new schema (greyed out in their version
//! picker, never served to them) without touching the layout files. Apps whose
//! old layouts would still draw need a new version file declaring the new
//! schema.

use std::collections::BTreeSet;

use crate::{ActionIntent, Align, Chrome, Position, TextStyle, UiNode};

include!(concat!(env!("OUT_DIR"), "/features.rs"));

/// Request header carrying the client's schema version.
pub const SCHEMA_HEADER: &str = "x-construct-schema";
/// Request header carrying the features the client draws, comma-separated
/// ([`FEATURES`]). Absent means none.
pub const FEATURES_HEADER: &str = "x-construct-features";
/// The schema version this build draws: the base vocabulary. Bumped only by a
/// breaking change (with a [`RETIRED`] entry); additive vocabulary is a
/// feature ([`FEATURES`]).
pub const SCHEMA_VERSION: u32 = 12;

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

/// What a client draws: its schema and its features.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientCaps {
    pub schema: u32,
    pub features: BTreeSet<String>,
}

impl ClientCaps {
    /// What this build draws ([`SCHEMA_VERSION`], [`FEATURES`]).
    pub fn this_build() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            features: FEATURES.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    /// A client of `schema` with no features.
    pub fn schema(schema: u32) -> Self {
        Self {
            schema,
            features: BTreeSet::new(),
        }
    }

    /// `self` with the features of a [`FEATURES_HEADER`] value.
    pub fn with_features_header(mut self, value: &str) -> Self {
        self.features = parse_features(value);
        self
    }

    /// Whether this client can draw a layout written for `schema` that uses
    /// `features`, and why not.
    pub fn can_draw<S: AsRef<str>>(&self, schema: u32, features: &[S]) -> Compatibility {
        match compatibility(schema, self.schema) {
            Compatibility::Compatible
                if features.iter().any(|f| !self.features.contains(f.as_ref())) =>
            {
                Compatibility::UpdateRequired
            }
            c => c,
        }
    }
}

/// The [`FEATURES_HEADER`] value of this build.
pub fn features_header() -> String {
    FEATURES.join(",")
}

/// The feature names of a [`FEATURES_HEADER`] value (blank entries dropped).
pub fn parse_features(value: &str) -> BTreeSet<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The features `node` (and everything under it) uses, sorted.
pub fn required_features(node: &UiNode) -> Vec<&'static str> {
    let mut out = BTreeSet::new();
    for n in node.walk() {
        node_features(n, &mut out);
        for i in n.own_intents() {
            intent_features(i, &mut out);
        }
    }
    out.into_iter().collect()
}

/// The features one node uses itself (not its children). Add one arm per
/// feature, e.g. `if let UiNode::Foo { .. } = node { out.insert(feature::FOO); }`
/// (`crates/protocol/features/README.md`).
fn node_features(node: &UiNode, out: &mut BTreeSet<&'static str>) {
    use crate::feature;
    match node {
        UiNode::Progress { .. } => {
            out.insert(feature::PROGRESS);
        }
        UiNode::Backdrop { .. } => {
            out.insert(feature::BACKDROP);
        }
        UiNode::Clock { .. } => {
            out.insert(feature::CLOCK);
        }
        UiNode::Swipe { .. }
        | UiNode::Tappable {
            on_long_press: Some(_),
            ..
        } => {
            out.insert(feature::GESTURES);
        }
        UiNode::OnAppear { .. } => {
            out.insert(feature::ON_APPEAR);
        }
        UiNode::TextField {
            autofocus: true, ..
        } => {
            out.insert(feature::AUTOFOCUS);
        }
        UiNode::Workspace {
            chrome, screens, ..
        } if *chrome == Chrome::Hidden || screens.iter().any(|s| s.chrome == Chrome::Hidden) => {
            out.insert(feature::HIDDEN_CHROME);
        }
        _ => {}
    }
}

/// The features one action intent uses; see [`node_features`].
fn intent_features(intent: &ActionIntent, out: &mut BTreeSet<&'static str>) {
    match intent {
        ActionIntent::Sequence { .. } => {
            out.insert(crate::feature::ACTION_SEQUENCE);
        }
        ActionIntent::LocalSet { .. } => {
            out.insert(crate::feature::LOCAL_SET);
        }
        ActionIntent::OpenLink { .. } => {
            out.insert(crate::feature::OPEN_LINK);
        }
        ActionIntent::OpenApp { .. } => {
            out.insert(crate::feature::APP_OPEN);
        }
        ActionIntent::LaunchDeviceApp { .. } => {
            out.insert(crate::feature::DEVICE_APPS);
        }
        ActionIntent::OpenShell { .. } => {
            out.insert(crate::feature::HIDDEN_CHROME);
        }
        _ => {}
    }
}

fn intent_schema(i: &ActionIntent) -> u32 {
    // `$form?.<field>` (schema 9) anywhere in the intent.
    let optional_form = serde_json::to_string(i).is_ok_and(|j| j.contains("$form?."));
    let own = match i {
        ActionIntent::Create {
            attachments,
            position,
            dismiss,
            ..
        } if !attachments.is_empty() || *position != Position::End || *dismiss => 9,
        ActionIntent::LoadData { .. } | ActionIntent::Create { .. } => 2,
        ActionIntent::OpenSession { .. } => 7,
        _ => 1,
    };
    if optional_form { own.max(9) } else { own }
}

/// Icons added in schema 6 (D44).
const ICONS_6: [&str; 2] = ["flame", "timer"];
/// Icons added in schema 9 (D51).
const ICONS_9: [&str; 3] = ["add_photo", "add", "refresh"];

fn icon_schema(icon: &str) -> u32 {
    if ICONS_9.contains(&icon) {
        9
    } else if ICONS_6.contains(&icon) {
        6
    } else {
        3
    }
}

fn node_schema(node: &UiNode) -> u32 {
    match node {
        UiNode::Sticky { .. } => 11,
        UiNode::List { focus: Some(_), .. } => 12,
        UiNode::ChoiceChips { remember: true, .. } => 10,
        UiNode::MediaPicker { .. } => 9,
        UiNode::TextField { lines: Some(_), .. } => 9,
        UiNode::Chip { icon: Some(i), .. } | UiNode::Badge { icon: i, .. } => icon_schema(i),
        UiNode::IconButton {
            icon, active_icon, ..
        } => icon_schema(icon).max(active_icon.as_deref().map_or(3, icon_schema)),
        UiNode::List {
            columns,
            spacing_dp,
            padding_h_dp,
            ..
        } if *columns > 1 || *spacing_dp > 0 || *padding_h_dp > 0 => 8,
        UiNode::Conditional { value, equals, .. }
            if equals.is_some() || value.starts_with("$form.") =>
        {
            8
        }
        UiNode::Box { .. }
        | UiNode::ChoiceChips { .. }
        | UiNode::Text {
            style: TextStyle::Display,
            ..
        } => 6,
        UiNode::List {
            where_: Some(_), ..
        } => 6,
        UiNode::Column { align, .. } if *align != Align::Start => 6,
        UiNode::Row { align: Some(_), .. } => 6,
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
        | UiNode::Chip { .. } => 3,
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
        // Schema 6 (D44): boxes, choice chips, `where`, Display text, new icons.
        let boxed = node(json!({"type": "core://Box", "background": "$item.color",
            "content": {"type": "core://Text", "value_expr": "a", "style": "Body"}}));
        assert_eq!(required_schema(&boxed), 6);
        assert_eq!(boxed.children().len(), 1);
        let chips = node(json!({"type": "core://ChoiceChips", "field": "cat",
            "options": [{"label": "All"}, {"label": "Desserts", "value": "Desserts"}]}));
        assert_eq!(required_schema(&chips), 6);
        let centred = node(json!({"type": "core://Column", "align": "Center", "children": []}));
        assert_eq!(required_schema(&centred), 6);
        let display = node(json!({"type": "core://Text", "value_expr": "a", "style": "Display"}));
        assert_eq!(required_schema(&display), 6);
        let wher = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"},
            "where": {"field": "category", "equals": "$form.cat"}}));
        assert_eq!(required_schema(&wher), 6);
        let flame = node(json!({"type": "core://Chip", "text": "425°F", "icon": "flame"}));
        assert_eq!(required_schema(&flame), 6);
        let warning = node(json!({"type": "core://Chip", "text": "x", "icon": "warning"}));
        assert_eq!(required_schema(&warning), 3);
        // Schema 8 (D48): grids and form-driven conditionals.
        let grid = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"}, "columns": 2, "spacing_dp": 12}));
        assert_eq!(required_schema(&grid), 8);
        let one = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"}, "columns": 1}));
        assert_eq!(required_schema(&one), 3);
        let by_form = node(json!({"type": "core://Conditional", "value": "$form.view",
            "content": {"type": "core://Divider"}}));
        assert_eq!(required_schema(&by_form), 8);
        let equals = node(json!({"type": "core://Conditional", "value": "$item.kind",
            "equals": "photo", "content": {"type": "core://Divider"}}));
        assert_eq!(required_schema(&equals), 8);
        let plain = node(
            json!({"type": "core://Conditional", "value": "$item.caption",
            "content": {"type": "core://Divider"}}),
        );
        assert_eq!(required_schema(&plain), 3);
        // Schema 9 (D51): the media picker, multi-line fields, richer creates.
        let picker = node(json!({"type": "core://MediaPicker", "field": "photos", "max_items": 4}));
        assert_eq!(required_schema(&picker), 9);
        let caption = node(json!({"type": "core://TextField", "field": "c", "lines": 4}));
        assert_eq!(required_schema(&caption), 9);
        let add = node(json!({"type": "core://IconButton", "icon": "add_photo",
            "content_description": "New post", "on_click": {"intent": "action.navigation.back"}}));
        assert_eq!(required_schema(&add), 9);
        let attach = node(
            json!({"type": "core://Button", "label": "Share", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
            "entity": {"media": "$attachments.photos"}, "endpoint": "/v1/x",
            "payload": {"media": "$attachments.photos"},
            "attachments": {"photos": "$form.photos"}}}),
        );
        assert_eq!(required_schema(&attach), 9);
        let start = node(
            json!({"type": "core://Button", "label": "Add", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
            "entity": {}, "endpoint": "/v1/x", "payload": {}, "position": "start"}}),
        );
        assert_eq!(required_schema(&start), 9);
        let optional = node(
            json!({"type": "core://Button", "label": "Add", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
            "entity": {}, "endpoint": "/v1/x", "payload": {"c": "$form?.caption"}}}),
        );
        assert_eq!(required_schema(&optional), 9);
        // Schema 11: sticky content; its content is a child.
        let sticky = node(json!({"type": "core://Sticky", "content":
            {"type": "core://TextField", "field": "q"}}));
        assert_eq!(required_schema(&sticky), 11);
        assert_eq!(sticky.children().len(), 1);
        // Round trip keeps the new fields.
        let back: UiNode = serde_json::from_value(serde_json::to_value(&attach).unwrap()).unwrap();
        assert_eq!(back, attach);
        // Schema 10: remembered choice chips.
        let remembered = node(
            json!({"type": "core://ChoiceChips", "field": "view", "remember": true,
            "options": [{"label": "List"}, {"label": "Cards", "value": "cards"}]}),
        );
        assert_eq!(required_schema(&remembered), 10);
        let back: UiNode =
            serde_json::from_value(serde_json::to_value(&remembered).unwrap()).unwrap();
        assert_eq!(back, remembered);
        let forgotten = node(
            json!({"type": "core://ChoiceChips", "field": "view", "remember": false,
            "options": [{"label": "List"}]}),
        );
        assert_eq!(required_schema(&forgotten), 6);
        assert!(
            serde_json::to_value(&forgotten)
                .unwrap()
                .get("remember")
                .is_none()
        );
        // Schema 12: a list that scrolls to and tints one item.
        let focus = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"},
            "focus": {"uri": "x://c/1", "background": "#FFF3CF"}}));
        assert_eq!(required_schema(&focus), 12);
        let back: UiNode = serde_json::from_value(serde_json::to_value(&focus).unwrap()).unwrap();
        assert_eq!(back, focus);
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
                "core://Box" => json!({"type": t, "content": {"type": "core://Divider"}}),
                "core://ChoiceChips" => json!({"type": t, "field": "f", "options": []}),
                "core://MediaPicker" => json!({"type": t, "field": "f", "max_items": 4}),
                "core://Sticky" => json!({"type": t, "content": {"type": "core://Divider"}}),
                "core://Progress" => json!({"type": t}),
                "core://Backdrop" => json!({"type": t, "content": {"type": "core://Divider"}}),
                "core://Clock" => json!({"type": t, "format": "date"}),
                "core://Swipe" => json!({"type": t, "content": {"type": "core://Divider"},
                                         "on_swipe_up": {"intent": "action.navigation.back"}}),
                "core://OnAppear" => {
                    json!({"type": t, "intent": {"intent": "action.app.open", "app_id": "x"}})
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
    fn features_gate_layouts() {
        use Compatibility::*;
        let old = ClientCaps::schema(12);
        let new = ClientCaps::schema(12).with_features_header(" foo, ,bar ");
        assert_eq!(
            new.features.iter().map(String::as_str).collect::<Vec<_>>(),
            ["bar", "foo"]
        );
        let none: [&str; 0] = [];
        assert_eq!(old.can_draw(12, &none), Compatible);
        assert_eq!(old.can_draw(12, &["foo"]), UpdateRequired);
        assert_eq!(new.can_draw(11, &["foo"]), Compatible);
        assert_eq!(new.can_draw(12, &["foo", "bar"]), Compatible);
        assert_eq!(new.can_draw(12, &["foo", "baz"]), UpdateRequired);
        // The schema still decides first: a retired layout stays retired.
        assert_eq!(new.can_draw(2, &none), Retired);
        assert_eq!(new.can_draw(13, &["foo"]), UpdateRequired);
        // This build draws every feature it knows, by name.
        let this = ClientCaps::this_build();
        assert_eq!(this.schema, SCHEMA_VERSION);
        assert_eq!(parse_features(&features_header()), this.features);
        assert!(FEATURES.windows(2).all(|w| w[0] < w[1]), "sorted");
        // Nothing before features needs one.
        let sticky = node(json!({"type": "core://Sticky", "content": {"type": "core://Divider"}}));
        assert!(required_features(&sticky).is_empty());
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
        // Schema 6 (D44) is additive too.
        assert_eq!(oldest_layout_schema(6), 3);
        assert_eq!(compatibility(5, 6), Compatible);
        assert_eq!(compatibility(6, 5), UpdateRequired);
        // Schema 8 (D48) is additive too.
        assert_eq!(oldest_layout_schema(8), 3);
        assert_eq!(compatibility(7, 8), Compatible);
        assert_eq!(compatibility(8, 7), UpdateRequired);
        // Schema 9 (D51) is additive too.
        assert_eq!(oldest_layout_schema(9), 3);
        assert_eq!(compatibility(8, 9), Compatible);
        assert_eq!(compatibility(9, 8), UpdateRequired);
        // Schema 10 is additive too.
        assert_eq!(oldest_layout_schema(10), 3);
        assert_eq!(compatibility(9, 10), Compatible);
        assert_eq!(compatibility(10, 9), UpdateRequired);
        // Schema 11 (sticky nodes) is additive too.
        assert_eq!(oldest_layout_schema(11), 3);
        assert_eq!(compatibility(10, 11), Compatible);
        assert_eq!(compatibility(11, 10), UpdateRequired);
    }
}
