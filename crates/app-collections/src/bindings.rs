//! Whether a custom layout fits the collections it uses (decision
//! `collections`): every collection it names exists, and every binding to
//! a row names a field the collection has (a live name, an alias, or a
//! system field). The pod runs this on every custom-app write, and on every
//! app that uses a collection when its definition changes, so a change set
//! that would break an app is refused with the app and the binding.
//!
//! What counts as a binding to a row of collection `c`:
//! - `$item.<f>` inside a `core://List` whose `query_key` is
//!   `collection:<c>`, and its `filter.fields`, `group_by.field`,
//!   `where.field`;
//! - a `core://Text` `count`'s `where[].field` when its `query_key` is
//!   `collection:<c>`;
//! - `${collections://<c>/...#<f>}`;
//! - a mutation of `collections://<c>/...` (or of `$item.uri` in such a
//!   list): its `field`, `also[].field` and `entity.<f>` in `value_expr`;
//!   a create's `entity` keys;
//! - the payload keys of `/v1/collections/<c>/{create,update,delete}`
//!   (`refresh` takes none).

use std::collections::HashMap;

use pod_core::validate::LayoutError;
use serde_json::{Map, Value};

use crate::model::{Definition, SYSTEM_FIELDS};

/// The collection a query key lists (`collection:<c>`).
pub fn collection_of_key(key: &str) -> Option<&str> {
    let c = key.strip_prefix("collection:")?;
    (!c.is_empty() && !c.contains(':')).then_some(c)
}

/// The collection of a `collections://<c>/...` URI.
fn collection_of_uri(uri: &str) -> Option<&str> {
    let rest = uri.trim().strip_prefix("collections://")?;
    let c = rest.split('/').next().unwrap_or("");
    (!c.is_empty()).then_some(c)
}

/// `(collection, op)` of a `/v1/collections/<c>/<op>` path.
fn route_of(path: &str) -> Option<(&str, &str)> {
    let rest = path.trim().strip_prefix("/v1/collections/")?;
    let rest = rest.split('?').next().unwrap_or("");
    let (c, op) = rest.split_once('/')?;
    Some((c, op))
}

fn err(at: &str, message: String, allowed: Vec<String>) -> LayoutError {
    LayoutError {
        pointer: at.to_owned(),
        message,
        allowed,
    }
}

fn escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

/// Field names after `marker` in `s` (`$item.` -> the `<f>` of each
/// `$item.<f>`).
fn names_after<'a>(s: &'a str, marker: &str) -> Vec<&'a str> {
    s.match_indices(marker)
        .filter_map(|(i, _)| {
            let rest = &s[i + marker.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (end > 0).then(|| &rest[..end])
        })
        .collect()
}

struct Checker<'a> {
    defs: &'a HashMap<String, Definition>,
}

