//! Layout features (docs/decisions/named-vocabulary.md). Every piece of the
//! layout vocabulary is a named feature, one file `crates/protocol/features/
//! <name>.md` (see its README), mapped from the nodes and intents that use it
//! in [`node_features`] / [`intent_features`]. A client sends every feature
//! it draws in [`FEATURES_HEADER`]; a layout needs the features
//! [`required_features`] finds in it, worked out by the pod (layout files
//! don't declare them). A client draws a layout when the layout's features are
//! a subset of its own ([`ClientCaps::can_draw`]).
//!
//! There is no schema number: adding vocabulary is adding a file, so parallel
//! changes never conflict. Removing vocabulary is moving its file to
//! `features/removed/`: clients built after that stop listing it, the pod
//! retires only the versions that use it, and the field stays in the types so
//! those versions are still served unchanged to clients that draw them.

use std::collections::BTreeSet;

use crate::{ActionIntent, Align, Position, TextStyle, UiNode};

include!(concat!(env!("OUT_DIR"), "/features.rs"));

/// Request header carrying the features the client draws, comma-separated
/// ([`FEATURES`]). Absent means none.
pub const FEATURES_HEADER: &str = "x-construct-features";

/// Whether a client can draw a layout version, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    /// Uses a feature the client doesn't draw yet: the app needs an update.
    UpdateRequired,
    /// Uses a feature no client draws any more ([`REMOVED_FEATURES`]).
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

/// What a client draws: its features.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientCaps {
    pub features: BTreeSet<String>,
}

