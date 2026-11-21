//! Whether a custom layout fits the collections it uses (decision
//! `collections`): every collection it names exists, and every binding to
//! a row names a field the collection has (a live name, an alias, or a
//! system field). The pod runs this on every custom-app write, and on every
//! app that uses a collection when its definition changes, so a change set
//! that would break an app is refused with the app and the binding.
//!
//! What counts as a binding to a row of collection `c`:
//! - `$item.<f>` inside a `core://List` whose `query_key` is
//!   `collection:<c>` or one of its views, `collection:<c>:view:<v>` (the
//!   view must exist), and its `filter.fields`, `group_by.field`,
//!   `where.field` (or each `where[].field`); a list that also merges
//!   `merge_query_keys` lists rows of each of their collections, so a
//!   binding there need only name a field of one of them;
//! - a `core://Text` or `core://Conditional` `count`'s `where[].field` when
//!   its `query_key` is `collection:<c>`;
//! - `${collections://<c>/<id>#<f>}` (`…/fillers/<id>#<f>` and
//!   `…/alerts/<id>#<f>` name a filler's or an alert's own fields instead);
//! - a mutation of `collections://<c>/...` (or of `$item.uri` in such a
//!   list): its `field`, `also[].field` and `entity.<f>` in `value_expr`;
//!   a create's `entity` keys;
//! - the payload keys of `/v1/collections/<c>/{create,update,delete}`
//!   (`refresh` takes none); `/v1/collections/<c>/actions/<a>` names an
//!   action of `c`, its payload keys are the action's params (and
//!   `client_id`), and the mutation's own field is a view flag (not
//!   checked).

use std::collections::HashMap;

use pod_core::validate::LayoutError;
use serde_json::{Map, Value};

use crate::model::{Definition, SYSTEM_FIELDS};

/// The collection a query key lists (`collection:<c>`).
pub fn collection_of_key(key: &str) -> Option<&str> {
    let c = key.strip_prefix("collection:")?;
    (!c.is_empty() && !c.contains(':')).then_some(c)
}

/// The collection a row list's query key lists, and the view when it is
/// one (`collection:<c>` or `collection:<c>:view:<v>`).
pub fn list_of_key(key: &str) -> Option<(&str, Option<&str>)> {
    if let Some(c) = collection_of_key(key) {
        return Some((c, None));
    }
    let (c, v) = key.strip_prefix("collection:")?.split_once(":view:")?;
    (!c.is_empty() && !c.contains(':') && !v.is_empty() && !v.contains(':')).then_some((c, Some(v)))
}

/// The collection of a `collections://<c>/...` URI.
fn collection_of_uri(uri: &str) -> Option<&str> {
    let rest = uri.trim().strip_prefix("collections://")?;
    let c = rest.split('/').next().unwrap_or("");
    (!c.is_empty()).then_some(c)
}

