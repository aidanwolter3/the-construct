//! Namespaced UI AST (DESIGN.md §2.2A).
//!
//! Every node is generic (`core://`): apps are composed from these on the pod
//! (docs/DECISIONS.md D34-D36); no client knows any app. On the wire each node
//! is a JSON object whose `"type"` field selects the variant. A node of a type
//! this build does not know (e.g. the removed `pixelfed://` widgets in old
//! layout versions) deserializes as [`UiNode::Unknown`] holding its JSON, and
//! serializes back unchanged, so a pod can still serve old versions to the
//! clients that draw them.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::intent::ActionIntent;

/// Node types this build knows (an other type is [`UiNode::Unknown`]).
pub const NODE_TYPES: [&str; 16] = [
    "core://Workspace",
    "core://Container",
    "core://Text",
    "core://Button",
    "core://List",
    "core://Image",
    "core://TextField",
    "core://Row",
    "core://Column",
    "core://Spacer",
    "core://Divider",
    "core://Conditional",
    "core://Carousel",
    "core://IconButton",
    "core://Chip",
    "core://Badge",
];

/// Icon names clients draw (`core://IconButton`, `core://Chip`).
pub const ICONS: [&str; 10] = [
    "favorite",
    "favorite_border",
    "chat_bubble",
    "bookmark",
    "bookmark_border",
    "repeat",
    "warning",
    "send",
    "arrow_back",
    "close",
];

