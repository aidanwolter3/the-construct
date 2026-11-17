//! Views (decision `collection-views`): small derived lists of a
//! collection, declared in its definition and served as their own query
//! keys, `collection:<c>:view:<name>`, beside the whole list
//! (`collection:<c>`). A view keeps the rows that match every `where`
//! condition (compared as text, as a layout's list `where` compares) and
//! whose date falls in a window relative to today in the pod's time zone,
//! sorts them by its own order (else the collection's) and keeps the first
//! `limit`: "the next 3 upcoming", "today's", "next week's".
//!
//! Fields are stored by id (a rename keeps the view); a view naming a
//! removed field is refused, like an alert. The rows are worked out when
//! the list is served; a view with a date window is told to reload at the
//! pod's local midnight by the adapter's poll ([`day_changed`]).

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use web_extract::when::{When, Zone, days_from_civil};

use crate::db::Row;
use crate::model::{self, Condition, Definition, Kind};

/// Most views per collection.
pub const MAX_VIEWS: usize = 10;
/// Most rows a view keeps.
pub const MAX_LIMIT: u32 = 200;
/// Farthest a date window reaches from today, either way.
pub const MAX_DAYS: i32 = 3660;

/// One view of a collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    /// `[a-z][a-z0-9_]*`, unique in its collection: the query key's last
    /// part.
    pub name: String,
    /// Every condition must match (field ids).
    #[serde(rename = "where", default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<DateWindow>,
    /// Field ids, first key first; empty: the collection's own order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order_by: Vec<String>,
    /// Every order key descending (with `order_by`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub descending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Rows whose date field (an id) falls from `from_days` to `to_days`
/// days after today (0: today, negative: past; either bound open).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateWindow {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_days: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_days: Option<i32>,
}

/// The query key of view `view` of `collection`.
pub fn key(collection: &str, view: &str) -> String {
    format!("collection:{collection}:view:{view}")
}

/// The view of `def` named `name`.
pub fn find<'a>(def: &'a Definition, name: &str) -> Option<&'a View> {
    def.views.iter().find(|v| v.name == name)
}

/// The pod's time zone (UTC when none is set), read on `conn`.
pub fn zone_on(conn: &rusqlite::Connection) -> rusqlite::Result<Zone> {
    let tz = pod_core::store::pod_setting_on(conn, pod_core::tz::TIME_ZONE_SETTING)?
        .and_then(|n| pod_core::tz::parse(&n).ok())
        .unwrap_or(pod_core::tz::jiff::tz::TimeZone::UTC);
    Ok(Zone::Named(tz))
}

/// Days since 1970-01-01 of today at `now` in `zone`.
pub fn today(zone: &Zone, now: i64) -> i64 {
    (now + i64::from(zone.offset_at(now)) * 60).div_euclid(86_400)
}

/// The local day (days since 1970-01-01) of a date or datetime value: a
/// time with an offset by its date in `zone`; a date, or a time without
/// an offset, by the date it was written with.
pub fn local_day(v: &Value, zone: &Zone) -> Option<i64> {
    let w = When::parse(v.as_str()?)?.shifted_to(Some(zone));
    Some(days_from_civil(w.year, w.month, w.day))
}

impl View {
    /// The field ids it reads.
    pub fn sources(&self) -> Vec<String> {
        let mut out: Vec<String> = self.conditions.iter().map(|c| c.field.clone()).collect();
        out.extend(self.date.iter().map(|d| d.field.clone()));
        out.extend(self.order_by.iter().cloned());
        out
    }

    /// Whether `row` of `def` is in the view on day `today` in `zone`.
    pub fn matches(&self, def: &Definition, row: &Row, zone: &Zone, today: i64) -> bool {
        if !self.conditions.iter().all(|c| c.matches(def, &row.values)) {
            return false;
        }
        let Some(d) = &self.date else {
            return true;
        };
        let v = row
            .values
            .get(&d.field)
            .cloned()
            .or_else(|| def.by_id(&d.field).and_then(|f| f.default.clone()))
            .unwrap_or(Value::Null);
        let Some(day) = local_day(&v, zone) else {
            return false;
        };
        d.from_days.is_none_or(|n| day >= today + i64::from(n))
            && d.to_days.is_none_or(|n| day <= today + i64::from(n))
    }

