//! Where a pod keeps its notifications and its channel switches (decision
//! `notifications-in-a-collection`).
//!
//! The pipeline ([`crate::notify`]) checks what sources report, skips what
//! is switched off, announces and pushes; a [`NotificationStore`] keeps the
//! rows. A source may offer one ([`crate::Source::notification_store`]):
//! Collections keeps them as the rows of a collection the user can bind,
//! filter and edit like any other. Without one, [`CoreStore`] keeps them in
//! the core's own tables. The core never reads the rows any other way.

use std::collections::{BTreeMap, HashMap, HashSet};

use protocol::{Notification, NotificationChannel};
use rusqlite::{Connection, params};

use crate::Pod;
use crate::notify::{KEEP_COUNT, KEEP_SECS, NewNotification};
use crate::util::now_secs;

/// `(layout, channel)`: a channel of a layout.
pub type ChannelId = (String, String);

/// Which notifications a mark-read touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadSel {
    Ids(Vec<String>),
    /// Every unread one, or every unread one of one layout.
    All(Option<String>),
}

/// A page of notifications, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    /// Only those older than this notification (its id).
    pub before: Option<String>,
    /// Only those of this layout's channels.
    pub layout: Option<String>,
    pub limit: usize,
    /// The layouts notifications may belong to (the others are left out;
    /// empty: any).
    pub layouts: Vec<String>,
    /// Channels switched off: their notifications are left out.
    pub hidden: HashSet<ChannelId>,
}

/// A notification to store: what a source reported, checked, and the
/// layout to present it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToStore {
    pub item: NewNotification,
    /// The layout it opens ([`Notification::layout`]).
    pub open: String,
}

/// Keeps a pod's notifications and channel switches. Every call blocks
/// (it runs off the async threads); a store that tells clients of its own
/// writes does so itself.
pub trait NotificationStore: Send + Sync {
    /// The channels sources declare now (each call of the pipeline's), so
    /// a store that lists them can keep its list current.
    fn channels_declared(
        &self,
        pod: &Pod,
        channels: &[NotificationChannel],
    ) -> rusqlite::Result<()> {
        let _ = (pod, channels);
        Ok(())
    }

    /// Store `items` (oldest first), skipping each whose `(layout,
    /// dedupe_key)` is already stored; answers those stored, each with
    /// whether it was quiet (stored as read, announced to no one).
    fn insert(&self, pod: &Pod, items: &[ToStore]) -> rusqlite::Result<Vec<(Notification, bool)>>;

    /// A page of [`ListQuery`]: newest first, `limit + 1` at most (so the
    /// caller sees whether there are more).
    fn list(&self, pod: &Pod, q: &ListQuery) -> rusqlite::Result<Vec<Notification>>;

    /// Unread counts of `layouts` (switched-off channels left out): the
    /// total and per layout (layouts with none left out).
    fn unread(
        &self,
        pod: &Pod,
        layouts: &[String],
        hidden: &HashSet<ChannelId>,
    ) -> rusqlite::Result<(u32, BTreeMap<String, u32>)>;

    fn mark_read(&self, pod: &Pod, sel: &ReadSel) -> rusqlite::Result<()>;

    /// The switches set so far (a channel not in it is at its default).
    fn switches(&self, pod: &Pod) -> rusqlite::Result<HashMap<ChannelId, bool>>;

    fn set_switches(&self, pod: &Pod, choices: &[(String, String, bool)]) -> rusqlite::Result<()>;

    /// Remove the notifications about `subjects` (entity URIs that are
    /// gone); answers how many went.
    fn remove_about(&self, pod: &Pod, subjects: &[String]) -> rusqlite::Result<usize>;

    /// Give each stored notification whose `layout` is empty, or names no
    /// layout of the pod (a core layout, a layout since deleted), the layout
    /// `opens` finds for it, where that differs (issue #337: rows stored
    /// before the pod resolved them). Answers how many changed. A store
    /// that resolves when it lists (the core's) needs nothing here.
    fn fill_layouts(
        &self,
        pod: &Pod,
        opens: &crate::layouts::NotificationOpens,
    ) -> rusqlite::Result<usize> {
        let _ = (pod, opens);
        Ok(0)
    }
}

/// The core's own tables (`notifications`, `notification_switches`).
#[derive(Debug, Default)]
pub struct CoreStore;

const COLS: &str = "id, layout, channel, title, body, image_url, target_uri, group_key, created_at, read_at, subject";

fn row_to_notification(r: &rusqlite::Row<'_>) -> rusqlite::Result<Notification> {
    Ok(Notification {
        id: r.get::<_, i64>(0)?.to_string(),
        channel_layout: r.get(1)?,
        channel: r.get(2)?,
        title: r.get(3)?,
        body: r.get(4)?,
        image_url: r.get(5)?,
        target_uri: r.get(6)?,
        layout: None,
        group_key: r.get(7)?,
        created_at: r.get(8)?,
        read: r.get::<_, Option<i64>>(9)?.is_some(),
        subject: r.get(10)?,
    })
}

