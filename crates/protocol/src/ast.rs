//! Namespaced UI AST (DESIGN.md §2.2A).
//!
//! Nodes are coarse-grained and domain-namespaced (`core://`, `pixelfed://`).
//! On the wire each node is a JSON object whose `"type"` field selects the
//! variant.

use serde::{Deserialize, Serialize};

use crate::intent::ActionIntent;

/// A single node in the server-driven UI tree.
// `ActionBar` (three inline intents) dwarfs other variants. We keep the
// DESIGN.md §2.2A field types rather than boxing intents; layout trees are
// small and built rarely, so the size cost is irrelevant.
#[allow(clippy::large_enum_variant)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
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
    #[serde(rename = "core://Text")]
    Text {
        /// CEL expression or raw string.
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
    /// otherwise it fills the width at `aspect_ratio` (default 1).
    #[serde(rename = "core://Image")]
    Image {
        url: String,
        #[serde(default)]
        shape: ImageShape,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aspect_ratio: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_description: Option<String>,
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

    // Pixelfed coarse components
    #[serde(rename = "pixelfed://PostCard")]
    PostCard {
        /// Entity URI or binding, e.g. `"$item.uri"`.
        post_ref: String,
        author_slot: Box<UiNode>,
        media_slot: Box<UiNode>,
        action_slot: Box<UiNode>,
        caption_slot: Box<UiNode>,
    },
    #[serde(rename = "pixelfed://MediaCarousel")]
    MediaCarousel {
        /// Either a binding expression (`"$item.media_uris"`) or a literal
        /// list of media entity URIs. See `docs/DECISIONS.md`.
        media_refs: ListBinding,
        aspect_ratio: f32,
    },
    #[serde(rename = "pixelfed://AuthorHeader")]
    AuthorHeader {
        user_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_click: Option<ActionIntent>,
    },
    #[serde(rename = "pixelfed://ActionBar")]
    ActionBar {
        like_intent: ActionIntent,
        bookmark_intent: ActionIntent,
        comment_intent: ActionIntent,
    },
    #[serde(rename = "pixelfed://FeedList")]
    FeedList {
        query_key: String,
        item_template: Box<UiNode>,
    },
}

/// A screen an app can open (docs/DECISIONS.md D34). `route` is a URI
/// pattern whose `{name}` segments capture route parameters (each
/// `[A-Za-z0-9_-]{1,64}`), e.g. `pixelfed://posts/{id}/comments`. Every
/// `$route.<name>` in `title`, `data`, `content` and `footer` is replaced by
/// the captured value. `data`, when set, is a pod path loaded when the screen
/// opens (`action.data.load`); its answer is a [`crate::DataBundle`].
/// `footer` stays at the bottom while `content` scrolls.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Indent {
    pub field: String,
    pub step_dp: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u16>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageShape {
    #[default]
    Square,
    Rounded,
    Circle,
}

/// A named region of a workspace, filled by a content provider.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SlotSpec {
    pub slot_id: String,
    pub provider_uri: String,
    pub content: UiNode,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    Headline,
    Title,
    Body,
    Caption,
}

/// A list of strings that may be given literally or as a binding expression
/// resolved against the current item / entity at render time.
///
/// Serialized untagged so the original wire form is preserved:
/// `"$item.media_uris"` <-> `Expr`, `["pixelfed://media/1"]` <-> `List`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
