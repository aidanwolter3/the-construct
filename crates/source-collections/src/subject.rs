//! Subject computes (decision `notifications-in-a-collection`): a field
//! whose value is what the row an entity URI names serves
//! ([`Compute::Subject`]), e.g. a notification's `subject_alert_matched`,
//! whether the event it is about still matches an alert. Live: when a write
//! changes that row, or its collection's definition (an alert edited), the
//! rows that read it are computed again in the same transaction
//! ([`crate::follow::settle`]); [`refresh`] catches answers that change
//! with time alone (an alert's window of days).

use std::collections::HashSet;

use rusqlite::Connection;

use crate::compute::{self, Lookups};
use crate::db;
use crate::model::Compute;

/// Compute again the subject fields of every row that reads a row in
/// `rows` (entity URIs) or any row of a collection in `defs` (`all`: every
/// row); save the rows whose value changed.
pub fn follow(
    tx: &Connection,
    rows: &HashSet<String>,
    defs: &HashSet<String>,
    all: bool,
    now: i64,
    max_chars: usize,
) -> rusqlite::Result<usize> {
    if rows.is_empty() && defs.is_empty() && !all {
        return Ok(0);
    }
    let mut changed = 0;
    for (_, def) in db::all_defs(tx)? {
        let fields: Vec<(&crate::model::Field, &String)> = def
            .live_fields()
            .filter_map(|f| {
                f.compute.iter().find_map(|c| match c {
                    Compute::Subject { from, .. } => Some((f, from)),
                    _ => None,
                })
            })
            .collect();
        if fields.is_empty() {
            continue;
        }
        let mut lookups = Lookups::default();
        for mut row in db::all_rows(tx, &def.name, false)? {
            let before = row.values.clone();
            for (field, from) in &fields {
                let uri = row.values.get(*from).and_then(|v| v.as_str()).unwrap_or("");
                let reads = all
                    || rows.contains(uri)
                    || uri
                        .strip_prefix("collections://")
                        .and_then(|r| r.split_once('/'))
                        .is_some_and(|(c, _)| defs.contains(c));
                if reads {
                    let _ = compute::recompute(tx, &def, field, &mut row, &mut lookups, max_chars)?;
                }
            }
            if row.values != before {
                row.updated_at = now;
                db::save_row(tx, &def.name, &row)?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// Whether any collection has a subject compute (most pods: the
/// notifications only).
pub fn any(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(db::all_defs(conn)?.iter().any(|(_, d)| {
        d.live_fields().any(|f| {
            f.compute
                .iter()
                .any(|c| matches!(c, Compute::Subject { .. }))
        })
    }))
}

/// Compute every subject field again and tell clients of what changed
/// (the pod's hourly pass).
pub async fn refresh(pod: &pod_core::Pod) {
    let caps = crate::adapter(pod).caps;
    let now = pod_core::util::now_secs();
    let done = pod
        .blocking(move |s| {
            s.transaction(|tx| {
                if !any(tx)? {
                    return Ok(None);
                }
                follow(
                    tx,
                    &HashSet::new(),
                    &HashSet::new(),
                    true,
                    now,
                    caps.text_chars,
                )?;
                Ok(Some(crate::follow::settle(tx, now, caps)?))
            })
        })
        .await;
    if let Ok(Some(followed)) = done
        && !followed.rows.is_empty()
    {
        followed.publish(pod, None);
    }
}