fn one() -> f64 {
    1.0
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

fn is_square(s: &ImageShape) -> bool {
    *s == ImageShape::Square
}

/// A single node in the server-driven UI tree.
// `IconButton` (an inline intent) dwarfs other variants; layout trees are
// small and built rarely, so the size cost is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", remote = "Self")]
pub enum UiNode {
    // Core layout primitives
    #[serde(rename = "core://Workspace")]
    Workspace {
        id: String,
        title: String,
        slots: Vec<SlotSpec>,
        /// Screens this app can open (docs/DECISIONS.md D34): an
        /// `action.navigation.open` whose `target_uri` matches a screen's
        /// `route` pushes that screen. Omitted when empty; clients that predate
        /// screens ignore it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        screens: Vec<Screen>,
    },
    #[serde(rename = "core://Container")]
    Container {
        orientation: Orientation,
        padding_dp: u16,
        children: Vec<UiNode>,
    },
    /// `value_expr`: `$item.<field>` (inside a list or carousel template), text
    /// with embedded `$item.<field>` tokens, `${<uri>#<field>}` placeholders,
    /// or plain text.
    #[serde(rename = "core://Text")]
    Text {
        value_expr: String,
        style: TextStyle,
    },
    #[serde(rename = "core://Button")]
    Button {
        label: String,
        on_click: ActionIntent,
    },

    /// A list of the entities of `queries[query_key]`, each rendered with
    /// `item_template` (`$item.<field>` binds the entity). `indent` shifts an
    /// item right by `item[indent.field] * step_dp` (capped at `max` steps).
    /// `empty_text` shows when the query is present but empty.
    #[serde(rename = "core://List")]
    List {
        query_key: String,
        item_template: Box<UiNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        indent: Option<Indent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        empty_text: Option<String>,
    },
    /// An image from a URL (a literal or a binding such as
    /// `"$item.avatar_url"`). `size_dp` makes it a square of that size;
    /// otherwise it fills the width at `aspect_ratio` (default 1). `blurhash`
    /// (a binding or literal) colours the placeholder; when the image fails,
    /// `content_description` is shown in its place.
    #[serde(rename = "core://Image")]
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "is_square")]
        shape: ImageShape,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aspect_ratio: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blurhash: Option<String>,
    },
    /// A text input whose value is the form value `$form.<field>` for the
    /// intents of its screen. `on_submit` (a send button, and the keyboard's
    /// submit) is dispatched with the value bound, then the field is cleared;
    /// it is not dispatched while the field is blank.
    #[serde(rename = "core://TextField")]
    TextField {
        field: String,
        #[serde(default)]
        placeholder: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_length: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_submit: Option<ActionIntent>,
    },

    // Schema 3 (docs/DECISIONS.md D36)
    /// Children side by side, vertically centred, never wrapping: a
    /// `core://Spacer` without a size takes the remaining width.
    #[serde(rename = "core://Row")]
    Row {
        children: Vec<UiNode>,
        #[serde(default, skip_serializing_if = "is_zero")]
        spacing_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_h_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_v_dp: u16,
    },
    /// Children stacked, full width.
    #[serde(rename = "core://Column")]
    Column {
        children: Vec<UiNode>,
        #[serde(default, skip_serializing_if = "is_zero")]
        spacing_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_h_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_v_dp: u16,
    },
    /// Empty space: `size_dp` along the parent's axis, or (no size, in a
    /// `core://Row`) all the remaining width.
    #[serde(rename = "core://Spacer")]
    Spacer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
    },
    /// A thin horizontal line.
    #[serde(rename = "core://Divider")]
    Divider {},
    /// `content` only when `value` (a binding such as `$item.caption`) is
    /// truthy: not missing, null, false, 0, "", "0" or an empty array.
    #[serde(rename = "core://Conditional")]
    Conditional { value: String, content: Box<UiNode> },
    /// A swipeable pager over entities: `items` is a binding to a list of
    /// entity URIs (`"$item.media_uris"`) or a literal list; each is drawn with
    /// `item_template` (`$item` = that entity). Page dots and an "n/N" badge
    /// show with more than one item. The aspect ratio is the first item's
    /// `aspect_field` (clamped to `min_aspect..=max_aspect`), else
    /// `aspect_ratio`. `tag` (bindable) names it for tests and accessibility.
    #[serde(rename = "core://Carousel")]
    Carousel {
        items: ListBinding,
        item_template: Box<UiNode>,
        #[serde(default = "one")]
        aspect_ratio: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aspect_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_aspect: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_aspect: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    /// An icon button. `active` (a binding to a bool, e.g. `$item.is_liked`)
    /// switches to `active_icon`, `active_tint` (`#RRGGBB`) and
    /// `active_content_description`. Icons are named ([`ICONS`]).
    #[serde(rename = "core://IconButton")]
    IconButton {
        icon: String,
        content_description: String,
        on_click: ActionIntent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_icon: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_tint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_content_description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    /// A small label with an optional icon, coloured by `tone`.
    #[serde(rename = "core://Chip")]
    Chip {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default)]
        tone: Tone,
    },
    /// A one-line note with a small icon, e.g. "Bob boosted".
    #[serde(rename = "core://Badge")]
    Badge { text: String, icon: String },

    /// A node of a type this build does not know: its JSON, kept as is.
    #[serde(skip)]
    Unknown(Value),
}

impl Serialize for UiNode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            UiNode::Unknown(raw) => raw.serialize(s),
            // The derived (remote = "Self") serializer.
            known => UiNode::serialize(known, s),
        }
    }
}

impl<'de> Deserialize<'de> for UiNode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        let known = raw
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| NODE_TYPES.contains(&t));
        if !known {
            return Ok(UiNode::Unknown(raw));
        }
        // The derived (remote = "Self") deserializer: a known type must be valid.
        UiNode::deserialize(raw).map_err(D::Error::custom)
    }
}

impl UiNode {
    /// The node's `"type"` tag (an unknown node's own, or "").
    pub fn type_tag(&self) -> String {
        match self {
            UiNode::Unknown(raw) => raw
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            known => serde_json::to_value(known)
                .ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default(),
        }
    }

