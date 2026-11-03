//! Changing a collection's definition (decision `collections`): the change
//! ops of a change set, and restoring an older version, each turned into a
//! new definition plus the fields whose stored values must be converted.
//!
//! | Op | Effect |
//! |---|---|
//! | `{op: add_field, name, type, options?, optional?, default?, compute?}` | a new field (optional or with a default: existing rows read empty or the default; a computed one is then computed for them) |
//! | `{op: rename_field, field, to}` | a new name; the old one stays an alias |
//! | `{op: remove_field, field}` | soft removal: hidden, data kept for the purge window |
//! | `{op: restore_field, field}` | undo a removal (its data comes back) |
//! | `{op: update_field, field, type?, options?, optional?, default?, compute?}` | values converted row by row; what doesn't fit is kept aside (`compute: null` stops computing; values stay) |
//! | `{op: set_order, field?, descending?}` | the list order: a field or a list of fields, first key first (none: oldest first) |
//! | `{op: set_title, title}` | the collection's title |

use serde_json::Value;

use crate::model::{self, Definition, Field};

/// A new definition, the ids of the fields whose values must be converted
/// (their type or options changed) and of the fields to compute for
/// existing rows (their `compute` is new or changed).
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub def: Definition,
    pub convert: Vec<String>,
    pub compute: Vec<String>,
}

/// The field an op names (by name or alias, live or, for `restore_field`,
/// removed).
fn find<'a>(def: &'a mut Definition, name: &str, removed: bool) -> Result<&'a mut Field, String> {
    let names: Vec<String> = def
        .fields
        .iter()
        .filter(|f| f.live() != removed)
        .map(|f| f.name.clone())
        .collect();
    def.fields
        .iter_mut()
        .filter(|f| f.live() != removed)
        .find(|f| f.answers_to(name))
        .ok_or_else(|| {
            format!(
                "no {}field '{name}' (fields: {})",
                if removed { "removed " } else { "" },
                names.join(", ")
            )
        })
}

/// Apply change `ops` to `def`. `Err((op index, why))`.
pub fn plan(
    def: &Definition,
    ops: &[Value],
    now: i64,
    max_chars: usize,
    max_fields: usize,
) -> Result<Planned, (usize, String)> {
    let mut def = def.clone();
    let before = def.clone();
    for (i, op) in ops.iter().enumerate() {
        apply_op(&mut def, op, now, max_chars).map_err(|e| (i, e))?;
    }
    model::check_definition(&def, max_fields).map_err(|e| (ops.len().saturating_sub(1), e))?;
    Ok(Planned {
        convert: to_convert(&before, &def),
        compute: to_compute(&before, &def),
        def,
    })
}

fn to_compute(before: &Definition, after: &Definition) -> Vec<String> {
    after
        .live_fields()
        .filter(|f| {
            f.compute.is_some()
                && before.by_id(&f.id).is_none_or(|b| {
                    !b.live()
                        || b.compute != f.compute
                        || b.kind != f.kind
                        || b.options != f.options
                })
        })
        .map(|f| f.id.clone())
        .collect()
}

fn to_convert(before: &Definition, after: &Definition) -> Vec<String> {
    after
        .fields
        .iter()
        .filter(|f| {
            before
                .by_id(&f.id)
                .is_some_and(|b| b.kind != f.kind || b.options != f.options)
        })
        .map(|f| f.id.clone())
        .collect()
}

