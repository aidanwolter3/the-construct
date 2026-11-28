//! Snapshots (decision `shopping-kit`): a collection keeps a daily copy of
//! some of its fields as rows of another collection, one row per row and
//! day, so a value that changes (a price, a weight, a balance) has a
//! history a layout can chart and rollups can take the lowest or highest
//! of.
//!
//! `{into, link, day, fields: {field there: field here}}`: `into`'s ref
//! field `link` points at the row, its date field `day` holds the day (the
//! pod's zone), and each of `fields` gets the row's value. The day's row is
//! made or brought up to date whenever the row is written and once a day
//! for every live row ([`daily`]), so a value that holds still still has a
//! point each day. A row whose fields are all empty is skipped.
//! `into` is read when the snapshot is taken: a missing one, or one whose
//! fields don't fit, skips it.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Caps;
use crate::compute::{self, Job};
use crate::db::{self, Row};
use crate::model::{self, Definition, Kind, Write};

/// The most snapshots a collection keeps.
pub const MAX_SNAPSHOTS: usize = 3;
/// The most fields one snapshot copies.
pub const MAX_SNAPSHOT_FIELDS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The collection the copies go to.
    pub into: String,
    /// Its ref field (name) to this collection.
    pub link: String,
    /// Its date field (name).
    pub day: String,
    /// Its field (name) -> this collection's field (id).
    pub fields: BTreeMap<String, String>,
}

/// `snapshots` as given to `define_collection` (fields by name here).
pub fn parse(def: &Definition, v: Option<&Value>) -> Result<Vec<Snapshot>, String> {
    let items = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        Some(_) => return Err("snapshots is a list of {into, link, day, fields}".into()),
    };
    if items.len() > MAX_SNAPSHOTS {
        return Err(format!(
            "a collection has at most {MAX_SNAPSHOTS} snapshots"
        ));
    }
    let mut out = Vec::new();
    for (i, s) in items.iter().enumerate() {
        let at = format!("snapshots/{i}");
        let text = |k: &str| {
            s.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let (into, link, day) = (text("into"), text("link"), text("day"));
        if into.is_empty() || link.is_empty() || day.is_empty() {
            return Err(format!(
                "{at}: a snapshot is {{into: collection, link: its ref field here, day: its date field, fields: {{field there: field here}}}}"
            ));
        }
        if into == def.name {
            return Err(format!("{at}.into is another collection"));
        }
        let Some(Value::Object(m)) = s.get("fields") else {
            return Err(format!("{at}.fields is {{field there: field here}}"));
        };
        if m.is_empty() || m.len() > MAX_SNAPSHOT_FIELDS {
            return Err(format!("{at}.fields has 1..={MAX_SNAPSHOT_FIELDS} fields"));
        }
        let mut fields = BTreeMap::new();
        for (there, here) in m {
            let here = here.as_str().unwrap_or("");
            let Some(f) = def.field(here).filter(|f| f.live()) else {
                return Err(format!(
                    "{at}.fields.{there}: this collection has no field '{here}'"
                ));
            };
            fields.insert(there.clone(), f.id.clone());
        }
        out.push(Snapshot {
            into,
            link,
            day,
            fields,
        });
    }
    Ok(out)
}

/// As given, for Claude.
pub fn describe(def: &Definition, s: &Snapshot) -> Value {
    let fields: Map<String, Value> = s
        .fields
        .iter()
        .map(|(k, id)| (k.clone(), json!(def.name_of(id))))
        .collect();
    json!({ "into": s.into, "link": s.link, "day": s.day, "fields": fields })
}

fn empty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        _ => false,
    }
}