    /// Direct child nodes (screens' content and footers included).
    pub fn children(&self) -> Vec<&UiNode> {
        match self {
            UiNode::Workspace { slots, screens, .. } => slots
                .iter()
                .map(|s| &s.content)
                .chain(
                    screens
                        .iter()
                        .flat_map(|s| std::iter::once(&s.content).chain(s.footer.as_ref())),
                )
                .collect(),
            UiNode::Container { children, .. }
            | UiNode::Row { children, .. }
            | UiNode::Column { children, .. } => children.iter().collect(),
            UiNode::List { item_template, .. } | UiNode::Carousel { item_template, .. } => {
                vec![item_template]
            }
            UiNode::Conditional { content, .. } => vec![content],
            UiNode::Text { .. }
            | UiNode::Button { .. }
            | UiNode::Image { .. }
            | UiNode::TextField { .. }
            | UiNode::Spacer { .. }
            | UiNode::Divider {}
            | UiNode::IconButton { .. }
            | UiNode::Chip { .. }
            | UiNode::Badge { .. }
            | UiNode::Unknown(_) => Vec::new(),
        }
    }

    /// The intents this node itself holds (not its children's).
    pub fn own_intents(&self) -> Vec<&ActionIntent> {
        match self {
            UiNode::Button { on_click, .. } | UiNode::IconButton { on_click, .. } => vec![on_click],
            UiNode::TextField { on_submit, .. } => on_submit.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Every node of the tree, depth first (this one first).
    pub fn walk(&self) -> Vec<&UiNode> {
        let mut out = vec![self];
        let mut i = 0;
        while i < out.len() {
            let children = out[i].children();
            out.extend(children);
            i += 1;
        }
        out
    }
}

/// A screen an app can open (docs/DECISIONS.md D34). `route` is a URI
/// pattern whose `{name}` segments capture route parameters (each
/// `[A-Za-z0-9_-]{1,64}`), e.g. `pixelfed://posts/{id}/comments`. Every
/// `$route.<name>` in `title`, `data`, `content` and `footer` is replaced by
/// the captured value. `data`, when set, is a pod path loaded when the screen
/// opens (`action.data.load`); its answer is a [`crate::DataBundle`].
/// `footer` stays at the bottom while `content` scrolls.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Screen {
    pub id: String,
    pub route: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    pub content: UiNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footer: Option<UiNode>,
}

/// `core://List` indentation: `item[field]` (a number) times `step_dp`,
/// capped at `max` steps (default 4).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Indent {
    pub field: String,
    pub step_dp: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u16>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageShape {
    #[default]
    Square,
    Rounded,
    Circle,
}

/// A named region of a workspace, filled by a content provider.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SlotSpec {
    pub slot_id: String,
    pub provider_uri: String,
    pub content: UiNode,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    Headline,
    Title,
    Body,
    Caption,
    /// Semi-bold, small title size (schema 3).
    Label,
    /// Small and muted (schema 3).
    Secondary,
}

/// `core://Chip` colour.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Neutral,
    Warning,
}

/// A list of strings that may be given literally or as a binding expression
/// resolved against the current item / entity at render time.
///
/// Serialized untagged so the original wire form is preserved:
/// `"$item.media_uris"` <-> `Expr`, `["pixelfed://media/1"]` <-> `List`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum ListBinding {
    Expr(String),
    List(Vec<String>),
}

impl ListBinding {
    /// The binding expression, if this is an `Expr`.
    pub fn as_expr(&self) -> Option<&str> {
        match self {
            ListBinding::Expr(e) => Some(e),
            ListBinding::List(_) => None,
        }
    }

    /// The literal list, if this is a `List`.
    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            ListBinding::Expr(_) => None,
            ListBinding::List(l) => Some(l),
        }
    }
}

impl From<Vec<String>> for ListBinding {
    fn from(v: Vec<String>) -> Self {
        ListBinding::List(v)
    }
}

impl From<&str> for ListBinding {
    fn from(s: &str) -> Self {
        ListBinding::Expr(s.to_owned())
    }
}

impl From<String> for ListBinding {
    fn from(s: String) -> Self {
        ListBinding::Expr(s)
    }
}