fn apply_op(def: &mut Definition, op: &Value, now: i64, max_chars: usize) -> Result<(), String> {
    let arg = |k: &str| op[k].as_str().map(str::trim).unwrap_or("");
    match op["op"].as_str().unwrap_or("") {
        "add_field" => {
            let id = format!("f{}", def.next_field);
            let field = model::parse_field(op, id, max_chars)?;
            if !field.optional && field.default.is_none() {
                return Err(format!(
                    "a new field must be optional or have a default: existing {} have no '{}'",
                    def.title.to_lowercase(),
                    field.name
                ));
            }
            if def.field(&field.name).is_some() {
                return Err(format!("there is already a field '{}'", field.name));
            }
            def.next_field += 1;
            let id = field.id.clone();
            def.fields.push(field);
            model::set_compute(def, &id, op.get("compute"))?;
        }
        "rename_field" => {
            let to = arg("to").to_owned();
            if !model::valid_name(&to) || model::SYSTEM_FIELDS.contains(&to.as_str()) {
                return Err(format!(
                    "'{to}' is not a field name ([a-z][a-z0-9_]*, not a system field)"
                ));
            }
            let name = arg("field").to_owned();
            if let Some(other) = def.field(&to)
                && !other.answers_to(&name)
            {
                return Err(format!("'{to}' is already the field '{}'", other.name));
            }
            let f = find(def, &name, false)?;
            if f.name != to {
                let old = std::mem::replace(&mut f.name, to.clone());
                f.aliases.retain(|a| *a != to);
                if !f.aliases.contains(&old) {
                    f.aliases.push(old);
                }
            }
        }
        "remove_field" => {
            let f = find(def, arg("field"), false)?;
            f.removed_at = Some(now);
            let id = f.id.clone();
            def.order_by.retain(|o| *o != id);
        }
        "restore_field" => {
            let name = arg("field").to_owned();
            let f = find(def, &name, true)?;
            if f.purged {
                return Err(format!(
                    "the data of '{}' was purged; it can't come back (add a new field)",
                    f.name
                ));
            }
            f.removed_at = None;
            let restored = f.clone();
            if let Some(n) = std::iter::once(&restored.name)
                .chain(&restored.aliases)
                .find(|n| {
                    def.live_fields()
                        .any(|o| o.id != restored.id && o.answers_to(n))
                })
            {
                return Err(format!(
                    "'{n}' is now another field's name; rename that one first"
                ));
            }
        }
        "update_field" => {
            let f = find(def, arg("field"), false)?;
            if let Some(t) = op.get("type").and_then(Value::as_str) {
                f.kind = model::Kind::parse(t).ok_or_else(|| {
                    format!("type '{t}' is not one of {}", model::Kind::ALL.join(", "))
                })?;
            }
            model::set_options(f, op.get("options"))?;
            if let Some(o) = op.get("optional").and_then(Value::as_bool) {
                f.optional = o;
            }
            model::set_default(f, op.get("default"), max_chars)?;
            if !f.optional && f.default.is_none() && op.get("optional") == Some(&Value::Bool(false))
            {
                return Err(format!(
                    "a field made required needs a default: existing rows may have no '{}'",
                    f.name
                ));
            }
            let id = f.id.clone();
            model::set_compute(def, &id, op.get("compute"))?;
        }
        "set_order" => {
            let names = model::order_names(op.get("field"))?;
            def.order_by = model::resolve_order(def, &names)?;
            def.descending = op["descending"].as_bool().unwrap_or(false);
        }
        "set_title" => {
            def.title = arg("title").to_owned();
        }
        other => {
            return Err(format!(
                "unknown op '{other}' (add_field, rename_field, remove_field, restore_field, update_field, set_order, set_title)"
            ));
        }
    }
    Ok(())
}

