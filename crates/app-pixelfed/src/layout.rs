//! The `pixelfed_home` data. The signed-in feed's layout comes from
//! `layouts/pixelfed_home/NNNN.json` (generic nodes, docs/DECISIONS.md D36;
//! the core merges it in, D40); this module builds the entities/queries part
//! and the signed-out screen.

use std::collections::BTreeMap;

use protocol::{
    ActionIntent, DataBundle, EntityRecord, Orientation, SlotSpec, TextStyle, UiNode, WorkspaceSpec,
};

use super::HOME_QUERY;

pub const WORKSPACE_ID: &str = "pixelfed_home";
pub const LIKE_ENDPOINT: &str = "/v1/pixelfed/posts/like";
pub const BOOKMARK_ENDPOINT: &str = "/v1/pixelfed/posts/bookmark";
/// The pod's sign-in endpoints, carried by the sign-in button's intent.
pub const AUTH_APP_PATH: &str = "/v1/auth/pixelfed/app";
pub const AUTH_TOKEN_PATH: &str = "/v1/auth/pixelfed/token";

/// Signed-in home feed data: the timeline's entities and
/// `queries["timeline:home"]`.
pub fn home_feed(entities: BTreeMap<String, EntityRecord>, order: Vec<String>) -> DataBundle {
    DataBundle {
        entities,
        queries: BTreeMap::from([(HOME_QUERY.to_owned(), order)]),
    }
}

/// Signed-out screen of app `app_id` (titled `title`) with a "Sign in with
/// Pixelfed" button.
pub fn signed_out(instance_url: &str, app_id: &str, title: &str) -> WorkspaceSpec {
    WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: UiNode::Workspace {
            id: app_id.into(),
            title: title.into(),
            screens: Vec::new(),
            slots: vec![SlotSpec {
                slot_id: "auth".into(),
                provider_uri: "pixelfed://auth".into(),
                content: UiNode::Container {
                    orientation: Orientation::Vertical,
                    padding_dp: 16,
                    // No title Text: the workspace title already says "Pixelfed".
                    children: vec![
                        UiNode::Text {
                            value_expr: format!(
                                "Sign in to {instance_url} to see your home feed. Your Pixelfed \
                                 token stays on your pod."
                            ),
                            style: TextStyle::Body,
                        },
                        UiNode::Button {
                            label: "Sign in with Pixelfed".into(),
                            on_click: ActionIntent::AuthLogin {
                                instance_url: instance_url.into(),
                                app_path: AUTH_APP_PATH.into(),
                                token_path: AUTH_TOKEN_PATH.into(),
                            },
                        },
                    ],
                },
            }],
        },
        entities: BTreeMap::new(),
        queries: BTreeMap::new(),
    }
}
