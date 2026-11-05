//! Reminders (decision `collection-reminders`, issue #57): declarative rules
//! on a collection that notify the user when a row's date is reached ("tell
//! me when a todo is due"), through the D52 pipeline. A reminder is data in
//! the definition, not code: a date field, a time of day, an optional
//! `unless` (a field equal to a value, such as `done = true`) and the
//! notification's text with `{field}` placeholders.
//!
//! The host's scheduler polls the Collections adapter
//! ([`crate::CollectionsAdapter`]'s `poll_notifications`, which calls
//! [`run`]): a row notifies once per date value, when that date at `at` in
//! the pod's time zone ([`pod_core::tz`]) has passed, unless the `unless`
//! matches then. `collections_reminders_sent` remembers what was sent, so a
//! changed date notifies again and nothing notifies twice.
//!
//! Each reminder is a notification channel (its id) of the custom app that
//! shows its collection: the reminder's `app`, else the oldest live custom
//! app with the collection as a feed. Without such an app nothing is sent.

use std::collections::HashMap;
use std::time::Duration;

use pod_core::custom;
use pod_core::store::Store;
use pod_core::tz::jiff::{Timestamp, civil, tz::TimeZone};
use pod_core::{Catalog, NewNotification, Pod};
use protocol::{Importance, NotificationChannel};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::NAME;
use crate::db::{self, Row};
use crate::model::{self, Definition, Field, Kind};

/// Most reminders per collection.
pub const MAX_REMINDERS: usize = 5;
/// Longest notification title template.
pub const MAX_TITLE_CHARS: usize = 100;
/// Longest notification body template.
pub const MAX_BODY_CHARS: usize = 300;
/// A reminder this late (the pod polled nobody meanwhile) is not sent.
pub const LATE_SECS: i64 = 7 * 86_400;
/// Most notifications one poll sends (the rest follow on the next).
pub const MAX_PER_POLL: usize = 50;
/// The time of day a reminder fires at when it names none.
pub const DEFAULT_AT: &str = "09:00";
/// How long between polls at most while a reminder exists (rows added
/// meanwhile notify at most this late).
pub const MAX_WAIT: Duration = Duration::from_secs(900);
/// How long between polls when no collection has a reminder.
pub const IDLE_WAIT: Duration = Duration::from_secs(3600);

/// "Unless field `field` equals `equals`" (`null`: unless it is empty).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unless {
    /// A field id.
    pub field: String,
    pub equals: Value,
}

/// One reminder of a collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    /// `n` and 10 base32 characters, unique across the pod: the
    /// notification channel's id, so devices keep their choice for it.
    pub id: String,
    /// The date field (an id).
    pub field: String,
    /// `HH:MM` in the pod's time zone.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<Unless>,
    /// The channel's title in the notification settings.
    pub label: String,
    /// The notification's title and body, `{field}` replaced by the row's
    /// value.
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// The custom app it belongs to (else the oldest showing the collection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// When it was added (unix seconds): dates before that day never notify,
    /// so a new reminder doesn't announce every old row.
    pub created_at: i64,
}

impl Reminder {
    /// The field ids it reads.
    pub fn sources(&self) -> Vec<String> {
        let mut out = vec![self.field.clone()];
        if let Some(u) = &self.unless {
            out.push(u.field.clone());
        }
        out
    }

    /// As a spec with field names, for Claude.
    pub fn describe(&self, def: &Definition) -> Value {
        let mut v = json!({ "id": self.id, "field": def.name_of(&self.field), "at": self.at,
            "label": self.label, "title": self.title });
        if let Some(u) = &self.unless {
            v["unless"] = json!({ "field": def.name_of(&u.field), "equals": u.equals });
        }
        if !self.body.is_empty() {
            v["body"] = json!(self.body);
        }
        if let Some(a) = &self.app {
            v["app"] = json!(a);
        }
        v
    }
}

