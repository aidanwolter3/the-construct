//! Alerts (decision `collection-fillers`): declarative rules that notify when
//! a row matches ("tell me when Improv
//! Millbrook lists a Brass Lanterns show"). Data in the definition, not code:
//! phrases of which one (`any`), every one (`all`) or none (`none`) must
//! appear in the row's text fields (ignoring case and spacing), optionally
//! only rows of one filler (a site), on some days of the week or within
//! some days, read from a date or datetime field.
//!
//! A matching row notifies once per alert (`collections_alerts_sent`),
//! after a fill and on the adapter's poll (so a row that comes into an
//! alert's window later notifies then). Rows are served with
//! `alert_matched` and `alert_names` ("Brass Lanterns"), worked out when
//! served. A row whose date has passed (more than six hours ago) no longer
//! matches.
//!
//! An alert is a notification channel (its id) of the custom app showing
//! its collection (`app`, else the oldest), as reminders are; a system
//! collection's alerts use its built-in app's channel instead.

use std::collections::HashMap;

use pod_core::custom;
use pod_core::store::Store;
use pod_core::{Catalog, NewNotification, Pod};
use protocol::{Importance, NotificationChannel};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::NAME;
use crate::db::{self, Row};
use crate::model::{self, Definition, Kind};

/// Most alerts per collection (the first Events app allowed 100 rules).
pub const MAX_ALERTS: usize = 100;
/// Most phrases per list, and their length.
pub const MAX_PHRASES: usize = 20;
pub const MAX_PHRASE_CHARS: usize = 100;
/// Day names, Monday first.
pub const DAY_NAMES: [&str; 7] = web_extract::when::DAY_NAMES;
/// More notifications than this from one run arrive quietly.
pub const LOUD_PER_RUN: usize = 10;
/// A dated row stops matching this long after its date.
pub const PAST_SLACK_SECS: i64 = 6 * 3600;

/// One alert of a collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    /// `a` and 10 base32 characters, unique across the pod: the
    /// notification channel's id for a custom app.
    pub id: String,
    /// What people see ("Brass Lanterns").
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub none: Vec<String>,
    /// The fields (ids) searched; empty: every text field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// Only rows this filler wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filler: Option<String>,
    /// `mon` … `sun`; empty: any day (of `date_field`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<String>,
    /// The date or datetime field (id) `days`, `within_days` and the past
    /// read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within_days: Option<u32>,
    /// The notification's title and body: `{field}` (and served keys such as
    /// `{start_text}`), `{alert}` and `{filler}` replaced. Empty: the
    /// collection's defaults.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// The channel's title in the notification settings.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub created_at: i64,
}

