//! The screen a caller without a pod token sees (D46).

use std::collections::BTreeMap;

use protocol::{ActionIntent, Orientation, SlotSpec, TextStyle, UiNode, WorkspaceSpec};
use serde_json::json;

/// The form field holding the typed pod code.
pub const TOKEN_FIELD: &str = "pod_token";
pub const OPEN_PATH: &str = "/v1/pods/open";
pub const TEMPORARY_PATH: &str = "/v1/pods/temporary";

/// "Open your pod" for workspace `id` (the client asked for it, so it keeps
/// its id): a pod code box, and with temporary pods a button for one.
pub fn open_your_pod(id: &str, temporary_hours: Option<u64>) -> WorkspaceSpec {
    let mut children = vec![
        UiNode::Text {
            value_expr: "Enter your pod code to open your apps. Your data lives in your own \
                         private pod."
                .into(),
            style: TextStyle::Body,
        },
        UiNode::TextField {
            field: TOKEN_FIELD.into(),
            placeholder: "Pod code".into(),
            max_length: Some(128),
            on_submit: None,
        },
        UiNode::Button {
            label: "Open pod".into(),
            on_click: ActionIntent::OpenSession {
                path: OPEN_PATH.into(),
                payload: json!({ "token": format!("$form.{TOKEN_FIELD}") }),
            },
        },
    ];
    if let Some(hours) = temporary_hours {
        children.extend([
            UiNode::Text {
                value_expr: format!(
                    "No code? Try a temporary pod. It and everything in it is deleted after \
                     {hours} hours."
                ),
                style: TextStyle::Body,
            },
            UiNode::Button {
                label: "Try a temporary pod".into(),
                on_click: ActionIntent::OpenSession {
                    path: TEMPORARY_PATH.into(),
                    payload: json!({}),
                },
            },
        ]);
    }
    WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: UiNode::Workspace {
            id: id.into(),
            title: "The Construct".into(),
            screens: Vec::new(),
            slots: vec![SlotSpec {
                slot_id: "pod".into(),
                provider_uri: "construct://pod".into(),
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
