//! Live computes (decision `live-computed-fields`): a `lookup` or `count`
//! field follows the collection it reads. Every row write notes the row
//! and its state before ([`db::take_changed`]); before a transaction
//! commits, [`settle`] finds the computed fields of any collection that
//! read a changed row's collection and computes them again for the rows
//! whose answer could have changed:
//!
//! - a `lookup` whose `collection` is the changed row's: when the row was
//!   created, deleted or restored, or its `key` or `value` changed, the
//!   rows whose `from` matches its key before or after (by the lookup's
//!   match mode);
//! - a `count` whose `collection` is the changed row's: when the row was
//!   created, deleted or restored, or its `key` or a `where` field changed,
//!   the rows whose `from` equals its key before or after.
//!
//! A restored row's own lookups and counts are computed again too (they
//! didn't follow while it was deleted). The rows this changes are saved
//! like any other write (clients are told) and noted in turn, so a change
//! cascades in waves (a lookup into a count); after [`MAX_WAVES`] waves the
//! rest is dropped, so a cycle of computes ends. Only the collections that
//! read a changed one are read.

use std::collections::{BTreeMap, HashMap, HashSet};

use pod_core::Pod;
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::compute::{self, Job, Lookups};
use crate::db::{self, Row};
use crate::lookup;
use crate::model::{Compute, Definition, Match};
use crate::{Caps, NAME};

/// Waves of recomputing one transaction runs at most: a chain of computes
/// across collections is at most this long, and a cycle stops here.
pub const MAX_WAVES: usize = 8;

/// What [`settle`] did: the rows it changed and the model jobs it made.
#[derive(Debug, Default)]
#[must_use = "finish it after the commit"]
pub struct Followed {
    /// Rows saved with a new computed value.
    pub changed: usize,
    /// Model jobs, by collection.
    pub jobs: BTreeMap<String, Vec<Job>>,
}

impl Followed {
    /// After the commit: tell clients and ask the model.
    pub fn finish(self, pod: &Pod) {
        if self.changed > 0 {
            pod.publish_data_all(NAME);
        }
        for (collection, jobs) in self.jobs {
            compute::spawn(pod, &collection, jobs);
        }
    }
}

/// A computed field's kind that reads another collection.
#[derive(Debug, Clone)]
struct Dep {
    /// The collection of the computed field.
    collection: String,
    field: String,
    kind: usize,
}

/// A field's value in `values` (empty: its default).
fn value_of(def: &Definition, id: &str, values: &Map<String, Value>) -> Value {
    values
        .get(id)
        .cloned()
        .or_else(|| def.by_id(id).map(|f| f.empty_value()))
        .unwrap_or(Value::Null)
}

/// The fields of `def` that read another collection (a lookup or a count).
fn following(def: &Definition) -> Vec<String> {
    def.live_fields()
        .filter(|f| f.compute.iter().any(|c| c.reads().is_some()))
        .map(|f| f.id.clone())
        .collect()
}

/// Compute again the lookups and counts the rows changed in this
/// transaction may have changed, in waves, inside the transaction. Call it
/// before the commit, and [`Followed::finish`] after.
pub fn settle(tx: &Connection, now: i64, caps: Caps) -> rusqlite::Result<Followed> {
    let mut out = Followed::default();
    let mut defs: Option<HashMap<String, Definition>> = None;
    let mut deps: HashMap<String, Vec<Dep>> = HashMap::new();
    for wave in 0.. {
        let changed = db::take_changed(tx)?;
        if changed.is_empty() {
            break;
        }
        if wave >= MAX_WAVES {
            tracing::warn!(
                rows = changed.len(),
                "computed fields still changing after {MAX_WAVES} waves (a cycle?); stopped"
            );
            break;
        }
        let defs = match &mut defs {
            Some(d) => d,
            None => {
                let d = db::defs_by_name(tx)?;
                for def in d.values() {
                    for f in def.live_fields() {
                        for (i, c) in f.compute.iter().enumerate() {
                            if let Some(t) = c.reads() {
                                deps.entry(t.to_owned()).or_default().push(Dep {
                                    collection: def.name.clone(),
                                    field: f.id.clone(),
                                    kind: i,
                                });
                            }
                        }
                    }
                }
                defs.insert(d)
            }
        };
        if deps.is_empty() {
            // Nothing reads another collection: nothing to follow.
            break;
        }
        wave_step(tx, defs, &deps, changed, now, caps, &mut out)?;
    }
    Ok(out)
}

