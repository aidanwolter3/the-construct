//! The pages that stand in for a layout the loader is still fetching, or
//! failed to fetch: a tap opens one at once instead of waiting on the pod
//! (a phone's UI thread stuck for seconds reads as a frozen app), and the
//! layout replaces it where it is open when it comes in
//! ([`super::Session`]'s changes from the loader).

use protocol::UiNode;
use serde_json::{Value, json};

/// The page id of a layout on its way (`construct:opening:<layout>`).
const OPENING: &str = "construct:opening:";
/// The page id of a layout that failed to load (`construct:failed:<layout>`).
const FAILED: &str = "construct:failed:";

/// Back, where the page can go back (as `core:back_button`).
fn back() -> Value {
    json!({ "type": "core://Conditional", "value": "${local://nav#can_go_back}",
        "content": { "type": "core://IconButton", "icon": "arrow_back",
            "content_description": "Back", "tag": "back",
            "on_click": { "intent": "action.navigation.back" } } })
}

fn page(id: String, lines: Value) -> UiNode {
    serde_json::from_value(json!({
        "type": "core://Page", "id": id, "root": { "type": "core://Column",
            "padding_h_dp": 8, "padding_v_dp": 8, "spacing_dp": 12,
            "children": [back(), { "type": "core://Column", "padding_h_dp": 8,
                "spacing_dp": 8, "children": lines }] } }))
    .expect("a valid page")
}

/// What layout `id` shows while the loader fetches it.
pub fn opening(id: &str) -> UiNode {
    page(
        format!("{OPENING}{id}"),
        json!([{ "type": "core://Row", "spacing_dp": 8, "children": [
            { "type": "core://Progress", "size_dp": 16 },
            { "type": "core://Text", "value_expr": "Loading…", "style": "Secondary" }
        ] }]),
    )
}

/// What layout `id` shows when it couldn't be loaded: `error` is a sentence
/// for the owner. Opening it again asks the loader again.
pub fn failed(id: &str, error: &str) -> UiNode {
    page(
        format!("{FAILED}{id}"),
        json!([
            { "type": "core://Text", "value_expr": "Couldn't open this page", "style": "Title" },
            { "type": "core://Text", "value_expr": error, "style": "Secondary" }
        ]),
    )
}

/// Whether `root` is [`failed`]'s page.
pub fn is_failed(root: &UiNode) -> bool {
    matches!(root, UiNode::Page { id, .. } if id.starts_with(FAILED))
}

/// Whether `root` is [`opening`]'s page.
pub fn is_opening(root: &UiNode) -> bool {
    matches!(root, UiNode::Page { id, .. } if id.starts_with(OPENING))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pages_are_valid_and_told_apart() {
        let o = opening("tracking");
        let f = failed("tracking", "Can't reach your pod.");
        assert!(is_opening(&o) && !is_failed(&o));
        assert!(is_failed(&f) && !is_opening(&f));
    }
}
