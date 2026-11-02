//! Layout storage measurements (D26): bytes per version for the built-in
//! layouts, raw vs zstd, and VACUUM'd SQLite file sizes for N agent-edit-like
//! versions plus N restores under three schemes. Run:
//! `cargo run -p server --release --example layout_storage [N]`.

use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use server::layouts;
use server::store::{LayoutMeta, SOURCE_AGENT, SOURCE_RESTORE, Store};

fn file_size(path: &Path) -> u64 {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
        .unwrap();
    drop(conn);
    std::fs::metadata(path).unwrap().len()
}

/// The `i`-th distinct agent-edit-like variant of the classic feed: what the
/// rules provider produces (caption hidden/shown, media moved, weather slot),
/// plus a unique title so no two are equal.
fn edited(i: usize) -> (String, String) {
    let mut doc: Value = serde_json::from_str(
        &layouts::catalog()
            .iter()
            .find(|s| s.id == "pixelfed_home.classic")
            .unwrap()
            .json,
    )
    .unwrap();
    doc["title"] = json!(format!("Pixelfed {i}"));
    let card = &mut doc["slots"][0]["content"]["item_template"];
    if i.is_multiple_of(2) {
        card["caption_slot"] =
            json!({ "type": "core://Text", "value_expr": "", "style": "Caption" });
    }
    if i.is_multiple_of(3) {
        let a = card["author_slot"].take();
        let m = card["media_slot"].take();
        card["author_slot"] = m;
        card["media_slot"] = a;
    }
    if i.is_multiple_of(5) {
        let weather = json!({ "slot_id": "weather", "provider_uri": "core://placeholder",
            "content": server::agent::rules::weather_content() });
        doc["slots"].as_array_mut().unwrap().insert(0, weather);
    }
    let node: protocol::UiNode = serde_json::from_value(doc).unwrap();
    let patch = json!([
        { "op": "replace", "path": "/title", "value": format!("Pixelfed {i}") },
        { "op": "replace", "path": "/slots/0/content/item_template/caption_slot",
          "value": { "type": "core://Text", "value_expr": "", "style": "Caption" } }
    ])
    .to_string();
    (serde_json::to_string(&node).unwrap(), patch)
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1000);
    let dir = tempfile::tempdir().unwrap();

    println!("## Built-in layouts (bytes)\n");
    println!("| layout | raw JSON | zstd-3 | zstd-19 | stored (v6) |");
    println!("|---|---:|---:|---:|---:|");
    for s in layouts::catalog() {
        let raw = s.json.len();
        let z3 = zstd::bulk::compress(s.json.as_bytes(), 3).unwrap().len();
        let z19 = zstd::bulk::compress(s.json.as_bytes(), 19).unwrap().len();
        let stored = server::store::encode_blob(&s.json).1.len();
        println!("| {} | {raw} | {z3} | {z19} | {stored} |", s.id);
    }
    let (sample, patch) = edited(10);
    let pz = zstd::bulk::compress(patch.as_bytes(), 19).unwrap().len();
    // Not used by the store (it would freeze the dictionary into the format):
    // a raw-content dictionary of the built-ins, for comparison.
    let dict: Vec<u8> = layouts::catalog()
        .iter()
        .flat_map(|s| s.json.bytes())
        .collect();
    let mut with_dict = zstd::bulk::Compressor::with_dictionary(19, &dict).unwrap();
    println!(
        "\nwith a built-ins dictionary ({} B): agent-edited variant zstd-19 {} B",
        dict.len(),
        with_dict.compress(sample.as_bytes()).unwrap().len()
    );
    println!(
        "\nagent-edited variant: raw {} B, zstd-19 {} B; its 2-op patch: raw {} B, zstd-19 {pz} B\n",
        sample.len(),
        server::store::encode_blob(&sample).1.len(),
        patch.len()
    );

    // Scheme A: one row per version with the raw JSON text (the v5 table).
    let a_path = dir.path().join("a.db");
    let a_empty;
    {
        let conn = Connection::open(&a_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE layout_versions (id INTEGER PRIMARY KEY, workspace_id TEXT NOT NULL,
             version INTEGER NOT NULL, json TEXT NOT NULL, prompt TEXT, provider TEXT,
             patch_json TEXT, created_at INTEGER NOT NULL, UNIQUE (workspace_id, version));",
        )
        .unwrap();
        drop(conn);
        a_empty = file_size(&a_path);
        let mut conn = Connection::open(&a_path).unwrap();
        let tx = conn.transaction().unwrap();
        let (first, _) = edited(0);
        for v in 0..2 * n {
            let (json, patch, prompt) = if v < n {
                let (j, p) = edited(v);
                (j, Some(p), "hide captions".to_owned())
            } else {
                (first.clone(), None, format!("rollback to v{}", v % 7 + 1))
            };
            tx.execute(
                "INSERT INTO layout_versions (workspace_id, version, json, prompt, provider, patch_json, created_at)
                 VALUES ('pixelfed_home', ?1, ?2, ?3, 'rules', ?4, 1700000000)",
                params![v as i64 + 1, json, prompt, patch],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let a_size = file_size(&a_path);

    // Scheme B: dedupe without compression (blob table keyed by hash, raw data).
    let b_path = dir.path().join("b.db");
    let b_empty;
    {
        let conn = Connection::open(&b_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (hash TEXT PRIMARY KEY NOT NULL, data BLOB NOT NULL);
             CREATE TABLE versions (layout_id TEXT NOT NULL, version INTEGER NOT NULL,
               hash TEXT NOT NULL, source TEXT NOT NULL, prompt TEXT, provider TEXT,
               patch_json TEXT, created_at INTEGER NOT NULL, PRIMARY KEY (layout_id, version)) WITHOUT ROWID;",
        )
        .unwrap();
        drop(conn);
        b_empty = file_size(&b_path);
        let mut conn = Connection::open(&b_path).unwrap();
        let tx = conn.transaction().unwrap();
        let (first, _) = edited(0);
        for v in 0..2 * n {
            let (json, patch) = if v < n {
                let (j, p) = edited(v);
                (j, Some(p))
            } else {
                (first.clone(), None)
            };
            let hash = server::store::content_hash(&json);
            tx.execute(
                "INSERT OR IGNORE INTO blobs (hash, data) VALUES (?1, ?2)",
                params![hash, json.as_bytes()],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO versions VALUES ('pixelfed_home.classic', ?1, ?2, 'agent', 'hide captions', 'rules', ?3, 1700000000)",
                params![v as i64 + 1, hash, patch],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let b_size = file_size(&b_path);

    // Scheme C: the real v6 store (dedupe + zstd-19), N edits then N restores.
    let c_path = dir.path().join("server.db");
    let c_seeded;
    let after_edits;
    {
        let store = Store::open(&c_path).unwrap();
        layouts::ensure_seeded(&store).unwrap();
        drop(store);
        c_seeded = file_size(&c_path);
        let store = Store::open(&c_path).unwrap();
        let started = std::time::Instant::now();
        for v in 0..n {
            let (json, patch) = edited(v);
            let meta = LayoutMeta {
                prompt: Some("hide captions".into()),
                provider: Some("rules".into()),
                patch_json: Some(patch),
            };
            store
                .append_active("pixelfed_home", None, &json, SOURCE_AGENT, &meta)
                .unwrap()
                .unwrap();
        }
        let edit_ms = started.elapsed().as_secs_f64() * 1000.0 / n as f64;
        drop(store);
        after_edits = file_size(&c_path);
        println!(
            "v6 phases: seeded file {c_seeded} B; {:.0} B per distinct edit",
            (after_edits - c_seeded) as f64 / n as f64
        );
        let store = Store::open(&c_path).unwrap();
        for v in 0..n {
            let meta = LayoutMeta {
                prompt: Some(format!("restore v{}", v % 7 + 1)),
                ..Default::default()
            };
            store
                .restore_version(
                    "pixelfed_home.classic",
                    (v % 7 + 1) as i64,
                    SOURCE_RESTORE,
                    &meta,
                    false,
                )
                .unwrap();
        }
        let (blobs, blob_bytes, versions) = store.layout_storage().unwrap();
        println!(
            "v6 store: {versions} versions, {blobs} blobs, {blob_bytes} B of blob data; {edit_ms:.2} ms per edit (incl. zstd-19)\n"
        );
    }
    let c_size = file_size(&c_path);
    println!(
        "v6 phases: {:.0} B per restore (version row + revision row, no blob)\n",
        (c_size - after_edits) as f64 / n as f64
    );

    let per = |total: u64, base: u64| (total.saturating_sub(base)) as f64 / (2 * n) as f64;
    println!("## {n} distinct edits + {n} restores, VACUUM'd file size\n");
    println!(
        "| scheme | file | bytes/version (avg of {}) | 1k versions | 100k versions |",
        2 * n
    );
    println!("|---|---:|---:|---:|---:|");
    for (name, total, base) in [
        ("A: raw JSON per row (v5)", a_size, a_empty),
        ("B: dedupe, raw blobs", b_size, b_empty),
        ("C: dedupe + zstd-19 (v6)", c_size, c_seeded),
    ] {
        let p = per(total, base);
        println!(
            "| {name} | {total} B | {p:.0} | {:.0} KB | {:.1} MB |",
            p * 1000.0 / 1024.0,
            p * 100_000.0 / 1024.0 / 1024.0
        );
    }
}
