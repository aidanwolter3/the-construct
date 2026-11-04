//! The `pixelfed_home` data. The signed-in feed's layout comes from
//! `layouts/pixelfed_home/NNNN.json` (generic nodes, docs/DECISIONS.md D36;
//! the core merges it in, D40); this module builds the entities/queries part
//! and the signed-out screen.

use std::collections::BTreeMap;

use protocol::{
    ActionIntent, DataBundle, EntityRecord, Orientation, SlotSpec, TextStyle, UiNode, WorkspaceSpec,
};

use super::HOME_QUERY;
use crate::config::PixelfedConfig;

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

/// The form field the signed-out screen's server box edits.
pub const INSTANCE_FIELD: &str = "instance";

/// The host (and port) of a base URL, for labels.
fn host_label(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.trim_end_matches('/').to_owned()
}

fn sign_in_intent(instance_url: &str) -> ActionIntent {
    ActionIntent::AuthLogin {
        instance_url: instance_url.into(),
        app_path: AUTH_APP_PATH.into(),
        token_path: AUTH_TOKEN_PATH.into(),
    }
}

/// How Settings starts the Pixelfed sign-in: the default instance's intent,
/// and a server box when users may pick another instance (D45; `others`:
/// this pod allows it).
pub fn sign_in_start(config: &PixelfedConfig, others: bool) -> pod_core::SignInStart {
    pod_core::SignInStart {
        intent: sign_in_intent(&config.instance_url),
        server_placeholder: (others && config.other_instances())
            .then(|| format!("Server, e.g. {}", example_server(config))),
    }
}

/// An example server for the server box's placeholder.
fn example_server(config: &PixelfedConfig) -> String {
    if config.any_instance {
        "pixelfed.social".to_owned()
    } else {
        config
            .allowed_instances
            .first()
            .map(|u| host_label(u))
            .unwrap_or_default()
    }
}

/// Signed-out screen of app `app_id` (titled `title`): a "Sign in with
/// Pixelfed" button for the default instance, and when users may pick
/// another instance (D45) a server box (`$form.instance`) with its own
/// button. The box needs no new client vocabulary: the client binds the
/// typed value into the intent and sends nothing while it is blank.
pub fn signed_out(config: &PixelfedConfig, app_id: &str, title: &str) -> WorkspaceSpec {
    let default = host_label(&config.public_url);
    let mut children = vec![
        // No title Text: the workspace title already says "Pixelfed".
        UiNode::Text {
            value_expr: format!(
                "Sign in to {default} to see your home feed. Your Pixelfed token stays on your pod."
            ),
            style: TextStyle::Body,
        },
        UiNode::Button {
            label: "Sign in with Pixelfed".into(),
            on_click: sign_in_intent(&config.instance_url),
        },
    ];
    if config.other_instances() {
        let example = example_server(config);
        children.extend([
            UiNode::Text {
                value_expr: "Or sign in to another Pixelfed server:".into(),
                style: TextStyle::Body,
            },
            UiNode::TextField {
                field: INSTANCE_FIELD.into(),
                placeholder: format!("Server, e.g. {example}"),
                max_length: Some(253),
                on_submit: None,
                lines: None,
                autofocus: false,
            },
            UiNode::Button {
                label: "Sign in to this server".into(),
                on_click: sign_in_intent(&format!("$form.{INSTANCE_FIELD}")),
            },
        ]);
    }
    WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: UiNode::Workspace {
            id: app_id.into(),
            title: title.into(),
            screens: Vec::new(),
            chrome: Default::default(),
            slots: vec![SlotSpec {
                slot_id: "auth".into(),
                provider_uri: "pixelfed://auth".into(),
                content: UiNode::Container {
                    orientation: Orientation::Vertical,
                    padding_dp: 16,
                    children,
                },
            }],
        },
        entities: BTreeMap::new(),
        queries: BTreeMap::new(),
    }
}