    /// The rows of `rows` (live, in the collection's order) in the view, in
    /// its order, at most `limit`.
    pub fn select<'r>(
        &self,
        def: &Definition,
        rows: &'r [Row],
        zone: &Zone,
        now: i64,
    ) -> Vec<&'r Row> {
        let today = today(zone, now);
        let mut out: Vec<&Row> = rows
            .iter()
            .filter(|r| self.matches(def, r, zone, today))
            .collect();
        let keys: Vec<&model::Field> = self
            .order_by
            .iter()
            .filter_map(|id| def.by_id(id))
            .filter(|f| f.live())
            .collect();
        if !keys.is_empty() {
            // Stable: equal rows keep the collection's order.
            out.sort_by(|a, b| {
                keys.iter()
                    .map(|f| crate::db::compare_rows(f, a, b, self.descending))
                    .find(|o| o.is_ne())
                    .unwrap_or(Ordering::Equal)
            });
        }
        if let Some(n) = self.limit {
            out.truncate(n as usize);
        }
        out
    }

    /// As a spec with field names, for Claude.
    pub fn describe(&self, def: &Definition) -> Value {
        let mut v = json!({ "name": self.name, "query_key": key(&def.name, &self.name) });
        if !self.conditions.is_empty() {
            v["where"] = Value::Array(
                self.conditions
                    .iter()
                    .map(|c| json!({ "field": def.name_of(&c.field), "equals": c.equals }))
                    .collect(),
            );
        }
        if let Some(d) = &self.date {
            let mut w = json!({ "field": def.name_of(&d.field) });
            if let Some(n) = d.from_days {
                w["from_days"] = json!(n);
            }
            if let Some(n) = d.to_days {
                w["to_days"] = json!(n);
            }
            v["date"] = w;
        }
        if !self.order_by.is_empty() {
            v["order_by"] =
                model::order_json(self.order_by.iter().map(|id| def.name_of(id)).collect());
            v["descending"] = json!(self.descending);
        }
        if let Some(n) = self.limit {
            v["limit"] = json!(n);
        }
        v
    }
}

