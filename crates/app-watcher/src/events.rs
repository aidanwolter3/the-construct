//! The Events app's data (decision `collection-fillers`): the system
//! collection `events`, which every pod has, filled by one fetch filler per
//! watched website and watched by alerts. This replaced the Watcher's own
//! tables, pipeline and rules (decision `watcher`): the website reading
//! became the `events` extractor, rules became alerts, and checks became
//! fills.
//!
//! [`ensure`] creates the collection; [`import`] moves a pod's Watcher sites
//! and rules into it once (`watcher.imported`), each site's first fill
//! recording the matches it finds without notifying again.

use pod_core::store::{Store, pod_setting_on};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};

use app_collections::model::{Definition, System};
use app_collections::{alerts, db, fill, filler, model};

use crate::legacy;

/// The collection's name.
pub const COLLECTION: &str = "events";
/// Pod setting: the Watcher's sites and rules were moved (`1`).
pub const IMPORTED_SETTING: &str = "watcher.imported";
/// The note of the import's definition version.
const IMPORT_NOTE: &str = "The Watcher's sites and rules";

/// The `events` definition a pod starts with.
pub fn definition() -> Definition {
    let specs = [
        json!({"name": "source_id", "type": "text", "optional": true}),
        json!({"name": "title", "type": "text", "optional": true}),
        json!({"name": "start", "type": "datetime", "optional": true}),
        json!({"name": "end", "type": "datetime", "optional": true}),
        json!({"name": "venue", "type": "text", "optional": true}),
        json!({"name": "description", "type": "text", "optional": true}),
        json!({"name": "url", "type": "text", "optional": true}),
        json!({"name": "tickets_url", "type": "text", "optional": true}),
        json!({"name": "seen", "type": "bool", "default": false}),
    ];
    let fields: Vec<model::Field> = specs
        .iter()
        .enumerate()
        .map(|(i, s)| model::parse_field(s, format!("f{}", i + 1), 4_000).expect("a valid field"))
        .collect();
    Definition {
        name: COLLECTION.into(),
        title: "Events".into(),
        next_field: fields.len() as u32 + 1,
        fields,
        order_by: vec!["f3".into()],
        descending: false,
        reminders: Vec::new(),
        fillers: Vec::new(),
        alerts: Vec::new(),
        system: Some(System {
            app: crate::APP_ID.into(),
            channel: crate::CHANNEL.into(),
            alert_title: "{title}".into(),
            alert_body: "{start_text} · {venue} — {alert}".into(),
        }),
    }
}

