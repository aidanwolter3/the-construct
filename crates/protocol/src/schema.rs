//! Client schema versions (docs/DECISIONS.md D34). A client sends the newest
//! layout vocabulary it can draw in [`SCHEMA_HEADER`]; the pod serves an
//! app's newest layout version whose [`required_schema`] is not above it.
//!
//! - 1: everything before screens (no header means 1).
//! - 2: `Workspace.screens`, `core://List`, `core://Image`,
//!   `core://TextField`, `action.data.load`, `action.mutation.create`.

use crate::{ActionIntent, UiNode};

/// Request header carrying the client's schema version.
pub const SCHEMA_HEADER: &str = "x-construct-schema";
/// The schema version this build draws.
pub const SCHEMA_VERSION: u32 = 2;

/// The lowest schema version that can draw `node` (and its screens).
pub fn required_schema(node: &UiNode) -> u32 {
    let mut need = 1;
    visit(node, &mut need);
    need
}

fn intent_schema(i: &ActionIntent) -> u32 {
    match i {
        ActionIntent::LoadData { .. } | ActionIntent::Create { .. } => 2,
        _ => 1,
    }
}

fn visit(node: &UiNode, need: &mut u32) {
    fn intent(need: &mut u32, i: &ActionIntent) {
        *need = (*need).max(intent_schema(i));
    }
    match node {
        UiNode::Workspace { slots, screens, .. } => {
            if !screens.is_empty() {
                *need = (*need).max(2);
            }
            for s in slots {
                visit(&s.content, need);
            }
            for s in screens {
                visit(&s.content, need);
                if let Some(f) = &s.footer {
                    visit(f, need);
                }
            }
        }
        UiNode::Container { children, .. } => children.iter().for_each(|c| visit(c, need)),
        UiNode::Button { on_click, .. } => intent(need, on_click),
        UiNode::Text { .. } | UiNode::MediaCarousel { .. } => {}
        UiNode::List { item_template, .. } => {
            *need = (*need).max(2);
            visit(item_template, need);
        }
        UiNode::Image { .. } => *need = (*need).max(2),
        UiNode::TextField { on_submit, .. } => {
            *need = (*need).max(2);
            if let Some(i) = on_submit {
                intent(need, i);
            }
        }
        UiNode::PostCard {
            author_slot,
            media_slot,
            action_slot,
            caption_slot,
            ..
        } => {
            for s in [author_slot, media_slot, action_slot, caption_slot] {
                visit(s, need);
            }
        }
        UiNode::AuthorHeader { on_click, .. } => {
            if let Some(i) = on_click {
                intent(need, i);
            }
        }
        UiNode::ActionBar {
            like_intent,
            bookmark_intent,
            comment_intent,
        } => {
            for i in [like_intent, bookmark_intent, comment_intent] {
                intent(need, i);
            }
        }
        UiNode::FeedList { item_template, .. } => visit(item_template, need),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(v: serde_json::Value) -> UiNode {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn old_vocabulary_is_schema_1_and_new_is_2() {
        let old = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "core://Button", "label": "Go",
             "on_click": {"intent": "action.navigation.open", "target_uri": "x://y", "presentation": "Push"}}}]}),
        );
        assert_eq!(required_schema(&old), 1);
        let list = node(
            json!({"type": "core://Workspace", "id": "a", "title": "A", "slots": [
            {"slot_id": "s", "provider_uri": "p", "content": {"type": "core://List", "query_key": "q",
             "item_template": {"type": "core://Text", "value_expr": "$item.text", "style": "Body"}}}]}),
        );
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
    }
}
