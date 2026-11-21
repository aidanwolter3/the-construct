//! Publishing a layout that uses collections (decision `sharing`): each
//! collection it names becomes an input (`collection:$in.<c>`,
//! `collections://$in.<c>/…`, `/v1/collections/$in.<c>/…`), and each field
//! of it the layout reads or writes a field input (`$in.<c>__<f>`), so an
//! adopter binds its own collection, renaming fields if it calls them
//! something else. System fields (`id`, `uri`, …) keep their names. The
//! positions are the ones [`crate::bindings`] checks.

use std::collections::{BTreeMap, HashMap};

use pod_core::construct::share::{DataInput, DataSet, FieldUse, field_input};
use pod_core::validate::LayoutError;
use serde_json::{Map, Value, json};

use crate::model::{Definition, SYSTEM_FIELDS};

/// The tool that makes a collection (the suggested schema's).
const DEFINE_TOOL: &str = "define_collection";

fn err(at: &str, message: String) -> LayoutError {
    LayoutError {
        pointer: at.to_owned(),
        message,
        allowed: Vec::new(),
    }
}

fn escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A collection's suggested schema: its definition without data, fillers,
/// alerts, reminders, actions or computed values.
fn schema(def: &Definition) -> Value {
    let fields: Vec<Value> = def
        .live_fields()
        .map(|f| {
            let mut o = json!({ "name": f.name, "type": f.kind, "optional": f.optional });
            if !f.options.is_empty() {
                o["options"] = json!(f.options);
            }
            if f.unique {
                o["unique"] = json!(true);
            }
            if let Some(d) = &f.default {
                o["default"] = d.clone();
            }
            if let Some(c) = &f.collection {
                o["collection"] = json!(c);
            }
            o
        })
        .collect();
    json!({ "tool": DEFINE_TOOL, "args": { "name": def.name, "title": def.title, "fields": fields } })
}

/// The collections the layout names, as inputs being made.
struct Rewriter<'a> {
    defs: &'a HashMap<String, Definition>,
    made: BTreeMap<String, DataInput>,
}

