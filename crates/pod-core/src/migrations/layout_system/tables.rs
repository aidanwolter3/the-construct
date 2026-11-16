//! The old layout system's tables, read once into memory with plain SQL:
//! `custom_apps`, `app_versions` (with `layout_blobs`), `components` and
//! the `apps.hidden` setting. Only the conversion of a pod that wasn't
//! switched reads them.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::migrations::core::get_blob;

/// The setting listing the layouts hidden from `local:layouts`.
pub const HIDDEN_SETTING: &str = "apps.hidden";

/// One of the pod's layouts (a "custom app").
#[derive(Debug, Clone)]
pub struct OldLayout {
    pub id: String,
    pub title: String,
    pub icon: String,
}

/// One recorded version of a layout or blueprint.
#[derive(Debug, Clone)]
pub struct OldVersion {
    pub note: Option<String>,
    pub json: String,
    pub meta_json: Option<String>,
}

/// Everything the conversion reads.
#[derive(Debug, Clone, Default)]
pub struct Old {
    /// Live layouts: home first, then oldest first.
    pub layouts: Vec<OldLayout>,
    pub hidden: Vec<String>,
    /// Home's feeds (`custom_apps.feeds_json` of `home`).
    pub home_feeds: Vec<(String, String)>,
    /// Each component's versions (`core:x`, `pod:x`).
    pub components: BTreeMap<String, BTreeMap<i64, Value>>,
    /// Each layout's and blueprint's versions.
    pub versions: BTreeMap<String, BTreeMap<i64, OldVersion>>,
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
        .exists([table])
}

/// Whether the pod's old tables hold a live layout (a pod made by a build
/// from before the construct).
pub fn has_layouts(conn: &Connection) -> rusqlite::Result<bool> {
    if !table_exists(conn, "custom_apps")? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM custom_apps WHERE deleted_at IS NULL)",
        [],
        |r| r.get(0),
    )
}

impl Old {
    /// Read the old tables (empty where a table is gone).
    pub fn read(conn: &Connection) -> rusqlite::Result<Old> {
        let mut old = Old::default();
        if table_exists(conn, "custom_apps")? {
            let mut stmt = conn.prepare(
                "SELECT id, title, icon, feeds_json FROM custom_apps WHERE deleted_at IS NULL
                 ORDER BY id = 'home' DESC, created_at, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (id, title, icon, feeds) = row?;
                if id == crate::HOME_APP {
                    old.home_feeds = serde_json::from_str(&feeds).unwrap_or_default();
                }
                old.layouts.push(OldLayout { id, title, icon });
            }
        }
        old.hidden = conn
            .query_row(
                "SELECT value FROM pod_settings WHERE key = ?1",
                [HIDDEN_SETTING],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        if table_exists(conn, "components")? {
            let mut stmt = conn.prepare("SELECT name, version, json FROM components")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (name, version, json) = row?;
                if let Ok(def) = serde_json::from_str::<Value>(&json) {
                    old.components.entry(name).or_default().insert(version, def);
                }
            }
        }
        if table_exists(conn, "app_versions")? {
            type Row = (String, i64, String, Option<String>, Option<String>);
            let rows: Vec<Row> = {
                let mut stmt = conn
                    .prepare("SELECT app_id, version, hash, note, meta_json FROM app_versions")?;
                stmt.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<_>>()?
            };
            for (app, version, hash, note, meta_json) in rows {
                let json = get_blob(conn, &hash)?;
                old.versions.entry(app).or_default().insert(
                    version,
                    OldVersion {
                        note,
                        json,
                        meta_json,
                    },
                );
            }
        }
        Ok(old)
    }

    /// Component `name` at `version`, else its newest: `(version, def)`.
    pub fn component(&self, name: &str, version: Option<i64>) -> Option<(i64, Value)> {
        let all = self.components.get(name)?;
        match version {
            Some(n) => all.get(&n).map(|d| (n, d.clone())),
            None => all.iter().next_back().map(|(n, d)| (*n, d.clone())),
        }
    }

    /// Layout or blueprint `id` at `version`, else its newest.
    pub fn version(&self, id: &str, version: Option<i64>) -> Option<(i64, &OldVersion)> {
        let all = self.versions.get(id)?;
        match version {
            Some(n) => all.get(&n).map(|v| (n, v)),
            None => all.iter().next_back().map(|(n, v)| (*n, v)),
        }
    }
}