fn field_id(def: &Definition, view: &str, what: &str, name: &str) -> Result<String, String> {
    def.field(name.trim()).map(|f| f.id.clone()).ok_or_else(|| {
        format!(
            "view '{view}': {what} '{name}' is not one of the fields ({})",
            def.live_fields()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn days(v: &Value, view: &str, key: &str) -> Result<Option<i32>, String> {
    match v {
        Value::Null => Ok(None),
        v => v
            .as_i64()
            .filter(|n| n.abs() <= i64::from(MAX_DAYS))
            .map(|n| Some(n as i32))
            .ok_or_else(|| {
                format!(
                    "view '{view}': date.{key} is a whole number of days from today (0 today, -1 yesterday, at most {MAX_DAYS} either way)"
                )
            }),
    }
}

/// A view from a spec with field names: `{name, where?: {field, equals} |
/// [...], date?: {field, from_days?, to_days?}, order_by?: name | [names],
/// descending?, limit?}`.
pub fn parse(def: &Definition, spec: &Value) -> Result<View, String> {
    if !spec.is_object() {
        return Err("a view is {name, where?, date?, order_by?, descending?, limit?}".into());
    }
    let name = spec["name"].as_str().unwrap_or("").trim().to_owned();
    if !model::valid_name(&name) {
        return Err(format!(
            "view name '{name}' must be [a-z][a-z0-9_]*, at most {} characters",
            model::MAX_NAME_CHARS
        ));
    }
    let items: Vec<&Value> = match spec.get("where") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().collect(),
        Some(one @ Value::Object(_)) => vec![one],
        Some(_) => {
            return Err(format!(
                "view '{name}': where is {{field, equals}} or a list of them"
            ));
        }
    };
    if items.len() > model::MAX_CONDITIONS {
        return Err(format!(
            "view '{name}': where has at most {} conditions",
            model::MAX_CONDITIONS
        ));
    }
    let mut conditions = Vec::new();
    for c in items {
        let field = c["field"].as_str().unwrap_or("");
        let equals = match c.get("equals") {
            Some(Value::String(s)) => s.clone(),
            Some(v @ (Value::Bool(_) | Value::Number(_))) => v.to_string(),
            _ => {
                return Err(format!(
                    "view '{name}': where '{field}' needs equals (text, true, false or a number)"
                ));
            }
        };
        conditions.push(Condition {
            field: field_id(def, &name, "where", field)?,
            equals,
        });
    }
    let date = match spec.get("date") {
        None | Some(Value::Null) => None,
        Some(d @ Value::Object(_)) => {
            let field = field_id(def, &name, "date.field", d["field"].as_str().unwrap_or(""))?;
            let w = DateWindow {
                field,
                from_days: days(&d["from_days"], &name, "from_days")?,
                to_days: days(&d["to_days"], &name, "to_days")?,
            };
            if w.from_days.is_none() && w.to_days.is_none() {
                return Err(format!(
                    "view '{name}': date needs from_days, to_days or both (0 is today)"
                ));
            }
            Some(w)
        }
        Some(_) => {
            return Err(format!(
                "view '{name}': date is {{field, from_days?, to_days?}}"
            ));
        }
    };
    let order =
        model::order_names(spec.get("order_by")).map_err(|e| format!("view '{name}': {e}"))?;
    let order_by = order
        .iter()
        .map(|n| field_id(def, &name, "order_by", n))
        .collect::<Result<Vec<_>, _>>()?;
    let limit = match spec.get("limit") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| (1..=u64::from(MAX_LIMIT)).contains(n))
                .ok_or_else(|| format!("view '{name}': limit is 1..={MAX_LIMIT} rows"))?
                as u32,
        ),
    };
    Ok(View {
        name,
        conditions,
        date,
        order_by,
        descending: spec["descending"].as_bool().unwrap_or(false),
        limit,
    })
}

/// Add `view`, or replace the one of its name.
pub fn set(def: &mut Definition, view: View) -> Result<(), String> {
    match def.views.iter().position(|v| v.name == view.name) {
        Some(i) => def.views[i] = view,
        None => {
            if def.views.len() >= MAX_VIEWS {
                return Err(format!("a collection has at most {MAX_VIEWS} views"));
            }
            def.views.push(view);
        }
    }
    Ok(())
}

/// Remove the view named `name`.
pub fn remove(def: &mut Definition, name: &str) -> Result<(), String> {
    let before = def.views.len();
    def.views.retain(|v| v.name != name);
    if def.views.len() == before {
        return Err(format!(
            "no view '{name}' (views: {})",
            names(def).join(", ")
        ));
    }
    Ok(())
}

/// The names of `def`'s views.
pub fn names(def: &Definition) -> Vec<String> {
    def.views.iter().map(|v| v.name.clone()).collect()
}

/// Whether `def` has a view with a date window (its rows change at
/// midnight).
pub fn dated(def: &Definition) -> bool {
    def.views.iter().any(|v| v.date.is_some())
}