/// Unread counts per layout, of `layouts`, leaving out `hidden` channels.
fn unread_on(
    conn: &Connection,
    layouts: &[String],
    hidden: &HashSet<ChannelId>,
) -> rusqlite::Result<(u32, BTreeMap<String, u32>)> {
    let mut stmt = conn.prepare_cached(
        "SELECT layout, channel, COUNT(*) FROM notifications WHERE read_at IS NULL
         GROUP BY layout, channel",
    )?;
    let mut by = BTreeMap::new();
    let mut total = 0u32;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (layout, channel, n) = row?;
        if layouts.contains(&layout) && !hidden.contains(&(layout.clone(), channel)) {
            let n = u32::try_from(n).unwrap_or(u32::MAX);
            total = total.saturating_add(n);
            *by.entry(layout).or_insert(0u32) += n;
        }
    }
    Ok((total, by))
}

impl NotificationStore for CoreStore {
    fn insert(&self, pod: &Pod, items: &[ToStore]) -> rusqlite::Result<Vec<(Notification, bool)>> {
        pod.store().transaction(|conn| {
            let now = now_secs();
            let mut out = Vec::new();
            for t in items {
                let n = &t.item;
                let changed = conn.execute(
                    "INSERT OR IGNORE INTO notifications (layout, channel, dedupe_key, title, body,
                         image_url, target_uri, group_key, created_at, inserted_at, read_at, subject)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        n.layout,
                        n.channel,
                        n.dedupe_key,
                        n.title,
                        n.body,
                        n.image_url,
                        n.target_uri,
                        n.group_key,
                        n.created_at,
                        now,
                        n.quiet.then_some(now),
                        n.subject,
                    ],
                )?;
                if changed == 1 {
                    let id = conn.last_insert_rowid();
                    let mut row = conn.query_row(
                        &format!("SELECT {COLS} FROM notifications WHERE id = ?1"),
                        [id],
                        row_to_notification,
                    )?;
                    row.layout = Some(t.open.clone());
                    out.push((row, n.quiet));
                }
            }
            // Retention: the newest KEEP_COUNT that arrived in the last KEEP_SECS.
            conn.execute(
                "DELETE FROM notifications WHERE inserted_at < ?1
                     OR id <= (SELECT id FROM notifications ORDER BY id DESC LIMIT 1 OFFSET ?2)",
                params![now - KEEP_SECS, KEEP_COUNT],
            )?;
            Ok(out)
        })
    }

    fn list(&self, pod: &Pod, q: &ListQuery) -> rusqlite::Result<Vec<Notification>> {
        let before = match &q.before {
            None => i64::MAX,
            Some(b) => b.parse::<i64>().unwrap_or(0),
        };
        pod.store().with_conn(|c| {
            let mut stmt = c.prepare_cached(&format!(
                "SELECT {COLS} FROM notifications
                 WHERE id < ?1 AND (?2 IS NULL OR layout = ?2)
                 ORDER BY id DESC"
            ))?;
            let mut out = Vec::new();
            for n in stmt.query_map(params![before, q.layout], row_to_notification)? {
                let n = n?;
                if (q.layouts.is_empty() || q.layouts.contains(&n.channel_layout))
                    && !q
                        .hidden
                        .contains(&(n.channel_layout.clone(), n.channel.clone()))
                {
                    out.push(n);
                    if out.len() > q.limit {
                        break;
                    }
                }
            }
            Ok(out)
        })
    }

    fn unread(
        &self,
        pod: &Pod,
        layouts: &[String],
        hidden: &HashSet<ChannelId>,
    ) -> rusqlite::Result<(u32, BTreeMap<String, u32>)> {
        pod.store().with_conn(|c| unread_on(c, layouts, hidden))
    }

    fn mark_read(&self, pod: &Pod, sel: &ReadSel) -> rusqlite::Result<()> {
        pod.store().transaction(|tx| {
            let now = now_secs();
            match sel {
                ReadSel::Ids(ids) => {
                    for id in ids {
                        let Ok(id) = id.parse::<i64>() else { continue };
                        tx.execute(
                            "UPDATE notifications SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL",
                            params![id, now],
                        )?;
                    }
                }
                ReadSel::All(layout) => {
                    tx.execute(
                        "UPDATE notifications SET read_at = ?1
                         WHERE read_at IS NULL AND (?2 IS NULL OR layout = ?2)",
                        params![now, layout],
                    )?;
                }
            }
            Ok(())
        })
    }

    fn switches(&self, pod: &Pod) -> rusqlite::Result<HashMap<ChannelId, bool>> {
        pod.store().with_conn(|c| {
            let mut stmt =
                c.prepare_cached("SELECT layout, channel, enabled FROM notification_switches")?;
            stmt.query_map([], |r| {
                Ok(((r.get(0)?, r.get(1)?), r.get::<_, i64>(2)? != 0))
            })?
            .collect()
        })
    }

    fn set_switches(&self, pod: &Pod, choices: &[(String, String, bool)]) -> rusqlite::Result<()> {
        pod.store().transaction(|tx| {
            for (layout, channel, enabled) in choices {
                tx.execute(
                    "INSERT INTO notification_switches (layout, channel, enabled) VALUES (?1, ?2, ?3)
                     ON CONFLICT(layout, channel) DO UPDATE SET enabled = excluded.enabled",
                    params![layout, channel, i64::from(*enabled)],
                )?;
            }
            Ok(())
        })
    }

    fn remove_about(&self, pod: &Pod, subjects: &[String]) -> rusqlite::Result<usize> {
        pod.store().transaction(|tx| {
            let mut n = 0;
            for s in subjects {
                n += tx.execute("DELETE FROM notifications WHERE subject = ?1", [s])?;
            }
            Ok(n)
        })
    }
}
