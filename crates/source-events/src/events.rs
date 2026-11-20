//! The Events app's data (decision `collection-fillers`): the system
//! collection `events`, which every pod has, filled by one fetch filler per
//! watched website and watched by alerts.
//!
//! [`ensure`] creates the collection, and keeps the app it belongs to
//! current: a pod made before the app was renamed (decision
//! `rename-apps-and-folders`) has `events` recorded under the old app id.

use pod_core::store::Store;
use rusqlite::Transaction;
use serde_json::json;

use source_collections::db;
use source_collections::model::{self, Definition, System};

/// The collection's name.
pub const COLLECTION: &str = "events";

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
        going_spec(),
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
        system: Some(system()),
        actions: Vec::new(),
        views: Vec::new(),
        keep: None,
    }
}

/// The user's own "I'm going" on an event: a toggle no filler writes.
const GOING: &str = "going";

fn going_spec() -> serde_json::Value {
    json!({"name": GOING, "type": "bool", "default": false})
}

/// Add `going` to a pod's `events` from before the field shipped, unless it
/// has (or removed) a field of that name; its rows read false until set.
fn add_going(def: &mut Definition) -> bool {
    if def.field(GOING).is_some() || def.is_removed_name(GOING) {
        return false;
    }
    let id = format!("f{}", def.next_field);
    def.fields
        .push(model::parse_field(&going_spec(), id, 4_000).expect("a valid field"));
    def.next_field += 1;
    true
}

/// What makes `events` the Events app's: its app and notification channel.
fn system() -> System {
    System {
        app: crate::APP_ID.into(),
        channel: crate::CHANNEL.into(),
        alert_title: "{title}".into(),
        alert_body: "{start_text} · {venue} — {alert}".into(),
    }
}

/// Create the `events` collection unless the pod has one, record it under
/// the app's current id and channel, and add the fields it gained since.
pub fn ensure(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<()> {
    let Some((_, mut def)) = db::latest_def(tx, COLLECTION)? else {
        db::append_def(
            tx,
            &definition(),
            "The Events app's events",
            pod_core::layouts::AUTHOR_POD,
            now,
        )?;
        return Ok(());
    };
    let want = system();
    let current = def
        .system
        .as_ref()
        .map(|s| (s.app.as_str(), s.channel.as_str()));
    let mut notes = Vec::new();
    if current != Some((want.app.as_str(), want.channel.as_str())) {
        let old = def.system.take();
        def.system = Some(System {
            alert_title: old
                .as_ref()
                .map_or(want.alert_title.clone(), |s| s.alert_title.clone()),
            alert_body: old.map_or(want.alert_body.clone(), |s| s.alert_body),
            ..want
        });
        notes.push("The Events app's new id");
    }
    if add_going(&mut def) {
        notes.push("The Events app's going field");
    }
    if !notes.is_empty() {
        db::append_def(
            tx,
            &def,
            &notes.join("; "),
            pod_core::layouts::AUTHOR_POD,
            now,
        )?;
    }
    Ok(())
}

/// [`ensure`] on `store`.
pub fn prepare(store: &Store) -> rusqlite::Result<()> {
    let now = pod_core::util::now_secs();
    store.transaction(|tx| ensure(tx, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_definition_is_valid() {
        model::check_definition(&definition(), 40).unwrap();
    }

    /// A pod made before `going` shipped gets it when it opens, once, and
    /// keeps a field of that name it already has.
    #[test]
    fn an_older_pod_gains_going() {
        let mut def = definition();
        def.fields.pop();
        def.next_field -= 1;
        assert!(def.field(GOING).is_none());
        assert!(add_going(&mut def));
        assert_eq!(def, definition());
        assert!(!add_going(&mut def));
        model::check_definition(&def, 40).unwrap();
    }
}