impl ClientCaps {
    /// What this build draws ([`FEATURES`]).
    pub fn this_build() -> Self {
        Self {
            features: FEATURES.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    /// A caller that sent no [`FEATURES_HEADER`] (a tool such as curl, not a
    /// client): it is served the latest version, whatever it uses.
    pub fn unfiltered() -> Self {
        Self {
            features: FEATURES
                .iter()
                .chain(REMOVED_FEATURES)
                .map(|f| (*f).to_owned())
                .collect(),
        }
    }

    /// A client that draws the features of a [`FEATURES_HEADER`] value.
    pub fn from_header(value: &str) -> Self {
        Self {
            features: parse_features(value),
        }
    }

    /// Whether this client can draw a layout that uses `features`, and why
    /// not.
    pub fn can_draw<S: AsRef<str>>(&self, features: &[S]) -> Compatibility {
        let mut missing = features
            .iter()
            .map(AsRef::as_ref)
            .filter(|f| !self.features.contains(*f))
            .peekable();
        if missing.peek().is_none() {
            Compatibility::Compatible
        } else if missing.any(|f| REMOVED_FEATURES.contains(&f)) {
            Compatibility::Retired
        } else {
            Compatibility::UpdateRequired
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
    if uses_local_data(node) {
        out.insert(feature::LOCAL_DATA);
    }
    out.into_iter().collect()
}

/// Whether a layout reads data the device provides: a `local://` entity or
/// a `local:` query key anywhere in it.
fn uses_local_data(node: &UiNode) -> bool {
    serde_json::to_string(node).is_ok_and(|j| j.contains("local://") || j.contains("\"local:"))
}

/// The features one node uses itself (not its children). Add one arm per
/// feature, e.g. `if let UiNode::Foo { .. } = node { out.insert(feature::FOO); }`
/// (`crates/protocol/features/README.md`).
fn node_features(node: &UiNode, out: &mut BTreeSet<&'static str>) {
    let mut add = |f: &'static str| {
        out.insert(f);
    };
    match node {
        UiNode::Workspace {
            screens,
            title_scrolls,
            ..
        } => {
            add(feature::WORKSPACE);
            if !screens.is_empty() {
                add(feature::SCREENS);
            }
            if *title_scrolls {
                add(feature::TITLE_SCROLLS);
            }
        }
        UiNode::Container { .. } => add(feature::CONTAINER),
        UiNode::Text {
            style,
            format,
            count,
            ..
        } => {
            add(feature::TEXT);
            match style {
                TextStyle::Label | TextStyle::Secondary => add(feature::TEXT_LABEL_STYLES),
                TextStyle::Display => add(feature::TEXT_DISPLAY),
                _ => {}
            }
            if format.is_some() {
                add(feature::DATE_FORMAT);
            }
            if count.is_some() {
                add(feature::COUNT);
            }
        }
        UiNode::Button { .. } => add(feature::BUTTON),
        UiNode::List {
            filter,
            group_by,
            where_,
            columns,
            spacing_dp,
            padding_h_dp,
            focus,
            ..
        } => {
            add(feature::LIST);
            if filter.is_some() {
                add(feature::LIST_FILTER);
            }
            if group_by.is_some() {
                add(feature::LIST_GROUP_BY);
            }
            if where_.is_some() {
                add(feature::LIST_WHERE);
            }
            if *columns > 1 || *spacing_dp > 0 || *padding_h_dp > 0 {
                add(feature::LIST_GRID);
            }
            if focus.is_some() {
                add(feature::LIST_FOCUS);
            }
        }
        UiNode::Image { .. } => add(feature::IMAGE),
        UiNode::TextField { lines, .. } => {
            add(feature::TEXT_FIELD);
            if lines.is_some() {
                add(feature::TEXT_FIELD_LINES);
            }
        }
        UiNode::Row { align, .. } => {
            add(feature::ROW);
            if align.is_some() {
                add(feature::ALIGN);
            }
        }
        UiNode::Column { align, .. } => {
            add(feature::COLUMN);
            if *align != Align::Start {
                add(feature::ALIGN);
            }
        }
        UiNode::Spacer { .. } => add(feature::SPACER),
        UiNode::Divider {} => add(feature::DIVIDER),
        UiNode::Conditional { value, equals, .. } => {
            add(feature::CONDITIONAL);
            if equals.is_some() || value.starts_with("$form.") {
                add(feature::CONDITIONAL_FORM);
            }
        }
        UiNode::Carousel { .. } => add(feature::CAROUSEL),
        UiNode::IconButton { .. } => add(feature::ICON_BUTTON),
        UiNode::Chip { .. } => add(feature::CHIP),
        UiNode::Badge { .. } => add(feature::BADGE),
        UiNode::Tappable { .. } => add(feature::TAPPABLE),
        UiNode::Box { fill, .. } => {
            add(feature::BOX);
            if *fill {
                add(feature::BOX_FILL);
            }
        }
        UiNode::ChoiceChips { remember, .. } => {
            add(feature::CHOICE_CHIPS);
            if *remember {
                add(feature::CHOICE_CHIPS_REMEMBER);
            }
        }
        UiNode::MediaPicker { .. } => add(feature::MEDIA_PICKER),
        UiNode::Sticky { .. } => add(feature::STICKY),
        UiNode::Progress { .. } => add(feature::PROGRESS),
        UiNode::Dock { .. } => add(feature::DOCK),
        UiNode::AppFrame { .. } => add(feature::APP_FRAME),
        UiNode::Switch { .. } => add(feature::SWITCH),
        UiNode::Drawer { .. } => add(feature::DRAWER),
        // A node type this build does not know (the removed `pixelfed://`
        // widgets): no client draws it any more.
        UiNode::Unknown(_) => add(feature::LEGACY_NODES),
    }
    let icons: [Option<&str>; 2] = match node {
        UiNode::IconButton {
            icon, active_icon, ..
        } => [Some(icon), active_icon.as_deref()],
        UiNode::Chip { icon, .. } => [icon.as_deref(), None],
        UiNode::Badge { icon, .. } => [Some(icon), None],
        _ => [None, None],
    };
    for i in icons.into_iter().flatten() {
        if TODO_ICONS.contains(&i) {
            add(feature::TODO_ICONS);
        } else if SHELL_ICONS.contains(&i) {
            add(feature::ICONS_SHELL);
        } else if ICONS_9.contains(&i) {
            add(feature::ICONS_POST);
        } else if ICONS_6.contains(&i) {
            add(feature::ICONS_FLAME_TIMER);
        }
    }
}

/// Icons of the `icons_shell` feature.
const SHELL_ICONS: [&str; 2] = ["settings", "notifications"];
/// Icons of the `todo_icons` feature.
const TODO_ICONS: [&str; 4] = ["check", "check_box", "check_box_outline_blank", "delete"];
/// Icons added in schema 6 (D44).
const ICONS_6: [&str; 2] = ["flame", "timer"];
/// Icons added in schema 9 (D51).
const ICONS_9: [&str; 3] = ["add_photo", "add", "refresh"];

/// The features one action intent uses; see [`node_features`].
fn intent_features(intent: &ActionIntent, out: &mut BTreeSet<&'static str>) {
    let mut add = |f: &'static str| {
        out.insert(f);
    };
    // `$form?.<field>` anywhere in the intent (a sequence's steps are
    // intents of their own).
    if !matches!(intent, ActionIntent::Sequence { .. })
        && serde_json::to_string(intent).is_ok_and(|j| j.contains("$form?."))
    {
        add(feature::FORM_OPTIONAL);
    }
    match intent {
        ActionIntent::Navigate { .. } => add(feature::NAVIGATE),
        ActionIntent::NavigateBack => add(feature::NAVIGATE_BACK),
        ActionIntent::OptimisticMutate { .. } => add(feature::OPTIMISTIC_MUTATION),
        ActionIntent::AuthLogin { .. } => add(feature::AUTH_LOGIN),
        ActionIntent::OpenSession { .. } => add(feature::AUTH_SESSION),
        ActionIntent::LoadData { .. } => add(feature::LOAD_DATA),
        ActionIntent::Create {
            attachments,
            position,
            dismiss,
            ..
        } => {
            add(feature::CREATE);
            if !attachments.is_empty() {
                add(feature::CREATE_ATTACHMENTS);
            }
            if *position != Position::End {
                add(feature::CREATE_POSITION);
            }
            if *dismiss {
                add(feature::CREATE_DISMISS);
            }
        }
        ActionIntent::Sequence { .. } => add(feature::ACTION_SEQUENCE),
        ActionIntent::LocalSet { .. } => add(feature::LOCAL_SET),
        ActionIntent::OpenLink { .. } => add(feature::OPEN_LINK),
        ActionIntent::Device { .. } => add(feature::DEVICE_ACTION),
        ActionIntent::Unknown => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(v: serde_json::Value) -> UiNode {
        serde_json::from_value(v).unwrap()
    }

    /// The numbered schema that added each feature, before features
    /// replaced schema numbers: the detection checks below still read in
    /// terms of that history.
    const HISTORY: &[(&str, u32)] = &[
        ("screens", 2),
        ("list", 2),
        ("image", 2),
        ("text_field", 2),
        ("load_data", 2),
        ("create", 2),
        ("row", 3),
        ("column", 3),
        ("spacer", 3),
        ("divider", 3),
        ("conditional", 3),
        ("carousel", 3),
        ("icon_button", 3),
        ("chip", 3),
        ("badge", 3),
        ("text_label_styles", 3),
        ("tappable", 4),
        ("list_filter", 5),
        ("list_group_by", 5),
        ("box", 6),
        ("choice_chips", 6),
        ("list_where", 6),
        ("align", 6),
        ("text_display", 6),
        ("icons_flame_timer", 6),
        ("auth_session", 7),
        ("list_grid", 8),
        ("conditional_form", 8),
        ("media_picker", 9),
        ("text_field_lines", 9),
        ("create_attachments", 9),
        ("create_position", 9),
        ("create_dismiss", 9),
        ("form_optional", 9),
        ("icons_post", 9),
        ("choice_chips_remember", 10),
        ("sticky", 11),
        ("list_focus", 12),
    ];

    fn schema_of(n: &UiNode) -> u32 {
        required_features(n)
            .iter()
            .filter_map(|f| HISTORY.iter().find(|(h, _)| h == f).map(|(_, s)| *s))
            .fold(1, u32::max)
    }

    #[test]
    fn each_vocabulary_has_its_feature() {
        let old = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "core://Button", "label": "Go",
             "on_click": {"intent": "action.navigation.open", "target_uri": "x://y", "presentation": "Push"}}}]}),
        );
        assert_eq!(schema_of(&old), 1);
        // A removed widget is an unknown node: legacy, no numbered schema.
        let legacy = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "pixelfed://FeedList",
             "query_key": "q", "item_template": {"type": "core://Text", "value_expr": "x", "style": "Body"}}}]}),
        );
        assert_eq!(schema_of(&legacy), 1);
        let list = node(json!({"type": "core://List", "query_key": "q",
             "item_template": {"type": "core://Text", "value_expr": "$item.text", "style": "Body"}}));
        assert_eq!(schema_of(&list), 2);
        let screens = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [],
            "screens": [{"id": "s", "route": "x://{id}", "title": "S",
                         "content": {"type": "core://Text", "value_expr": "hi", "style": "Body"}}]}),
        );
        assert_eq!(schema_of(&screens), 2);
        let create = node(
            json!({"type": "core://Button", "label": "Add", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://tmp/$nonce",
            "entity": {}, "endpoint": "/v1/x", "payload": {}}}),
        );
        assert_eq!(schema_of(&create), 2);
        let row = node(json!({"type": "core://Row", "children": [
            {"type": "core://Text", "value_expr": "a", "style": "Body"}]}));
        assert_eq!(schema_of(&row), 3);
        let label = node(json!({"type": "core://Text", "value_expr": "a", "style": "Label"}));
        assert_eq!(schema_of(&label), 3);
        let deep = node(
            json!({"type": "core://List", "query_key": "q", "item_template":
            {"type": "core://IconButton", "icon": "favorite", "content_description": "Like",
             "on_click": {"intent": "action.navigation.back"}}}),
        );
        assert_eq!(schema_of(&deep), 3);
        let tappable = node(
            json!({"type": "core://List", "query_key": "q", "item_template":
            {"type": "core://Tappable",
             "on_click": {"intent": "action.navigation.open", "target_uri": "x://$item.id",
                          "presentation": "Push"},
             "content": {"type": "core://Text", "value_expr": "$item.title", "style": "Body"}}}),
        );
        assert_eq!(schema_of(&tappable), 4);
        // Its intent is checked like a button's, and its content is a child.
        let t = &tappable.children()[0];
        assert_eq!(t.own_intents().len(), 1);
        assert_eq!(t.children().len(), 1);
        // A filtered or grouped list is schema 5 (D43); the group header is a child.
        let filtered = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Text", "value_expr": "$item.title", "style": "Body"},
            "filter": {"value": "$form.q", "fields": ["title"]}}));
        assert_eq!(schema_of(&filtered), 5);
        let grouped = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"},
            "group_by": {"field": "category", "header":
                {"type": "core://Tappable", "on_click": {"intent": "action.navigation.back"},
                 "content": {"type": "core://Divider"}}}}));
        assert_eq!(schema_of(&grouped), 5);
        assert_eq!(grouped.children().len(), 2);
        assert_eq!(grouped.walk().len(), 4);
        // Schema 6 (D44): boxes, choice chips, `where`, Display text, new icons.
        let boxed = node(json!({"type": "core://Box", "background": "$item.color",
            "content": {"type": "core://Text", "value_expr": "a", "style": "Body"}}));
        assert_eq!(schema_of(&boxed), 6);
        assert_eq!(boxed.children().len(), 1);
        let chips = node(json!({"type": "core://ChoiceChips", "field": "cat",
            "options": [{"label": "All"}, {"label": "Desserts", "value": "Desserts"}]}));
        assert_eq!(schema_of(&chips), 6);
        let centred = node(json!({"type": "core://Column", "align": "Center", "children": []}));
        assert_eq!(schema_of(&centred), 6);
        let display = node(json!({"type": "core://Text", "value_expr": "a", "style": "Display"}));
        assert_eq!(schema_of(&display), 6);
        let wher = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"},
            "where": {"field": "category", "equals": "$form.cat"}}));
        assert_eq!(schema_of(&wher), 6);
        let flame = node(json!({"type": "core://Chip", "text": "425°F", "icon": "flame"}));
        assert_eq!(schema_of(&flame), 6);
        let warning = node(json!({"type": "core://Chip", "text": "x", "icon": "warning"}));
        assert_eq!(schema_of(&warning), 3);
        // Schema 8 (D48): grids and form-driven conditionals.
        let grid = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"}, "columns": 2, "spacing_dp": 12}));
        assert_eq!(schema_of(&grid), 8);
        let one = node(json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"}, "columns": 1}));
        assert_eq!(schema_of(&one), 3);
        let by_form = node(json!({"type": "core://Conditional", "value": "$form.view",
            "content": {"type": "core://Divider"}}));
        assert_eq!(schema_of(&by_form), 8);
        let equals = node(json!({"type": "core://Conditional", "value": "$item.kind",
            "equals": "photo", "content": {"type": "core://Divider"}}));
        assert_eq!(schema_of(&equals), 8);
        let plain = node(
            json!({"type": "core://Conditional", "value": "$item.caption",
            "content": {"type": "core://Divider"}}),
        );
        assert_eq!(schema_of(&plain), 3);
        // Schema 9 (D51): the media picker, multi-line fields, richer creates.
        let picker = node(json!({"type": "core://MediaPicker", "field": "photos", "max_items": 4}));
        assert_eq!(schema_of(&picker), 9);
        let caption = node(json!({"type": "core://TextField", "field": "c", "lines": 4}));
        assert_eq!(schema_of(&caption), 9);
        let add = node(json!({"type": "core://IconButton", "icon": "add_photo",
            "content_description": "New post", "on_click": {"intent": "action.navigation.back"}}));
        assert_eq!(schema_of(&add), 9);
        let attach = node(
            json!({"type": "core://Button", "label": "Share", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
            "entity": {"media": "$attachments.photos"}, "endpoint": "/v1/x",
            "payload": {"media": "$attachments.photos"},
            "attachments": {"photos": "$form.photos"}}}),
        );
        assert_eq!(schema_of(&attach), 9);
        let start = node(
            json!({"type": "core://Button", "label": "Add", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
            "entity": {}, "endpoint": "/v1/x", "payload": {}, "position": "start"}}),
        );
        assert_eq!(schema_of(&start), 9);
        let optional = node(
            json!({"type": "core://Button", "label": "Add", "on_click": {
            "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
            "entity": {}, "endpoint": "/v1/x", "payload": {"c": "$form?.caption"}}}),
        );
        assert_eq!(schema_of(&optional), 9);
        // Schema 11: sticky content; its content is a child.
        let sticky = node(json!({"type": "core://Sticky", "content":
            {"type": "core://TextField", "field": "q"}}));
        assert_eq!(schema_of(&sticky), 11);
        assert_eq!(sticky.children().len(), 1);
        // Round trip keeps the new fields.
        let back: UiNode = serde_json::from_value(serde_json::to_value(&attach).unwrap()).unwrap();
        assert_eq!(back, attach);
        // Schema 10: remembered choice chips.
        let remembered = node(
            json!({"type": "core://ChoiceChips", "field": "view", "remember": true,
            "options": [{"label": "List"}, {"label": "Cards", "value": "cards"}]}),
        );
        assert_eq!(schema_of(&remembered), 10);
        let back: UiNode =
            serde_json::from_value(serde_json::to_value(&remembered).unwrap()).unwrap();
        assert_eq!(back, remembered);
        let forgotten = node(
            json!({"type": "core://ChoiceChips", "field": "view", "remember": false,
            "options": [{"label": "List"}]}),
        );
        assert_eq!(schema_of(&forgotten), 6);
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
        assert_eq!(schema_of(&focus), 12);
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
                "core://Dock" => json!({"type": t, "field": "tab", "after": [
                    {"value": "a", "label": "A", "content": {"type": "core://Divider"}}]}),
                "core://AppFrame" => json!({"type": t, "app": "home"}),
                "core://Drawer" => json!({"type": t, "field": "page",
                    "after": [{"value": "a", "content": {"type": "core://Divider"}}],
                    "drawer": {"type": "core://Divider"}}),
                "core://Switch" => json!({"type": t, "checked": "$item.on",
                    "on_click": {"intent": "action.navigation.back"}}),
                other => panic!("no sample for {other}"),
            };
            let n: UiNode = serde_json::from_value(v).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert!(!matches!(n, UiNode::Unknown(_)), "{t}");
            assert_eq!(n.type_tag(), t);
        }
    }

    #[test]
    fn the_shell_vocabulary_needs_its_features() {
        let ws: UiNode = serde_json::from_value(json!({"type": "core://Workspace", "id": "home",
            "title": "Home", "slots": [{"slot_id": "dock", "provider_uri": "local://apps", "content": {
              "type": "core://Dock", "field": "tab", "query_key": "local:apps",
              "item": {"value": "$item.id", "label": "$item.title",
                       "content": {"type": "core://AppFrame", "app": "$item.id"}},
              "after": [{"value": "more", "label": "More", "content": {"type": "core://Button",
                "label": "Settings", "on_click": {"intent": "action.device", "command": "open_settings"}}}]}}]}))
        .unwrap();
        let f = required_features(&ws);
        for want in [
            feature::DOCK,
            feature::APP_FRAME,
            feature::DEVICE_ACTION,
            feature::LOCAL_DATA,
        ] {
            assert!(f.contains(&want), "{want}: {f:?}");
        }
        assert_eq!(
            ws.walk().len(),
            4,
            "the dock's tab contents are its children"
        );
    }

    #[test]
    fn features_gate_layouts() {
        use Compatibility::*;
        let client = ClientCaps::from_header(" foo, ,bar ");
        assert_eq!(
            client
                .features
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["bar", "foo"]
        );
        let none: [&str; 0] = [];
        assert_eq!(client.can_draw(&none), Compatible);
        assert_eq!(client.can_draw(&["foo", "bar"]), Compatible);
        assert_eq!(client.can_draw(&["foo", "baz"]), UpdateRequired);
        assert_eq!(ClientCaps::default().can_draw(&["foo"]), UpdateRequired);
        // A removed feature retires the layout, for every client that
        // doesn't list it.
        assert_eq!(client.can_draw(&["foo", feature::CREATE_DISMISS]), Retired);
        assert_eq!(client.can_draw(&[feature::LEGACY_NODES]), Retired);
        let old = ClientCaps::from_header("foo,create_dismiss");
        assert_eq!(old.can_draw(&["foo", feature::CREATE_DISMISS]), Compatible);
        assert_eq!(Compatible.reason(), None);
        assert_eq!(UpdateRequired.reason(), Some("update_required"));
        assert_eq!(Retired.reason(), Some("retired"));
        // This build draws every feature it knows, by name, and none removed.
        let this = ClientCaps::this_build();
        assert_eq!(parse_features(&features_header()), this.features);
        assert!(FEATURES.windows(2).all(|w| w[0] < w[1]), "sorted");
        assert!(REMOVED_FEATURES.iter().all(|f| !FEATURES.contains(f)));
        let sticky = node(json!({"type": "core://Sticky", "content": {"type": "core://Divider"}}));
        assert_eq!(required_features(&sticky), ["divider", "sticky"]);
        assert_eq!(this.can_draw(&required_features(&sticky)), Compatible);
    }

    /// Decision `pinned-app-title`: only a title that scrolls away needs
    /// the feature; a pinned one (the default) is plain `workspace`.
    #[test]
    fn a_scrolling_title_needs_its_feature() {
        let ws = |scrolls: bool| {
            node(
                json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [],
                        "title_scrolls": scrolls}),
            )
        };
        assert_eq!(required_features(&ws(false)), ["workspace"]);
        assert_eq!(required_features(&ws(true)), ["title_scrolls", "workspace"]);
        // Written only when set.
        let v = serde_json::to_value(ws(false)).unwrap();
        assert!(v.get("title_scrolls").is_none());
        assert_eq!(
            serde_json::to_value(ws(true)).unwrap()["title_scrolls"],
            true
        );
    }

    /// Decision `named-vocabulary`: `dismiss` is removed; only the layouts
    /// that use it are retired.
    #[test]
    fn a_removed_feature_retires_only_the_layouts_that_use_it() {
        use Compatibility::*;
        let create = |dismiss: bool| {
            node(
                json!({"type": "core://Button", "label": "Share", "on_click": {
                "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
                "entity": {}, "endpoint": "/v1/x", "payload": {}, "dismiss": dismiss}}),
            )
        };
        assert_eq!(
            required_features(&create(true)),
            ["button", "create", "create_dismiss"]
        );
        assert_eq!(required_features(&create(false)), ["button", "create"]);
        let this = ClientCaps::this_build();
        assert_eq!(this.can_draw(&required_features(&create(true))), Retired);
        assert_eq!(
            this.can_draw(&required_features(&create(false))),
            Compatible
        );
        // Inside a sequence too.
        let nested = node(json!({"type": "core://Button", "label": "Go", "on_click": {
            "intent": "action.sequence", "steps": [
                {"intent": "action.mutation.create", "query_key": "q", "entity_uri": "x://p/$nonce",
                 "entity": {}, "endpoint": "/v1/x", "payload": {}, "dismiss": true}]}}));
        assert!(required_features(&nested).contains(&feature::CREATE_DISMISS));
        // The field still round-trips: old versions are served unchanged.
        let again: UiNode =
            serde_json::from_str(&serde_json::to_string(&create(true)).unwrap()).unwrap();
        assert_eq!(again, create(true));
        // A node type this build doesn't know is legacy.
        let legacy = node(json!({"type": "pixelfed://FeedList", "query_key": "q"}));
        assert_eq!(required_features(&legacy), ["legacy_nodes"]);
    }
}