/// The checks of a whole definition: names valid and unique, every field
/// a view reads live, a date window on a date or datetime field.
pub fn check(def: &Definition) -> Result<(), String> {
    if def.views.len() > MAX_VIEWS {
        return Err(format!("a collection has at most {MAX_VIEWS} views"));
    }
    for (i, v) in def.views.iter().enumerate() {
        if !model::valid_name(&v.name) {
            return Err(format!(
                "view name '{}' must be [a-z][a-z0-9_]*, at most {} characters",
                v.name,
                model::MAX_NAME_CHARS
            ));
        }
        if def.views[..i].iter().any(|o| o.name == v.name) {
            return Err(format!("there are two views named '{}'", v.name));
        }
        for id in v.sources() {
            if !def.by_id(&id).is_some_and(model::Field::live) {
                return Err(format!(
                    "view '{}' reads '{}', which is removed; change or remove the view first",
                    v.name,
                    def.name_of(&id)
                ));
            }
        }
        if let Some(d) = &v.date {
            if def
                .by_id(&d.field)
                .is_some_and(|f| !matches!(f.kind, Kind::Date | Kind::Datetime))
            {
                return Err(format!(
                    "view '{}': date.field '{}' is not a date or datetime",
                    v.name,
                    def.name_of(&d.field)
                ));
            }
            if let (Some(a), Some(b)) = (d.from_days, d.to_days)
                && a > b
            {
                return Err(format!(
                    "view '{}': date.from_days ({a}) is after to_days ({b})",
                    v.name
                ));
            }
        }
        if v.order_by.len() > model::MAX_ORDER_KEYS {
            return Err(format!(
                "view '{}' is ordered by at most {} fields",
                v.name,
                model::MAX_ORDER_KEYS
            ));
        }
        for (j, o) in v.order_by.iter().enumerate() {
            if v.order_by[..j].contains(o) {
                return Err(format!(
                    "view '{}': a field is listed twice in its order",
                    v.name
                ));
            }
        }
        if v.limit.is_some_and(|n| !(1..=MAX_LIMIT).contains(&n)) {
            return Err(format!("view '{}': limit is 1..={MAX_LIMIT} rows", v.name));
        }
    }
    Ok(())
}

/// The pod setting holding the last local day the dated views were served
/// for.
pub const DAY_SETTING: &str = "collections_views_day";

/// At `now` in `zone`: whether the local day differs from `last` (the
/// day the dated views were last told to reload), today, and the seconds
/// until the next local midnight.
pub fn day_changed(last: Option<&str>, zone: &Zone, now: i64) -> (bool, i64, i64) {
    let today = today(zone, now);
    let changed = last.and_then(|l| l.parse::<i64>().ok()) != Some(today);
    // The next midnight's offset may differ (daylight saving): find it by
    // stepping from tomorrow's midnight at today's offset.
    let off = i64::from(zone.offset_at(now)) * 60;
    let mut next = (today + 1) * 86_400 - off;
    let off2 = i64::from(zone.offset_at(next)) * 60;
    if off2 != off {
        next = (today + 1) * 86_400 - off2;
    }
    (changed, today, (next - now).max(1))
}