/// Take today's snapshots of `row` (made or brought up to date). Answers
/// the computed-field jobs the writes need, by collection.
pub fn take(
    tx: &Connection,
    def: &Definition,
    row: &Row,
    today: &str,
    now: i64,
    caps: Caps,
) -> rusqlite::Result<Vec<(String, Vec<Job>)>> {
    let mut jobs: Vec<(String, Vec<Job>)> = Vec::new();
    if row.deleted_at.is_some() {
        return Ok(jobs);
    }
    for s in &def.snapshots {
        let Some((_, into)) = db::latest_def(tx, &s.into)? else {
            continue;
        };
        let (Some(link), Some(day)) = (into.field(&s.link), into.field(&s.day)) else {
            continue;
        };
        if link.kind != Kind::Ref
            || link.collection.as_deref() != Some(def.name.as_str())
            || !matches!(day.kind, Kind::Date | Kind::Text)
        {
            continue;
        }
        let mut write = Map::new();
        for (there, here) in &s.fields {
            if into.field(there).is_some_and(|f| f.live() && !f.computed()) {
                write.insert(
                    there.clone(),
                    row.values.get(here).cloned().unwrap_or(Value::Null),
                );
            }
        }
        if write.values().all(|v| empty(Some(v))) {
            continue;
        }
        let (link_id, day_id) = (link.id.clone(), day.id.clone());
        let existing = db::all_rows(tx, &into.name, false)?.into_iter().find(|r| {
            r.values.get(&link_id).and_then(Value::as_str) == Some(row.id.as_str())
                && r.values.get(&day_id).and_then(Value::as_str) == Some(today)
        });
        match existing {
            Some(mut r) => {
                let before = r.values.clone();
                if model::apply_write(&into, &mut r.values, &write, Write::Update, caps.text_chars)
                    .is_err()
                    || r.values == before
                {
                    continue;
                }
                let j = compute::after_write(tx, &into, Some(&before), &mut r, &write, now, caps)?;
                jobs.push((into.name.clone(), j));
                r.updated_at = now;
                db::save_row(tx, &into.name, &r)?;
            }
            None => {
                write.insert(s.link.clone(), json!(row.id));
                write.insert(s.day.clone(), json!(today));
                let mut r = Row {
                    id: db::new_id("r"),
                    client_id: None,
                    values: Map::new(),
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
                    filler: None,
                };
                if model::apply_write(&into, &mut r.values, &write, Write::Create, caps.text_chars)
                    .is_err()
                {
                    continue;
                }
                let j = compute::after_write(tx, &into, None, &mut r, &write, now, caps)?;
                jobs.push((into.name.clone(), j));
                db::insert_row(tx, &into.name, &r)?;
            }
        }
    }
    Ok(jobs)
}

/// Every live row's snapshots for today, once a day (the scheduler's tick
/// asks; rows with today's copy already in place write nothing).
pub async fn daily(pod: &pod_core::Pod) {
    let Some(adapter) = pod.adapter::<crate::CollectionsAdapter>() else {
        return;
    };
    let caps = adapter.caps();
    let now = pod_core::util::now_secs();
    let done = pod
        .blocking(move |s| {
            s.transaction(|tx| {
                let today = crate::fill::today_on(tx, now)?;
                let mut jobs = Vec::new();
                let mut any = false;
                for (_, def) in db::all_defs(tx)? {
                    if def.snapshots.is_empty() {
                        continue;
                    }
                    any = true;
                    for r in db::all_rows(tx, &def.name, false)? {
                        jobs.extend(take(tx, &def, &r, &today, now, caps)?);
                    }
                }
                let followed = crate::follow::settle(tx, now, caps)?;
                Ok((any, jobs, followed))
            })
        })
        .await;
    if let Ok((any, jobs, followed)) = done {
        followed.finish(pod);
        if any {
            spawn(pod, jobs);
            pod.publish_data_all(crate::NAME);
        }
    }
}

/// Start the jobs [`take`] answered.
pub fn spawn(pod: &pod_core::Pod, jobs: Vec<(String, Vec<Job>)>) {
    for (c, j) in jobs {
        compute::spawn(pod, &c, j);
    }
}
