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

use app_collections::db;
use app_collections::model::{self, Definition, System};

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
    }
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

/// Create the `events` collection unless the pod has one, and record it
/// under the app's current id and channel.
pub fn ensure(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<()> {
    let Some((_, mut def)) = db::latest_def(tx, COLLECTION)? else {
        db::append_def(
            tx,
            &definition(),
            "The Events app's events",
            pod_core::custom::AUTHOR_POD,
            now,
        )?;
        return Ok(());
    };
    let want = system();
    let current = def
        .system
        .as_ref()
        .map(|s| (s.app.as_str(), s.channel.as_str()));
    if current != Some((want.app.as_str(), want.channel.as_str())) {
        let old = def.system.take();
        def.system = Some(System {
            alert_title: old
                .as_ref()
                .map_or(want.alert_title.clone(), |s| s.alert_title.clone()),
            alert_body: old.map_or(want.alert_body.clone(), |s| s.alert_body),
            ..want
        });
        db::append_def(
            tx,
            &def,
            "The Events app's new id",
            pod_core::custom::AUTHOR_POD,
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
}
