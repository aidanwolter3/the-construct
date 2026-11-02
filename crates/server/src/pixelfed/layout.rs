//! The `pixelfed_home` layouts (typed, so they always deserialize as
//! [`protocol::WorkspaceSpec`]).

use std::collections::BTreeMap;

use protocol::{
    ActionIntent, EntityRecord, FieldUpdate, ListBinding, Orientation, PresentationMode, SlotSpec,
    TextStyle, UiNode, WorkspaceSpec,
};
use serde_json::json;

use super::HOME_QUERY;

pub const BUNDLE_VERSION: &str = "1.0.0";
pub const WORKSPACE_ID: &str = "pixelfed_home";
pub const LIKE_ENDPOINT: &str = "/v1/pixelfed/posts/like";
pub const BOOKMARK_ENDPOINT: &str = "/v1/pixelfed/posts/bookmark";
/// `like_count` follows the like toggle (evaluated on the pre-mutation entity).
pub const LIKE_COUNT_EXPR: &str = "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1";

fn post_card() -> UiNode {
    UiNode::PostCard {
        post_ref: "$item.uri".into(),
        author_slot: Box::new(UiNode::AuthorHeader {
            user_ref: "$item.author_uri".into(),
            on_click: None,
        }),
        media_slot: Box::new(UiNode::MediaCarousel {
            media_refs: ListBinding::Expr("$item.media_uris".into()),
            aspect_ratio: 1.0,
        }),
        action_slot: Box::new(UiNode::ActionBar {
            like_intent: ActionIntent::OptimisticMutate {
                entity_uri: "$item.uri".into(),
                field: "is_liked".into(),
                value_expr: "!entity.is_liked".into(),
                rollback_value: json!(false),
                endpoint: LIKE_ENDPOINT.into(),
                payload: json!({ "id": "$item.id", "liked": "$value" }),
                also: vec![FieldUpdate {
                    field: "like_count".into(),
                    value_expr: LIKE_COUNT_EXPR.into(),
                }],
            },
            bookmark_intent: ActionIntent::OptimisticMutate {
                entity_uri: "$item.uri".into(),
                field: "is_bookmarked".into(),
                value_expr: "!entity.is_bookmarked".into(),
                rollback_value: json!(false),
                endpoint: BOOKMARK_ENDPOINT.into(),
                payload: json!({ "id": "$item.id", "bookmarked": "$value" }),
                also: vec![],
            },
            comment_intent: ActionIntent::Navigate {
                target_uri: "pixelfed://posts/$item.id/comments".into(),
                presentation: PresentationMode::Modal,
            },
        }),
        caption_slot: Box::new(UiNode::Text {
            value_expr: "entity.caption".into(),
            style: TextStyle::Body,
        }),
    }
}

/// Signed-in home feed: FeedList over `queries["timeline:home"]`.
pub fn home_feed(entities: BTreeMap<String, EntityRecord>, order: Vec<String>) -> WorkspaceSpec {
    WorkspaceSpec {
        version: BUNDLE_VERSION.into(),
        workspace: UiNode::Workspace {
            id: WORKSPACE_ID.into(),
            title: "Pixelfed".into(),
            slots: vec![SlotSpec {
                slot_id: "feed".into(),
                provider_uri: "pixelfed://home_timeline".into(),
                content: UiNode::FeedList {
                    query_key: HOME_QUERY.into(),
                    item_template: Box::new(post_card()),
                },
            }],
        },
        entities,
        queries: BTreeMap::from([(HOME_QUERY.to_owned(), order)]),
    }
}

/// Signed-out screen with a "Sign in with Pixelfed" button.
pub fn signed_out(instance_url: &str) -> WorkspaceSpec {
    WorkspaceSpec {
        version: BUNDLE_VERSION.into(),
        workspace: UiNode::Workspace {
            id: WORKSPACE_ID.into(),
            title: "Pixelfed".into(),
            slots: vec![SlotSpec {
                slot_id: "auth".into(),
                provider_uri: "pixelfed://auth".into(),
                content: UiNode::Container {
                    orientation: Orientation::Vertical,
                    padding_dp: 16,
                    children: vec![
                        UiNode::Text {
                            value_expr: "Pixelfed".into(),
                            style: TextStyle::Title,
                        },
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