/// The definition that makes `old` (an earlier version) current again,
/// from `current`: fields keep their ids; a field removed since comes back
/// (with its data, unless purged); a field added since is soft-removed;
/// every name a field had stays an alias.
pub fn restore(
    current: &Definition,
    old: &Definition,
    now: i64,
    max_fields: usize,
) -> Result<Planned, String> {
    let mut def = old.clone();
    def.next_field = current.next_field.max(old.next_field);
    def.fields = current
        .fields
        .iter()
        .map(|cur| match old.by_id(&cur.id) {
            Some(o) => {
                let mut f = o.clone();
                let mut aliases: Vec<String> = cur.aliases.clone();
                for a in o.aliases.iter().chain([&cur.name]) {
                    if !aliases.contains(a) {
                        aliases.push(a.clone());
                    }
                }
                aliases.retain(|a| *a != f.name);
                f.aliases = aliases;
                if cur.purged {
                    f.purged = true;
                    f.removed_at = cur.removed_at;
                }
                f
            }
            None => {
                let mut f = cur.clone();
                if f.removed_at.is_none() {
                    f.removed_at = Some(now);
                }
                f
            }
        })
        .collect();
    let order: Vec<String> = def
        .order_by
        .iter()
        .filter(|id| def.by_id(id).is_some_and(Field::live))
        .cloned()
        .collect();
    def.order_by = order;
    // A computed field whose source is gone again stops computing.
    let gone: Vec<String> = def
        .fields
        .iter()
        .filter(|f| {
            f.compute.as_ref().is_some_and(|c| {
                c.sources()
                    .iter()
                    .any(|id| !def.by_id(id).is_some_and(Field::live))
            })
        })
        .map(|f| f.id.clone())
        .collect();
    for f in def.fields.iter_mut().filter(|f| gone.contains(&f.id)) {
        f.compute = None;
    }
    model::check_definition(&def, max_fields)?;
    Ok(Planned {
        convert: to_convert(current, &def),
        compute: to_compute(current, &def),
        def,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn todos() -> Definition {
        let fields = [
            json!({"name": "title", "type": "text"}),
            json!({"name": "done", "type": "bool", "default": false}),
            json!({"name": "due", "type": "date", "optional": true}),
        ];
        Definition {
            name: "todos".into(),
            title: "Todos".into(),
            fields: fields
                .iter()
                .enumerate()
                .map(|(i, s)| model::parse_field(s, format!("f{}", i + 1), 99).unwrap())
                .collect(),
            order_by: Vec::new(),
            descending: false,
            next_field: 4,
        }
    }

    #[test]
    fn ops_add_rename_remove_and_retype() {
        let p = plan(
            &todos(),
            &[
                json!({"op": "add_field", "name": "priority", "type": "choice", "options": ["low", "high"], "default": "low"}),
                json!({"op": "rename_field", "field": "due", "to": "deadline"}),
                json!({"op": "remove_field", "field": "done"}),
                json!({"op": "update_field", "field": "title", "type": "number"}),
                json!({"op": "set_order", "field": "deadline"}),
            ],
            100,
            99,
            10,
        )
        .unwrap();
        let d = &p.def;
        assert_eq!(d.field("priority").unwrap().id, "f4");
        assert_eq!(d.next_field, 5);
        assert_eq!(
            d.field("due").unwrap().name,
            "deadline",
            "the old name is an alias"
        );
        assert!(d.field("done").is_none() && d.is_removed_name("done"));
        assert_eq!(d.order_by, vec!["f3".to_owned()]);
        assert_eq!(p.convert, vec!["f1".to_owned()]);
        // Renaming back drops the alias.
        let back = plan(
            d,
            &[json!({"op": "rename_field", "field": "deadline", "to": "due"})],
            1,
            99,
            10,
        )
        .unwrap();
        assert_eq!(
            back.def.field("due").unwrap().aliases,
            vec!["deadline".to_owned()]
        );
    }

    #[test]
    fn orders_take_several_fields_and_computed_fields_are_reported() {
        let p = plan(
            &todos(),
            &[
                json!({"op": "add_field", "name": "kind", "type": "choice",
                       "options": ["home", "work"], "default": "home",
                       "compute": {"kind": "model", "from": "title", "prompt": "Where is the task done?"}}),
                json!({"op": "add_field", "name": "label", "type": "text", "optional": true,
                       "compute": {"kind": "expr", "expr": "entity.title + \" (\" + entity.kind + \")\""}}),
                json!({"op": "set_order", "field": ["kind", "done"]}),
            ],
            1,
            99,
            10,
        );
        // A computed field can't read another computed field (yet).
        assert!(p.unwrap_err().1.contains("computed itself"));
        let p = plan(
            &todos(),
            &[
                json!({"op": "add_field", "name": "kind", "type": "choice",
                       "options": ["home", "work"], "default": "home",
                       "compute": {"kind": "model", "from": "title", "prompt": "Where is the task done?"}}),
                json!({"op": "add_field", "name": "label", "type": "text", "optional": true,
                       "compute": {"kind": "expr", "expr": "entity.done ? \"done: \" + entity.title : entity.title"}}),
                json!({"op": "set_order", "field": ["kind", "done"]}),
            ],
            1,
            99,
            10,
        )
        .unwrap();
        assert_eq!(p.def.order_by, vec!["f4".to_owned(), "f2".to_owned()]);
        assert_eq!(p.compute, vec!["f4".to_owned(), "f5".to_owned()]);
        // Removing an order field drops it from the order; the rest stays.
        let p2 = plan(
            &p.def,
            &[
                json!({"op": "set_order", "field": "done"}),
                json!({"op": "remove_field", "field": "done"}),
            ],
            2,
            99,
            10,
        );
        assert!(
            p2.unwrap_err().1.contains("which is removed"),
            "a source can't be removed while a field computes from it"
        );
        let p2 = plan(
            &p.def,
            &[json!({"op": "remove_field", "field": "due"})],
            2,
            99,
            10,
        )
        .unwrap();
        assert!(
            p2.compute.is_empty(),
            "unchanged computes are not reported again"
        );
        // Changing a choice's options recomputes it.
        let p2 = plan(
            &p.def,
            &[json!({"op": "update_field", "field": "kind", "options": ["home", "work", "errand"]})],
            2,
            99,
            10,
        )
        .unwrap();
        assert_eq!(p2.compute, vec!["f4".to_owned()]);
        // Stop computing, then the order goes back to oldest first.
        let p3 = plan(
            &p.def,
            &[
                json!({"op": "update_field", "field": "kind", "compute": null}),
                json!({"op": "set_order"}),
            ],
            3,
            99,
            10,
        )
        .unwrap();
        assert!(p3.def.field("kind").unwrap().compute.is_none());
        assert!(p3.def.order_by.is_empty());
        for bad in [
            json!({"op": "set_order", "field": ["done", "nope"]}),
            json!({"op": "set_order", "field": ["done", "done"]}),
            json!({"op": "add_field", "name": "x", "type": "text",
                   "compute": {"kind": "expr", "expr": "entity.title"}}),
            json!({"op": "add_field", "name": "x", "type": "text", "optional": true,
                   "compute": {"kind": "expr", "expr": "entity.title *"}}),
        ] {
            assert!(
                plan(&todos(), std::slice::from_ref(&bad), 1, 99, 10).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn bad_ops_name_the_op() {
        let cases = [
            json!({"op": "add_field", "name": "notes", "type": "text"}),
            json!({"op": "add_field", "name": "title", "type": "text", "optional": true}),
            json!({"op": "rename_field", "field": "due", "to": "title"}),
            json!({"op": "rename_field", "field": "nope", "to": "x"}),
            json!({"op": "remove_field", "field": "nope"}),
            json!({"op": "restore_field", "field": "title"}),
            json!({"op": "update_field", "field": "due", "optional": false}),
            json!({"op": "explode"}),
        ];
        for op in cases {
            let ok = json!({"op": "set_title", "title": "Tasks"});
            let err = plan(&todos(), &[ok, op.clone()], 1, 99, 10).unwrap_err();
            assert_eq!(err.0, 1, "{op}: {}", err.1);
        }
        let all_gone = [
            json!({"op": "remove_field", "field": "title"}),
            json!({"op": "remove_field", "field": "done"}),
            json!({"op": "remove_field", "field": "due"}),
        ];
        assert!(plan(&todos(), &all_gone, 1, 99, 10).is_err());
    }

    #[test]
    fn restore_brings_back_removed_fields_and_removes_new_ones() {
        let v1 = todos();
        let v2 = plan(
            &v1,
            &[
                json!({"op": "remove_field", "field": "due"}),
                json!({"op": "add_field", "name": "priority", "type": "number", "default": 1}),
                json!({"op": "rename_field", "field": "title", "to": "name"}),
                json!({"op": "update_field", "field": "done", "type": "text"}),
            ],
            5,
            99,
            10,
        )
        .unwrap()
        .def;
        let r = restore(&v2, &v1, 9, 10).unwrap();
        assert!(r.def.field("due").is_some(), "removed field is back");
        assert!(r.def.field("priority").is_none() && r.def.is_removed_name("priority"));
        let title = r.def.field("title").unwrap();
        assert_eq!(title.aliases, vec!["name".to_owned()]);
        assert_eq!(r.convert, vec!["f2".to_owned()]);
        assert_eq!(r.def.next_field, 5);
        // A purged field stays gone.
        let mut v2p = v2.clone();
        v2p.fields[2].purged = true;
        let r = restore(&v2p, &v1, 9, 10).unwrap();
        assert!(r.def.field("due").is_none());
    }
}