impl Rewriter<'_> {
    fn input(&mut self, c: &str) -> &mut DataInput {
        let def = self.defs.get(c);
        self.made.entry(c.to_owned()).or_insert_with(|| DataInput {
            name: c.to_owned(),
            source: crate::NAME.to_owned(),
            kind: "collection".to_owned(),
            title: def.map_or_else(|| c.to_owned(), |d| d.title.clone()),
            feed: format!("$in.{c}"),
            fields: Vec::new(),
            schema: def.map_or(Value::Null, schema),
        })
    }

    /// Record a field use and answer the field input standing for it
    /// (`None`: a system field, which keeps its name).
    fn field(&mut self, c: &str, f: &str, written: bool) -> Option<String> {
        if f.is_empty() || f.starts_with('$') || SYSTEM_FIELDS.contains(&f) {
            return None;
        }
        let kind = self
            .defs
            .get(c)
            .and_then(|d| d.field(f))
            .and_then(|fl| serde_json::to_value(fl.kind).ok())
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        let input = self.input(c);
        match input.fields.iter_mut().find(|x| x.name == f) {
            Some(x) => {
                if written {
                    x.written = true;
                } else {
                    x.read = true;
                }
            }
            None => input.fields.push(FieldUse {
                name: f.to_owned(),
                kind,
                read: !written,
                written,
            }),
        }
        Some(format!("$in.{}", field_input(c, f)))
    }

    /// `s` with each `<marker><f>` (a field name) replaced by its field input.
    fn fields_after(&mut self, s: &str, marker: &str, c: &str, written: bool) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find(marker) {
            out.push_str(&rest[..i + marker.len()]);
            let after = &rest[i + marker.len()..];
            let end = after
                .find(|ch: char| !is_name_char(ch))
                .unwrap_or(after.len());
            let f = &after[..end];
            // `$item.$in.…` was rewritten already.
            let preceded = i > 0 && rest.as_bytes()[i - 1] == b'.';
            match (!preceded).then(|| self.field(c, f, written)).flatten() {
                Some(fi) => out.push_str(&fi),
                None => out.push_str(f),
            }
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    }

    /// A field-name string (`where.field`, a mutation's `field`).
    fn field_value(&mut self, v: &mut Value, c: &str, written: bool) {
        if let Some(f) = v.as_str()
            && let Some(fi) = self.field(c, f, written)
        {
            *v = json!(fi);
        }
    }

    /// Collection names in a string: `collection:<c>` (the whole string),
    /// `collections://<c>/<row>` and `/v1/collections/<c>/`.
    fn names(&mut self, s: &str, at: &str) -> Result<String, LayoutError> {
        // Every key of a collection (`collection:<c>`, `…:view:<v>`,
        // `…:alerts`, `…:filler:<id>`, ...) names it first.
        if let Some(rest) = s.strip_prefix("collection:") {
            let end = rest
                .find(|ch: char| !is_name_char(ch))
                .unwrap_or(rest.len());
            let (c, tail) = rest.split_at(end);
            // An input or an argument (a component's): bound where it is used.
            if rest.starts_with('$') {
                return Ok(s.to_owned());
            }
            if c.is_empty() || !(tail.is_empty() || tail.starts_with(':')) {
                return Err(err(
                    at,
                    format!(
                        "{s:?} names a collection the copy can't turn into an input: name it \
                         literally (collection:<name>…), or leave this element out"
                    ),
                ));
            }
            self.input(c);
            return Ok(format!("collection:$in.{c}{tail}"));
        }
        let mut out = s.to_owned();
        for marker in ["collections://", "/v1/collections/"] {
            let mut next = String::new();
            let mut rest = out.as_str();
            while let Some(i) = rest.find(marker) {
                next.push_str(&rest[..i + marker.len()]);
                let after = &rest[i + marker.len()..];
                let end = after
                    .find(|ch: char| !is_name_char(ch))
                    .unwrap_or(after.len());
                let c = &after[..end];
                if c.is_empty() {
                    rest = after;
                    continue;
                }
                if marker == "collections://" {
                    // One row by its literal id is the owner's data.
                    let row = after[end..]
                        .strip_prefix('/')
                        .map(|r| r.split(['#', '}', ' ']).next().unwrap_or(""))
                        .unwrap_or("");
                    if !row.is_empty() && !row.contains('$') {
                        return Err(err(
                            at,
                            format!(
                                "{s:?} names one row of collection '{c}': a shared copy can't \
                                 carry it (bind a list's item instead, or leave this element out)"
                            ),
                        ));
                    }
                }
                self.input(c);
                next.push_str(&format!("$in.{c}"));
                rest = &after[end..];
            }
            next.push_str(rest);
            out = next;
        }
        Ok(out)
    }

    fn walk(&mut self, v: &mut Value, at: &str, item: Option<&str>) -> Result<(), LayoutError> {
        match v {
            Value::Object(map) => self.object(map, at, item),
            Value::Array(items) => {
                for (i, c) in items.iter_mut().enumerate() {
                    self.walk(c, &format!("{at}/{i}"), item)?;
                }
                Ok(())
            }
            Value::String(s) => {
                let mut t = self.names(s, at)?;
                if let Some(c) = item {
                    t = self.fields_after(&t, "$item.", c, false);
                }
                *s = t;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn object(
        &mut self,
        map: &mut Map<String, Value>,
        at: &str,
        item: Option<&str>,
    ) -> Result<(), LayoutError> {
        if map.get("intent").is_some_and(Value::is_string) {
            self.intent(map, item);
        }
        let mut inner = item.map(str::to_owned);
        let listed = map
            .get("query_key")
            .and_then(Value::as_str)
            .and_then(crate::bindings::list_of_key)
            .map(|(c, _)| c)
            .filter(|c| !c.starts_with('$'))
            .map(str::to_owned);
        if let Some(c) = listed {
            if map.contains_key("item_template") {
                inner = Some(c.clone());
                if let Some(fields) = map
                    .get_mut("filter")
                    .and_then(|f| f.get_mut("fields"))
                    .and_then(Value::as_array_mut)
                {
                    for f in fields {
                        self.field_value(f, &c, false);
                    }
                }
                if let Some(g) = map.get_mut("group_by").and_then(|g| g.get_mut("field")) {
                    self.field_value(g, &c, false);
                }
            }
            match map.get_mut("where") {
                Some(Value::Array(conds)) => {
                    for w in conds {
                        if let Some(f) = w.get_mut("field") {
                            self.field_value(f, &c, false);
                        }
                    }
                }
                Some(w) => {
                    if let Some(f) = w.get_mut("field") {
                        self.field_value(f, &c, false);
                    }
                }
                None => {}
            }
        } else if map.contains_key("item_template") {
            // A list of something else: its items aren't rows.
            inner = None;
        }
        let keys: Vec<String> = map.keys().cloned().collect();
        for k in keys {
            let here = format!("{at}/{}", escape(&k));
            let ctx = if k == "item_template" || k == "group_by" {
                inner.as_deref()
            } else {
                item
            };
            if let Some(child) = map.get_mut(&k) {
                self.walk(child, &here, ctx)?;
            }
        }
        Ok(())
    }

    /// A mutation's written fields and the fields its expressions read.
    fn intent(&mut self, map: &mut Map<String, Value>, item: Option<&str>) {
        let s =
            |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
        let route = s(map, "endpoint").and_then(|e| {
            let rest = e.trim().strip_prefix("/v1/collections/")?.to_owned();
            let rest = rest.split('?').next().unwrap_or("").to_owned();
            let (c, op) = rest.split_once('/')?;
            Some((c.to_owned(), op.trim_end_matches('/').to_owned()))
        });
        let action = route
            .as_ref()
            .is_some_and(|(_, op)| op.starts_with("actions/"));
        // A create's or update's payload keys are fields.
        if let Some((c, op)) = &route
            && (op == "create" || op == "update")
            && !c.starts_with('$')
            && let Some(Value::Object(p)) = map.get_mut("payload")
        {
            let old = std::mem::take(p);
            for (k, v) in old {
                let key = if k == "client_id" {
                    k
                } else {
                    self.field(c, &k, true).unwrap_or(k)
                };
                p.insert(key, v);
            }
        }
        let target = match s(map, "entity_uri") {
            Some(u) if u.trim() == "$item.uri" => item.map(str::to_owned),
            Some(u) => u
                .trim()
                .strip_prefix("collections://")
                .and_then(|r| r.split('/').next())
                .filter(|c| !c.is_empty() && !c.starts_with('$'))
                .map(str::to_owned),
            None => None,
        };
        let Some(c) = target else {
            return;
        };
        match s(map, "intent").as_deref() {
            Some("action.mutation.optimistic") => {
                if !action && let Some(f) = map.get_mut("field") {
                    self.field_value(f, &c, true);
                }
                if let Some(also) = map.get_mut("also").and_then(Value::as_array_mut) {
                    for a in also {
                        if !action && let Some(f) = a.get_mut("field") {
                            self.field_value(f, &c, true);
                        }
                        if let Some(e) = a.get("value_expr").and_then(Value::as_str) {
                            let e = self.fields_after(e, "entity.", &c, false);
                            a["value_expr"] = json!(e);
                        }
                    }
                }
                if let Some(e) = s(map, "value_expr") {
                    let e = self.fields_after(&e, "entity.", &c, false);
                    map.insert("value_expr".into(), json!(e));
                }
            }
            Some("action.mutation.create") => {
                if let Some(Value::Object(e)) = map.get_mut("entity") {
                    let old = std::mem::take(e);
                    for (k, v) in old {
                        let key = self.field(&c, &k, true).unwrap_or(k);
                        e.insert(key, v);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The collection a query key, data path, endpoint or entity URI names
/// (`collection:<c>…`, `/v1/collections/<c>/…`, `collections://<c>/…`):
/// what an adopted element's reach is judged by (decision
/// `adoption-capabilities`). `None` when it names none literally.
pub fn named_collection(value: &str) -> Option<&str> {
    let v = value.trim();
    let rest = v
        .strip_prefix("collection:")
        .or_else(|| v.strip_prefix("/v1/collections/"))
        .or_else(|| v.strip_prefix("collections://"))?;
    let end = rest
        .find(|ch: char| !is_name_char(ch))
        .unwrap_or(rest.len());
    let (c, tail) = rest.split_at(end);
    (!c.is_empty() && (tail.is_empty() || tail.starts_with([':', '/', '?']))).then_some(c)
}

/// See the module docs.
pub fn share_inputs(
    node: &mut Value,
    defs: &HashMap<String, Definition>,
) -> Result<Vec<DataInput>, LayoutError> {
    let mut r = Rewriter {
        defs,
        made: BTreeMap::new(),
    };
    r.walk(node, "/node", None)?;
    let mut made: Vec<DataInput> = r.made.into_values().collect();
    for d in &mut made {
        d.fields.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(made)
}

/// The pod's collections as data sets an adopter can bind.
pub fn data_sets(defs: &[(i64, Definition)]) -> Vec<DataSet> {
    defs.iter()
        .map(|(_, d)| DataSet {
            source: crate::NAME.to_owned(),
            kind: "collection".to_owned(),
            name: d.name.clone(),
            title: d.title.clone(),
            fields: d
                .live_fields()
                .map(|f| {
                    let t = serde_json::to_value(f.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    (f.name.clone(), t)
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs() -> HashMap<String, Definition> {
        let def: Definition = serde_json::from_value(json!({
            "name": "todos", "title": "To-dos", "next_field": 3,
            "fields": [{"id": "f1", "name": "title", "type": "text"},
                       {"id": "f2", "name": "done", "type": "bool"}]}))
        .unwrap();
        HashMap::from([("todos".to_owned(), def)])
    }

    #[test]
    fn collections_and_fields_become_inputs() {
        let mut node = json!({"type": "core://List", "query_key": "collection:todos",
            "where": {"field": "done", "equals": "false"},
            "item_template": {"type": "core://Row", "children": [
                {"type": "core://Text", "value_expr": "$item.title"},
                {"type": "core://Switch", "value_expr": "$item.done", "on_change": {
                    "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                    "field": "done", "value_expr": "!entity.done",
                    "endpoint": "/v1/collections/todos/update",
                    "payload": {"id": "$item.id", "done": "$value"}}}]}});
        let made = share_inputs(&mut node, &defs()).unwrap();
        assert_eq!(node["query_key"], "collection:$in.todos");
        assert_eq!(node["where"]["field"], "$in.todos__done");
        let row = &node["item_template"]["children"];
        assert_eq!(row[0]["value_expr"], "$item.$in.todos__title");
        let m = &row[1]["on_change"];
        assert_eq!(m["field"], "$in.todos__done");
        assert_eq!(m["value_expr"], "!entity.$in.todos__done");
        assert_eq!(m["endpoint"], "/v1/collections/$in.todos/update");
        assert_eq!(m["payload"]["$in.todos__done"], "$value");
        assert_eq!(m["payload"]["id"], "$item.id");
        assert_eq!(m["entity_uri"], "$item.uri");
        assert_eq!(made.len(), 1);
        let d = &made[0];
        assert_eq!(d.name, "todos");
        assert_eq!(d.feed, "$in.todos");
        let done = d.fields.iter().find(|f| f.name == "done").unwrap();
        assert!(done.read && done.written);
        assert_eq!(done.kind, "bool");
        assert_eq!(d.schema["tool"], DEFINE_TOOL);
        assert_eq!(d.schema["args"]["fields"][0]["name"], "title");
    }

    #[test]
    fn every_key_of_a_collection_becomes_an_input() {
        for (key, want) in [
            ("collection:todos:view:all", "collection:$in.todos:view:all"),
            ("collection:todos:alerts", "collection:$in.todos:alerts"),
            (
                "collection:todos:filler:3:rows",
                "collection:$in.todos:filler:3:rows",
            ),
        ] {
            let mut node = json!({"type": "core://List", "query_key": key,
                "item_template": {"type": "core://Text", "value_expr": "$item.title"}});
            let made = share_inputs(&mut node, &defs()).unwrap();
            assert_eq!(node["query_key"], want);
            assert_eq!(made[0].name, "todos");
        }
        let mut node = json!({"type": "core://List", "query_key": "collection:todos:view:all",
            "item_template": {"type": "core://Text", "value_expr": "$item.title"}});
        share_inputs(&mut node, &defs()).unwrap();
        assert_eq!(
            node["item_template"]["value_expr"],
            "$item.$in.todos__title"
        );
        let mut odd = json!({"type": "core://List", "query_key": "collection:to-dos"});
        assert!(share_inputs(&mut odd, &defs()).is_err());
        assert_eq!(
            named_collection("collection:secret:view:all"),
            Some("secret")
        );
        assert_eq!(named_collection("/v1/collections/a/update"), Some("a"));
        assert_eq!(named_collection("collections://a/$item.id"), Some("a"));
        assert_eq!(named_collection("collection:$arg.c"), None);
    }

    #[test]
    fn one_row_is_refused() {
        let mut node =
            json!({"type": "core://Text", "value_expr": "${collections://todos/7#title}"});
        let e = share_inputs(&mut node, &defs()).unwrap_err();
        assert_eq!(e.pointer, "/node/value_expr");
        let mut create = json!({"intent": "action.mutation.create", "query_key": "collection:todos",
            "entity_uri": "collections://todos/pending-$nonce", "entity": {"id": "pending-$nonce", "title": "$form.title"},
            "endpoint": "/v1/collections/todos/create", "payload": {"title": "$form.title", "client_id": "$nonce"}});
        share_inputs(&mut create, &defs()).unwrap();
        assert_eq!(
            create["entity_uri"],
            "collections://$in.todos/pending-$nonce"
        );
        assert_eq!(create["entity"]["$in.todos__title"], "$form.title");
        assert_eq!(create["payload"]["$in.todos__title"], "$form.title");
        assert_eq!(create["payload"]["client_id"], "$nonce");
    }
}