/// Create the `events` collection unless the pod has one.
pub fn ensure(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<()> {
    if db::latest_def(tx, COLLECTION)?.is_none() {
        db::append_def(
            tx,
            &definition(),
            "The Events app's events",
            pod_core::custom::AUTHOR_POD,
            now,
        )?;
    }
    Ok(())
}

/// A Watcher site as a filler spec: the `events` extractor (the Watcher's
/// `ics`, `jsonld` and `auto`) or `llm`, its zone, interval and details.
pub fn filler_spec(t: &legacy::Target) -> Value {
    let zone = t.time_zone.clone().unwrap_or_else(|| {
        t.utc_offset
            .map(web_extract::when::offset_text)
            .unwrap_or_default()
    });
    let llm = t.extractor == "llm"
        || (t.extractor == "auto" && t.last_extractor.as_deref() == Some("llm"));
    let every =
        (t.interval_secs / 60).clamp(i64::from(filler::MIN_EVERY), i64::from(filler::MAX_EVERY));
    let mut spec = json!({
        "kind": "fetch", "title": t.title, "url": t.url, "zone": zone,
        "upcoming": "start", "every_minutes": every,
    });
    if llm {
        spec["extract"] = json!(filler::LLM);
        spec["fields"] = json!(["title", "start", "end", "venue", "description", "url"]);
        spec["hint"] = json!(t.hint);
        return spec;
    }
    spec["extract"] = json!("events");
    spec["key"] = json!("source_id");
    let mut map = json!({"source_id": "id", "title": "title", "start": "start", "end": "end",
        "venue": "venue", "url": "url", "tickets_url": "tickets_url"});
    if t.details {
        spec["details"] = json!({"link": "url", "extract": "events",
            "map": {"description": "description"}, "text": "description"});
    } else {
        map["description"] = json!("description");
    }
    spec["map"] = map;
    spec
}

/// A Watcher rule as an alert spec (its site as `filler`; a venue becomes a
/// phrase every match must contain).
pub fn alert_spec(r: &legacy::Rule, filler_id: Option<&str>) -> Value {
    let mut all = r.spec.all.clone();
    if !r.spec.venue.trim().is_empty() {
        all.push(r.spec.venue.trim().to_owned());
    }
    let mut spec = json!({
        "name": r.title, "any": r.spec.any, "all": all, "none": r.spec.none,
        "days": r.spec.days, "date_field": "start",
    });
    if let Some(n) = r.spec.within_days {
        spec["within_days"] = json!(n);
    }
    if let Some(f) = filler_id {
        spec["filler"] = json!(f);
    }
    spec
}

/// Move the pod's Watcher sites and rules into `events` once: one filler
/// per live site (its first fill quiet, so matches already announced aren't
/// announced again) and one alert per live rule. Answers the fillers
/// added. A site or rule that no longer validates is skipped (logged).
pub fn import(
    tx: &Transaction<'_>,
    allow_private: bool,
    now: i64,
) -> rusqlite::Result<Vec<String>> {
    if pod_setting_on(tx, IMPORTED_SETTING)?.as_deref() == Some("1") {
        return Ok(Vec::new());
    }
    ensure(tx, now)?;
    let targets = legacy::targets(tx)?;
    let rules = legacy::rules(tx)?;
    let mut added = Vec::new();
    if !targets.is_empty() || rules.iter().any(|r| !r.archived) {
        let Some((_, mut def)) = db::latest_def(tx, COLLECTION)? else {
            return Ok(Vec::new());
        };
        // Old site id -> filler id.
        let mut ids: Vec<(String, String)> = Vec::new();
        for t in &targets {
            let id = db::new_id("s");
            match filler::parse(&def, &filler_spec(t), id.clone(), allow_private) {
                Ok(f) => {
                    def.fillers.push(f);
                    ids.push((t.id.clone(), id.clone()));
                    added.push(id);
                }
                Err(e) => {
                    tracing::warn!(site = %t.title, error = %e, "a Watcher site was not moved")
                }
            }
        }
        for r in rules.iter().filter(|r| !r.archived) {
            let site = r.spec.targets.first();
            let filler = site.and_then(|s| {
                ids.iter()
                    .find(|(old, _)| old == s)
                    .map(|(_, f)| f.as_str())
            });
            if site.is_some() && filler.is_none() {
                continue;
            }
            let mut one = def.clone();
            match alerts::add(&mut one, &alert_spec(r, filler), now) {
                Ok(_) => def = one,
                Err(e) => {
                    tracing::warn!(rule = %r.title, error = %e, "a Watcher rule was not moved")
                }
            }
        }
        if let Err(e) = model::check_definition(&def, usize::MAX) {
            tracing::error!(error = %e, "the Watcher's sites and rules could not be moved");
            return Ok(Vec::new());
        }
        db::append_def(tx, &def, IMPORT_NOTE, pod_core::custom::AUTHOR_POD, now)?;
        for f in &added {
            fill::save_state(
                tx,
                COLLECTION,
                f,
                &fill::State {
                    quiet_next: true,
                    ..fill::State::default()
                },
            )?;
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, '1')",
        params![IMPORTED_SETTING],
    )?;
    Ok(added)
}

/// [`ensure`] and [`import`] on `store`.
pub fn prepare(store: &Store, allow_private: bool) -> rusqlite::Result<Vec<String>> {
    let now = pod_core::util::now_secs();
    store.transaction(|tx| import(tx, allow_private, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_definition_is_valid_and_sites_and_rules_convert() {
        let def = definition();
        model::check_definition(&def, 40).unwrap();
        let site = legacy::Target {
            id: "t1".into(),
            title: "Improv Boulder".into(),
            url: "https://www.meetup.com/improv-boulder/events/".into(),
            extractor: "auto".into(),
            hint: String::new(),
            details: true,
            interval_secs: 6 * 3600,
            utc_offset: None,
            time_zone: Some("America/Denver".into()),
            archived: false,
            last_extractor: Some("jsonld".into()),
        };
        let f = filler::parse(&def, &filler_spec(&site), "s1".into(), false).unwrap();
        assert_eq!(f.title, "Improv Boulder");
        assert_eq!(f.every_minutes, Some(360));
        let mut with = def.clone();
        with.fillers.push(f);
        filler::check(&with, 20).unwrap();
        let rule = legacy::Rule {
            id: "r1".into(),
            title: "Budget Cuts".into(),
            spec: legacy::RuleSpec {
                any: vec!["Budget Cuts".into()],
                none: vec!["class".into()],
                venue: "The End".into(),
                days: vec!["fri".into()],
                targets: vec!["t1".into()],
                ..legacy::RuleSpec::default()
            },
            archived: false,
        };
        alerts::add(&mut with, &alert_spec(&rule, Some("s1")), 0).unwrap();
        let a = &with.alerts[0];
        assert_eq!(a.filler.as_deref(), Some("s1"));
        assert_eq!(a.all, vec!["The End".to_owned()]);
        assert_eq!(a.date_field.as_deref(), Some("f3"));
        model::check_definition(&with, 40).unwrap();
        let llm = legacy::Target {
            extractor: "llm".into(),
            hint: "shows only".into(),
            details: false,
            utc_offset: Some(-420),
            time_zone: None,
            ..site
        };
        let spec = filler_spec(&llm);
        assert_eq!(
            (spec["extract"].clone(), spec["zone"].clone()),
            (json!("llm"), json!("-07:00"))
        );
        assert!(
            filler::parse(&def, &spec, "s2".into(), false)
                .unwrap()
                .is_llm()
        );
    }
}
