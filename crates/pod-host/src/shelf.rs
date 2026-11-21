//! The shelf (decision `sharing`): owner handles, publications and the
//! objects of their current versions, in `host.db`. Pods reach it through
//! [`pod_core::construct::share::Shelf`]; only a publication's link reads
//! its objects.

use std::sync::Arc;

use pod_core::construct::share::{
    CalendarCopy, NewVersion, PublicCopy, Publication, Shelf, ShelfError, VISIBILITY_LINK,
    is_token_id, new_link, token_id,
};
use pod_core::public_copy::{MAX_LIVE, MAX_REVOKED};
use pod_core::util::now_secs;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::db::HostStore;

/// [`Shelf`] over the host's store.
pub struct HostShelf {
    store: Arc<HostStore>,
    /// The host serves public links (decision `public-web-links`).
    public: bool,
    /// The pod is temporary: no handle, no publishing.
    temporary: bool,
}

impl HostShelf {
    pub fn new(store: Arc<HostStore>) -> Self {
        Self {
            store,
            public: false,
            temporary: false,
        }
    }

    /// For a temporary pod (anyone can make one): it may not take a handle
    /// or publish.
    pub fn temporary(mut self, temporary: bool) -> Self {
        self.temporary = temporary;
        self
    }

    /// With public links on (`CONSTRUCT_PUBLIC_WEB=1`).
    pub fn with_public_links(mut self, on: bool) -> Self {
        self.public = on;
        self
    }
}

fn public_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PublicCopy> {
    let rows: String = r.get(3)?;
    let copy: String = r.get(5)?;
    Ok(PublicCopy {
        link: r.get(0)?,
        owner_pod: r.get(1)?,
        layout: r.get(2)?,
        rows: serde_json::from_str(&rows).unwrap_or_default(),
        title: r.get(4)?,
        copy: serde_json::from_str(&copy).unwrap_or(Value::Null),
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
        revoked_at: r.get(8)?,
        slug: r.get(9)?,
    })
}

const PUBLIC_COLUMNS: &str =
    "link, owner_pod, layout, rows, title, copy, created_at, updated_at, revoked_at, slug";

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

