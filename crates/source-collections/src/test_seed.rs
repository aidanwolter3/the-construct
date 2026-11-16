//! A made-up `dishes` collection for other crates' tests (the `test-seed`
//! feature, enabled as a dev-dependency), so the pod binary never holds it:
//! eight rows with fixed ids (`collections://dishes/test-garden-salad`, ...),
//! listed by the feed `["collections", "dishes"]` (query key
//! `collection:dishes`).

use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::db::{self, Row};
use crate::model::{self, Definition};

/// The seed file (`tests/data/test-seed.json`).
#[cfg(any(test, feature = "test-seed"))]
pub static TEST_SEED: &str = include_str!("../tests/data/test-seed.json");
/// The seeded collection's name.
pub const COLLECTION: &str = "dishes";

/// Define [`COLLECTION`] with its rows, unless the pod already has it.
pub fn seed_test_dishes(conn: &Connection) -> rusqlite::Result<()> {
    if db::latest_def(conn, COLLECTION)?.is_some() {
        return Ok(());
    }
    let seed: Value = serde_json::from_str(TEST_SEED).expect("the test seed is JSON");
    let fields = seed["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .enumerate()
        .map(|(i, f)| model::parse_field(f, format!("f{}", i + 1), 10_000).expect("a field"))
        .collect::<Vec<_>>();
    let def = Definition {
        name: COLLECTION.to_owned(),
        title: seed["title"].as_str().unwrap_or("Dishes").to_owned(),
        next_field: fields.len() as u32 + 1,
        fields,
        reminders: Vec::new(),
        fillers: Vec::new(),
        alerts: Vec::new(),
        system: None,
        actions: Vec::new(),
        order_by: Vec::new(),
        descending: false,
        keep: None,
    };
    let now = pod_core::util::now_secs();
    let tx = conn.unchecked_transaction()?;
    db::append_def(&tx, &def, "Test dishes", "user", now)?;
    for (i, r) in seed["rows"].as_array().expect("rows").iter().enumerate() {
        let mut values = Map::new();
        let given = r["values"].as_object().cloned().unwrap_or_default();
        model::apply_write(&def, &mut values, &given, model::Write::Create, 10_000)
            .expect("seed values fit the fields");
        let at = now + i as i64;
        db::insert_row(
            &tx,
            COLLECTION,
            &Row {
                id: r["id"].as_str().expect("an id").to_owned(),
                client_id: None,
                values,
                created_at: at,
                updated_at: at,
                deleted_at: None,
                filler: None,
            },
        )?;
    }
    tx.commit()
}
