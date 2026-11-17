//! The shelf (decision `sharing`): owner handles, publications and the
//! objects of their current versions, in `host.db`. Pods reach it through
//! [`pod_core::construct::share::Shelf`]; only a publication's link reads
//! its objects.

use std::sync::Arc;

use pod_core::construct::share::{
    NewVersion, Publication, Shelf, ShelfError, VISIBILITY_LINK, new_link,
};
use pod_core::util::now_secs;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::db::HostStore;

/// [`Shelf`] over the host's store.
pub struct HostShelf {
    store: Arc<HostStore>,
}

impl HostShelf {
    pub fn new(store: Arc<HostStore>) -> Self {
        Self { store }
    }
}

fn err(e: rusqlite::Error) -> ShelfError {
    ShelfError::Store(e.to_string())
}

const COLUMNS: &str = "link, name, owner_pod, source, visibility, version, root, interface, \
    approved, keep_shared, pending, created_at, updated_at, revoked_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Publication> {
    let json = |s: String| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null);
    Ok(Publication {
        link: r.get(0)?,
        name: r.get(1)?,
        owner_pod: r.get(2)?,
        source: r.get(3)?,
        visibility: r.get(4)?,
        version: r.get(5)?,
        root: r.get(6)?,
        interface: json(r.get(7)?),
        approved: json(r.get(8)?),
        keep_shared: r.get::<_, i64>(9)? != 0,
        pending: r.get::<_, Option<String>>(10)?.map(json),
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        revoked_at: r.get(13)?,
    })
}

fn by_link(conn: &Connection, link: &str) -> rusqlite::Result<Option<Publication>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM publications p
             WHERE link = ?1 AND EXISTS (SELECT 1 FROM pods WHERE id = p.owner_pod
                 AND (expires_at IS NULL OR expires_at > ?2))"
        ),
        params![link, now_secs()],
        row,
    )
    .optional()
}

/// Delete the shelf's objects no publication holds.
fn collect_garbage(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM shelf_objects WHERE hash NOT IN (SELECT hash FROM publication_objects)",
        [],
    )
}

/// Forget a pod's handle and publications (its pod is being deleted).
pub(crate) fn forget_pod(conn: &Connection, pod_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM publication_objects WHERE link IN
             (SELECT link FROM publications WHERE owner_pod = ?1)",
        [pod_id],
    )?;
    conn.execute("DELETE FROM publications WHERE owner_pod = ?1", [pod_id])?;
    conn.execute("DELETE FROM handles WHERE pod_id = ?1", [pod_id])?;
    collect_garbage(conn)?;
    Ok(())
}

impl Shelf for HostShelf {
    fn handle(&self, pod: &str) -> Result<Option<String>, ShelfError> {
        self.store
            .conn()
            .query_row("SELECT handle FROM handles WHERE pod_id = ?1", [pod], |r| {
                r.get(0)
            })
            .optional()
            .map_err(err)
    }

