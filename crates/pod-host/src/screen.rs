//! The screen a caller without a pod token sees (D46).

use protocol::{ActionIntent, Orientation, TextStyle, UiNode};
use serde_json::{Value, json};

/// The form field holding the typed pod code.
pub const TOKEN_FIELD: &str = "pod_token";
pub const OPEN_PATH: &str = "/v1/pods/open";
pub const TEMPORARY_PATH: &str = "/v1/pods/temporary";

/// "Open your pod", the home of a caller without a pod: a construct
/// element (decision `content-addressed-construct`) whose page has a pod
/// code box, and with temporary pods a button for one. It refers to
/// nothing, so it is its own whole construct.
pub fn open_your_pod(temporary_hours: Option<u64>) -> Value {
    let mut children = vec![
        UiNode::Text {
            value_expr: "Enter your pod code to open your apps. Your data lives in your own \
                         private pod."
                .into(),
            style: TextStyle::Body,
            format: None,
            count: None,
        },
        UiNode::TextField {
            field: TOKEN_FIELD.into(),
            placeholder: "Pod code".into(),
            max_length: Some(128),
            on_submit: None,
            lines: None,
            secret: false,
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
                format: None,
                count: None,
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
    let page = UiNode::Page {
        id: pod_core::HOME_APP.into(),
        title: "The Construct".into(),
        root: Box::new(UiNode::Container {
            orientation: Orientation::Vertical,
            padding_dp: 16,
            children,
        }),
        screens: Vec::new(),
    };
    json!({
        "kind": "element",
        "title": "The Construct",
        "icon": "dashboard",
        "node": serde_json::to_value(&page).unwrap_or(Value::Null),
    })
}