impl<'a> Checker<'a> {
    fn def(&self, at: &str, name: &str) -> Result<&'a Definition, LayoutError> {
        self.defs.get(name).ok_or_else(|| {
            let mut names: Vec<String> = self.defs.keys().cloned().collect();
            names.sort();
            err(
                at,
                format!("there is no collection '{name}' (define it with define_collection)"),
                names,
            )
        })
    }

    /// `name` must be a field (or system field) of `def`.
    fn bindable(
        &self,
        at: &str,
        def: &Definition,
        name: &str,
        what: &str,
    ) -> Result<(), LayoutError> {
        if def.field(name).is_some() || SYSTEM_FIELDS.contains(&name) {
            return Ok(());
        }
        let why = if def.is_removed_name(name) {
            format!(
                "{what} names '{name}', which was removed from collection '{}'",
                def.name
            )
        } else {
            format!(
                "{what} names '{name}', which is not a field of collection '{}'",
                def.name
            )
        };
        Err(err(at, why, def.bindable()))
    }

    /// A writable field: a live field, or `deleted`.
    fn writable(
        &self,
        at: &str,
        def: &Definition,
        name: &str,
        what: &str,
    ) -> Result<(), LayoutError> {
        if name == "deleted" || def.field(name).is_some() {
            return Ok(());
        }
        let mut allowed: Vec<String> = def.live_fields().map(|f| f.name.clone()).collect();
        allowed.push("deleted".into());
        let why = if def.is_removed_name(name) {
            format!(
                "{what} writes '{name}', which was removed from collection '{}'",
                def.name
            )
        } else {
            format!(
                "{what} writes '{name}', which is not a field of collection '{}'",
                def.name
            )
        };
        Err(err(at, why, allowed))
    }

    fn walk(&self, v: &Value, at: &str, item: Option<&'a Definition>) -> Result<(), LayoutError> {
        match v {
            Value::Object(map) => self.object(map, at, item),
            Value::Array(items) => items
                .iter()
                .enumerate()
                .try_for_each(|(i, c)| self.walk(c, &format!("{at}/{i}"), item)),
            Value::String(s) => self.string(s, at, item),
            _ => Ok(()),
        }
    }

    fn object(
        &self,
        map: &Map<String, Value>,
        at: &str,
        item: Option<&'a Definition>,
    ) -> Result<(), LayoutError> {
        if map.get("intent").is_some_and(Value::is_string) {
            self.intent(map, at, item)?;
        }
        let mut inner = item;
        if let Some(q) = map.get("query_key").and_then(Value::as_str)
            && let Some(c) = collection_of_key(q)
        {
            let def = self.def(&format!("{at}/query_key"), c)?;
            if map.contains_key("item_template") {
                inner = Some(def);
                self.list_fields(map, at, def)?;
            } else if let Some(conds) = map.get("where").and_then(Value::as_array) {
                // A core://Text count: every condition names a field.
                for (i, w) in conds.iter().enumerate() {
                    if let Some(f) = w["field"].as_str() {
                        self.bindable(&format!("{at}/where/{i}/field"), def, f, "the count")?;
                    }
                }
            }
        } else if map.contains_key("item_template") {
            // A list or carousel of something else: its items aren't rows.
            inner = None;
        }
        for (k, child) in map {
            let here = format!("{at}/{}", escape(k));
            let ctx = if k == "item_template" || k == "group_by" {
                inner
            } else {
                item
            };
            self.walk(child, &here, ctx)?;
        }
        Ok(())
    }

    /// A row list's `filter.fields`, `group_by.field` and `where.field`.
    fn list_fields(
        &self,
        map: &Map<String, Value>,
        at: &str,
        def: &Definition,
    ) -> Result<(), LayoutError> {
        if let Some(fields) = map.get("filter").and_then(|f| f["fields"].as_array()) {
            for (i, f) in fields.iter().enumerate() {
                if let Some(f) = f.as_str() {
                    self.bindable(&format!("{at}/filter/fields/{i}"), def, f, "the filter")?;
                }
            }
        }
        for (k, what) in [("group_by", "group_by"), ("where", "where")] {
            if let Some(f) = map.get(k).and_then(|g| g["field"].as_str()) {
                self.bindable(&format!("{at}/{k}/field"), def, f, what)?;
            }
        }
        Ok(())
    }

    fn intent(
        &self,
        map: &Map<String, Value>,
        at: &str,
        item: Option<&'a Definition>,
    ) -> Result<(), LayoutError> {
        let s = |k: &str| map.get(k).and_then(Value::as_str);
        let target = match s("entity_uri") {
            Some(u) if collection_of_uri(u).is_some() => Some(self.def(
                &format!("{at}/entity_uri"),
                collection_of_uri(u).unwrap_or(""),
            )?),
            Some(u) if u.trim() == "$item.uri" => item,
            _ => None,
        };
        match s("intent").unwrap_or("") {
            "action.mutation.optimistic" => {
                if let Some(def) = target {
                    if let Some(f) = s("field") {
                        self.writable(&format!("{at}/field"), def, f, "the mutation")?;
                    }
                    if let Some(also) = map.get("also").and_then(Value::as_array) {
                        for (i, a) in also.iter().enumerate() {
                            if let Some(f) = a["field"].as_str() {
                                self.writable(
                                    &format!("{at}/also/{i}/field"),
                                    def,
                                    f,
                                    "the mutation",
                                )?;
                            }
                            for n in names_after(a["value_expr"].as_str().unwrap_or(""), "entity.")
                            {
                                self.bindable(
                                    &format!("{at}/also/{i}/value_expr"),
                                    def,
                                    n,
                                    "the expression",
                                )?;
                            }
                        }
                    }
                    for n in names_after(s("value_expr").unwrap_or(""), "entity.") {
                        self.bindable(&format!("{at}/value_expr"), def, n, "the expression")?;
                    }
                }
            }
            "action.mutation.create" => {
                if let (Some(def), Some(entity)) =
                    (target, map.get("entity").and_then(Value::as_object))
                {
                    for k in entity.keys() {
                        self.bindable(
                            &format!("{at}/entity/{}", escape(k)),
                            def,
                            k,
                            "the new item",
                        )?;
                    }
                }
            }
            _ => {}
        }
        if let Some(endpoint) = s("endpoint")
            && let Some((c, op)) = route_of(endpoint)
        {
            let def = self.def(&format!("{at}/endpoint"), c)?;
            let payload = map.get("payload").and_then(Value::as_object);
            let has = |k: &str| payload.is_some_and(|p| p.contains_key(k));
            if matches!(op, "update" | "delete") && !has("id") {
                return Err(err(
                    &format!("{at}/payload"),
                    format!(
                        "a {op} of collection '{c}' needs \"id\" in its payload (e.g. \"$item.id\")"
                    ),
                    vec!["id".into()],
                ));
            }
            if let Some(p) = payload
                && !matches!(op, "delete" | "refresh")
            {
                for k in p.keys() {
                    if matches!(k.as_str(), "id" | "client_id") {
                        continue;
                    }
                    self.writable(
                        &format!("{at}/payload/{}", escape(k)),
                        def,
                        k,
                        "the payload",
                    )?;
                }
            }
        }
        Ok(())
    }

    fn string(&self, s: &str, at: &str, item: Option<&Definition>) -> Result<(), LayoutError> {
        if let Some(def) = item {
            for marker in ["$item.", "$item?."] {
                for n in names_after(s, marker) {
                    self.bindable(at, def, n, &format!("the binding '{marker}{n}'"))?;
                }
            }
        }
        for binding in s.split("${").skip(1) {
            let inner = binding.split('}').next().unwrap_or("");
            let (uri, field) = inner.split_once('#').unwrap_or((inner, ""));
            if let Some(c) = collection_of_uri(uri) {
                let def = self.def(at, c)?;
                if !field.is_empty() {
                    self.bindable(at, def, field, &format!("the binding '${{{inner}}}'"))?;
                }
            }
        }
        if let Some((c, _)) = route_of(s) {
            self.def(at, c)?;
        }
        Ok(())
    }
}

