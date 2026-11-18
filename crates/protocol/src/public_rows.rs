//! A public link's rows by name (decision `public-row-links`): the address
//! of one row of a shared list, `/<handle>/<slug>/<row>`, names the row by
//! its own name (`apple-finnish-pancakes`), so the page opens that row's
//! screen. The host (for the page's title) and the web client (to open the
//! screen, and to show the address as rows are opened) read the copy the
//! same way, here.
//!
//! The rows are those of the first list in the copy's root whose rows open
//! a screen (`action.navigation.open`). A row's name is its `name` (or
//! `title`) made into lowercase words joined by `-`; a row without one is
//! named by its `id`. Rows with the same name, in the list's order: the
//! first keeps it, the next are `-2`, `-3`, … (a row named `view` or
//! `image`, the page's own paths, is `-2` too). A renamed row answers its
//! new name only: its old address opens the list.

use serde_json::Value;

/// The page's own paths after `/<handle>/<slug>/`: a row named so is `-2`.
const RESERVED: &[&str] = &["view", "image"];

/// One row of a public copy's list, with the address part that names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The last part of the row's address (`apple-finnish-pancakes`).
    pub name: String,
    /// The row's own title (`Apple (Finnish) Pancakes`).
    pub title: String,
    /// The entity's URI in the copy's data.
    pub uri: String,
    /// What a tap on the row runs: the list's `on_click` with the row's
    /// `$item.<field>` bound.
    pub intent: Value,
}

/// Whether `s` may be the last part of a row's address.
pub fn is_row_name(s: &str) -> bool {
    (1..=96).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The rows of a copy (`{node, data, …}`) that have an address, in the
/// list's order.
pub fn rows(copy: &Value) -> Vec<Row> {
    let Some((query, intent)) = row_list(&copy["node"]) else {
        return Vec::new();
    };
    let data = &copy["data"];
    let mut out: Vec<Row> = Vec::new();
    for uri in data["queries"][&query].as_array().into_iter().flatten() {
        let Some(uri) = uri.as_str() else { continue };
        let entity = &data["entities"][uri];
        if !entity.is_object() {
            continue;
        }
        let title = ["name", "title"]
            .iter()
            .find_map(|f| entity[*f].as_str().filter(|t| !t.trim().is_empty()))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let mut base = words(&title);
        if base.is_empty() {
            base = entity["id"]
                .as_str()
                .map(str::to_ascii_lowercase)
                .filter(|id| is_row_name(id))
                .unwrap_or_default();
        }
        if base.is_empty() {
            continue;
        }
        let mut name = base.clone();
        let mut n = 1;
        while RESERVED.contains(&name.as_str()) || out.iter().any(|r| r.name == name) {
            n += 1;
            name = format!("{base}-{n}");
        }
        out.push(Row {
            name,
            title,
            uri: uri.to_owned(),
            intent: bind_item(&intent, entity),
        });
    }
    out
}

/// The row a copy's address names, if any.
pub fn row(copy: &Value, name: &str) -> Option<Row> {
    rows(copy).into_iter().find(|r| r.name == name)
}

/// The row whose screen is open with these route params (the screen a tap
/// on it opened), if any.
pub fn row_with_params<'a>(
    rows: &'a [Row],
    params: &std::collections::BTreeMap<String, String>,
) -> Option<&'a Row> {
    if params.is_empty() {
        return None;
    }
    rows.iter().find(|r| {
        let Some(p) = r.intent["params"].as_object() else {
            return false;
        };
        !p.is_empty() && p.iter().all(|(k, v)| params.get(k) == Some(&text(v)))
    })
}

/// The first list under `node` whose rows open a screen: its query and the
/// intent a row's tap runs (with `$item.` still in it).
fn row_list(node: &Value) -> Option<(String, Value)> {
    fn opens(v: &Value) -> Option<Value> {
        match v {
            Value::Object(m) => {
                if m.get("intent").and_then(Value::as_str) == Some("action.navigation.open") {
                    return Some(v.clone());
                }
                m.values().find_map(opens)
            }
            Value::Array(a) => a.iter().find_map(opens),
            _ => None,
        }
    }
    match node {
        Value::Object(m) => {
            if m.get("type").and_then(Value::as_str) == Some("core://List")
                && let (Some(q), Some(intent)) = (
                    m.get("query_key").and_then(Value::as_str),
                    m.get("item_template").and_then(opens),
                )
            {
                return Some((q.to_owned(), intent));
            }
            m.values().find_map(row_list)
        }
        Value::Array(a) => a.iter().find_map(row_list),
        _ => None,
    }
}

/// `intent` with each `$item.<field>` replaced by the row's field.
fn bind_item(intent: &Value, entity: &Value) -> Value {
    match intent {
        Value::String(s) if s.contains("$item.") => {
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(at) = rest.find("$item.") {
                out.push_str(&rest[..at]);
                let after = &rest[at + "$item.".len()..];
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                out.push_str(&text(&entity[&after[..end]]));
                rest = &after[end..];
            }
            out.push_str(rest);
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| bind_item(x, entity)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), bind_item(x, entity)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A value as text (a string unquoted).
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A title as lowercase words joined by `-` (letters and digits only, at
/// most 80 characters).
fn words(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(80).collect();
    out.trim_end_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn copy() -> Value {
        json!({ "format": 2, "layout": "c-meals", "title": "Recipes",
            "node": { "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Recipes" },
                { "type": "core://List", "query_key": "collection:meals", "item_template":
                    { "type": "core://Tappable", "content": { "type": "core://Text", "value_expr": "$item.name" },
                      "on_click": { "intent": "action.navigation.open", "layout": "c-recipe",
                        "params": { "id": "$item.id" }, "presentation": "Push" } } } ] },
            "data": { "entities": {
                "collections://meals/m1": { "id": "m1", "name": "Apple (Finnish) Pancakes" },
                "collections://meals/m2": { "id": "m2", "name": "Shakshuka" },
                "collections://meals/m3": { "id": "m3", "name": "shakshuka!" },
                "collections://meals/m4": { "id": "m4", "name": "  " },
                "collections://meals/m5": { "id": "m5", "name": "View" } },
              "queries": { "collection:meals": [
                "collections://meals/m1", "collections://meals/m2",
                "collections://meals/m3", "collections://meals/m4",
                "collections://meals/m5"] } } })
    }

    #[test]
    fn rows_are_named_by_their_names() {
        let names: Vec<String> = rows(&copy()).into_iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            [
                "apple-finnish-pancakes",
                "shakshuka",
                "shakshuka-2",
                "m4",
                "view-2"
            ]
        );
    }

    #[test]
    fn a_row_opens_its_screen_with_its_params() {
        let r = row(&copy(), "shakshuka-2").unwrap();
        assert_eq!(r.uri, "collections://meals/m3");
        assert_eq!(r.intent["params"], json!({ "id": "m3" }));
        assert_eq!(r.intent["layout"], "c-recipe");
        assert!(row(&copy(), "lentil-soup").is_none());
    }

    #[test]
    fn an_open_screen_names_its_row() {
        let all = rows(&copy());
        let params = [("id".to_owned(), "m2".to_owned())].into();
        assert_eq!(row_with_params(&all, &params).unwrap().name, "shakshuka");
        assert!(row_with_params(&all, &Default::default()).is_none());
    }

    #[test]
    fn a_copy_whose_rows_open_nothing_has_no_rows() {
        let mut c = copy();
        c["node"]["children"][1]["item_template"]["on_click"] =
            json!({ "intent": "action.link.open", "url": "$item.url" });
        assert!(rows(&c).is_empty());
    }
}