/// A publication by its stored id (the hash of its link).
fn by_id(conn: &Connection, link: &str) -> rusqlite::Result<Option<Publication>> {
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

/// Matches a row of the table aliased `c` whose pod is still live: a
/// temporary pod's pages and feeds stop answering the moment it expires,
/// not when the sweep deletes it.
const LIVE_POD: &str = "EXISTS (SELECT 1 FROM pods WHERE id = c.owner_pod \
    AND (expires_at IS NULL OR expires_at > strftime('%s', 'now')))";

/// The stored id of a token the pod names: the token's hash, or, when the
/// pod names a row of its own by the id the list gave, that id. Also the
/// token to show (a new one) so a caller can hand it out once.
fn owned_id(
    conn: &Connection,
    table: &str,
    column: &str,
    named: &str,
    pod: &str,
) -> Result<(String, Option<String>), ShelfError> {
    if is_token_id(named) {
        let mine: bool = conn
            .query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM {table} WHERE {column} = ?1 AND owner_pod = ?2)"
                ),
                params![named, pod],
                |r| r.get(0),
            )
            .map_err(err)?;
        if mine {
            return Ok((named.to_owned(), None));
        }
    }
    Ok((token_id(named), Some(named.to_owned())))
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
    conn.execute("DELETE FROM public_copies WHERE owner_pod = ?1", [pod_id])?;
    conn.execute("DELETE FROM calendar_feeds WHERE owner_pod = ?1", [pod_id])?;
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
        if self.temporary {
            return Err(ShelfError::TemporaryPod);
        }
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
            // Publications and short paths (`/<handle>/<slug>`) carry it.
            let live: bool = conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM publications WHERE owner_pod = ?1 AND revoked_at IS NULL)
                     OR EXISTS (SELECT 1 FROM public_copies
                                WHERE owner_pod = ?1 AND slug IS NOT NULL AND revoked_at IS NULL)",
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
        if self.temporary {
            return Err(ShelfError::TemporaryPod);
        }
        let mut conn = self.store.conn();
        let tx = conn.transaction().map_err(err)?;
        let now = now_secs();
        let mut shown: Option<String> = None;
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
                // Only its hash is kept; the caller gets the link itself.
                let raw = new_link();
                let link = token_id(&raw);
                shown = Some(raw);
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
        let mut p = by_id(&tx, &link)
            .map_err(err)?
            .ok_or_else(|| ShelfError::Store("the publication went missing".into()))?;
        tx.commit().map_err(err)?;
        // A new link is shown once, here; a later version carries only its id.
        if let Some(raw) = shown {
            p.link = raw;
        }
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
        by_id(&self.store.conn(), &token_id(link)).map_err(err)
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
        let live = by_id(&conn, &token_id(link))
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
            .query_map([token_id(link)], |r| {
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
        let p = by_id(&tx, &link).map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(p)
    }

    fn may_publish(&self) -> bool {
        !self.temporary
    }

    fn public_links_enabled(&self) -> bool {
        self.public
    }

    fn put_public(&self, c: PublicCopy) -> Result<PublicCopy, ShelfError> {
        let mut conn = self.store.conn();
        let tx = conn.transaction().map_err(err)?;
        let now = now_secs();
        let rows = serde_json::to_string(&c.rows).unwrap_or_default();
        let copy = c.copy.to_string();
        // The pod names a link it already has by its id (from the list) and
        // a new one by the link itself, of which only the hash is kept.
        let (id, shown) = owned_id(&tx, "public_copies", "link", &c.link, &c.owner_pod)?;
        let c = PublicCopy { link: id, ..c };
        if let Some(slug) = &c.slug {
            let taken: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM public_copies WHERE owner_pod = ?1 AND slug = ?2
                         AND revoked_at IS NULL AND link != ?3)",
                    params![c.owner_pod, slug, c.link],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if taken {
                return Err(ShelfError::SlugTaken);
            }
        }
        // A new link only while the pod keeps fewer than MAX_LIVE, counted
        // in this transaction so two shares at once can't both pass.
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM public_copies WHERE link = ?1)",
                [&c.link],
                |r| r.get(0),
            )
            .map_err(err)?;
        if !exists {
            let live: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM public_copies WHERE owner_pod = ?1 AND revoked_at IS NULL",
                    [&c.owner_pod],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if live >= MAX_LIVE as i64 {
                return Err(ShelfError::TooManyLinks);
            }
        }
        tx.execute(
            "INSERT INTO public_copies (link, owner_pod, layout, rows, title, copy, created_at, updated_at, slug)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)
             ON CONFLICT(link) DO UPDATE SET title = ?5, copy = ?6, updated_at = ?7, rows = ?4, slug = ?8
             WHERE owner_pod = ?2 AND revoked_at IS NULL",
            params![c.link, c.owner_pod, c.layout, rows, c.title, copy, now, c.slug],
        )
        .map_err(err)?;
        let stored = tx
            .query_row(
                &format!("SELECT {PUBLIC_COLUMNS} FROM public_copies WHERE link = ?1"),
                [&c.link],
                public_row,
            )
            .map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(PublicCopy {
            link: shown.unwrap_or(stored.link.clone()),
            ..stored
        })
    }

    fn public_of_pod(&self, pod: &str) -> Result<Vec<PublicCopy>, ShelfError> {
        let conn = self.store.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {PUBLIC_COLUMNS} FROM public_copies WHERE owner_pod = ?1 ORDER BY created_at DESC"
            ))
            .map_err(err)?;
        let rows = stmt
            .query_map([pod], public_row)
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        Ok(rows)
    }

    fn public_by_link(&self, link: &str) -> Result<Option<PublicCopy>, ShelfError> {
        self.store
            .conn()
            .query_row(
                &format!(
                    "SELECT {PUBLIC_COLUMNS} FROM public_copies c WHERE link = ?1 AND revoked_at IS NULL
                         AND {LIVE_POD}"
                ),
                [token_id(link)],
                public_row,
            )
            .optional()
            .map_err(err)
    }

    fn public_by_slug(&self, handle: &str, slug: &str) -> Result<Option<PublicCopy>, ShelfError> {
        self.store
            .conn()
            .query_row(
                &format!(
                    "SELECT {PUBLIC_COLUMNS} FROM public_copies c
                     WHERE owner_pod = (SELECT pod_id FROM handles WHERE handle = ?1)
                       AND slug = ?2 AND revoked_at IS NULL AND {LIVE_POD}"
                ),
                [handle, slug],
                public_row,
            )
            .optional()
            .map_err(err)
    }

    fn revoke_public(&self, pod: &str, link: &str) -> Result<bool, ShelfError> {
        let mut conn = self.store.conn();
        let tx = conn.transaction().map_err(err)?;
        let (link, _) = owned_id(&tx, "public_copies", "link", link, pod)?;
        let link = link.as_str();
        let n = tx
            .execute(
                "UPDATE public_copies SET revoked_at = ?3, updated_at = ?3, copy = 'null',
                     title = '', rows = '[]'
                 WHERE link = ?1 AND owner_pod = ?2 AND revoked_at IS NULL",
                params![link, pod, now_secs()],
            )
            .map_err(err)?;
        // The owner's list keeps the newest MAX_REVOKED revoked links; older
        // rows go, so sharing and unsharing doesn't grow host.db.
        tx.execute(
            "DELETE FROM public_copies WHERE owner_pod = ?1 AND revoked_at IS NOT NULL
               AND link NOT IN (SELECT link FROM public_copies
                   WHERE owner_pod = ?1 AND revoked_at IS NOT NULL
                   ORDER BY revoked_at DESC, rowid DESC LIMIT ?2)",
            params![pod, MAX_REVOKED as i64],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(n > 0)
    }

    fn put_calendar(&self, c: CalendarCopy) -> Result<(), ShelfError> {
        self.store
            .conn()
            .execute(
                "INSERT INTO calendar_feeds (token, owner_pod, name, ics, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(token) DO UPDATE SET name = ?3, ics = ?4, updated_at = ?5
                 WHERE owner_pod = ?2 AND revoked_at IS NULL",
                params![token_id(&c.token), c.owner_pod, c.name, c.ics, now_secs()],
            )
            .map_err(err)?;
        Ok(())
    }

    fn calendar_by_token(&self, token: &str) -> Result<Option<CalendarCopy>, ShelfError> {
        self.store
            .conn()
            .query_row(
                &format!(
                    "SELECT token, owner_pod, name, ics, created_at, updated_at, revoked_at
                 FROM calendar_feeds c WHERE token = ?1 AND revoked_at IS NULL AND {LIVE_POD}"
                ),
                [token_id(token)],
                |r| {
                    Ok(CalendarCopy {
                        token: token.to_owned(),
                        owner_pod: r.get(1)?,
                        name: r.get(2)?,
                        ics: r.get(3)?,
                        created_at: r.get(4)?,
                        updated_at: r.get(5)?,
                        revoked_at: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(err)
    }

    fn revoke_calendar(&self, pod: &str, token: &str) -> Result<bool, ShelfError> {
        let n = self
            .store
            .conn()
            .execute(
                "UPDATE calendar_feeds SET revoked_at = ?3, updated_at = ?3, ics = ''
                 WHERE token = ?1 AND owner_pod = ?2 AND revoked_at IS NULL",
                params![token_id(token), pod, now_secs()],
            )
            .map_err(err)?;
        Ok(n > 0)
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
        assert_eq!(
            (q.link.as_str(), q.version),
            (token_id(&p.link).as_str(), 2)
        );
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
        assert_ne!(token_id(&again.link), token_id(&p.link));
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

    /// Decision `public-web-links`: a copy is kept per link, updated in
    /// place, readable by its link while live, gone once revoked or its pod is.
    #[test]
    fn public_copies_update_revoke_and_go_with_their_pod() {
        let (shelf, store) = shelf();
        assert!(!shelf.public_links_enabled());
        let shelf = shelf.with_public_links(true);
        let copy = |text: &str| PublicCopy {
            link: "pub-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            owner_pod: "a".into(),
            layout: "c-recipes".into(),
            rows: vec![],
            title: "Recipes".into(),
            copy: json!({ "title": "Recipes", "node": { "type": "core://Text", "value_expr": text } }),
            slug: None,
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        };
        let first = shelf.put_public(copy("one")).unwrap();
        assert!(first.created_at > 0);
        shelf.put_public(copy("two")).unwrap();
        let live = shelf.public_by_link(&first.link).unwrap().unwrap();
        assert_eq!(live.copy["node"]["value_expr"], "two");
        assert_eq!(shelf.public_of_pod("a").unwrap().len(), 1);
        // Another pod can't revoke it, nor write over it.
        assert!(!shelf.revoke_public("b", &first.link).unwrap());
        let mut theirs = copy("hijacked");
        theirs.owner_pod = "b".into();
        let _ = shelf.put_public(theirs);
        assert_eq!(
            shelf.public_by_link(&first.link).unwrap().unwrap().copy["node"]["value_expr"],
            "two"
        );
        assert!(shelf.revoke_public("a", &first.link).unwrap());
        assert!(shelf.public_by_link(&first.link).unwrap().is_none());
        let listed = shelf.public_of_pod("a").unwrap();
        assert!(listed[0].revoked_at.is_some() && listed[0].copy.is_null());
        // Issue #446: nor what it showed (its title, the rows it asked for).
        assert!(listed[0].title.is_empty() && listed[0].rows.is_empty());
        // A deleted pod takes its copies.
        shelf
            .put_public(PublicCopy {
                link: "pub-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                ..copy("x")
            })
            .unwrap();
        forget_pod(&store.conn(), "a").unwrap();
        assert!(shelf.public_of_pod("a").unwrap().is_empty());
    }

    /// Issue #391: the live cap holds in the shelf's own transaction, even
    /// for shares at once, and a pod keeps only its newest revoked rows.
    #[test]
    fn public_links_are_capped_live_and_revoked() {
        let (shelf, _store) = shelf();
        let shelf = Arc::new(shelf.with_public_links(true));
        let copy = |n: usize| PublicCopy {
            link: format!("pub-{n:032}"),
            owner_pod: "a".into(),
            layout: "c-recipes".into(),
            rows: vec![],
            title: "Recipes".into(),
            copy: json!({ "title": "Recipes" }),
            slug: None,
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        };
        // Many shares at once: exactly MAX_LIVE get in.
        let handles: Vec<_> = (0..MAX_LIVE + 10)
            .map(|n| {
                let shelf = shelf.clone();
                let c = copy(n);
                std::thread::spawn(move || shelf.put_public(c))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), MAX_LIVE);
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|e| *e == ShelfError::TooManyLinks)
        );
        let live: Vec<_> = shelf.public_of_pod("a").unwrap();
        assert_eq!(live.len(), MAX_LIVE);
        // A live link still takes its next copy at the cap; another pod has its own cap.
        shelf.put_public(live[0].clone()).unwrap();
        shelf
            .put_public(PublicCopy {
                owner_pod: "b".into(),
                ..copy(1000)
            })
            .unwrap();
        // Share and unshare over and over: the revoked rows stay at MAX_REVOKED.
        for c in &live {
            assert!(shelf.revoke_public("a", &c.link).unwrap());
        }
        for n in 2000..2000 + MAX_REVOKED + 5 {
            let c = shelf.put_public(copy(n)).unwrap();
            assert!(shelf.revoke_public("a", &c.link).unwrap());
        }
        let listed = shelf.public_of_pod("a").unwrap();
        assert_eq!(listed.len(), MAX_REVOKED);
        assert!(listed.iter().all(|c| c.revoked_at.is_some()));
        // The newest are the ones kept.
        let last = token_id(&format!("pub-{:032}", 2000 + MAX_REVOKED + 4));
        assert!(listed.iter().any(|c| c.link == last));
        assert_eq!(shelf.public_of_pod("b").unwrap().len(), 1);
    }

    /// Decision `ical-feeds`: a feed's text is kept per token, updated in
    /// place by its pod only, gone once revoked (a refresh can't bring a
    /// revoked token back) or its pod is.
    #[test]
    fn calendar_feeds_update_revoke_and_go_with_their_pod() {
        let (shelf, store) = shelf();
        let shelf = shelf.with_public_links(true);
        let feed = |token: &str, ics: &str| CalendarCopy {
            token: token.into(),
            owner_pod: "a".into(),
            name: "mine".into(),
            ics: ics.into(),
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        };
        let t = "cal-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        shelf.put_calendar(feed(t, "one")).unwrap();
        shelf.put_calendar(feed(t, "two")).unwrap();
        assert_eq!(shelf.calendar_by_token(t).unwrap().unwrap().ics, "two");
        let mut theirs = feed(t, "hijacked");
        theirs.owner_pod = "b".into();
        shelf.put_calendar(theirs).unwrap();
        assert_eq!(shelf.calendar_by_token(t).unwrap().unwrap().ics, "two");
        assert!(!shelf.revoke_calendar("b", t).unwrap());
        assert!(shelf.revoke_calendar("a", t).unwrap());
        assert!(shelf.calendar_by_token(t).unwrap().is_none());
        shelf.put_calendar(feed(t, "three")).unwrap();
        assert!(shelf.calendar_by_token(t).unwrap().is_none());
        let u = "cal-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        shelf.put_calendar(feed(u, "x")).unwrap();
        forget_pod(&store.conn(), "a").unwrap();
        assert!(shelf.calendar_by_token(u).unwrap().is_none());
    }

    /// Decision `public-links-share-the-closure`: a chosen short path
    /// answers the same copy by the owner's handle, one live per pod and
    /// slug, and holds the handle while it lives.
    #[test]
    fn short_paths_answer_by_handle_and_hold_it() {
        let (shelf, _store) = shelf();
        let shelf = shelf.with_public_links(true);
        shelf.set_handle("a", "ann").unwrap();
        let copy = |link: &str, slug: &str| PublicCopy {
            link: link.into(),
            owner_pod: "a".into(),
            layout: "c-meals".into(),
            rows: vec![],
            title: "Meals".into(),
            copy: json!({ "title": "Meals" }),
            slug: Some(slug.into()),
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        };
        let first = shelf
            .put_public(copy("pub-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "meals"))
            .unwrap();
        assert_eq!(
            shelf.public_by_slug("ann", "meals").unwrap().unwrap().link,
            token_id(&first.link)
        );
        assert!(shelf.public_by_slug("bob", "meals").unwrap().is_none());
        assert_eq!(
            shelf.put_public(copy("pub-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "meals")),
            Err(ShelfError::SlugTaken)
        );
        assert_eq!(shelf.set_handle("a", "anna"), Err(ShelfError::HandleInUse));
        assert!(shelf.revoke_public("a", &first.link).unwrap());
        assert!(shelf.public_by_slug("ann", "meals").unwrap().is_none());
        shelf.set_handle("a", "anna").unwrap();
    }

    fn public(link: &str, slug: Option<&str>) -> PublicCopy {
        PublicCopy {
            link: link.into(),
            owner_pod: "a".into(),
            layout: "home".into(),
            rows: vec![],
            title: "T".into(),
            copy: json!({"node": {}}),
            slug: slug.map(str::to_owned),
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        }
    }

    /// Decision `link-tokens-hashed`: the host keeps only hashes, looks
    /// links up by their hash, and the owner's list names a link by its id.
    #[test]
    fn tokens_are_kept_hashed_and_looked_up_by_hash() {
        let (shelf, store) = shelf();
        let shelf = shelf.with_public_links(true);
        let raw = "pub-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let made = shelf.put_public(public(raw, None)).unwrap();
        assert_eq!(made.link, raw, "the new link is returned once");
        let id = token_id(raw);
        assert_eq!(shelf.public_by_link(raw).unwrap().unwrap().link, id);
        // The id is no link: it opens nothing.
        assert!(shelf.public_by_link(&id).unwrap().is_none());
        let listed = shelf.public_of_pod("a").unwrap();
        assert_eq!(listed[0].link, id);
        // The list's id updates the same copy, and revokes it, for its owner only.
        let again = shelf.put_public(public(&id, None)).unwrap();
        assert_eq!(again.link, id);
        assert_eq!(shelf.public_of_pod("a").unwrap().len(), 1);
        assert!(!shelf.revoke_public("b", &id).unwrap());
        assert!(shelf.revoke_public("a", &id).unwrap());
        assert!(shelf.public_by_link(raw).unwrap().is_none());

        let t = "cal-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        shelf
            .put_calendar(CalendarCopy {
                token: t.into(),
                owner_pod: "a".into(),
                name: "mine".into(),
                ics: "x".into(),
                created_at: 0,
                updated_at: 0,
                revoked_at: None,
            })
            .unwrap();
        assert_eq!(shelf.calendar_by_token(t).unwrap().unwrap().token, t);
        assert!(shelf.calendar_by_token(&token_id(t)).unwrap().is_none());

        let p = shelf.publish(version("x")).unwrap();
        assert!(p.link.starts_with("share-"), "{}", p.link);
        assert_eq!(
            shelf.by_link(&p.link).unwrap().unwrap().link,
            token_id(&p.link)
        );
        assert_eq!(shelf.objects(&p.link).unwrap().len(), 1);
        assert!(shelf.by_link(&token_id(&p.link)).unwrap().is_none());
        let next = shelf.publish(version("y")).unwrap();
        assert_eq!(
            next.link,
            token_id(&p.link),
            "a later version has no link to show"
        );

        // Nothing in the database is a working token.
        let conn = store.conn();
        for (table, column) in [
            ("publications", "link"),
            ("publication_objects", "link"),
            ("public_copies", "link"),
            ("calendar_feeds", "token"),
        ] {
            let stored: Vec<String> = conn
                .prepare(&format!("SELECT {column} FROM {table}"))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(!stored.is_empty(), "{table}");
            assert!(stored.iter().all(|t| is_token_id(t)), "{table}: {stored:?}");
        }
    }

    /// Links made before the hash keep working once the migration hashes them.
    #[test]
    fn the_migration_keeps_existing_links_working() {
        let (shelf, store) = shelf();
        let shelf = shelf.with_public_links(true);
        let old = "pub-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let share = "share-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let feed = "cal-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        {
            let conn = store.conn();
            conn.execute(
                "INSERT INTO public_copies (link, owner_pod, layout, rows, title, copy, created_at, updated_at)
                 VALUES (?1, 'a', 'home', '[]', 'T', '{}', 1, 1)",
                [old],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calendar_feeds (token, owner_pod, name, ics, created_at, updated_at)
                 VALUES (?1, 'a', 'm', 'x', 1, 1)",
                [feed],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO publications (link, name, owner_pod, source, visibility, version, root,
                     interface, approved, created_at, updated_at)
                 VALUES (?1, '@a/x', 'a', 'x', 'link', 1, 'r', '{}', '{}', 1, 1)",
                [share],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO publication_objects (link, hash) VALUES (?1, 'h')",
                [share],
            )
            .unwrap();
        }
        {
            let mut conn = store.conn();
            let tx = conn.transaction().unwrap();
            crate::db::hash_link_tokens(&tx).unwrap();
            tx.commit().unwrap();
        }
        assert!(shelf.public_by_link(old).unwrap().is_some());
        assert!(shelf.calendar_by_token(feed).unwrap().is_some());
        assert!(shelf.by_link(share).unwrap().is_some());
        let n: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM publication_objects WHERE link = ?1",
                [token_id(share)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    /// An expired temporary pod's pages and feeds stop answering before the
    /// sweep deletes it.
    #[test]
    fn an_expired_pod_s_links_answer_nothing() {
        let (shelf, store) = shelf();
        let shelf = shelf.with_public_links(true);
        let raw = "pub-cccccccccccccccccccccccccccccccc";
        shelf.put_public(public(raw, Some("pg"))).unwrap();
        shelf.set_handle("a", "ann").unwrap();
        let t = "cal-cccccccccccccccccccccccccccccccc";
        shelf
            .put_calendar(CalendarCopy {
                token: t.into(),
                owner_pod: "a".into(),
                name: "m".into(),
                ics: "x".into(),
                created_at: 0,
                updated_at: 0,
                revoked_at: None,
            })
            .unwrap();
        assert!(shelf.public_by_link(raw).unwrap().is_some());
        assert!(shelf.public_by_slug("ann", "pg").unwrap().is_some());
        assert!(shelf.calendar_by_token(t).unwrap().is_some());
        store
            .conn()
            .execute(
                "UPDATE pods SET expires_at = ?1 WHERE id = 'a'",
                [now_secs() - 1],
            )
            .unwrap();
        assert!(shelf.public_by_link(raw).unwrap().is_none());
        assert!(shelf.public_by_slug("ann", "pg").unwrap().is_none());
        assert!(shelf.calendar_by_token(t).unwrap().is_none());
    }
}
