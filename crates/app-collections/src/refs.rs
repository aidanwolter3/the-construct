//! `ref` fields and `unique` (decision `collection-primitives`).
//!
//! A ref field holds the id of a row of its `collection`. Every write that
//! comes from outside (a layout, an MCP edit, an action) goes through
//! [`prepare_write`]: a ref value must name a live row, either by its id or
//! as `{"<unique field>": value}`, which is resolved to that row's id. When
//! the row a ref points at is deleted later, the ref keeps the id (layouts
//! reading through it see nothing, lookups through it answer nothing) and a
//! restore heals it.
//!
//! A `unique` field (text or ref) has no two live rows with the same value,
//! text compared as exact lookups compare (ignoring case and spacing).
//! [`check_unique`] runs on every create, update and restore, and
//! [`duplicates`] when a definition change makes a field unique.

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::db::{self, Row};
use crate::model::{self, Definition, Field, Kind};

/// A value as a unique key: text normalized, a ref's id as is; `None` when
/// empty.
pub fn unique_key(field: &Field, v: &Value) -> Option<String> {
    let t = crate::lookup::text_of(v)?;
    Some(if field.kind == Kind::Text {
        model::cache_key(&t)
    } else {
        t
    })
}

/// The live row of `target` whose unique field `field` holds `value`.
fn by_unique(
    conn: &Connection,
    target: &Definition,
    field: &Field,
    value: &Value,
) -> rusqlite::Result<Option<Row>> {
    let Some(want) = unique_key(field, value) else {
        return Ok(None);
    };
    Ok(db::all_rows(conn, &target.name, false)?
        .into_iter()
        .find(|r| {
            r.values
                .get(&field.id)
                .and_then(|v| unique_key(field, v))
                .is_some_and(|k| k == want)
        }))
}

/// The id a value of ref field `field` names: a live row id (or a pending
/// create's `pending-<client id>`), or `{"<unique field>": value}`.
/// `Ok(Err(why))` when it names none.
pub fn resolve(
    conn: &Connection,
    field: &Field,
    v: &Value,
) -> rusqlite::Result<Result<String, String>> {
    let target = field.collection.as_deref().unwrap_or("");
    let Some((_, tdef)) = db::latest_def(conn, target)? else {
        return Ok(Err(format!(
            "'{}' points at '{target}', which doesn't exist",
            field.name
        )));
    };
    match v {
        Value::String(s) => {
            let id = s.trim();
            match crate::find_row(conn, target, id)? {
                Some(r) if r.deleted_at.is_none() => Ok(Ok(r.id)),
                Some(_) => Ok(Err(format!(
                    "'{}': row '{id}' of '{target}' is deleted",
                    field.name
                ))),
                None => Ok(Err(format!(
                    "'{}': '{id}' is not a row of '{target}' (give a row id, or {{\"<unique field>\": value}})",
                    field.name
                ))),
            }
        }
        Value::Object(o) if o.len() == 1 => {
            let (k, want) = o.iter().next().expect("one entry");
            if k == "id" {
                return resolve(conn, field, want);
            }
            let unique: Vec<&str> = tdef
                .live_fields()
                .filter(|f| f.unique)
                .map(|f| f.name.as_str())
                .collect();
            let Some(uf) = tdef.field(k).filter(|f| f.unique) else {
                return Ok(Err(format!(
                    "'{}': '{k}' is not a unique field of '{target}' (unique fields: {})",
                    field.name,
                    if unique.is_empty() {
                        "none; make one unique with update_field".to_owned()
                    } else {
                        unique.join(", ")
                    }
                )));
            };
            match by_unique(conn, &tdef, uf, want)? {
                Some(r) => Ok(Ok(r.id)),
                None => Ok(Err(format!(
                    "'{}': no row of '{target}' has {k} {want}",
                    field.name
                ))),
            }
        }
        other => Ok(Err(format!(
            "'{}': {other} is not a row of '{target}' (give a row id, or {{\"<unique field>\": value}})",
            field.name
        ))),
    }
}