/// Lowercase with whitespace runs as one space.
pub fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Alert {
    /// The field ids it reads.
    pub fn sources(&self) -> Vec<String> {
        let mut out = self.fields.clone();
        out.extend(self.date_field.clone());
        out
    }

    /// One line for people: `"brass lanterns" · not "class" · Fri, Sat`.
    pub fn summary(&self) -> String {
        let q = |v: &[String], join: &str| {
            v.iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(join)
        };
        let mut parts = Vec::new();
        if !self.any.is_empty() {
            parts.push(q(&self.any, " or "));
        }
        if !self.all.is_empty() {
            parts.push(format!("all of {}", q(&self.all, ", ")));
        }
        if !self.none.is_empty() {
            parts.push(format!("not {}", q(&self.none, " or ")));
        }
        if !self.days.is_empty() {
            let days: Vec<String> = self
                .days
                .iter()
                .map(|d| {
                    let mut c = d.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect())
                        .unwrap_or_default()
                })
                .collect();
            parts.push(days.join(", "));
        }
        if let Some(n) = self.within_days {
            parts.push(format!("within {n} days"));
        }
        if parts.is_empty() {
            "every row".to_owned()
        } else {
            parts.join(" · ")
        }
    }

    /// As a spec with field names, for Claude.
    pub fn describe(&self, def: &Definition) -> Value {
        let mut v = json!({ "id": self.id, "name": self.name, "label": self.label,
            "summary": self.summary() });
        for (k, list) in [("any", &self.any), ("all", &self.all), ("none", &self.none)] {
            if !list.is_empty() {
                v[k] = json!(list);
            }
        }
        if !self.fields.is_empty() {
            v["fields"] = json!(
                self.fields
                    .iter()
                    .map(|f| def.name_of(f))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(f) = &self.filler {
            v["filler"] = json!(f);
            if let Some(x) = crate::filler::find(def, f) {
                v["filler_title"] = json!(x.title);
            }
        }
        if !self.days.is_empty() {
            v["days"] = json!(self.days);
        }
        if let Some(d) = &self.date_field {
            v["date_field"] = json!(def.name_of(d));
        }
        if let Some(n) = self.within_days {
            v["within_days"] = json!(n);
        }
        for (k, t) in [("title", &self.title), ("body", &self.body)] {
            if !t.is_empty() {
                v[k] = json!(t);
            }
        }
        if let Some(a) = &self.app {
            v["app"] = json!(a);
        }
        v
    }

    /// Whether `row` of `def` matches at `now`.
    pub fn matches(&self, def: &Definition, row: &Row, now: i64) -> bool {
        if row.deleted_at.is_some() {
            return false;
        }
        if let Some(f) = &self.filler
            && row.filler.as_deref() != Some(f.as_str())
        {
            return false;
        }
        let value = |id: &str| {
            row.values
                .get(id)
                .cloned()
                .or_else(|| def.by_id(id).and_then(|f| f.default.clone()))
                .unwrap_or(Value::Null)
        };
        if let Some(d) = &self.date_field {
            let v = value(d);
            let Some(w) = v.as_str().and_then(web_extract::when::When::parse) else {
                // A dated condition needs a date.
                return self.days.is_empty() && self.within_days.is_none();
            };
            let at = w.timestamp(None);
            let end = if w.time.is_some() {
                at + PAST_SLACK_SECS
            } else {
                at + 86_400 + PAST_SLACK_SECS
            };
            if end < now {
                return false;
            }
            if !self.days.is_empty() && !self.days.iter().any(|x| x == DAY_NAMES[w.weekday()]) {
                return false;
            }
            if let Some(n) = self.within_days
                && at > now + i64::from(n) * 86_400 + 86_400
            {
                return false;
            }
        }
        let fields: Vec<String> = if self.fields.is_empty() {
            def.live_fields()
                .filter(|f| matches!(f.kind, Kind::Text | Kind::Choice))
                .map(|f| f.id.clone())
                .collect()
        } else {
            self.fields.clone()
        };
        let text = normalize(
            &fields
                .iter()
                .filter_map(|id| match value(id) {
                    Value::String(s) => Some(s),
                    Value::Null => None,
                    other => Some(other.to_string()),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let has = |p: &String| text.contains(&normalize(p));
        if !self.any.is_empty() && !self.any.iter().any(has) {
            return false;
        }
        self.all.iter().all(has) && !self.none.iter().any(has)
    }
}

/// The names of `def`'s alerts `row` matches at `now`.
pub fn matching(def: &Definition, row: &Row, now: i64) -> Vec<String> {
    def.alerts
        .iter()
        .filter(|a| a.matches(def, row, now))
        .map(|a| a.name.clone())
        .collect()
}

/// The alerts of filler `filler` (its site's rules), for the app.
pub fn of_filler<'a>(def: &'a Definition, filler: &'a str) -> impl Iterator<Item = &'a Alert> {
    def.alerts
        .iter()
        .filter(move |a| a.filler.as_deref() == Some(filler))
}

// ---------- ops ----------

/// A list of phrases from an array, or from text (`"brass lanterns, house
/// team"`).
fn phrases(v: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    let list: Vec<String> = match v {
        Value::Null => return Ok(None),
        Value::String(s) => s.split(',').map(str::to_owned).collect(),
        Value::Array(a) => a
            .iter()
            .map(|x| x.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or(format!("\"{key}\" is a list of text"))?,
        _ => return Err(format!("\"{key}\" is a list of text")),
    };
    let list: Vec<String> = list
        .iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    if list.len() > MAX_PHRASES || list.iter().any(|p| p.chars().count() > MAX_PHRASE_CHARS) {
        return Err(format!(
            "\"{key}\": at most {MAX_PHRASES} phrases of {MAX_PHRASE_CHARS} characters"
        ));
    }
    Ok(Some(list))
}

fn field_of(def: &Definition, name: &str, what: &str) -> Result<String, String> {
    def.field(name.trim()).map(|f| f.id.clone()).ok_or_else(|| {
        format!(
            "\"{what}\": no field '{name}' (fields: {})",
            def.live_fields()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn template(t: &Value, what: &str) -> Result<String, String> {
    let t = t.as_str().unwrap_or("").trim();
    if t.chars().count() > 300 {
        return Err(format!("\"{what}\" is at most 300 characters"));
    }
    Ok(t.to_owned())
}

/// Set the parts of `a` that `op` names (an `add_alert` or `update_alert`).
pub fn apply_spec(def: &Definition, a: &mut Alert, op: &Value) -> Result<(), String> {
    for (k, slot) in [
        ("any", &mut a.any),
        ("all", &mut a.all),
        ("none", &mut a.none),
    ] {
        if let Some(list) = phrases(&op[k], k)? {
            *slot = list;
        }
    }
    if let Some(v) = op.get("fields") {
        a.fields = match phrases(v, "fields")? {
            None => Vec::new(),
            Some(list) => list
                .iter()
                .map(|n| field_of(def, n, "fields"))
                .collect::<Result<_, _>>()?,
        };
    }
    match op.get("filler") {
        None => {}
        Some(Value::Null) => a.filler = None,
        Some(v) => {
            let id = v.as_str().unwrap_or("").trim();
            if crate::filler::find(def, id).is_none() {
                return Err(format!(
                    "\"filler\": no filler '{id}' (fillers: {})",
                    def.fillers
                        .iter()
                        .map(|f| format!("{} ({})", f.id, f.title))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            a.filler = Some(id.to_owned());
        }
    }
    if let Some(list) = phrases(&op["days"], "days")? {
        let mut days = Vec::new();
        for d in list {
            let d3: String = d.to_lowercase().chars().take(3).collect();
            if !DAY_NAMES.contains(&d3.as_str()) {
                return Err(format!("\"{d}\" is not a day (mon … sun)"));
            }
            if !days.contains(&d3) {
                days.push(d3);
            }
        }
        a.days = days;
    }
    match op.get("date_field") {
        None => {}
        Some(Value::Null) => a.date_field = None,
        Some(v) => a.date_field = Some(field_of(def, v.as_str().unwrap_or(""), "date_field")?),
    }
    match op.get("within_days") {
        None => {}
        Some(Value::Null) => a.within_days = None,
        Some(v) => {
            let n = v
                .as_u64()
                .filter(|n| (1..=366).contains(n))
                .ok_or("\"within_days\" is a number of days (1-366)")?;
            a.within_days = Some(n as u32);
        }
    }
    if let Some(n) = op.get("name").and_then(Value::as_str) {
        a.name = n.trim().to_owned();
    }
    if a.name.is_empty() {
        a.name = a
            .any
            .first()
            .or(a.all.first())
            .cloned()
            .unwrap_or_else(|| "Every row".to_owned());
    }
    if a.name.chars().count() > model::MAX_TITLE_CHARS {
        return Err(format!(
            "\"name\" is at most {} characters",
            model::MAX_TITLE_CHARS
        ));
    }
    if let Some(t) = op.get("title") {
        a.title = template(t, "title")?;
    }
    if let Some(b) = op.get("body") {
        a.body = template(b, "body")?;
    }
    if let Some(l) = op.get("label").and_then(Value::as_str) {
        a.label = l.trim().chars().take(model::MAX_TITLE_CHARS).collect();
    }
    if a.label.is_empty() {
        a.label = format!("{}: {}", def.title, a.name);
    }
    match op.get("app") {
        None => {}
        Some(Value::Null) => a.app = None,
        Some(v) => {
            let id = v.as_str().unwrap_or("").trim();
            if !custom::is_custom_id(id) {
                return Err(format!("\"app\": '{id}' is not a custom app's id"));
            }
            a.app = Some(id.to_owned());
        }
    }
    if (!a.days.is_empty() || a.within_days.is_some()) && a.date_field.is_none() {
        a.date_field = def
            .live_fields()
            .find(|f| matches!(f.kind, Kind::Date | Kind::Datetime))
            .map(|f| f.id.clone());
        if a.date_field.is_none() {
            return Err("days and within_days need a date or datetime field (date_field)".into());
        }
    }
    Ok(())
}

/// `{op: add_alert, name?, any?, all?, none?, fields?, filler?, days?,
/// date_field?, within_days?, title?, body?, label?, app?}`.
pub fn add(def: &mut Definition, op: &Value, now: i64) -> Result<String, String> {
    if def.alerts.len() >= MAX_ALERTS {
        return Err(format!("a collection has at most {MAX_ALERTS} alerts"));
    }
    let mut a = Alert {
        id: db::new_id("a"),
        name: String::new(),
        any: Vec::new(),
        all: Vec::new(),
        none: Vec::new(),
        fields: Vec::new(),
        filler: None,
        days: Vec::new(),
        date_field: None,
        within_days: None,
        title: String::new(),
        body: String::new(),
        label: String::new(),
        app: None,
        created_at: now,
    };
    apply_spec(def, &mut a, op)?;
    let id = a.id.clone();
    def.alerts.push(a);
    Ok(id)
}

fn position(def: &Definition, op: &Value) -> Result<usize, String> {
    let id = op["alert"].as_str().unwrap_or("").trim();
    def.alerts.iter().position(|a| a.id == id).ok_or_else(|| {
        format!(
            "no alert '{id}' (alerts: {})",
            def.alerts
                .iter()
                .map(|a| format!("{} ({})", a.id, a.name))
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// `{op: update_alert, alert, ...}`.
pub fn update(def: &mut Definition, op: &Value) -> Result<(), String> {
    let i = position(def, op)?;
    let mut a = def.alerts[i].clone();
    apply_spec(def, &mut a, op)?;
    def.alerts[i] = a;
    Ok(())
}

/// `{op: remove_alert, alert}`.
pub fn remove(def: &mut Definition, op: &Value) -> Result<(), String> {
    let i = position(def, op)?;
    def.alerts.remove(i);
    Ok(())
}

/// The checks of a whole definition: every field and filler an alert names
/// is live.
pub fn check(def: &Definition) -> Result<(), String> {
    if def.alerts.len() > MAX_ALERTS {
        return Err(format!("a collection has at most {MAX_ALERTS} alerts"));
    }
    for a in &def.alerts {
        for id in a.sources() {
            if !def.by_id(&id).is_some_and(model::Field::live) {
                return Err(format!(
                    "alert '{}' reads '{}', which is removed; change or remove the alert first",
                    a.name,
                    def.name_of(&id)
                ));
            }
        }
        if let Some(d) = &a.date_field
            && def
                .by_id(d)
                .is_some_and(|f| !matches!(f.kind, Kind::Date | Kind::Datetime))
        {
            return Err(format!(
                "alert '{}': '{}' is not a date or datetime",
                a.name,
                def.name_of(d)
            ));
        }
        if let Some(f) = &a.filler
            && crate::filler::find(def, f).is_none()
        {
            return Err(format!(
                "alert '{}' is for a filler that was removed; remove the alert first (remove_alert)",
                a.name
            ));
        }
    }
    Ok(())
}

// ---------- notifying ----------

/// `t` with `{key}` replaced by the row's served values, `{alert}` and
/// `{filler}`.
fn render(t: &str, entity: &Map<String, Value>, alert: &str, filler: &str) -> String {
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
        match name {
            "alert" => out.push_str(alert),
            "filler" => out.push_str(filler),
            n => match entity.get(n) {
                Some(Value::String(s)) => out.push_str(s),
                Some(Value::Null) | None => {}
                Some(other) => out.push_str(&other.to_string()),
            },
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    tidy_separators(&out)
}

/// `out` without the separators (` · `, ` — `) around values that were
/// empty: `"a ·  — b"` reads `"a — b"`.
fn tidy_separators(out: &str) -> String {
    // (separator before, text) pieces.
    let mut pieces: Vec<(&str, String)> = Vec::new();
    let mut sep = "";
    let mut rest = out;
    loop {
        let next = [" · ", " — "]
            .iter()
            .filter_map(|s| rest.find(s).map(|i| (i, *s)))
            .min();
        match next {
            Some((i, s)) => {
                pieces.push((sep, rest[..i].to_owned()));
                sep = s;
                rest = &rest[i + s.len()..];
            }
            None => {
                pieces.push((sep, rest.to_owned()));
                break;
            }
        }
    }
    let mut text = String::new();
    let mut pending = "";
    for (s, p) in pieces {
        // The stronger separator wins when an empty piece drops out.
        if pending.is_empty() || s == " — " {
            pending = s;
        }
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push_str(pending);
        }
        text.push_str(p);
        pending = "";
    }
    text
}

/// The default title and body templates of `def`'s alerts.
fn defaults(def: &Definition) -> (String, String) {
    if let Some(s) = &def.system
        && !s.alert_title.is_empty()
    {
        return (s.alert_title.clone(), s.alert_body.clone());
    }
    // A field called title or name, else the first text field.
    let first = ["title", "name"]
        .iter()
        .find_map(|n| def.field(n).filter(|f| f.kind == Kind::Text))
        .or_else(|| def.live_fields().find(|f| f.kind == Kind::Text))
        .map_or("id".to_owned(), |f| f.name.clone());
    let when = def
        .live_fields()
        .find(|f| f.kind == Kind::Datetime)
        .map(|f| format!("{{{}_text}} — ", f.name))
        .unwrap_or_default();
    (format!("{{{first}}}"), format!("{when}{{alert}}"))
}

/// The app and channel alert `a` of `def` notifies on now: a system
/// collection's own, the alert's `app` while it lives, else the layout
/// that shows the collection (`owners`, [`custom::feed_owners`]; decision
/// `notification-owner`).
fn target(
    apps: &[custom::CustomApp],
    owners: &HashMap<String, String>,
    def: &Definition,
    a: &Alert,
) -> Option<(String, String)> {
    if let Some(s) = &def.system {
        return Some((s.app.clone(), s.channel.clone()));
    }
    if let Some(app) = &a.app
        && apps.iter().any(|x| &x.id == app)
    {
        return Some((app.clone(), a.id.clone()));
    }
    owners.get(&def.name).map(|app| (app.clone(), a.id.clone()))
}

/// Every custom app's alert channels.
pub fn channels(store: &Store, catalog: &Catalog) -> Vec<NotificationChannel> {
    let read = custom::feed_owners(store, catalog, NAME).and_then(|owners| {
        store.with_conn(|c| -> rusqlite::Result<_> {
            Ok((db::all_defs(c)?, custom::custom_apps_on(c)?, owners))
        })
    });
    let Ok((defs, apps, owners)) = read else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, def) in defs {
        if def.system.is_some() {
            continue;
        }
        for a in &def.alerts {
            if let Some((app, channel)) = target(&apps, &owners, &def, a) {
                out.push(NotificationChannel {
                    app_id: app,
                    id: channel,
                    title: a.label.clone(),
                    description: format!(
                        "When one of your {} matches {}",
                        def.title.to_lowercase(),
                        a.summary()
                    ),
                    default_enabled: true,
                    importance: Importance::default(),
                });
            }
        }
    }
    out
}

fn sent(conn: &Connection, collection: &str, alert: &str, row: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM collections_alerts_sent WHERE collection = ?1 AND alert = ?2 AND row_id = ?3",
        params![collection, alert, row],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

fn record(
    conn: &Connection,
    collection: &str,
    alert: &str,
    row: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO collections_alerts_sent (collection, alert, row_id, sent_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![collection, alert, row, now],
    )?;
    Ok(())
}

/// Record every current match of `def`'s rows from filler `filler` as sent
/// without notifying (rows a migration brings in were already announced).
pub fn mark_sent(
    conn: &Connection,
    def: &Definition,
    filler: &str,
    now: i64,
) -> rusqlite::Result<()> {
    for row in db::all_rows(conn, &def.name, false)?
        .iter()
        .filter(|r| r.filler.as_deref() == Some(filler))
    {
        for a in &def.alerts {
            if a.matches(def, row, now) {
                record(conn, &def.name, &a.id, &row.id, now)?;
            }
        }
    }
    Ok(())
}

/// One notification an alert owes: `(collection, alert, row)`.
type Owed = (NewNotification, [String; 3]);

/// What `def`'s alerts owe at `now` (`only`: one collection; `owners`:
/// [`custom::feed_owners`] of Collections).
pub fn plan(
    conn: &Connection,
    owners: &HashMap<String, String>,
    only: Option<&str>,
    now: i64,
) -> rusqlite::Result<Vec<Owed>> {
    let apps = custom::custom_apps_on(conn)?;
    let mut out: Vec<Owed> = Vec::new();
    for (_, def) in db::all_defs(conn)? {
        if def.alerts.is_empty() || only.is_some_and(|o| o != def.name) {
            continue;
        }
        let rows = db::all_rows(conn, &def.name, false)?;
        let (title_t, body_t) = defaults(&def);
        for row in &rows {
            for a in &def.alerts {
                if !a.matches(&def, row, now) || sent(conn, &def.name, &a.id, &row.id)? {
                    continue;
                }
                let Some((app, channel)) = target(&apps, owners, &def, a) else {
                    continue;
                };
                let entity = model::row_entity(
                    &def,
                    &row.id,
                    &row.values,
                    &pod_core::layouts::rfc3339(row.created_at),
                    &pod_core::layouts::rfc3339(row.updated_at),
                );
                let filler = row
                    .filler
                    .as_deref()
                    .and_then(|f| crate::filler::find(&def, f))
                    .map_or("", |f| f.title.as_str());
                let title = if a.title.is_empty() {
                    &title_t
                } else {
                    &a.title
                };
                let body = if a.body.is_empty() { &body_t } else { &a.body };
                out.push((
                    NewNotification {
                        app_id: app,
                        channel,
                        dedupe_key: format!("alert:{}:{}:{}", def.name, a.id, row.id),
                        title: render(title, &entity, &a.name, filler),
                        body: render(body, &entity, &a.name, filler),
                        image_url: None,
                        target_uri: Some(db::row_uri(&def.name, &row.id)),
                        group_key: Some(a.id.clone()),
                        created_at: now,
                        quiet: false,
                    },
                    [def.name.clone(), a.id.clone(), row.id.clone()],
                ));
            }
        }
    }
    // One notification per row (its first alert), the rest recorded.
    Ok(out)
}

/// Send what the alerts owe (`only`: one collection, after its fill).
pub async fn run(pod: &Pod, only: Option<String>) {
    let now = pod_core::util::now_secs();
    let o = only.clone();
    let p = pod.clone();
    let Ok(owed) = pod
        .blocking(move |s| {
            let owners = custom::feed_owners(s, p.catalog(), NAME)?;
            s.with_conn(|c| plan(c, &owners, o.as_deref(), now))
        })
        .await
    else {
        return;
    };
    if owed.is_empty() {
        return;
    }
    let mut items = Vec::new();
    let mut seen_rows: Vec<String> = Vec::new();
    for (mut n, k) in owed.iter().cloned() {
        // A row matching several alerts notifies once.
        let row = format!("{}/{}", k[0], k[2]);
        if seen_rows.contains(&row) {
            continue;
        }
        seen_rows.push(row);
        n.quiet = items.len() >= LOUD_PER_RUN;
        items.push(n);
    }
    if let Err(e) = pod.notify(items).await {
        tracing::error!(status = ?e.0, "alert notifications could not be stored");
        return;
    }
    let keys: Vec<[String; 3]> = owed.into_iter().map(|(_, k)| k).collect();
    let _ = pod
        .blocking(move |s| {
            s.transaction(|tx| {
                for k in &keys {
                    record(tx, &k[0], &k[1], &k[2], now)?;
                }
                Ok(())
            })
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events() -> Definition {
        let specs = [
            json!({"name": "title", "type": "text", "optional": true}),
            json!({"name": "start", "type": "datetime", "optional": true}),
            json!({"name": "venue", "type": "text", "optional": true}),
            json!({"name": "description", "type": "text", "optional": true}),
        ];
        Definition {
            name: "events".into(),
            title: "Events".into(),
            fields: specs
                .iter()
                .enumerate()
                .map(|(i, s)| model::parse_field(s, format!("f{}", i + 1), 9_999).unwrap())
                .collect(),
            order_by: Vec::new(),
            descending: false,
            next_field: 5,
            reminders: Vec::new(),
            fillers: Vec::new(),
            alerts: Vec::new(),
            system: None,
            actions: Vec::new(),
        }
    }

    fn row(values: Value) -> Row {
        Row {
            id: "r1".into(),
            client_id: None,
            values: values.as_object().unwrap().clone(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            filler: Some("s1".into()),
        }
    }

    fn ts(s: &str) -> i64 {
        web_extract::when::When::parse(s).unwrap().timestamp(None)
    }

    #[test]
    fn phrases_days_windows_and_the_past() {
        let mut def = events();
        add(
            &mut def,
            &json!({"any": "Brass Lanterns, house team", "none": ["class"], "days": ["fri"], "within_days": 7}),
            0,
        )
        .unwrap();
        let a = def.alerts[0].clone();
        assert_eq!(a.name, "Brass Lanterns");
        assert_eq!(
            a.date_field.as_deref(),
            Some("f2"),
            "the first datetime field"
        );
        assert_eq!(
            a.summary(),
            "\"Brass Lanterns\" or \"house team\" · not \"class\" · Fri · within 7 days"
        );
        let fri = row(
            json!({"f1": "Friday Improv", "f2": "2026-11-06T19:30-07:00",
                             "f4": "Lineup: BRASS  lanterns, Teacup Riot"}),
        );
        let now = ts("2026-11-04T12:00Z");
        assert!(a.matches(&def, &fri, now));
        let mut class = fri.clone();
        class.values.insert("f1".into(), json!("Improv class"));
        assert!(!a.matches(&def, &class, now));
        assert!(
            !a.matches(&def, &fri, ts("2026-10-01T12:00Z")),
            "outside the window"
        );
        assert!(!a.matches(&def, &fri, ts("2026-11-08T12:00Z")), "past");
        let mut sat = fri.clone();
        sat.values
            .insert("f2".into(), json!("2026-11-07T19:30-07:00"));
        assert!(!a.matches(&def, &sat, now));
        // Scoped to a filler.
        def.fillers.push(
            crate::filler::parse(
                &def,
                &json!({"kind": "fetch", "url": "https://club.example",
            "extract": "events", "map": {"title": "title"}}),
                "s1".into(),
                false,
            )
            .unwrap(),
        );
        update(&mut def, &json!({"alert": a.id, "filler": "s1"})).unwrap();
        let mut other = fri.clone();
        other.filler = Some("s2".into());
        assert!(!def.alerts[0].matches(&def, &other, now));
        assert_eq!(matching(&def, &fri, now), vec!["Brass Lanterns".to_owned()]);
        assert!(check(&def).is_ok());
        let mut gone = def.clone();
        gone.fillers.clear();
        assert!(check(&gone).is_err());
        assert!(add(&mut def, &json!({"days": "someday"}), 0).is_err());
    }

    #[test]
    fn notification_text() {
        let def = events();
        let e = model::row_entity(
            &def,
            "r1",
            json!({"f1": "Show", "f2": "2026-11-06T19:30-07:00", "f3": "The Back Room"})
                .as_object()
                .unwrap(),
            "",
            "",
        );
        let (t, b) = defaults(&def);
        assert_eq!(render(&t, &e, "Brass Lanterns", "Club"), "Show");
        assert_eq!(
            render(&b, &e, "Brass Lanterns", "Club"),
            "Fri Nov 6 · 7:30 PM — Brass Lanterns"
        );
        assert_eq!(
            render("{start_text} · {venue} · {nope} — {alert}", &e, "BL", ""),
            "Fri Nov 6 · 7:30 PM · The Back Room — BL"
        );
    }
}