/// On the adapter's poll: when the pod's local day has changed since the
/// dated views were last told, tell clients to reload them (`event: data`
/// scoped to `collection:<c>:view` of each collection with one). Answers
/// how long until the next local midnight (`None`: no dated view).
pub async fn refresh_at_midnight(pod: &pod_core::Pod, now: i64) -> Option<std::time::Duration> {
    let found = pod
        .blocking(move |s| {
            let defs = s.with_conn(crate::db::all_defs)?;
            let dated: Vec<String> = defs
                .into_iter()
                .filter(|(_, d)| dated(d))
                .map(|(_, d)| d.name)
                .collect();
            if dated.is_empty() {
                return Ok(None);
            }
            let zone = s.with_conn(zone_on)?;
            let last = s.pod_setting(DAY_SETTING)?;
            let (changed, today, wait) = day_changed(last.as_deref(), &zone, now);
            if changed {
                s.set_pod_setting(DAY_SETTING, &today.to_string())?;
            }
            Ok(Some((changed.then_some(dated), wait)))
        })
        .await;
    let (publish, wait) = match found {
        Ok(Some(f)) => f,
        Ok(None) => return None,
        Err(e) => {
            tracing::error!(status = ?e.0, "dated views could not be read");
            return None;
        }
    };
    if let Some(names) = publish {
        let scope = pod_core::layouts::DataScope {
            uri_prefixes: Vec::new(),
            query_prefixes: names
                .iter()
                .map(|c| format!("{}:view", crate::db::query_key(c)))
                .collect(),
            detail: serde_json::Map::new(),
        };
        pod.publish_data_scoped(crate::NAME, scope, None);
    }
    // A little after midnight, so the day has turned.
    Some(std::time::Duration::from_secs((wait + 5) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    fn def() -> Definition {
        let fields = [
            json!({"name": "title", "type": "text"}),
            json!({"name": "done", "type": "bool", "default": false}),
            json!({"name": "start", "type": "datetime", "optional": true}),
            json!({"name": "due", "type": "date", "optional": true}),
            json!({"name": "points", "type": "number", "optional": true}),
        ];
        Definition {
            name: "plans".into(),
            title: "Plans".into(),
            fields: fields
                .iter()
                .enumerate()
                .map(|(i, s)| model::parse_field(s, format!("f{}", i + 1), 99).unwrap())
                .collect(),
            order_by: Vec::new(),
            descending: false,
            next_field: 6,
            reminders: Vec::new(),
            fillers: Vec::new(),
            alerts: Vec::new(),
            system: None,
            actions: Vec::new(),
            views: Vec::new(),
        }
    }

    fn row(id: &str, values: Value) -> Row {
        let values: Map<String, Value> = values.as_object().cloned().unwrap();
        Row {
            id: id.into(),
            client_id: None,
            values,
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            filler: None,
        }
    }

    fn ids(rows: &[&Row]) -> Vec<String> {
        rows.iter().map(|r| r.id.clone()).collect()
    }

    fn at(s: &str) -> i64 {
        s.parse::<pod_core::tz::jiff::Timestamp>()
            .unwrap()
            .as_second()
    }

    fn denver() -> Zone {
        Zone::Named(pod_core::tz::parse("America/Denver").unwrap())
    }

    #[test]
    fn where_compares_as_text() {
        let d = def();
        let v = parse(
            &d,
            &json!({"name": "open", "where": {"field": "done", "equals": false}}),
        )
        .unwrap();
        let rows = [
            row("a", json!({"f1": "Paint the fence", "f2": true})),
            row("b", json!({"f1": "Rake leaves", "f2": false})),
            // Empty: its default (false).
            row("c", json!({"f1": "Fix the gate"})),
        ];
        assert_eq!(ids(&v.select(&d, &rows, &Zone::Fixed(0), 0)), ["b", "c"]);
        let v = parse(
            &d,
            &json!({"name": "big", "where": [{"field": "points", "equals": 3}, {"field": "title", "equals": "Rake leaves"}]}),
        )
        .unwrap();
        let rows = [
            row("a", json!({"f1": "Rake leaves", "f5": 3})),
            row("b", json!({"f1": "Rake leaves", "f5": 2})),
        ];
        assert_eq!(ids(&v.select(&d, &rows, &Zone::Fixed(0), 0)), ["a"]);
    }

    #[test]
    fn date_windows_follow_the_pods_day() {
        let d = def();
        // 2026-11-17 22:00 in Denver is 2026-11-18 05:00 UTC.
        let now = at("2026-11-18T05:00:00Z");
        let rows = [
            row("yesterday", json!({"f4": "2026-11-16"})),
            row("today", json!({"f4": "2026-11-17"})),
            row("tomorrow", json!({"f4": "2026-11-18"})),
            row("week", json!({"f4": "2026-11-24"})),
            row("later", json!({"f4": "2026-11-25"})),
            row("none", json!({})),
        ];
        let window = |from: Value, to: Value| {
            parse(
                &d,
                &json!({"name": "w", "date": {"field": "due", "from_days": from, "to_days": to}}),
            )
            .unwrap()
        };
        let z = denver();
        assert_eq!(
            ids(&window(json!(0), json!(0)).select(&d, &rows, &z, now)),
            ["today"]
        );
        assert_eq!(
            ids(&window(json!(1), json!(7)).select(&d, &rows, &z, now)),
            ["tomorrow", "week"]
        );
        assert_eq!(
            ids(&window(json!(0), Value::Null).select(&d, &rows, &z, now)),
            ["today", "tomorrow", "week", "later"]
        );
        assert_eq!(
            ids(&window(Value::Null, json!(-1)).select(&d, &rows, &z, now)),
            ["yesterday"]
        );
        // In UTC it is already the 18th.
        assert_eq!(
            ids(&window(json!(0), json!(0)).select(&d, &rows, &Zone::Fixed(0), now)),
            ["tomorrow"]
        );
    }

    #[test]
    fn a_datetime_counts_by_its_local_date() {
        let d = def();
        let now = at("2026-11-17T18:00:00Z");
        let rows = [
            // 2026-11-18 03:00 UTC is the evening of the 17th in Denver.
            row("evening", json!({"f3": "2026-11-18T03:00Z"})),
            row("morning", json!({"f3": "2026-11-18T08:30-07:00"})),
            // No offset: the date it was written with.
            row("floating", json!({"f3": "2026-11-17T23:30"})),
            row("all_day", json!({"f3": "2026-11-18"})),
        ];
        let today = parse(
            &d,
            &json!({"name": "today", "date": {"field": "start", "from_days": 0, "to_days": 0}}),
        )
        .unwrap();
        assert_eq!(
            ids(&today.select(&d, &rows, &denver(), now)),
            ["evening", "floating"]
        );
        assert_eq!(
            ids(&today.select(&d, &rows, &Zone::Fixed(0), now)),
            ["floating"]
        );
    }

    #[test]
    fn order_descending_and_limit() {
        let d = def();
        let rows = [
            row("a", json!({"f3": "2026-11-20T10:00Z", "f5": 1})),
            row("b", json!({"f3": "2026-11-18T10:00Z", "f5": 5})),
            row("c", json!({"f3": "2026-11-19T10:00Z", "f5": 3})),
            row("d", json!({"f5": 2})),
        ];
        let now = at("2026-11-17T12:00:00Z");
        let upcoming = parse(
            &d,
            &json!({"name": "upcoming", "date": {"field": "start", "from_days": 0},
                    "order_by": "start", "limit": 2}),
        )
        .unwrap();
        assert_eq!(
            ids(&upcoming.select(&d, &rows, &Zone::Fixed(0), now)),
            ["b", "c"]
        );
        let top = parse(
            &d,
            &json!({"name": "top", "order_by": ["points"], "descending": true, "limit": 3}),
        )
        .unwrap();
        assert_eq!(
            ids(&top.select(&d, &rows, &Zone::Fixed(0), now)),
            ["b", "c", "d"]
        );
        // No order: the collection's own.
        let first = parse(&d, &json!({"name": "first", "limit": 1})).unwrap();
        assert_eq!(ids(&first.select(&d, &rows, &Zone::Fixed(0), now)), ["a"]);
    }

    #[test]
    fn bad_views_are_refused() {
        let d = def();
        let e = |spec: Value| parse(&d, &spec).unwrap_err();
        assert!(e(json!({"name": "Next 3"})).contains("must be [a-z]"));
        assert!(
            e(json!({"name": "x", "where": {"field": "colour", "equals": "red"}}))
                .contains("'colour' is not one of the fields")
        );
        assert!(e(json!({"name": "x", "where": {"field": "done"}})).contains("needs equals"));
        assert!(e(json!({"name": "x", "limit": 0})).contains("limit is 1..=200"));
        assert!(e(json!({"name": "x", "limit": 201})).contains("limit is 1..=200"));
        assert!(e(json!({"name": "x", "order_by": "nope"})).contains("order_by 'nope'"));
        assert!(e(json!({"name": "x", "date": {"field": "start"}})).contains("from_days, to_days"));
        assert!(
            e(json!({"name": "x", "date": {"field": "start", "from_days": 1.5}}))
                .contains("whole number")
        );
        let mut d2 = d.clone();
        d2.views.push(
            parse(
                &d,
                &json!({"name": "x", "date": {"field": "title", "from_days": 0}}),
            )
            .unwrap(),
        );
        assert!(check(&d2).unwrap_err().contains("not a date or datetime"));
        d2.views = vec![
            parse(
                &d,
                &json!({"name": "x", "date": {"field": "due", "from_days": 3, "to_days": 1}}),
            )
            .unwrap(),
        ];
        assert!(check(&d2).unwrap_err().contains("after to_days"));
        d2.views = vec![
            parse(&d, &json!({"name": "x"})).unwrap(),
            parse(&d, &json!({"name": "x"})).unwrap(),
        ];
        assert!(check(&d2).unwrap_err().contains("two views named 'x'"));
        d2.views = vec![parse(&d, &json!({"name": "x", "order_by": "points"})).unwrap()];
        d2.fields[4].removed_at = Some(1);
        assert!(check(&d2).unwrap_err().contains("which is removed"));
        let mut d3 = d.clone();
        for i in 0..MAX_VIEWS {
            set(
                &mut d3,
                parse(&d, &json!({"name": format!("v{i}")})).unwrap(),
            )
            .unwrap();
        }
        assert!(
            set(&mut d3, parse(&d, &json!({"name": "more"})).unwrap())
                .unwrap_err()
                .contains("at most 10 views")
        );
        // Replacing one by name is no new view.
        set(
            &mut d3,
            parse(&d, &json!({"name": "v0", "limit": 5})).unwrap(),
        )
        .unwrap();
        assert_eq!(d3.views[0].limit, Some(5));
        assert!(
            remove(&mut d3, "gone")
                .unwrap_err()
                .contains("no view 'gone'")
        );
    }

    #[test]
    fn views_are_described_by_name_and_stored_by_id() {
        let mut d = def();
        let v = parse(
            &d,
            &json!({"name": "upcoming", "where": {"field": "done", "equals": "false"},
                    "date": {"field": "start", "from_days": 0}, "order_by": "start", "limit": 3}),
        )
        .unwrap();
        assert_eq!(v.date.as_ref().unwrap().field, "f3");
        assert_eq!(v.order_by, ["f3"]);
        d.fields[2].name = "begins".into();
        d.views.push(v.clone());
        let text = v.describe(&d);
        assert_eq!(text["query_key"], "collection:plans:view:upcoming");
        assert_eq!(text["date"], json!({"field": "begins", "from_days": 0}));
        assert_eq!(text["order_by"], "begins");
        assert_eq!(text["where"], json!([{"field": "done", "equals": "false"}]));
        let stored = serde_json::to_value(&d).unwrap();
        let back: Definition = serde_json::from_value(stored).unwrap();
        assert_eq!(back.views, vec![v]);
        // A definition stored before views reads with none.
        let mut old = serde_json::to_value(def()).unwrap();
        assert!(old.get("views").is_none());
        old.as_object_mut().unwrap().remove("views");
        assert!(
            serde_json::from_value::<Definition>(old)
                .unwrap()
                .views
                .is_empty()
        );
    }

    #[test]
    fn the_day_changes_at_local_midnight() {
        let z = denver();
        let now = at("2026-11-18T06:59:00Z"); // 23:59 on the 17th in Denver
        let (changed, today, wait) = day_changed(None, &z, now);
        assert!(changed);
        assert_eq!(wait, 60);
        let (changed, _, _) = day_changed(Some(&today.to_string()), &z, now);
        assert!(!changed);
        let (changed, next, _) = day_changed(Some(&today.to_string()), &z, now + 61);
        assert!(changed);
        assert_eq!(next, today + 1);
        // Across the end of daylight saving (2026-11-01 02:00 MDT -> 01:00
        // MST): midnight of the 2nd is 07:00 UTC.
        let early = at("2026-11-01T06:30:00Z"); // 00:30 MDT
        let (_, _, wait) = day_changed(None, &z, early);
        assert_eq!(early + wait, at("2026-11-02T07:00:00Z"));
    }
}