/// A filler or alert URI (`collections://<c>/fillers/<id>`,
/// `…/alerts/<id>`): the kind and the fields its entity has. `None`: a row.
fn entity_of_uri(uri: &str) -> Option<(&'static str, &'static [&'static str])> {
    let rest = uri.trim().strip_prefix("collections://")?;
    let mut parts = rest.split('/');
    parts.next()?;
    let kind = parts.next()?;
    parts.next().filter(|id| !id.is_empty())?;
    match kind {
        "fillers" => Some(("a filler", crate::db::FILLER_FIELDS)),
        "alerts" => Some(("an alert", crate::db::ALERT_FIELDS)),
        _ => None,
    }
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

/// What `$item` is: a row of one of these collections (more than one in a
/// list that merges several queries). `None`: not a row.
type Rows<'r, 'a> = Option<&'r [&'a Definition]>;

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
        if def.field(name).is_some()
            || SYSTEM_FIELDS.contains(&name)
            || def.bindable().iter().any(|b| b == name)
        {
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

    /// `check` passes for one of `defs` (the first one's error otherwise).
    fn any(
        defs: &[&'a Definition],
        check: impl Fn(&'a Definition) -> Result<(), LayoutError>,
    ) -> Result<(), LayoutError> {
        let mut first = None;
        for def in defs {
            match check(def) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
        }
        first.map_or(Ok(()), Err)
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

    fn walk(&self, v: &Value, at: &str, item: Rows<'_, 'a>) -> Result<(), LayoutError> {
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
        item: Rows<'_, 'a>,
    ) -> Result<(), LayoutError> {
        if map.get("intent").is_some_and(Value::is_string) {
            self.intent(map, at, item)?;
        }
        let mut rows: Vec<&'a Definition> = Vec::new();
        let mut row_list = false;
        if let Some(q) = map.get("query_key").and_then(Value::as_str)
            && let Some((c, view)) = list_of_key(q)
        {
            let def = self.def(&format!("{at}/query_key"), c)?;
            if let Some(v) = view
                && crate::views::find(def, v).is_none()
            {
                return Err(err(
                    &format!("{at}/query_key"),
                    format!(
                        "collection '{c}' has no view '{v}' (add it with apply_change set_view)"
                    ),
                    crate::views::names(def)
                        .iter()
                        .map(|n| crate::views::key(c, n))
                        .collect(),
                ));
            }
            if map.contains_key("item_template") {
                row_list = true;
                rows.push(def);
                let merged = map.get("merge_query_keys").and_then(Value::as_array);
                for (i, k) in merged.into_iter().flatten().enumerate() {
                    let here = format!("{at}/merge_query_keys/{i}");
                    if let Some((c, _)) = k.as_str().and_then(list_of_key) {
                        let def = self.def(&here, c)?;
                        if !rows.iter().any(|d| d.name == def.name) {
                            rows.push(def);
                        }
                    }
                }
                self.list_fields(map, at, &rows)?;
            } else if let Some(conds) = map.get("where").and_then(Value::as_array) {
                // A core://Text or core://Conditional count: every
                // condition names a field.
                for (i, w) in conds.iter().enumerate() {
                    if let Some(f) = w["field"].as_str() {
                        self.bindable(&format!("{at}/where/{i}/field"), def, f, "the count")?;
                    }
                }
            }
        }
        let inner: Rows<'_, 'a> = if row_list {
            Some(&rows)
        } else if map.contains_key("item_template") {
            // A list or carousel of something else: its items aren't rows.
            None
        } else {
            item
        };
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
        defs: &[&'a Definition],
    ) -> Result<(), LayoutError> {
        let bindable =
            |at: &str, f: &str, what: &str| Self::any(defs, |def| self.bindable(at, def, f, what));
        if let Some(fields) = map.get("filter").and_then(|f| f["fields"].as_array()) {
            for (i, f) in fields.iter().enumerate() {
                if let Some(f) = f.as_str() {
                    bindable(&format!("{at}/filter/fields/{i}"), f, "the filter")?;
                }
            }
        }
        if let Some(f) = map.get("group_by").and_then(|g| g["field"].as_str()) {
            bindable(&format!("{at}/group_by/field"), f, "group_by")?;
        }
        // `where` is one condition or a list that must all match.
        match map.get("where") {
            Some(Value::Array(conds)) => {
                for (i, w) in conds.iter().enumerate() {
                    if let Some(f) = w["field"].as_str() {
                        bindable(&format!("{at}/where/{i}/field"), f, "where")?;
                    }
                }
            }
            Some(w) => {
                if let Some(f) = w["field"].as_str() {
                    bindable(&format!("{at}/where/field"), f, "where")?;
                }
            }
            None => {}
        }
        Ok(())
    }

    fn intent(
        &self,
        map: &Map<String, Value>,
        at: &str,
        item: Rows<'_, 'a>,
    ) -> Result<(), LayoutError> {
        let s = |k: &str| map.get(k).and_then(Value::as_str);
        // An action's endpoint: the mutation's field is only a view flag.
        if let Some(endpoint) = s("endpoint")
            && let Some((c, op)) = route_of(endpoint)
            && let Some(action) = op.strip_prefix("actions/")
        {
            let def = self.def(&format!("{at}/endpoint"), c)?;
            let Some(a) = crate::actions::find(def, action.trim_end_matches('/')) else {
                return Err(err(
                    &format!("{at}/endpoint"),
                    format!("collection '{c}' has no action '{action}'"),
                    def.actions.iter().map(|a| a.name.clone()).collect(),
                ));
            };
            if let Some(p) = map.get("payload").and_then(Value::as_object) {
                for k in p.keys() {
                    if k != "client_id" && !a.params.iter().any(|x| x.name == *k) {
                        let mut allowed: Vec<String> =
                            a.params.iter().map(|x| x.name.clone()).collect();
                        allowed.push("client_id".into());
                        return Err(err(
                            &format!("{at}/payload/{}", escape(k)),
                            format!("'{k}' is not a parameter of action '{action}' of '{c}'"),
                            allowed,
                        ));
                    }
                }
            }
            return Ok(());
        }
        let named;
        let target: Rows<'_, 'a> = match s("entity_uri") {
            Some(u) if collection_of_uri(u).is_some() => {
                named = [self.def(
                    &format!("{at}/entity_uri"),
                    collection_of_uri(u).unwrap_or(""),
                )?];
                Some(&named)
            }
            Some(u) if u.trim() == "$item.uri" => item,
            _ => None,
        };
        let writable = |defs: &[&'a Definition], at: &str, f: &str, what: &str| {
            Self::any(defs, |def| self.writable(at, def, f, what))
        };
        let bindable = |defs: &[&'a Definition], at: &str, f: &str, what: &str| {
            Self::any(defs, |def| self.bindable(at, def, f, what))
        };
        match s("intent").unwrap_or("") {
            "action.mutation.optimistic" => {
                if let Some(def) = target {
                    if let Some(f) = s("field") {
                        writable(def, &format!("{at}/field"), f, "the mutation")?;
                    }
                    if let Some(also) = map.get("also").and_then(Value::as_array) {
                        for (i, a) in also.iter().enumerate() {
                            if let Some(f) = a["field"].as_str() {
                                writable(def, &format!("{at}/also/{i}/field"), f, "the mutation")?;
                            }
                            for n in names_after(a["value_expr"].as_str().unwrap_or(""), "entity.")
                            {
                                bindable(
                                    def,
                                    &format!("{at}/also/{i}/value_expr"),
                                    n,
                                    "the expression",
                                )?;
                            }
                        }
                    }
                    for n in names_after(s("value_expr").unwrap_or(""), "entity.") {
                        bindable(def, &format!("{at}/value_expr"), n, "the expression")?;
                    }
                }
            }
            "action.mutation.create" => {
                if let (Some(def), Some(entity)) =
                    (target, map.get("entity").and_then(Value::as_object))
                {
                    for k in entity.keys() {
                        bindable(
                            def,
                            &format!("{at}/entity/{}", escape(k)),
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

    fn string(&self, s: &str, at: &str, item: Rows<'_, 'a>) -> Result<(), LayoutError> {
        if let Some(defs) = item {
            for marker in ["$item.", "$item?."] {
                for n in names_after(s, marker) {
                    let what = format!("the binding '{marker}{n}'");
                    Self::any(defs, |def| self.bindable(at, def, n, &what))?;
                }
            }
        }
        for binding in s.split("${").skip(1) {
            let inner = binding.split('}').next().unwrap_or("");
            let (uri, field) = inner.split_once('#').unwrap_or((inner, ""));
            if let Some(c) = collection_of_uri(uri) {
                let def = self.def(at, c)?;
                if field.is_empty() {
                    continue;
                }
                let what = format!("the binding '${{{inner}}}'");
                match entity_of_uri(uri) {
                    Some((kind, fields)) => {
                        if !fields.contains(&field) {
                            return Err(err(
                                at,
                                format!(
                                    "{what} names '{field}', which is not a field of {kind} of collection '{}'",
                                    def.name
                                ),
                                fields.iter().map(|f| (*f).to_owned()).collect(),
                            ));
                        }
                    }
                    None => self.bindable(at, def, field, &what)?,
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
            fillers: Vec::new(),
            alerts: Vec::new(),
            system: None,
            actions: Vec::new(),
            views: Vec::new(),
            keep: None,
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
        // Filler and alert keys may be bound before there is a filler or an
        // alert (a core layout on a new pod): they read blank until then.
        let derived = list(json!({"type": "core://Text",
            "value_expr": "$item.filler_title ${collections://todos/1#alert_names}"}));
        assert_eq!(check(&derived, &[], &defs()), Ok(()));
    }

    #[test]
    fn mutations_and_payloads_must_write_fields() {
        let toggle = |field: &str, payload: Value| {
            list(
                json!({"type": "core://IconButton", "icon": "check", "content_description": "Done",
                "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": field, "value_expr": format!("!entity.{field}"),
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
        // A conditional's count names fields the same way.
        let cond = |field: &str| {
            json!({"type": "core://Conditional", "content": {"type": "core://Divider"},
                "count": {"query_key": "collection:todos", "where": [{"field": field, "equals": "false"}]}})
        };
        assert_eq!(check(&cond("done"), &[], &defs()), Ok(()));
        let e = check(&cond("finished"), &[], &defs()).unwrap_err();
        assert_eq!(e.pointer, "/count/where/0/field");
    }

    #[test]
    fn every_list_where_condition_must_name_fields() {
        let list = |field: &str| {
            json!({"type": "core://List", "query_key": "collection:todos",
                "where": [{"field": "deleted", "equals": "false"}, {"field": field, "equals": "$form.tab"}],
                "item_template": {"type": "core://Divider"}})
        };
        assert_eq!(check(&list("done"), &[], &defs()), Ok(()));
        let e = check(&list("finished"), &[], &defs()).unwrap_err();
        assert_eq!(e.pointer, "/where/1/field");
    }

    #[test]
    fn a_merged_list_binds_fields_of_any_of_its_collections() {
        let mut d = defs();
        let mut events = d["todos"].clone();
        events.name = "events".into();
        events.fields = vec![
            model::parse_field(&json!({"name": "seen", "type": "bool"}), "f1".into(), 99).unwrap(),
        ];
        d.insert("events".into(), events);
        let list = |template: Value, field: &str| {
            json!({"type": "core://List", "query_key": "collection:todos",
                "merge_query_keys": ["collection:events"], "sort_by": "title",
                "where": [{"field": field, "equals": "false"}], "item_template": template})
        };
        let mark = |field: &str| {
            json!({"type": "core://Tappable", "content": {"type": "core://Divider"},
                "on_click": {"intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": field, "value_expr": "!entity.seen"}})
        };
        let text = json!({"type": "core://Text", "value_expr": "$item.title $item.seen"});
        assert_eq!(check(&list(text.clone(), "seen"), &[], &d), Ok(()));
        assert_eq!(check(&list(mark("seen"), "done"), &[], &d), Ok(()));
        let bad = json!({"type": "core://Text", "value_expr": "$item.colour"});
        let e = check(&list(bad, "done"), &[], &d).unwrap_err();
        assert!(e.message.contains("'$item.colour'"), "{}", e.message);
        assert!(check(&list(mark("colour"), "done"), &[], &d).is_err());
        assert!(check(&list(text.clone(), "colour"), &[], &d).is_err());
        // A merged key must name a collection there is.
        let mut missing = list(text, "done");
        missing["merge_query_keys"] = json!(["collection:notes"]);
        let e = check(&missing, &[], &d).unwrap_err();
        assert_eq!(e.pointer, "/merge_query_keys/0");
    }

    #[test]
    fn removed_fields_are_named_as_removed() {
        let mut d = defs();
        d.get_mut("todos").unwrap().fields[1].removed_at = Some(1);
        let bad = list(json!({"type": "core://Text", "value_expr": "$item.done"}));
        let e = check(&bad, &[], &d).unwrap_err();
        assert!(e.message.contains("was removed"), "{}", e.message);
    }

    #[test]
    fn a_view_lists_rows_of_its_collection() {
        let mut d = defs();
        let todos = d.get_mut("todos").unwrap();
        let v = crate::views::parse(todos, &json!({"name": "open", "limit": 3})).unwrap();
        todos.views.push(v);
        let list = |key: &str, template: Value| json!({"type": "core://List", "query_key": key, "item_template": template});
        let ok = list(
            "collection:todos:view:open",
            json!({"type": "core://Text", "value_expr": "$item.title"}),
        );
        assert_eq!(check(&ok, &[], &d), Ok(()));
        let bad = list(
            "collection:todos:view:open",
            json!({"type": "core://Text", "value_expr": "$item.colour"}),
        );
        let e = check(&bad, &[], &d).unwrap_err();
        assert!(e.message.contains("'$item.colour'"), "{}", e.message);
        let missing = list(
            "collection:todos:view:later",
            json!({"type": "core://Divider"}),
        );
        let e = check(&missing, &[], &d).unwrap_err();
        assert_eq!(e.pointer, "/query_key");
        assert!(e.message.contains("no view 'later'"), "{}", e.message);
        assert_eq!(e.allowed, ["collection:todos:view:open"]);
        // Removing the view breaks the layout the same way.
        d.get_mut("todos").unwrap().views.clear();
        assert!(check(&ok, &[], &d).is_err());
        assert_eq!(list_of_key("collection:todos"), Some(("todos", None)));
        assert_eq!(
            list_of_key("collection:todos:view:open"),
            Some(("todos", Some("open")))
        );
        assert_eq!(list_of_key("collection:todos:fillers"), None);
        assert_eq!(list_of_key("collection:todos:alerts"), None);
        assert_eq!(list_of_key("collection:todos:view:"), None);
    }

    #[test]
    fn filler_and_alert_bindings_name_their_entity_fields() {
        let d = defs();
        let text = |v: &str| json!({"type": "core://Text", "value_expr": v});
        for ok in [
            "${collections://todos/fillers/$route.id#alerts_text}",
            "${collections://todos/fillers/f1#title}",
            "${collections://todos/alerts/a1#summary}",
            "${collections://todos/1#title}",
        ] {
            assert_eq!(check(&text(ok), &[], &d), Ok(()), "{ok}");
        }
        let e = check(&text("${collections://todos/fillers/f1#colour}"), &[], &d).unwrap_err();
        assert!(
            e.message.contains("not a field of a filler"),
            "{}",
            e.message
        );
        assert!(
            e.allowed.contains(&"alerts_text".to_owned()),
            "{:?}",
            e.allowed
        );
        let e = check(&text("${collections://todos/alerts/a1#title}"), &[], &d).unwrap_err();
        assert!(
            e.message.contains("not a field of an alert"),
            "{}",
            e.message
        );
        // A row's binding still reads the collection's fields.
        assert!(check(&text("${collections://todos/1#alerts_text}"), &[], &d).is_err());
        // An unknown collection is still refused.
        assert!(check(&text("${collections://nope/fillers/f1#title}"), &[], &d).is_err());
    }
}
