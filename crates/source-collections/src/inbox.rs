//! The pod's notifications as collections (decision
//! `notifications-in-a-collection`): Collections is the pod's
//! [`NotificationStore`].
//!
//! - `notifications`: one row per notification any source sent, newest
//!   first. Layouts bind it like any collection (the inbox blueprint does),
//!   filter it, and edit it: marking read is setting `read`, removing one is
//!   deleting its row. `channel_enabled` and `channel_title` follow the
//!   channel's row (live lookups); `read_at` follows `read`.
//! - `notification_channels`: one row per channel sources declared (kept
//!   current as they declare them), with its pod-wide switch `enabled` and
//!   `remove_with_subject`: whether a notification about a row goes when
//!   that row does.
//!
//! Both are system collections: every pod has them, their fields can't be
//! renamed or removed, and they don't count against the pod's collections
//! or rows. `notifications` keeps the newest 500 of the last 30 days
//! ([`Keep`]). A pod's first open hands over what the core's own tables
//! held.

use std::collections::{BTreeMap, HashMap, HashSet};

use pod_core::notify_store::ChannelId;
use pod_core::{ListQuery, NotificationStore, Pod, ReadSel, ToStore};
use protocol::{Importance, Notification, NotificationChannel};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde_json::{Map, Value, json};

use crate::db::{self, Row};
use crate::model::{self, Compute, Definition, Keep, Match, System, Write};
use crate::{Caps, follow};

/// The notifications.
pub const NOTIFICATIONS: &str = "notifications";
/// The channels and their switches.
pub const CHANNELS: &str = "notification_channels";
/// The blueprint the system collections are recorded under.
const APP: &str = "inbox";

/// Whether `collection` is one of these (they don't count against the
/// pod's caps).
pub fn is_inbox(collection: &str) -> bool {
    collection == NOTIFICATIONS || collection == CHANNELS
}

fn system() -> Option<System> {
    Some(System {
        app: APP.into(),
        channel: String::new(),
        alert_title: String::new(),
        alert_body: String::new(),
    })
}

fn fields(specs: &[Value]) -> Vec<model::Field> {
    specs
        .iter()
        .enumerate()
        .map(|(i, s)| model::parse_field(s, format!("f{}", i + 1), 4_000).expect("a valid field"))
        .collect()
}

/// The `notification_channels` definition.
pub fn channels_definition() -> Definition {
    let fields = fields(&[
        json!({"name": "key", "type": "text", "unique": true}),
        json!({"name": "layout", "type": "text"}),
        json!({"name": "channel", "type": "text"}),
        json!({"name": "title", "type": "text"}),
        json!({"name": "description", "type": "text", "optional": true}),
        json!({"name": "importance", "type": "choice", "options": ["low", "default", "high"], "default": "default"}),
        json!({"name": "enabled", "type": "bool", "default": true}),
        json!({"name": "default_enabled", "type": "bool", "default": true}),
        json!({"name": "remove_with_subject", "type": "bool", "default": true}),
    ]);
    Definition {
        name: CHANNELS.into(),
        title: "Notification channels".into(),
        next_field: fields.len() as u32 + 1,
        fields,
        order_by: vec!["f1".into()],
        descending: false,
        reminders: Vec::new(),
        fillers: Vec::new(),
        alerts: Vec::new(),
        system: system(),
        actions: Vec::new(),
        views: Vec::new(),
        keep: None,
    }
}

