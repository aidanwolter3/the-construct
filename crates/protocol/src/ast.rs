//! Namespaced UI AST (DESIGN.md §2.2A).
//!
//! Nodes are coarse-grained and domain-namespaced (`core://`, `pixelfed://`).
//! On the wire each node is a JSON object whose `"type"` field selects the
//! variant.

use serde::{Deserialize, Serialize};

use crate::intent::ActionIntent;

/// A single node in the server-driven UI tree.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum UiNode {
    // Core layout primitives
    #[serde(rename = "core://Workspace")]
    Workspace {
        id: String,
        title: String,
        slots: Vec<SlotSpec>,
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