    fn set_handle(&self, pod: &str, handle: &str) -> Result<(), ShelfError> {
        let conn = self.store.conn();
        let current: Option<String> = conn
            .query_row("SELECT handle FROM handles WHERE pod_id = ?1", [pod], |r| {
                r.get(0)
            })
            .optional()
            .map_err(err)?;
        if current.as_deref() == Some(handle) {
            return Ok(());
        }
        let taken: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM handles WHERE handle = ?1)",
                [handle],
                |r| r.get(0),
            )
            .map_err(err)?;
        if taken {
            return Err(ShelfError::HandleTaken);
        }
        if current.is_some() {
            let live: bool = conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM publications WHERE owner_pod = ?1 AND revoked_at IS NULL)",
                    [pod],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if live {
                return Err(ShelfError::HandleInUse);
            }
        }
        conn.execute(
            "INSERT INTO handles (pod_id, handle, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(pod_id) DO UPDATE SET handle = ?2",
            params![pod, handle, now_secs()],
        )
        .map_err(err)?;
        Ok(())
    }

    fn publish(&self, v: NewVersion) -> Result<Publication, ShelfError> {
        let mut conn = self.store.conn();
        let tx = conn.transaction().map_err(err)?;
        let now = now_secs();
        let live: Option<(String, i64, String)> = tx
            .query_row(
                "SELECT link, version, owner_pod FROM publications WHERE name = ?1 AND revoked_at IS NULL",
                [&v.name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(err)?;
        let visibility = if v.visibility.is_empty() {
            VISIBILITY_LINK.to_owned()
        } else {
            v.visibility.clone()
        };
        let link = match live {
            Some((_, _, owner)) if owner != v.owner_pod => {
                return Err(ShelfError::Store(format!(
                    "{} belongs to another pod",
                    v.name
                )));
            }
            Some((link, version, _)) => {
                tx.execute(
                    "UPDATE publications SET source = ?2, visibility = ?3, version = ?4, root = ?5,
                         interface = ?6, approved = ?7, keep_shared = ?8, pending = NULL,
                         updated_at = ?9
                     WHERE link = ?1",
                    params![
                        link,
                        v.source,
                        visibility,
                        version + 1,
                        v.root,
                        v.interface.to_string(),
                        v.approved.to_string(),
                        i64::from(v.keep_shared),
                        now
                    ],
                )
                .map_err(err)?;
                tx.execute("DELETE FROM publication_objects WHERE link = ?1", [&link])
                    .map_err(err)?;
                link
            }
            None => {
                let link = new_link();
                tx.execute(
                    "INSERT INTO publications (link, name, owner_pod, source, visibility, version,
                         root, interface, approved, keep_shared, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?10)",
                    params![
                        link,
                        v.name,
                        v.owner_pod,
                        v.source,
                        visibility,
                        v.root,
                        v.interface.to_string(),
                        v.approved.to_string(),
                        i64::from(v.keep_shared),
                        now
                    ],
                )
                .map_err(err)?;
                link
            }
        };
        for (id, obj, features) in &v.objects {
            let bytes = construct_elements::canonical(obj);
            tx.execute(
                "INSERT OR IGNORE INTO shelf_objects (hash, bytes, features, size) VALUES (?1, ?2, ?3, ?4)",
                params![id, bytes.as_bytes(), features, bytes.len() as i64],
            )
            .map_err(err)?;
            tx.execute(
                "INSERT OR IGNORE INTO publication_objects (link, hash) VALUES (?1, ?2)",
                params![link, id],
            )
            .map_err(err)?;
        }
        collect_garbage(&tx).map_err(err)?;
        let p = by_link(&tx, &link)
            .map_err(err)?
            .ok_or_else(|| ShelfError::Store("the publication went missing".into()))?;
        tx.commit().map_err(err)?;
        Ok(p)
    }

    fn set_pending(&self, link: &str, pending: Option<&Value>) -> Result<(), ShelfError> {
        self.store
            .conn()
            .execute(
                "UPDATE publications SET pending = ?2 WHERE link = ?1",
                params![link, pending.map(Value::to_string)],
            )
            .map_err(err)?;
        Ok(())
    }

    fn by_link(&self, link: &str) -> Result<Option<Publication>, ShelfError> {
        by_link(&self.store.conn(), link).map_err(err)
    }

    fn of_pod(&self, pod: &str) -> Result<Vec<Publication>, ShelfError> {
        let conn = self.store.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM publications WHERE owner_pod = ?1 ORDER BY updated_at DESC, name"
            ))
            .map_err(err)?;
        stmt.query_map([pod], row)
            .map_err(err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(err)
    }

    fn objects(&self, link: &str) -> Result<Vec<(String, Value, String)>, ShelfError> {
        let conn = self.store.conn();
        let live = by_link(&conn, link)
            .map_err(err)?
            .is_some_and(|p| p.revoked_at.is_none());
        if !live {
            return Ok(Vec::new());
        }
        let mut stmt = conn
            .prepare(
                "SELECT o.hash, o.bytes, o.features FROM publication_objects p
                 JOIN shelf_objects o ON o.hash = p.hash WHERE p.link = ?1 ORDER BY o.hash",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([link], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        Ok(rows
            .into_iter()
            .filter_map(|(h, b, f)| serde_json::from_slice(&b).ok().map(|v| (h, v, f)))
            .collect())
    }

    fn revoke(&self, pod: &str, name: &str) -> Result<Option<Publication>, ShelfError> {
        let mut conn = self.store.conn();
        let tx = conn.transaction().map_err(err)?;
        let link: Option<String> = tx
            .query_row(
                "SELECT link FROM publications WHERE owner_pod = ?1 AND name = ?2 AND revoked_at IS NULL",
                params![pod, name],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        let Some(link) = link else {
            return Ok(None);
        };
        let now = now_secs();
        tx.execute(
            "UPDATE publications SET revoked_at = ?2, updated_at = ?2, pending = NULL, keep_shared = 0
             WHERE link = ?1",
            params![link, now],
        )
        .map_err(err)?;
        tx.execute("DELETE FROM publication_objects WHERE link = ?1", [&link])
            .map_err(err)?;
        collect_garbage(&tx).map_err(err)?;
        let p = by_link(&tx, &link).map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{PodKind, PodRow};
    use serde_json::json;

    fn shelf() -> (HostShelf, Arc<HostStore>) {
        let store = Arc::new(HostStore::open_in_memory().unwrap());
        for id in ["a", "b"] {
            store
                .insert_pod(
                    &PodRow {
                        id: id.into(),
                        name: format!("pod-{id}"),
                        kind: PodKind::Admin,
                        created_at: now_secs(),
                        expires_at: None,
                    },
                    &format!("hash-{id}"),
                )
                .unwrap();
        }
        (HostShelf::new(store.clone()), store)
    }

    fn version(root: &str) -> NewVersion {
        let obj = json!({"kind": "element", "node": {"type": "core://Text", "value_expr": root}});
        NewVersion {
            owner_pod: "a".into(),
            name: "@ann/list".into(),
            source: "c-1".into(),
            visibility: "link".into(),
            root: construct_elements::object_id(&obj),
            objects: vec![(construct_elements::object_id(&obj), obj, String::new())],
            interface: json!({}),
            approved: json!({}),
            keep_shared: false,
        }
    }

    #[test]
    fn handles_are_unique_and_stay_while_shared() {
        let (s, _) = shelf();
        s.set_handle("a", "ann").unwrap();
        assert_eq!(s.set_handle("b", "ann"), Err(ShelfError::HandleTaken));
        s.publish(version("x")).unwrap();
        assert_eq!(s.set_handle("a", "anna"), Err(ShelfError::HandleInUse));
        assert_eq!(s.handle("a").unwrap().as_deref(), Some("ann"));
    }

    #[test]
    fn versions_revoke_and_objects() {
        let (s, store) = shelf();
        let p = s.publish(version("one")).unwrap();
        assert_eq!(p.version, 1);
        let q = s.publish(version("two")).unwrap();
        assert_eq!((q.link.as_str(), q.version), (p.link.as_str(), 2));
        let objs = s.objects(&p.link).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].0, q.root);
        // The first version's object left the shelf.
        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM shelf_objects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert!(s.revoke("b", "@ann/list").unwrap().is_none());
        let r = s.revoke("a", "@ann/list").unwrap().unwrap();
        assert!(r.revoked_at.is_some());
        assert!(s.objects(&p.link).unwrap().is_empty());
        // Published again: a new link.
        let again = s.publish(version("three")).unwrap();
        assert_ne!(again.link, p.link);
        assert_eq!(again.version, 1);
        assert_eq!(s.of_pod("a").unwrap().len(), 2);
    }

    #[test]
    fn a_deleted_pod_takes_its_publications() {
        let (s, store) = shelf();
        s.set_handle("a", "ann").unwrap();
        let p = s.publish(version("one")).unwrap();
        store.delete_pod("a").unwrap();
        assert!(s.by_link(&p.link).unwrap().is_none());
        assert!(s.handle("a").unwrap().is_none());
    }
}