/// `HH:MM` as `(hour, minute)`.
fn parse_at(s: &str) -> Option<(i8, i8)> {
    let (h, m) = s.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m) = (h.parse::<i8>().ok()?, m.parse::<i8>().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

/// The `{name}` placeholders of a template.
fn placeholders(t: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        out.push(after[..close].trim());
        rest = &after[close + 1..];
    }
    out
}

fn check_template(def: &Definition, t: &str, what: &str, max: usize) -> Result<String, String> {
    let t = t.trim();
    if t.chars().count() > max {
        return Err(format!("\"{what}\" is at most {max} characters"));
    }
    for p in placeholders(t) {
        if def.field(p).is_none() && p != "id" {
            return Err(format!(
                "\"{what}\" names {{{p}}}, which is not a field (fields: {})",
                def.live_fields()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(t.to_owned())
}

fn live_field<'a>(def: &'a Definition, name: &str, what: &str) -> Result<&'a Field, String> {
    def.field(name).ok_or_else(|| {
        format!(
            "\"{what}\": no field '{name}' (fields: {})",
            def.live_fields()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Set the parts of `r` that `op` names (an `add_reminder` or
/// `update_reminder` op).
fn apply_spec(def: &Definition, r: &mut Reminder, op: &Value) -> Result<(), String> {
    if let Some(name) = op.get("field").and_then(Value::as_str) {
        let f = live_field(def, name.trim(), "field")?;
        if f.kind != Kind::Date {
            return Err(format!(
                "\"field\": '{}' is {}, not a date",
                f.name,
                f.kind.as_str()
            ));
        }
        r.field = f.id.clone();
    }
    if let Some(at) = op.get("at").and_then(Value::as_str) {
        let at = at.trim();
        if parse_at(at).is_none() {
            return Err(format!("\"at\" is a time of day HH:MM (24 h), not '{at}'"));
        }
        r.at = at.to_owned();
    }
    match op.get("unless") {
        None => {}
        Some(Value::Null) => r.unless = None,
        Some(u) => {
            let name = u["field"].as_str().unwrap_or("").trim();
            let f = live_field(def, name, "unless.field")?;
            let equals = model::coerce(f, u.get("equals").unwrap_or(&Value::Null), 4_000)
                .map_err(|e| format!("\"unless.equals\": {e}"))?
                .unwrap_or(Value::Null);
            r.unless = Some(Unless {
                field: f.id.clone(),
                equals,
            });
        }
    }
    if let Some(t) = op.get("title").and_then(Value::as_str) {
        r.title = check_template(def, t, "title", MAX_TITLE_CHARS)?;
    }
    if let Some(b) = op.get("body") {
        r.body = match b {
            Value::Null => String::new(),
            b => check_template(def, b.as_str().unwrap_or(""), "body", MAX_BODY_CHARS)?,
        };
    }
    if let Some(l) = op.get("label").and_then(Value::as_str) {
        let l = l.trim();
        if l.chars().count() > model::MAX_TITLE_CHARS {
            return Err(format!(
                "\"label\" is at most {} characters",
                model::MAX_TITLE_CHARS
            ));
        }
        r.label = l.to_owned();
    }
    match op.get("app") {
        None => {}
        Some(Value::Null) => r.app = None,
        Some(a) => {
            let a = a.as_str().unwrap_or("").trim();
            if !custom::is_custom_id(a) {
                return Err(format!("\"app\": '{a}' is not a custom app's id"));
            }
            r.app = Some(a.to_owned());
        }
    }
    if r.title.is_empty() {
        return Err("\"title\" is required (e.g. \"Due today: {title}\")".into());
    }
    if r.label.is_empty() {
        r.label = format!("{}: {}", def.title, def.name_of(&r.field));
    }
    Ok(())
}

/// `{op: add_reminder, field, title, body?, at?, unless?, label?, app?}`.
pub fn add(def: &mut Definition, op: &Value, now: i64) -> Result<(), String> {
    if def.reminders.len() >= MAX_REMINDERS {
        return Err(format!(
            "a collection has at most {MAX_REMINDERS} reminders"
        ));
    }
    if op.get("field").and_then(Value::as_str).is_none() {
        return Err("\"field\" names the date field".into());
    }
    let mut r = Reminder {
        id: db::new_id("n"),
        field: String::new(),
        at: DEFAULT_AT.into(),
        unless: None,
        label: String::new(),
        title: String::new(),
        body: String::new(),
        app: None,
        created_at: now,
    };
    apply_spec(def, &mut r, op)?;
    def.reminders.push(r);
    Ok(())
}

fn find<'a>(def: &'a mut Definition, op: &Value) -> Result<&'a mut Reminder, String> {
    let id = op["reminder"].as_str().unwrap_or("").trim().to_owned();
    let ids: Vec<String> = def.reminders.iter().map(|r| r.id.clone()).collect();
    def.reminders
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no reminder '{id}' (reminders: {})", ids.join(", ")))
}

/// `{op: update_reminder, reminder, ...}` (what it names changes).
pub fn update(def: &mut Definition, op: &Value) -> Result<(), String> {
    let mut r = find(def, op)?.clone();
    apply_spec(def, &mut r, op)?;
    *find(def, op)? = r;
    Ok(())
}

/// `{op: remove_reminder, reminder}`.
pub fn remove(def: &mut Definition, op: &Value) -> Result<(), String> {
    let id = find(def, op)?.id.clone();
    def.reminders.retain(|r| r.id != id);
    Ok(())
}

/// The checks of a whole definition: every field a reminder reads is live.
pub fn check(def: &Definition) -> Result<(), String> {
    if def.reminders.len() > MAX_REMINDERS {
        return Err(format!(
            "a collection has at most {MAX_REMINDERS} reminders"
        ));
    }
    for r in &def.reminders {
        for id in r.sources() {
            if !def.by_id(&id).is_some_and(Field::live) {
                return Err(format!(
                    "reminder '{}' reads '{}', which is removed; change or remove the reminder first",
                    r.id,
                    def.name_of(&id)
                ));
            }
        }
        if def.by_id(&r.field).is_some_and(|f| f.kind != Kind::Date) {
            return Err(format!(
                "reminder '{}' reads '{}', which is no longer a date; change or remove the reminder first",
                r.id,
                def.name_of(&r.field)
            ));
        }
    }
    Ok(())
}

// ---------- evaluation ----------

/// The moment `date` at `at` in `tz` (unix seconds).
pub fn moment(date: &str, at: &str, tz: &TimeZone) -> Option<i64> {
    let d: civil::Date = date.parse().ok()?;
    let (h, m) = parse_at(at)?;
    let z = d.at(h, m, 0, 0).to_zoned(tz.clone()).ok()?;
    Some(z.timestamp().as_second())
}

/// The calendar day of `t` in `tz`.
pub fn day(t: i64, tz: &TimeZone) -> String {
    Timestamp::from_second(t)
        .map(|ts| ts.to_zoned(tz.clone()).date().to_string())
        .unwrap_or_default()
}

fn value_of(field: &Field, values: &Map<String, Value>) -> Value {
    values
        .get(&field.id)
        .filter(|v| !v.is_null())
        .cloned()
        .or_else(|| field.default.clone())
        .unwrap_or(Value::Null)
}

/// `t` with `{name}` replaced by the row's values.
fn render(def: &Definition, t: &str, row: &Row) -> String {
    let mut out = String::new();
    let mut rest = t;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = after[..close].trim();
        let v = if name == "id" {
            Value::String(row.id.clone())
        } else {
            def.field(name)
                .map_or(Value::Null, |f| value_of(f, &row.values))
        };
        match v {
            Value::Null => {}
            Value::String(s) => out.push_str(&s),
            other => out.push_str(&other.to_string()),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// One notification a reminder owes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub row_id: String,
    pub date: String,
    pub title: String,
    pub body: String,
    /// When it was due (unix seconds).
    pub at: i64,
}

/// What reminder `r` of `def` owes at `now` over `rows` (sent or not), and
/// when it next owes something (unix seconds; `None`: nothing ahead).
pub fn due(
    def: &Definition,
    r: &Reminder,
    rows: &[Row],
    tz: &TimeZone,
    now: i64,
) -> (Vec<Due>, Option<i64>) {
    let (Some(field), since) = (def.by_id(&r.field), day(r.created_at, tz)) else {
        return (Vec::new(), None);
    };
    let unless = r
        .unless
        .as_ref()
        .and_then(|u| def.by_id(&u.field).map(|f| (f, &u.equals)));
    let mut out = Vec::new();
    let mut next: Option<i64> = None;
    for row in rows.iter().filter(|r| r.deleted_at.is_none()) {
        let Some(date) = value_of(field, &row.values).as_str().map(str::to_owned) else {
            continue;
        };
        if date < since {
            continue;
        }
        let Some(at) = moment(&date, &r.at, tz) else {
            continue;
        };
        if let Some((f, equals)) = unless
            && value_of(f, &row.values) == *equals
        {
            continue;
        }
        if at > now {
            next = Some(next.map_or(at, |n| n.min(at)));
        } else if now - at < LATE_SECS {
            out.push(Due {
                row_id: row.id.clone(),
                title: render(def, &r.title, row),
                body: render(def, &r.body, row),
                date,
                at,
            });
        }
    }
    out.sort_by(|a, b| (a.at, &a.row_id).cmp(&(b.at, &b.row_id)));
    (out, next)
}

/// The layout reminder `r` of collection `collection` belongs to now: its
/// own `app` while that layout lives, else the layout that shows the
/// collection (`owners`, [`custom::feed_owners`]; decision
/// `notification-owner`).
pub fn app_for(
    apps: &[custom::CustomApp],
    owners: &HashMap<String, String>,
    collection: &str,
    r: &Reminder,
) -> Option<String> {
    if let Some(a) = &r.app
        && apps.iter().any(|x| &x.id == a)
    {
        return Some(a.clone());
    }
    owners.get(collection).cloned()
}

/// Every reminder's channel, for the app it belongs to now.
pub fn channels(store: &Store, catalog: &Catalog) -> Vec<NotificationChannel> {
    let read = custom::feed_owners(store, catalog, NAME).and_then(|owners| {
        store.with_conn(|c| -> rusqlite::Result<_> {
            Ok((db::all_defs(c)?, custom::custom_apps_on(c)?, owners))
        })
    });
    let (defs, apps, owners) = match read {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "reminders could not be read");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for (_, def) in defs {
        for r in &def.reminders {
            if let Some(app) = app_for(&apps, &owners, &def.name, r) {
                out.push(NotificationChannel {
                    app_id: app,
                    id: r.id.clone(),
                    title: r.label.clone(),
                    description: format!(
                        "When the {} of one of your {} is reached",
                        def.name_of(&r.field),
                        def.title.to_lowercase()
                    ),
                    default_enabled: true,
                    importance: Importance::default(),
                });
            }
        }
    }
    out
}

/// What [`plan`] found: the notifications to send (each with its `(collection,
/// reminder, row, date)`) and when to look again.
#[derive(Debug, Default)]
pub struct Plan {
    pub send: Vec<(NewNotification, [String; 4])>,
    pub next: Option<i64>,
    pub any: bool,
}

fn sent(conn: &Connection, key: &[String; 4]) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM collections_reminders_sent
         WHERE collection = ?1 AND reminder = ?2 AND row_id = ?3 AND date = ?4",
        params![key[0], key[1], key[2], key[3]],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// What every reminder owes at `now` and hasn't sent (`owners`:
/// [`custom::feed_owners`] of Collections).
pub fn plan(
    conn: &Connection,
    owners: &HashMap<String, String>,
    tz: &TimeZone,
    now: i64,
) -> rusqlite::Result<Plan> {
    let apps = custom::custom_apps_on(conn)?;
    let mut out = Plan::default();
    for (_, def) in db::all_defs(conn)? {
        if def.reminders.is_empty() {
            continue;
        }
        out.any = true;
        let rows = db::all_rows(conn, &def.name, false)?;
        for r in &def.reminders {
            let Some(app) = app_for(&apps, owners, &def.name, r) else {
                continue;
            };
            let (owed, next) = due(&def, r, &rows, tz, now);
            if let Some(n) = next {
                out.next = Some(out.next.map_or(n, |x| x.min(n)));
            }
            for d in owed {
                let key = [def.name.clone(), r.id.clone(), d.row_id.clone(), d.date];
                if out.send.len() >= MAX_PER_POLL || sent(conn, &key)? {
                    continue;
                }
                out.send.push((
                    NewNotification {
                        app_id: app.clone(),
                        channel: r.id.clone(),
                        dedupe_key: format!("{}:{}:{}:{}", key[0], key[1], key[2], key[3]),
                        title: d.title,
                        body: d.body,
                        image_url: None,
                        target_uri: Some(db::row_uri(&def.name, &key[2])),
                        group_key: Some(r.id.clone()),
                        created_at: d.at,
                        quiet: false,
                    },
                    key,
                ));
            }
        }
    }
    Ok(out)
}

/// Send what the pod's reminders owe at `now`; answers how long until the
/// next look.
pub async fn run(pod: &Pod, now: i64) -> Duration {
    let p = pod.clone();
    let planned = pod
        .blocking(move |s| {
            let tz = pod_core::tz::time_zone(s)?;
            let owners = custom::feed_owners(s, p.catalog(), NAME)?;
            s.with_conn(|c| plan(c, &owners, &tz, now))
        })
        .await;
    let planned = match planned {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(status = ?e.0, "reminders could not be read");
            return MAX_WAIT;
        }
    };
    if !planned.any {
        return IDLE_WAIT;
    }
    let wait = planned.next.map_or(MAX_WAIT, |n| {
        Duration::from_secs((n - now).clamp(60, MAX_WAIT.as_secs() as i64) as u64)
    });
    if planned.send.is_empty() {
        return wait;
    }
    let (items, keys): (Vec<_>, Vec<_>) = planned.send.into_iter().unzip();
    let more = keys.len() >= MAX_PER_POLL;
    if let Err(e) = pod.notify(items).await {
        tracing::error!(status = ?e.0, "reminders could not be stored");
        return MAX_WAIT;
    }
    let stored = pod
        .blocking(move |s| {
            s.transaction(|tx| {
                for k in &keys {
                    tx.execute(
                        "INSERT OR IGNORE INTO collections_reminders_sent
                             (collection, reminder, row_id, date, sent_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![k[0], k[1], k[2], k[3], now],
                    )?;
                }
                Ok(())
            })
        })
        .await;
    if let Err(e) = stored {
        tracing::error!(status = ?e.0, "sent reminders could not be recorded");
    }
    if more { Duration::from_secs(60) } else { wait }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            reminders: Vec::new(),
            fillers: Vec::new(),
            alerts: Vec::new(),
            system: None,
        }
    }

    fn row(id: &str, values: Value) -> Row {
        Row {
            id: id.into(),
            client_id: None,
            values: values.as_object().unwrap().clone(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            filler: None,
        }
    }

    fn ts(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_second()
    }

    fn with_reminder(created: &str) -> Definition {
        let mut def = todos();
        add(
            &mut def,
            &json!({"field": "due", "title": "Due: {title}", "body": "On {due}",
                    "unless": {"field": "done", "equals": true}}),
            ts(created),
        )
        .unwrap();
        def
    }

    #[test]
    fn ops_check_fields_and_templates() {
        let mut def = todos();
        assert!(
            add(&mut def, &json!({"field": "title", "title": "x"}), 0)
                .unwrap_err()
                .contains("not a date")
        );
        assert!(
            add(&mut def, &json!({"field": "due", "title": "{nope}"}), 0)
                .unwrap_err()
                .contains("{nope}")
        );
        assert!(
            add(
                &mut def,
                &json!({"field": "due", "title": "x", "at": "25:00"}),
                0
            )
            .is_err()
        );
        assert!(add(&mut def, &json!({"field": "due"}), 0).is_err());
        add(
            &mut def,
            &json!({"field": "due", "title": "Due: {title}"}),
            0,
        )
        .unwrap();
        let r = def.reminders[0].clone();
        assert_eq!((r.at.as_str(), r.label.as_str()), ("09:00", "Todos: due"));
        assert!(r.id.starts_with('n') && pod_core::notify::valid_channel_id(&r.id));
        update(&mut def, &json!({"reminder": r.id, "at": "07:30"})).unwrap();
        assert_eq!(def.reminders[0].at, "07:30");
        assert_eq!(def.reminders[0].id, r.id);
        // A field a reminder reads can't be removed under it.
        def.fields[2].removed_at = Some(1);
        assert!(check(&def).unwrap_err().contains("removed"));
        def.fields[2].removed_at = None;
        remove(&mut def, &json!({"reminder": r.id})).unwrap();
        assert!(def.reminders.is_empty());
    }

    #[test]
    fn due_today_notifies_after_its_time_unless_done() {
        let tz = zone("America/Denver");
        let def = with_reminder("2026-11-01T12:00:00Z");
        let r = &def.reminders[0];
        let rows = [
            row("a", json!({"f1": "Pay rent", "f3": "2026-11-04"})),
            row("b", json!({"f1": "Ticked", "f2": true, "f3": "2026-11-04"})),
            row("c", json!({"f1": "Later", "f3": "2026-11-09"})),
            row("d", json!({"f1": "No date"})),
            row(
                "e",
                json!({"f1": "Before the reminder", "f3": "2026-10-20"}),
            ),
        ];
        // 08:59 in Denver (MST, UTC-7, since November 1).
        let (owed, next) = due(&def, r, &rows, &tz, ts("2026-11-04T15:59:00Z"));
        assert!(owed.is_empty());
        assert_eq!(next, Some(ts("2026-11-04T16:00:00Z")));
        let (owed, next) = due(&def, r, &rows, &tz, ts("2026-11-04T16:00:00Z"));
        assert_eq!(owed.len(), 1);
        assert_eq!(owed[0].row_id, "a");
        assert_eq!(owed[0].title, "Due: Pay rent");
        assert_eq!(owed[0].body, "On 2026-11-04");
        assert_eq!(next, Some(ts("2026-11-09T16:00:00Z")));
        // A week late it is dropped.
        let (owed, _) = due(&def, r, &rows, &tz, ts("2026-11-12T16:00:00Z"));
        assert_eq!(
            owed.iter().map(|d| d.row_id.as_str()).collect::<Vec<_>>(),
            ["c"]
        );
    }

    #[test]
    fn the_zone_moves_with_daylight_saving() {
        let tz = zone("America/Denver");
        assert_eq!(
            moment("2026-07-01", "09:00", &tz),
            Some(ts("2026-07-01T15:00:00Z"))
        );
        assert_eq!(
            moment("2026-12-01", "09:00", &tz),
            Some(ts("2026-12-01T16:00:00Z"))
        );
        assert_eq!(day(ts("2026-12-02T03:00:00Z"), &tz), "2026-12-01");
    }

    fn zone(name: &str) -> TimeZone {
        pod_core::tz::parse(name).unwrap()
    }
}