/// Check `layout` (as validated: parts expanded) of a custom app whose
/// Collections feeds are `feeds`, against the definitions `defs` (by name).
pub fn check(
    layout: &Value,
    feeds: &[String],
    defs: &HashMap<String, Definition>,
) -> Result<(), LayoutError> {
    let c = Checker { defs };
    for f in feeds {
        if !defs.contains_key(f) {
            let mut names: Vec<String> = defs.keys().map(|n| format!("collections/{n}")).collect();
            names.sort();
            return Err(err(
                "",
                format!(
                    "the feed 'collections/{f}' names no collection (define it with define_collection)"
                ),
                names,
            ));
        }
    }
    c.walk(layout, "", None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;
    use serde_json::json;

    fn defs() -> HashMap<String, Definition> {
        let fields = [
            json!({"name": "title", "type": "text"}),
            json!({"name": "done", "type": "bool", "default": false}),
        ];
        let mut def = Definition {
            name: "todos".into(),
            title: "Todos".into(),
            fields: fields
                .iter()
                .enumerate()
                .map(|(i, s)| model::parse_field(s, format!("f{}", i + 1), 99).unwrap())
                .collect(),
            order_by: Vec::new(),
            descending: false,
            next_field: 3,
            reminders: Vec::new(),
            filler: None,
        };
        def.fields[0].aliases.push("name".into());
        HashMap::from([("todos".to_owned(), def)])
    }

    fn list(template: Value) -> Value {
        json!({"type": "core://Workspace", "slots": [{"content": {"type": "core://List",
            "query_key": "collection:todos", "where": {"field": "deleted", "equals": "false"},
            "item_template": template}}]})
    }

    #[test]
    fn row_bindings_must_name_fields() {
        let ok = list(
            json!({"type": "core://Text", "value_expr": "$item.title ($item.name, $item.id)"}),
        );
        assert_eq!(check(&ok, &["todos".into()], &defs()), Ok(()));
        let bad = list(json!({"type": "core://Text", "value_expr": "Due $item.due"}));
        let e = check(&bad, &[], &defs()).unwrap_err();
        assert_eq!(e.pointer, "/slots/0/content/item_template/value_expr");
        assert!(e.message.contains("'$item.due'"), "{}", e.message);
        assert!(e.allowed.contains(&"title".to_owned()));
        let entity =
            json!({"type": "core://Text", "value_expr": "${collections://todos/1#colour}"});
        assert!(check(&entity, &[], &defs()).is_err());
        let missing = json!({"type": "core://List", "query_key": "collection:groceries",
            "item_template": {"type": "core://Divider"}});
        assert!(
            check(&missing, &[], &defs())
                .unwrap_err()
                .message
                .contains("no collection")
        );
        assert!(check(&ok, &["groceries".into()], &defs()).is_err());
    }

    #[test]
    fn mutations_and_payloads_must_write_fields() {
        let toggle = |field: &str, payload: Value| {
            list(
                json!({"type": "core://IconButton", "icon": "check", "content_description": "Done",
                "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": field, "value_expr": format!("!entity.{field}"), "rollback_value": false,
                    "endpoint": "/v1/collections/todos/update", "payload": payload}}),
            )
        };
        assert_eq!(
            check(
                &toggle("done", json!({"id": "$item.id", "done": "$value"})),
                &[],
                &defs()
            ),
            Ok(())
        );
        assert!(check(&toggle("finished", json!({"id": "$item.id"})), &[], &defs()).is_err());
        let e = check(&toggle("done", json!({"done": "$value"})), &[], &defs()).unwrap_err();
        assert!(e.message.contains("needs \"id\""), "{}", e.message);
        assert!(
            check(
                &toggle("done", json!({"id": "$item.id", "dun": "$value"})),
                &[],
                &defs()
            )
            .is_err()
        );
        let create = |entity: Value| {
            json!({"type": "core://Button", "label": "Add", "on_click": {
                "intent": "action.mutation.create", "query_key": "collection:todos",
                "entity_uri": "collections://todos/pending-$nonce", "entity": entity,
                "endpoint": "/v1/collections/todos/create",
                "payload": {"title": "$form.title", "client_id": "$nonce"}}})
        };
        assert_eq!(
            check(
                &create(json!({"id": "pending-$nonce", "title": "$form.title"})),
                &[],
                &defs()
            ),
            Ok(())
        );
        assert!(check(&create(json!({"colour": "red"})), &[], &defs()).is_err());
        let screen = json!({"screens": [{"data": "/v1/collections/notes/rows"}]});
        assert!(check(&screen, &[], &defs()).is_err());
    }

    #[test]
    fn a_count_must_name_fields() {
        let count = |field: &str| {
            json!({"type": "core://Text", "style": "Label", "value_expr": "$count left",
                "count": {"query_key": "collection:todos",
                    "where": [{"field": "deleted", "equals": "false"}, {"field": field, "equals": "false"}]}})
        };
        assert_eq!(check(&count("done"), &[], &defs()), Ok(()));
        let e = check(&count("finished"), &[], &defs()).unwrap_err();
        assert_eq!(e.pointer, "/count/where/1/field");
        assert!(
            e.message.contains("the count names 'finished'"),
            "{}",
            e.message
        );
        let other = json!({"type": "core://Text", "style": "Label", "value_expr": "$count",
            "count": {"query_key": "collection:groceries"}});
        assert!(check(&other, &[], &defs()).is_err());
    }

    #[test]
    fn removed_fields_are_named_as_removed() {
        let mut d = defs();
        d.get_mut("todos").unwrap().fields[1].removed_at = Some(1);
        let bad = list(json!({"type": "core://Text", "value_expr": "$item.done"}));
        let e = check(&bad, &[], &d).unwrap_err();
        assert!(e.message.contains("was removed"), "{}", e.message);
    }
}
