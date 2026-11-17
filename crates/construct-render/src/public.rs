//! A public link's page (decisions `public-web-links`,
//! `public-links-share-the-closure`): the session a visitor's browser draws
//! from a copy the host keeps, read-only and with no pod behind it. Shared
//! by the web client and its tests here.
//!
//! Every format of copy a host has served draws: links people have sent
//! keep working when the format changes.
//!
//! - **Format 1** (no `format` field): `{title, node, data, item?}`, one
//!   layout.
//! - **Format 2**: `{format: 2, layout, title, node, data, item?, params?,
//!   layouts?}`: the root drawn with `params` as `$route` and `item` as
//!   `$item`, and the layouts it opens and embeds by key, so a tap on a row
//!   pushes its screen from the copy (with the row's params) and Back
//!   returns.

use protocol::{DataBundle, UiNode};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::Viewport;
use crate::interact::Session;

/// The newest copy format this build draws.
pub const FORMAT: u64 = 2;

/// The session that draws a public copy. No loader: nothing it draws can
/// write or load more, and a layout it opens comes from the copy.
pub fn session(copy: &Value, viewport: Viewport) -> Result<Session, String> {
    let format = copy["format"].as_u64().unwrap_or(1);
    if format > FORMAT {
        return Err(format!(
            "this page is in a newer format ({format}): reload to update"
        ));
    }
    let root: UiNode = serde_json::from_value(copy["node"].clone()).map_err(|e| e.to_string())?;
    let data: DataBundle = serde_json::from_value(copy["data"].clone()).unwrap_or_default();
    let id = copy["layout"].as_str().unwrap_or("public");
    let mut session = Session::new(id, root, data, viewport);
    for (key, node) in copy["layouts"].as_object().into_iter().flatten() {
        // A layout that doesn't read (a newer node in an older build) only
        // can't be opened; the rest still draws.
        if let Ok(n) = serde_json::from_value::<UiNode>(node.clone()) {
            session.layouts.insert(key.clone(), n);
        }
    }
    let route: BTreeMap<String, String> = copy["params"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), crate::bind::render(v)))
        .collect();
    let item = copy["item"].as_str().map(str::to_owned);
    if !route.is_empty() || item.is_some() {
        session.set_root(route, item);
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interact::Target;
    use serde_json::json;

    fn texts(s: &mut Session) -> Vec<String> {
        s.scene()
            .unwrap()
            .semantics
            .iter()
            .filter_map(|n| n.text_content.clone())
            .collect()
    }

    /// A copy stored in format 1 (before `format`) still draws.
    #[test]
    fn a_format_1_copy_still_draws() {
        let copy = json!({ "title": "Recipes",
            "node": { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Title", "value_expr": "Recipes" },
                { "type": "core://List", "query_key": "collection:recipes", "item_template":
                  { "type": "core://Text", "style": "Body", "value_expr": "$item.title" } } ] },
            "data": { "entities": { "collections://recipes/1": { "id": "1", "title": "Lentil soup" } },
                      "queries": { "collection:recipes": ["collections://recipes/1"] } } });
        let mut s = session(&copy, Viewport::PHONE).unwrap();
        let t = texts(&mut s);
        assert!(t.iter().any(|t| t == "Lentil soup"), "{t:?}");
    }

    /// Format 2: the root draws with its params; a row pushes its screen
    /// from the copy with the row's params, and Back returns.
    #[test]
    fn a_row_opens_its_screen_from_the_copy_and_back_returns() {
        let recipe = json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Title", "value_expr": "${collections://meals/$route.id#name}" },
            { "type": "core://List", "query_key": "collection:lines",
              "where": [{ "field": "meal_id", "equals": "$route.id" }],
              "empty_text": "No ingredients yet",
              "item_template": { "type": "core://Text", "style": "Body", "value_expr": "$item.thing" } },
            { "type": "core://Button", "label": "Back", "on_click": { "intent": "action.navigation.back" } } ] });
        let copy = json!({ "format": 2, "layout": "c-meals", "title": "Meals",
            "node": { "type": "core://List", "query_key": "collection:meals", "item_template":
                { "type": "core://Tappable", "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.name" },
                  "on_click": { "intent": "action.navigation.open", "layout": "c-recipe",
                    "params": { "id": "$item.id" }, "presentation": "Push" } } },
            "layouts": { "c-recipe": recipe },
            "data": { "entities": {
                "collections://meals/m1": { "id": "m1", "name": "Lentil soup" },
                "collections://meals/m2": { "id": "m2", "name": "Shakshuka" },
                "collections://lines/a": { "id": "a", "meal_id": "m1", "thing": "Red lentils" },
                "collections://lines/b": { "id": "b", "meal_id": "m2", "thing": "Eggs" } },
              "queries": { "collection:meals": ["collections://meals/m1", "collections://meals/m2"],
                           "collection:lines": ["collections://lines/a", "collections://lines/b"] } } });
        let mut s = session(&copy, Viewport::PHONE).unwrap();
        assert!(texts(&mut s).contains(&"Shakshuka".to_owned()));
        s.tap(&Target::Text("Shakshuka".into())).unwrap();
        s.settle(std::time::Duration::from_secs(2));
        let t = texts(&mut s);
        assert!(t.contains(&"Eggs".to_owned()), "the pushed recipe: {t:?}");
        assert!(!t.contains(&"Red lentils".to_owned()), "{t:?}");
        s.tap(&Target::Text("Back".into())).unwrap();
        s.settle(std::time::Duration::from_secs(2));
        let t = texts(&mut s);
        assert!(
            t.contains(&"Lentil soup".to_owned()) && !t.contains(&"Eggs".to_owned()),
            "{t:?}"
        );
    }

    /// A root drawn with `params` reads them as `$route`.
    #[test]
    fn the_root_reads_its_params() {
        let copy = json!({ "format": 2, "layout": "c-recipe", "title": "Recipe", "params": { "id": "m1" },
            "node": { "type": "core://Text", "style": "Title", "value_expr": "${collections://meals/$route.id#name}" },
            "data": { "entities": { "collections://meals/m1": { "id": "m1", "name": "Lentil soup" } } } });
        let mut s = session(&copy, Viewport::PHONE).unwrap();
        assert!(texts(&mut s).contains(&"Lentil soup".to_owned()));
        assert!(session(&json!({ "format": 99, "node": {} }), Viewport::PHONE).is_err());
    }
}