/// `write` (`{name: value}`) with every ref value resolved to the id of a
/// live row of its collection ([`resolve`]). `Ok(Err(why))` names the first
/// value that names none.
pub fn prepare_write(
    conn: &Connection,
    def: &Definition,
    write: &Map<String, Value>,
) -> rusqlite::Result<Result<Map<String, Value>, String>> {
    let mut out = write.clone();
    for (name, v) in write {
        let Some(f) = def.field(name).filter(|f| f.kind == Kind::Ref) else {
            continue;
        };
        if v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()) {
            continue;
        }
        match resolve(conn, f, v)? {
            Ok(id) => {
                out.insert(name.clone(), json!(id));
            }
            Err(e) => return Ok(Err(e)),
        }
    }
    Ok(Ok(out))
}

/// Whether `row` (about to be live in `def`'s collection) keeps every
/// unique field unique. `Ok(Err(why))` names the field and the other row.
pub fn check_unique(
    conn: &Connection,
    def: &Definition,
    row: &Row,
) -> rusqlite::Result<Result<(), String>> {
    let fields: Vec<&Field> = def.live_fields().filter(|f| f.unique).collect();
    if fields.is_empty() {
        return Ok(Ok(()));
    }
    let keys: Vec<(&Field, String)> = fields
        .iter()
        .filter_map(|f| Some((*f, unique_key(f, row.values.get(&f.id)?)?)))
        .collect();
    if keys.is_empty() {
        return Ok(Ok(()));
    }
    for other in db::all_rows(conn, &def.name, false)? {
        if other.id == row.id {
            continue;
        }
        for (f, k) in &keys {
            if other
                .values
                .get(&f.id)
                .and_then(|v| unique_key(f, v))
                .is_some_and(|o| o == *k)
            {
                return Ok(Err(format!(
                    "'{}' is unique, and row '{}' of '{}' already has {}",
                    f.name,
                    other.id,
                    def.name,
                    row.values.get(&f.id).cloned().unwrap_or(Value::Null)
                )));
            }
        }
    }
    Ok(Ok(()))
}

/// The live rows sharing a value of a unique field of `def`: `{field,
/// value, rows}` for each value held more than once.
pub fn duplicates(conn: &Connection, def: &Definition) -> rusqlite::Result<Vec<Value>> {
    let mut out = Vec::new();
    let fields: Vec<&Field> = def.live_fields().filter(|f| f.unique).collect();
    if fields.is_empty() {
        return Ok(out);
    }
    let rows = db::all_rows(conn, &def.name, false)?;
    for f in fields {
        let mut seen: Vec<(String, Value, Vec<String>)> = Vec::new();
        for r in &rows {
            let Some(v) = r.values.get(&f.id) else {
                continue;
            };
            let Some(k) = unique_key(f, v) else {
                continue;
            };
            match seen.iter_mut().find(|(x, _, _)| *x == k) {
                Some((_, _, ids)) => ids.push(r.id.clone()),
                None => seen.push((k, v.clone(), vec![r.id.clone()])),
            }
        }
        for (_, v, ids) in seen.into_iter().filter(|(_, _, ids)| ids.len() > 1) {
            out.push(json!({ "field": f.name, "value": v, "rows": ids }));
        }
    }
    Ok(out)
}

/// The id of the live row of `target` that text `text` names, for turning a
/// text field into a ref: a row id, else the value of one of `target`'s
/// unique text fields (ignoring case and spacing).
pub fn resolve_text(
    conn: &Connection,
    target: &Definition,
    text: &str,
) -> rusqlite::Result<Option<String>> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if let Some(r) = db::row(conn, &target.name, t)?.filter(|r| r.deleted_at.is_none()) {
        return Ok(Some(r.id));
    }
    for f in target
        .live_fields()
        .filter(|f| f.unique && f.kind == Kind::Text)
    {
        if let Some(r) = by_unique(conn, target, f, &json!(t))? {
            return Ok(Some(r.id));
        }
    }
    Ok(None)
}