/// The `notifications` definition (its lookups read `channels`).
pub fn notifications_definition(channels: &Definition) -> Definition {
    let mut fields = fields(&[
        json!({"name": "title", "type": "text"}),
        json!({"name": "body", "type": "text", "optional": true}),
        json!({"name": "channel_layout", "type": "text"}),
        json!({"name": "channel", "type": "text"}),
        json!({"name": "channel_key", "type": "text"}),
        json!({"name": "layout", "type": "text", "optional": true}),
        json!({"name": "target_uri", "type": "text", "optional": true}),
        json!({"name": "image_url", "type": "text", "optional": true}),
        json!({"name": "group_key", "type": "text", "optional": true}),
        json!({"name": "subject", "type": "text", "optional": true}),
        json!({"name": "dedupe_key", "type": "text", "optional": true}),
        json!({"name": "at", "type": "datetime", "optional": true}),
        json!({"name": "read", "type": "bool", "default": false}),
        json!({"name": "read_at", "type": "datetime", "optional": true}),
        json!({"name": "channel_enabled", "type": "bool", "default": true}),
        json!({"name": "channel_title", "type": "text", "optional": true}),
    ]);
    let id = |d: &Definition, n: &str| d.field(n).expect("a field").id.clone();
    let key = id(channels, "key");
    let from = "f5".to_owned();
    for (name, value) in [
        ("channel_enabled", id(channels, "enabled")),
        ("channel_title", id(channels, "title")),
    ] {
        let f = fields.iter_mut().find(|f| f.name == name).expect("a field");
        f.compute = vec![Compute::Lookup {
            from: from.clone(),
            collection: CHANNELS.into(),
            key: key.clone(),
            value,
            mode: Match::Exact,
            learn: false,
        }];
    }
    let mut def = Definition {
        name: NOTIFICATIONS.into(),
        title: "Notifications".into(),
        next_field: fields.len() as u32 + 1,
        fields,
        order_by: vec!["f12".into()],
        descending: true,
        reminders: Vec::new(),
        fillers: Vec::new(),
        alerts: Vec::new(),
        system: system(),
        actions: Vec::new(),
        views: Vec::new(),
        keep: Some(Keep {
            rows: pod_core::notify::KEEP_COUNT as u32,
            days: (pod_core::notify::KEEP_SECS / 86_400) as u32,
        }),
    };
    // A layout's "Mark all read" and "Clear read" are its own actions.
    let mut defs = model::Defs::default();
    defs.insert(def.name.clone(), def.clone());
    for spec in [
        json!({"name": "mark_all_read", "title": "Mark all read",
            "source": {"collection": NOTIFICATIONS, "where": [{"field": "read", "equals": false}]},
            "update": {"read": true}}),
        json!({"name": "clear_read", "title": "Clear read notifications",
            "source": {"collection": NOTIFICATIONS, "where": [{"field": "read", "equals": true}]},
            "delete": true}),
    ] {
        let a = crate::actions::parse(&def, &spec, &defs).expect("a valid action");
        def.actions.push(a);
    }
    with_subject_matched(&mut def);
    def
}

/// The field naming whether the row a notification is about still matches
/// an alert of its collection.
pub const SUBJECT_MATCHED: &str = "subject_alert_matched";

/// Add [`SUBJECT_MATCHED`] to `def` unless it has it: a bool, true unless
/// the notification's subject row is served with `alert_matched` false
/// (decision `notifications-follow-their-alerts`). Answers whether it was
/// added.
fn with_subject_matched(def: &mut Definition) -> bool {
    if def.field(SUBJECT_MATCHED).is_some() {
        return false;
    }
    let Some(from) = def.field("subject").map(|f| f.id.clone()) else {
        return false;
    };
    let id = format!("f{}", def.next_field);
    let mut f = model::parse_field(
        &json!({"name": SUBJECT_MATCHED, "type": "bool", "default": true}),
        id,
        4_000,
    )
    .expect("a valid field");
    f.compute = vec![Compute::Subject {
        from,
        value: "alert_matched".into(),
    }];
    def.fields.push(f);
    def.next_field += 1;
    true
}