/// One wave: the rows `changed` touched, recomputed where they read them.
fn wave_step(
    tx: &Connection,
    defs: &HashMap<String, Definition>,
    deps: &HashMap<String, Vec<Dep>>,
    changed: Vec<db::Changed>,
    now: i64,
    caps: Caps,
    out: &mut Followed,
) -> rusqlite::Result<()> {
    // (dependent collection, field id, kind) → the keys to match.
    let mut keys: BTreeMap<(String, String, usize), Vec<String>> = BTreeMap::new();
    // Collection → restored rows (their own lookups and counts).
    let mut restored: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for ch in changed {
        let Some(tdef) = defs.get(&ch.collection) else {
            continue;
        };
        let after = db::row(tx, &ch.collection, &ch.id)?
            .filter(|r| r.deleted_at.is_none())
            .map(|r| r.values);
        if ch.before == after {
            continue;
        }
        if ch.existed && ch.before.is_none() && after.is_some() {
            restored
                .entry(ch.collection.clone())
                .or_default()
                .insert(ch.id.clone());
        }
        let Some(ds) = deps.get(&ch.collection) else {
            continue;
        };
        let sides: Vec<&Map<String, Value>> = ch.before.iter().chain(after.iter()).collect();
        let live_changed = ch.before.is_none() != after.is_none();
        let differs = |id: &str| match (&ch.before, &after) {
            (Some(b), Some(a)) => value_of(tdef, id, b) != value_of(tdef, id, a),
            _ => true,
        };
        for dep in ds {
            let Some(c) = defs
                .get(&dep.collection)
                .and_then(|d| d.by_id(&dep.field))
                .and_then(|f| f.compute.get(dep.kind))
            else {
                continue;
            };
            let (key, relevant) = match c {
                Compute::Lookup { key, value, .. } => {
                    (key, live_changed || differs(key) || differs(value))
                }
                Compute::Count {
                    key, conditions, ..
                } => (
                    key,
                    live_changed || differs(key) || conditions.iter().any(|w| differs(&w.field)),
                ),
                Compute::Expr { .. } | Compute::Model { .. } => continue,
            };
            if !relevant {
                continue;
            }
            let entry = keys
                .entry((dep.collection.clone(), dep.field.clone(), dep.kind))
                .or_default();
            for side in &sides {
                if let Some(k) = lookup::text_of(&value_of(tdef, key, side))
                    && !entry.contains(&k)
                {
                    entry.push(k);
                }
            }
        }
    }
    let mut collections: Vec<&String> = keys.keys().map(|(c, _, _)| c).collect();
    collections.extend(restored.keys());
    collections.sort();
    collections.dedup();
    for name in collections {
        let Some(def) = defs.get(name) else {
            continue;
        };
        // This collection's kinds to match, with their keys.
        let matching: Vec<(&String, usize, &Vec<String>)> = keys
            .iter()
            .filter(|((c, _, _), _)| c == name)
            .map(|((_, f, i), k)| (f, *i, k))
            .collect();
        let back = restored.get(name);
        let own = following(def);
        let mut lookups = Lookups::default();
        for mut row in db::all_rows(tx, name, false)? {
            let mut fields: Vec<&str> = Vec::new();
            if back.is_some_and(|b| b.contains(&row.id)) {
                fields.extend(own.iter().map(String::as_str));
            }
            for (field_id, i, ks) in &matching {
                if fields.contains(&field_id.as_str()) {
                    continue;
                }
                let Some(c) = def.by_id(field_id).and_then(|f| f.compute.get(*i)) else {
                    continue;
                };
                let (from, mode) = match c {
                    Compute::Lookup { from, mode, .. } => (from, *mode),
                    Compute::Count { from, .. } => (from, Match::Exact),
                    Compute::Expr { .. } | Compute::Model { .. } => continue,
                };
                let Some(input) = lookup::text_of(&value_of(def, from, &row.values)) else {
                    continue;
                };
                if ks.iter().any(|k| lookup::key_matches(mode, k, &input)) {
                    fields.push(field_id.as_str());
                }
            }
            if fields.is_empty() {
                continue;
            }
            if recompute_row(tx, def, &mut row, &fields, &mut lookups, caps, out)? {
                row.updated_at = now;
                db::save_row(tx, name, &row)?;
                out.changed += 1;
            }
        }
    }
    Ok(())
}

/// Compute `fields` of `row` again; whether a value changed.
fn recompute_row(
    tx: &Connection,
    def: &Definition,
    row: &mut Row,
    fields: &[&str],
    lookups: &mut Lookups,
    caps: Caps,
    out: &mut Followed,
) -> rusqlite::Result<bool> {
    let mut dirty = false;
    for id in fields {
        let Some(field) = def.by_id(id).filter(|f| f.live() && f.computed()) else {
            continue;
        };
        let old = row.values.get(&field.id).cloned();
        if let Some(job) = compute::recompute(tx, def, field, row, lookups, caps.text_chars)? {
            out.jobs.entry(def.name.clone()).or_default().push(job);
        }
        dirty |= row.values.get(&field.id) != old.as_ref();
    }
    Ok(dirty)
}