/// Create both collections unless the pod has them; a new
/// `notifications` takes over what the core's tables held.
pub fn ensure(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<(Definition, Definition)> {
    let channels = match db::latest_def(tx, CHANNELS)? {
        Some((_, d)) => d,
        None => {
            let d = channels_definition();
            db::append_def(
                tx,
                &d,
                "The pod's notification channels",
                pod_core::layouts::AUTHOR_POD,
                now,
            )?;
            d
        }
    };
    let notifications = match db::latest_def(tx, NOTIFICATIONS)? {
        Some((_, d)) => d,
        None => {
            let d = notifications_definition(&channels);
            db::append_def(
                tx,
                &d,
                "The pod's notifications",
                pod_core::layouts::AUTHOR_POD,
                now,
            )?;
            take_over(tx, &d, &channels, now)?;
            d
        }
    };
    let notifications = upgrade(tx, notifications, now)?;
    Ok((notifications, channels))
}

/// A pod's `notifications` from before [`SUBJECT_MATCHED`]: the field is
/// added, notifications handed over from the core without a `subject` get
/// their `target_uri` as one when it names a collection row, and every row
/// is computed.
fn upgrade(tx: &Transaction<'_>, mut def: Definition, now: i64) -> rusqlite::Result<Definition> {
    if !with_subject_matched(&mut def) {
        return Ok(def);
    }
    db::append_def(
        tx,
        &def,
        "Notifications follow their alerts: subject_alert_matched",
        pod_core::layouts::AUTHOR_POD,
        now,
    )?;
    let (Some(subject), Some(target)) = (
        def.field("subject").map(|f| f.id.clone()),
        def.field("target_uri").map(|f| f.id.clone()),
    ) else {
        return Ok(def);
    };
    for mut row in db::all_rows(tx, NOTIFICATIONS, true)? {
        let has = row
            .values
            .get(&subject)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let to = row
            .values
            .get(&target)
            .and_then(Value::as_str)
            .filter(|t| t.starts_with("collections://"))
            .map(str::to_owned);
        if !has && let Some(t) = to {
            row.values.insert(subject.clone(), json!(t));
            db::save_row_values(tx, NOTIFICATIONS, &row)?;
        }
    }
    crate::subject::follow(
        tx,
        &HashSet::new(),
        &HashSet::new(),
        true,
        now,
        crate::Caps::default().text_chars,
    )?;
    Ok(def)
}

/// Whether the notification row is shown at all: its channel's switch and
/// its subject's alert ([`SUBJECT_MATCHED`]) are layouts' to filter on;
/// the counts and the old list leave out what fails the latter here.
fn subject_matches(def: &Definition, row: &Row) -> bool {
    Names(def).flag(row, SUBJECT_MATCHED, true)
}

/// [`ensure`] on `store` (when the pod opens).
pub fn prepare(store: &pod_core::store::Store) -> rusqlite::Result<()> {
    let now = pod_core::util::now_secs();
    store.transaction(|tx| ensure(tx, now).map(|_| ()))
}

/// [`prepare`] when `collection` is one of these and the pod has it not
/// yet (a request that comes before the pod's start finished).
pub fn prepare_for(store: &pod_core::store::Store, collection: &str) -> rusqlite::Result<()> {
    if is_inbox(collection)
        && store
            .with_conn(|c| db::latest_def(c, collection))?
            .is_none()
    {
        prepare(store)?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
        .exists([name])
}

/// The datetime `secs` reads as on `tz`'s wall clock, with its offset.
fn datetime(secs: i64, tz: &pod_core::tz::jiff::tz::TimeZone) -> Value {
    match pod_core::tz::jiff::Timestamp::from_second(secs) {
        Ok(t) => json!(
            t.to_zoned(tz.clone())
                .strftime("%Y-%m-%dT%H:%M:%S%:z")
                .to_string()
        ),
        Err(_) => Value::Null,
    }
}

/// The pod's zone, read on `conn`.
fn zone(conn: &Connection) -> pod_core::tz::jiff::tz::TimeZone {
    conn.query_row(
        "SELECT value FROM pod_settings WHERE key = ?1",
        [pod_core::tz::TIME_ZONE_SETTING],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|n| pod_core::tz::parse(&n).ok())
    .unwrap_or(pod_core::tz::jiff::tz::TimeZone::UTC)
}

/// Hand the core's notifications and switches over to the collections
/// (the newest [`pod_core::notify::KEEP_COUNT`]), and empty its tables.
fn take_over(
    tx: &Transaction<'_>,
    notifications: &Definition,
    channels: &Definition,
    now: i64,
) -> rusqlite::Result<()> {
    if table_exists(tx, "notification_switches")? {
        let switches: Vec<(String, String, bool)> = {
            let mut stmt =
                tx.prepare("SELECT layout, channel, enabled FROM notification_switches")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (layout, channel, enabled) in switches {
            let write = json_map(json!({
                "key": protocol::channel_key(&layout, &channel), "layout": layout,
                "channel": channel, "title": channel, "enabled": enabled,
            }));
            create(tx, channels, &write, now, None)?;
        }
        tx.execute("DELETE FROM notification_switches", [])?;
    }
    if !table_exists(tx, "notifications")? {
        return Ok(());
    }
    let tz = zone(tx);
    // (notification, its dedupe key, when it arrived), newest first.
    let old: Vec<(Notification, String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT layout, channel, title, body, image_url, target_uri, group_key, created_at,
                 read_at, subject, dedupe_key, inserted_at
             FROM notifications ORDER BY id DESC LIMIT ?1",
        )?;
        stmt.query_map([pod_core::notify::KEEP_COUNT], |r| {
            Ok((
                Notification {
                    id: String::new(),
                    channel_layout: r.get(0)?,
                    channel: r.get(1)?,
                    title: r.get(2)?,
                    body: r.get(3)?,
                    image_url: r.get(4)?,
                    target_uri: r.get(5)?,
                    layout: None,
                    group_key: r.get(6)?,
                    created_at: r.get(7)?,
                    read: r.get::<_, Option<i64>>(8)?.is_some(),
                    subject: r.get(9)?,
                },
                r.get(10)?,
                r.get(11)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    // Oldest first, so newer rows sort later as they did.
    for (n, key, arrived) in old.into_iter().rev() {
        let mut write = notification_write(&n, &key, None, &tz);
        if n.read {
            write.insert("read_at".into(), datetime(arrived, &tz));
        }
        create(tx, notifications, &write, arrived, None)?;
    }
    tx.execute("DELETE FROM notifications", [])?;
    Ok(())
}

fn json_map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// A notification's row values, by field name.
fn notification_write(
    n: &Notification,
    dedupe_key: &str,
    open: Option<&str>,
    tz: &pod_core::tz::jiff::tz::TimeZone,
) -> Map<String, Value> {
    let mut w = json_map(json!({
        "title": n.title,
        "body": n.body,
        "channel_layout": n.channel_layout,
        "channel": n.channel,
        "channel_key": protocol::channel_key(&n.channel_layout, &n.channel),
        "dedupe_key": dedupe_key,
        "at": datetime(n.created_at, tz),
        "read": n.read,
    }));
    for (k, v) in [
        ("layout", open.map(str::to_owned)),
        ("target_uri", n.target_uri.clone()),
        ("image_url", n.image_url.clone()),
        ("group_key", n.group_key.clone()),
        ("subject", n.subject.clone()),
    ] {
        if let Some(v) = v {
            w.insert(k.into(), json!(v));
        }
    }
    w
}

/// Insert a row of `def` from `write` (names): checked as a create is, its
/// lookups computed. Answers the row.
fn create(
    tx: &Connection,
    def: &Definition,
    write: &Map<String, Value>,
    now: i64,
    caps: Option<Caps>,
) -> rusqlite::Result<Row> {
    let caps = caps.unwrap_or_default();
    let mut values = Map::new();
    if let Err(e) = model::apply_write(def, &mut values, write, Write::Create, caps.text_chars) {
        tracing::warn!(collection = %def.name, error = %e, "a notification row was refused");
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut row = Row {
        id: db::new_id("n"),
        client_id: None,
        values,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        filler: None,
    };
    let _ = crate::compute::after_write(tx, def, None, &mut row, write, now, caps)?;
    db::insert_row(tx, &def.name, &row)?;
    Ok(row)
}

/// Write `write` (names) to `row` of `def` and save it, when it changes
/// anything.
fn update(
    tx: &Connection,
    def: &Definition,
    row: &mut Row,
    write: &Map<String, Value>,
    now: i64,
    caps: Caps,
) -> rusqlite::Result<bool> {
    let before = row.values.clone();
    if model::apply_write(def, &mut row.values, write, Write::Update, caps.text_chars).is_err() {
        return Ok(false);
    }
    let _ = crate::compute::after_write(tx, def, Some(&before), row, write, now, caps)?;
    if row.values == before {
        return Ok(false);
    }
    row.updated_at = now;
    db::save_row(tx, &def.name, row)?;
    Ok(true)
}

/// Field names to ids of a definition.
struct Names<'d>(&'d Definition);

impl Names<'_> {
    fn get<'r>(&self, row: &'r Row, name: &str) -> Option<&'r Value> {
        self.0
            .field(name)
            .and_then(|f| row.values.get(&f.id))
            .filter(|v| !v.is_null())
    }

    fn text(&self, row: &Row, name: &str) -> Option<String> {
        self.get(row, name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    }

    fn flag(&self, row: &Row, name: &str, default: bool) -> bool {
        self.get(row, name)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }
}

/// A row as the core's [`Notification`].
fn to_notification(def: &Definition, row: &Row) -> Notification {
    let f = Names(def);
    Notification {
        id: row.id.clone(),
        channel_layout: f.text(row, "channel_layout").unwrap_or_default(),
        channel: f.text(row, "channel").unwrap_or_default(),
        title: f.text(row, "title").unwrap_or_default(),
        body: f.text(row, "body").unwrap_or_default(),
        image_url: f.text(row, "image_url"),
        target_uri: f.text(row, "target_uri"),
        layout: f.text(row, "layout"),
        group_key: f.text(row, "group_key"),
        created_at: f
            .get(row, "at")
            .and_then(model::instant)
            .unwrap_or(row.created_at),
        read: f.flag(row, "read", false),
        subject: f.text(row, "subject"),
    }
}

/// The live notification rows, newest first.
fn newest_first(conn: &Connection) -> rusqlite::Result<Vec<Row>> {
    let mut rows = db::all_rows(conn, NOTIFICATIONS, false)?;
    rows.reverse();
    Ok(rows)
}

/// Remove the live notifications about `subjects` (whose channel's
/// `remove_with_subject` is on) inside a transaction; answers how many.
pub fn remove_about_on(
    tx: &Connection,
    subjects: &HashSet<String>,
    now: i64,
) -> rusqlite::Result<usize> {
    if subjects.is_empty() {
        return Ok(0);
    }
    let (Some((_, defn)), Some((_, defc))) = (
        db::latest_def(tx, NOTIFICATIONS)?,
        db::latest_def(tx, CHANNELS)?,
    ) else {
        return Ok(0);
    };
    let (n, c) = (Names(&defn), Names(&defc));
    let kept: HashSet<String> = db::all_rows(tx, CHANNELS, false)?
        .iter()
        .filter(|r| !c.flag(r, "remove_with_subject", true))
        .filter_map(|r| c.text(r, "key"))
        .collect();
    let mut gone = 0;
    for mut row in db::all_rows(tx, NOTIFICATIONS, false)? {
        let about = n
            .text(&row, "subject")
            .is_some_and(|s| subjects.contains(&s));
        let keep = n
            .text(&row, "channel_key")
            .is_some_and(|k| kept.contains(&k));
        if about && !keep {
            row.deleted_at = Some(now);
            row.updated_at = now;
            db::save_row(tx, NOTIFICATIONS, &row)?;
            gone += 1;
        }
    }
    Ok(gone)
}

/// Remove the live notifications whose dedupe key is `key` or starts with
/// `key:` (a repeating alert's earlier notifications about one row,
/// decision `research-per-row`); answers how many.
pub fn remove_deduped_on(tx: &Connection, key: &str, now: i64) -> rusqlite::Result<usize> {
    let Some((_, defn)) = db::latest_def(tx, NOTIFICATIONS)? else {
        return Ok(0);
    };
    let n = Names(&defn);
    let prefix = format!("{key}:");
    let mut gone = 0;
    for mut row in db::all_rows(tx, NOTIFICATIONS, false)? {
        if n.text(&row, "dedupe_key")
            .is_some_and(|d| d == key || d.starts_with(&prefix))
        {
            row.deleted_at = Some(now);
            row.updated_at = now;
            db::save_row(tx, NOTIFICATIONS, &row)?;
            gone += 1;
        }
    }
    Ok(gone)
}

/// What [`follow::settle`] does for these collections each wave, with the
/// rows the wave changed: a row of another collection that went takes its
/// notifications with it, and `read_at` follows `read`.
pub fn follow_changes(tx: &Connection, changed: &[db::Changed], now: i64) -> rusqlite::Result<()> {
    let mut gone = HashSet::new();
    let mut reads = Vec::new();
    for ch in changed {
        if ch.collection == NOTIFICATIONS {
            reads.push(ch.id.clone());
            continue;
        }
        if is_inbox(&ch.collection) || ch.before.is_none() {
            continue;
        }
        let live = db::row(tx, &ch.collection, &ch.id)?.is_some_and(|r| r.deleted_at.is_none());
        if !live {
            gone.insert(db::row_uri(&ch.collection, &ch.id));
        }
    }
    remove_about_on(tx, &gone, now)?;
    if reads.is_empty() {
        return Ok(());
    }
    let Some((_, def)) = db::latest_def(tx, NOTIFICATIONS)? else {
        return Ok(());
    };
    let (Some(read), Some(read_at)) = (def.field("read"), def.field("read_at")) else {
        return Ok(());
    };
    let tz = zone(tx);
    for id in reads {
        let Some(mut row) = db::row(tx, NOTIFICATIONS, &id)?.filter(|r| r.deleted_at.is_none())
        else {
            continue;
        };
        let is_read = row
            .values
            .get(&read.id)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let has_at = row
            .values
            .get(&read_at.id)
            .is_some_and(|v| !v.is_null() && v != "");
        let set = match (is_read, has_at) {
            (true, false) => datetime(now, &tz),
            (false, true) => Value::Null,
            _ => continue,
        };
        row.values.insert(read_at.id.clone(), set);
        db::save_row_values(tx, NOTIFICATIONS, &row)?;
    }
    Ok(())
}

/// Collections as the pod's notification store.
#[derive(Debug, Default)]
pub struct InboxStore;

impl InboxStore {
    /// Run `f` in one transaction on the collections (made first when
    /// missing), settle what it changed and tell clients.
    fn write<T>(
        &self,
        pod: &Pod,
        f: impl FnOnce(&Transaction<'_>, &Definition, &Definition, i64, Caps) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let caps = crate::adapter(pod).caps;
        let now = pod_core::util::now_secs();
        let (out, followed) = pod.store().transaction(|tx| {
            let (n, c) = ensure(tx, now)?;
            let out = f(tx, &n, &c, now, caps)?;
            Ok((out, follow::settle(tx, now, caps)?))
        })?;
        if !followed.rows.is_empty() {
            followed.publish(pod, None);
        }
        Ok(out)
    }

    fn read<T>(
        &self,
        pod: &Pod,
        f: impl FnOnce(&Connection, &Definition, &Definition) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let defs = pod.store().with_conn(|c| -> rusqlite::Result<_> {
            Ok((
                db::latest_def(c, NOTIFICATIONS)?,
                db::latest_def(c, CHANNELS)?,
            ))
        })?;
        match defs {
            (Some((_, n)), Some((_, c))) => pod.store().with_conn(|conn| f(conn, &n, &c)),
            _ => {
                let (n, c) = self.write(pod, |_, n, c, _, _| Ok((n.clone(), c.clone())))?;
                pod.store().with_conn(|conn| f(conn, &n, &c))
            }
        }
    }
}

impl NotificationStore for InboxStore {
    fn fill_layouts(
        &self,
        pod: &Pod,
        opens: &pod_core::layouts::NotificationOpens,
    ) -> rusqlite::Result<usize> {
        // (row id, the layout it should open), for the rows that change.
        let want = |n: &Names<'_>, r: &Row| -> Option<String> {
            let now = n.text(r, "layout");
            if now.as_deref().is_some_and(|l| opens.is_layout(l)) {
                return None;
            }
            let open = opens.open(
                &n.text(r, "channel_layout")?,
                n.text(r, "target_uri").as_deref(),
            );
            (now.as_deref() != Some(open.as_str())).then_some(open)
        };
        let pending = self.read(pod, |conn, defn, _| {
            let n = Names(defn);
            Ok(db::all_rows(conn, NOTIFICATIONS, false)?
                .iter()
                .filter(|r| want(&n, r).is_some())
                .count())
        })?;
        if pending == 0 {
            return Ok(0);
        }
        self.write(pod, |tx, defn, _, now, caps| {
            let n = Names(defn);
            let mut changed = 0;
            for mut r in db::all_rows(tx, NOTIFICATIONS, false)? {
                let Some(open) = want(&n, &r) else {
                    continue;
                };
                let write = json_map(json!({ "layout": open }));
                if update(tx, defn, &mut r, &write, now, caps)? {
                    changed += 1;
                }
            }
            Ok(changed)
        })
    }

    fn channels_declared(
        &self,
        pod: &Pod,
        channels: &[NotificationChannel],
    ) -> rusqlite::Result<()> {
        // Most calls change nothing: look before writing.
        let wanted = |r: Option<&Row>,
                      defc: &Definition,
                      ch: &NotificationChannel|
         -> Option<Map<String, Value>> {
            let c = Names(defc);
            let importance = match ch.importance {
                Importance::Low => "low",
                Importance::Default => "default",
                Importance::High => "high",
            };
            let want = json_map(json!({
                "layout": ch.layout, "channel": ch.id, "title": ch.title,
                "description": ch.description, "importance": importance,
                "default_enabled": ch.default_enabled,
            }));
            match r {
                None => Some(want),
                Some(r) => {
                    let differs = want.iter().any(|(k, v)| match v {
                        Value::Bool(b) => c.flag(r, k, !b) != *b,
                        Value::String(s) => c.text(r, k).unwrap_or_default() != *s,
                        _ => false,
                    });
                    differs.then_some(want)
                }
            }
        };
        let pending = self.read(pod, |conn, _, defc| {
            let c = Names(defc);
            let rows = db::all_rows(conn, CHANNELS, false)?;
            Ok(channels
                .iter()
                .filter(|ch| {
                    let key = ch.key();
                    let row = rows
                        .iter()
                        .find(|r| c.text(r, "key").as_deref() == Some(&key));
                    wanted(row, defc, ch).is_some()
                })
                .count())
        })?;
        if pending == 0 {
            return Ok(());
        }
        self.write(pod, |tx, _, defc, now, caps| {
            let c = Names(defc);
            let mut rows = db::all_rows(tx, CHANNELS, false)?;
            for ch in channels {
                let key = ch.key();
                let pos = rows
                    .iter()
                    .position(|r| c.text(r, "key").as_deref() == Some(&key));
                let Some(mut want) = wanted(pos.map(|p| &rows[p]), defc, ch) else {
                    continue;
                };
                match pos {
                    Some(p) => {
                        update(tx, defc, &mut rows[p], &want, now, caps)?;
                    }
                    None => {
                        want.insert("key".into(), json!(key));
                        want.insert("enabled".into(), json!(ch.default_enabled));
                        rows.push(create(tx, defc, &want, now, Some(caps))?);
                    }
                }
            }
            Ok(())
        })
    }

    fn insert(&self, pod: &Pod, items: &[ToStore]) -> rusqlite::Result<Vec<(Notification, bool)>> {
        self.write(pod, |tx, defn, _, now, caps| {
            let n = Names(defn);
            let tz = zone(tx);
            let mut seen: HashSet<(String, String)> = db::all_rows(tx, NOTIFICATIONS, true)?
                .iter()
                .filter_map(|r| Some((n.text(r, "channel_layout")?, n.text(r, "dedupe_key")?)))
                .collect();
            let mut out = Vec::new();
            for t in items {
                let item = &t.item;
                if !seen.insert((item.layout.clone(), item.dedupe_key.clone())) {
                    continue;
                }
                let note = Notification {
                    id: String::new(),
                    channel_layout: item.layout.clone(),
                    channel: item.channel.clone(),
                    title: item.title.clone(),
                    body: item.body.clone(),
                    image_url: item.image_url.clone(),
                    target_uri: item.target_uri.clone(),
                    layout: Some(t.open.clone()),
                    group_key: item.group_key.clone(),
                    created_at: item.created_at,
                    read: item.quiet,
                    subject: item.subject.clone(),
                };
                let mut write = notification_write(&note, &item.dedupe_key, Some(&t.open), &tz);
                if item.quiet {
                    write.insert("read_at".into(), datetime(now, &tz));
                }
                let row = create(tx, defn, &write, now, Some(caps))?;
                out.push((to_notification(defn, &row), item.quiet));
            }
            Ok(out)
        })
    }

    fn list(&self, pod: &Pod, q: &ListQuery) -> rusqlite::Result<Vec<Notification>> {
        self.read(pod, |conn, defn, _| {
            let rows = newest_first(conn)?;
            let start = match &q.before {
                None => 0,
                Some(b) => rows
                    .iter()
                    .position(|r| &r.id == b)
                    .map_or(rows.len(), |p| p + 1),
            };
            Ok(rows[start..]
                .iter()
                .filter(|r| subject_matches(defn, r))
                .map(|r| to_notification(defn, r))
                .filter(|n| {
                    (q.layouts.is_empty() || q.layouts.contains(&n.channel_layout))
                        && q.layout.as_ref().is_none_or(|l| *l == n.channel_layout)
                        && !q
                            .hidden
                            .contains(&(n.channel_layout.clone(), n.channel.clone()))
                })
                .take(q.limit + 1)
                .collect())
        })
    }

    fn unread(
        &self,
        pod: &Pod,
        layouts: &[String],
        hidden: &HashSet<ChannelId>,
    ) -> rusqlite::Result<(u32, BTreeMap<String, u32>)> {
        self.read(pod, |conn, defn, _| {
            let mut by = BTreeMap::new();
            let mut total = 0u32;
            for r in db::all_rows(conn, NOTIFICATIONS, false)? {
                if !subject_matches(defn, &r) {
                    continue;
                }
                let n = to_notification(defn, &r);
                if !n.read
                    && layouts.contains(&n.channel_layout)
                    && !hidden.contains(&(n.channel_layout.clone(), n.channel.clone()))
                {
                    total = total.saturating_add(1);
                    *by.entry(n.channel_layout).or_insert(0u32) += 1;
                }
            }
            Ok((total, by))
        })
    }

    fn mark_read(&self, pod: &Pod, sel: &ReadSel) -> rusqlite::Result<()> {
        self.write(pod, |tx, defn, _, now, caps| {
            let n = Names(defn);
            let write = json_map(json!({ "read": true }));
            for mut row in db::all_rows(tx, NOTIFICATIONS, false)? {
                let pick = match sel {
                    ReadSel::Ids(ids) => ids.contains(&row.id),
                    ReadSel::All(None) => true,
                    ReadSel::All(Some(l)) => n.text(&row, "channel_layout").as_deref() == Some(l),
                };
                if pick && !n.flag(&row, "read", false) {
                    update(tx, defn, &mut row, &write, now, caps)?;
                }
            }
            Ok(())
        })
    }

    fn switches(&self, pod: &Pod) -> rusqlite::Result<HashMap<ChannelId, bool>> {
        self.read(pod, |conn, _, defc| {
            let c = Names(defc);
            Ok(db::all_rows(conn, CHANNELS, false)?
                .iter()
                .filter_map(|r| {
                    Some((
                        (c.text(r, "layout")?, c.text(r, "channel")?),
                        c.flag(r, "enabled", true),
                    ))
                })
                .collect())
        })
    }

    fn set_switches(&self, pod: &Pod, choices: &[(String, String, bool)]) -> rusqlite::Result<()> {
        self.write(pod, |tx, _, defc, now, caps| {
            let c = Names(defc);
            let mut rows = db::all_rows(tx, CHANNELS, false)?;
            for (layout, channel, enabled) in choices {
                let key = protocol::channel_key(layout, channel);
                let write = json_map(json!({ "enabled": enabled }));
                match rows
                    .iter_mut()
                    .find(|r| c.text(r, "key").as_deref() == Some(&key))
                {
                    Some(r) => {
                        update(tx, defc, r, &write, now, caps)?;
                    }
                    None => {
                        let mut w = write;
                        w.extend(json_map(json!({
                            "key": key, "layout": layout, "channel": channel, "title": channel,
                        })));
                        rows.push(create(tx, defc, &w, now, Some(caps))?);
                    }
                }
            }
            Ok(())
        })
    }

    fn remove_about(&self, pod: &Pod, subjects: &[String]) -> rusqlite::Result<usize> {
        let subjects: HashSet<String> = subjects.iter().cloned().collect();
        self.write(pod, |tx, _, _, now, _| remove_about_on(tx, &subjects, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_definitions_are_valid() {
        let c = channels_definition();
        model::check_definition(&c, 40).unwrap();
        let n = notifications_definition(&c);
        model::check_definition(&n, 40).unwrap();
        let defs: model::Defs = [(c.name.clone(), c), (n.name.clone(), n.clone())]
            .into_iter()
            .collect();
        crate::actions::check_all(&n, &defs).unwrap();
        assert!(n.actions.iter().all(|a| a.source == NOTIFICATIONS));
    }
}
