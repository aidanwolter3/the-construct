//! The old layout system's end (issue #300): a pod still on it is
//! converted to the construct when it opens (a dry run compared and
//! logged, then one transaction), a fresh pod starts with today's home,
//! and the named migration then drops the old tables. The fixtures in
//! `tests/data/migrations/` were made with the build before the old
//! system was removed: `old_pod.sql` a pod with layouts (links, embeds, a
//! cycle, `pod:` components, a pinned version, a blueprint part), and what
//! that build's switch made of it and of a fresh pod (`*_switched.json`,
//! `switched_pod.sql`).

use std::collections::BTreeMap;
use std::path::Path;

use pod_core::construct::store as cstore;
use serde_json::{Map, Value, json};
use server::AppState;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/migrations")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json_fixture(name: &str) -> Map<String, Value> {
    serde_json::from_str(&fixture(name)).unwrap()
}

/// A pod directory whose `server.db` is the fixtures `sql`, loaded in order.
fn pod_dir(sql: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
    for f in sql {
        conn.execute_batch(&fixture(f)).unwrap();
    }
    dir
}

fn tables(state: &AppState) -> Vec<String> {
    state
        .store()
        .with_conn(|c| {
            c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap()
}

const OLD_TABLES: [&str; 7] = [
    "custom_apps",
    "app_versions",
    "layout_blobs",
    "components",
    "layouts",
    "layout_versions",
    "workspace_revisions",
];

fn setting(state: &AppState, key: &str) -> Option<String> {
    state
        .store()
        .with_conn(|c| {
            use rusqlite::OptionalExtension;
            c.query_row(
                "SELECT value FROM pod_settings WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()
        })
        .unwrap()
}

/// An element without what this build's hashes decide: every reference's
/// `el` (the element it is sealed to) and the derived `needs`.
fn stripped(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(stripped).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| k.as_str() != "needs")
                .map(|(k, x)| {
                    let x = if k == "ref" {
                        match x {
                            Value::Object(r) => Value::Object(
                                r.iter()
                                    .filter(|(k, _)| *k != "el")
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect(),
                            ),
                            other => other.clone(),
                        }
                    } else {
                        stripped(x)
                    };
                    (k.clone(), x)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Every name of the pod: its element (stripped) and its history (author,
/// note, seq); for a layout, its element materialized and its edges' keys.
fn snapshot(state: &AppState) -> BTreeMap<String, Value> {
    state
        .store()
        .with_conn(|c| {
            let mut out = BTreeMap::new();
            for (name, hash) in cstore::names(c)? {
                let (el, _) = cstore::object(c, &hash)?.expect("a name's element is stored");
                let history: Vec<Value> = cstore::history(c, &name)?
                    .iter()
                    .map(|e| json!({ "author": e.author, "note": e.note, "seq": e.seq }))
                    .collect();
                let mut row = json!({ "element": stripped(&el), "history": history, "hash": hash });
                if !name.starts_with("@core/") && !name.starts_with("@me/") {
                    let objects = pod_core::construct::write::ConnObjects::new(c);
                    let m = construct_elements::materialize(&objects, &hash).unwrap();
                    row["materialized"] = m.node;
                    let mut edges: Vec<String> = m.edges.iter().map(|e| e.key.clone()).collect();
                    edges.sort();
                    edges.dedup();
                    row["edges"] = json!(edges);
                }
                out.insert(name, row);
            }
            Ok::<_, rusqlite::Error>(out)
        })
        .unwrap()
}

/// The distinct strings of a JSON array, sorted.
fn set(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// An old pod opens on the construct: the dry run compared every layout
/// and found what draws differently, and the pod's names are what the
/// old build's switch made of it.
#[test]
fn an_old_pod_is_converted_when_it_opens() {
    let dir = pod_dir(&["old_pod.sql"]);
    let state = server::open_dir(dir.path()).unwrap();

    // The dry run: every layout compared, the two that draw differently
    // (a link back to a layout that leads here is now a pop_to, and the
    // launcher listed the device's layouts).
    let report = state.conversion().expect("the pod was converted");
    let want = json_fixture("old_pod_dry_run.json");
    let compared: Vec<&str> = report.layouts.iter().map(|c| c.id.as_str()).collect();
    let mut want_ids: Vec<&str> = want["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    let mut got_ids = compared.clone();
    want_ids.sort();
    got_ids.sort();
    assert_eq!(got_ids, want_ids);
    let mut different: Vec<&str> = report.different().map(|c| c.id.as_str()).collect();
    different.sort();
    let mut want_different: Vec<&str> = want["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["same"] == false)
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    want_different.sort();
    assert_eq!(different, want_different, "{report:#?}");
    assert_eq!(different, ["c-jyiz6is4ld", "c-u4zg5a4d4t"]);

    // Switched, the old tables and settings gone.
    assert_eq!(setting(&state, "construct.switched").as_deref(), Some("1"));
    let now = tables(&state);
    for t in OLD_TABLES {
        assert!(!now.contains(&t.to_owned()), "{t} is dropped: {now:?}");
    }
    for key in [
        "apps.hidden",
        "updates.dismissed",
        "blueprints.convert",
        "layouts.convert",
    ] {
        assert_eq!(setting(&state, key), None, "{key}");
    }

    // The pod's own names are what the old build made of them.
    let got = snapshot(&state);
    let want = json_fixture("old_pod_switched.json");
    for (name, w) in &want {
        let g = got.get(name).unwrap_or_else(|| panic!("no name {name}"));
        if name.starts_with("@core/") {
            continue;
        }
        assert_eq!(g["element"], w["element"], "{name}'s element");
        // The history the old build recorded, then (when a blueprint it
        // uses changed in this build: settings) the pod's re-seal.
        let (g_hist, w_hist) = (
            g["history"].as_array().unwrap(),
            w["history"].as_array().unwrap(),
        );
        let extra = g_hist.len() - w_hist.len();
        assert_eq!(&g_hist[extra..], &w_hist[..], "{name}'s history");
        for e in &g_hist[..extra] {
            assert_eq!(e["author"], "pod", "{name}: {e}");
            assert!(
                e["note"].as_str().unwrap().starts_with("Re-sealed"),
                "{name}: {e}"
            );
        }
        assert_eq!(
            g["materialized"], w["materialized"],
            "{name} draws the same"
        );
        assert_eq!(set(&g["edges"]), set(&w["edges"]), "{name}'s edges");
    }
    let mine = |m: &BTreeMap<String, Value>| -> Vec<String> {
        m.keys()
            .filter(|n| !n.starts_with("@core/"))
            .cloned()
            .collect()
    };
    let want_names: Vec<String> = want
        .keys()
        .filter(|n| !n.starts_with("@core/"))
        .cloned()
        .collect();
    assert_eq!(mine(&got), want_names);
    // Every @core/ name the old build had is there, at this build's files.
    let core = pod_core::construct::core::load(&server::catalog()).unwrap();
    for name in want.keys().filter(|n| n.starts_with("@core/")) {
        assert_eq!(
            got[name]["hash"].as_str(),
            core.current(name),
            "{name} is this build's"
        );
    }

    // Opening again changes nothing.
    let generation = setting(&state, "construct.generation");
    drop(state);
    let again = server::open_dir(dir.path()).unwrap();
    assert!(again.conversion().is_none(), "converted once");
    assert_eq!(snapshot(&again), got);
    assert_eq!(setting(&again, "construct.generation"), generation);
}

/// A pod the old build had already switched keeps its names and their
/// history; the old tables it still had are dropped.
#[test]
fn an_already_switched_pod_keeps_its_names() {
    let dir = pod_dir(&["old_pod.sql", "switched_pod.sql"]);
    let before: Vec<(String, i64)> = {
        let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
        let mut stmt = conn
            .prepare("SELECT name, COUNT(*) FROM name_history GROUP BY name ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(before.len() > 10, "{before:?}");
    let state = server::open_dir(dir.path()).unwrap();
    assert!(state.conversion().is_none(), "already switched");
    let now = tables(&state);
    for t in OLD_TABLES {
        assert!(!now.contains(&t.to_owned()), "{t} is dropped: {now:?}");
    }
    let got = snapshot(&state);
    let want = json_fixture("old_pod_switched.json");
    for (name, recorded) in &before {
        let g = got.get(name).unwrap_or_else(|| panic!("{name} is kept"));
        let n = g["history"].as_array().unwrap().len() as i64;
        assert!(n >= *recorded, "{name}: {n} entries, had {recorded}");
        if let Some(w) = want.get(name)
            && !name.starts_with("@core/")
        {
            assert_eq!(g["element"], w["element"], "{name}'s element");
            assert_eq!(
                g["materialized"], w["materialized"],
                "{name} draws the same"
            );
        }
    }
}

/// A fresh pod starts on the construct with today's home (a use of the
/// home blueprint), without the old tables.
#[test]
fn a_fresh_pod_starts_with_todays_home() {
    let dir = tempfile::tempdir().unwrap();
    let state = server::open_dir(dir.path()).unwrap();
    assert!(state.conversion().is_none());
    assert_eq!(setting(&state, "construct.switched").as_deref(), Some("1"));
    let now = tables(&state);
    for t in OLD_TABLES {
        assert!(!now.contains(&t.to_owned()), "{t}: {now:?}");
    }
    let got = snapshot(&state);
    let want = json_fixture("fresh_pod_switched.json");
    assert_eq!(got["home"]["element"], want["home"]["element"]);
    assert_eq!(got["home"]["materialized"], want["home"]["materialized"]);
    assert_eq!(set(&got["home"]["edges"]), set(&want["home"]["edges"]));
    let names: Vec<&String> = got.keys().collect();
    let want_names: Vec<&String> = want.keys().collect();
    for n in &want_names {
        assert!(names.contains(n), "{n} in {names:?}");
    }
    drop(state);
    let again = server::open_dir(dir.path()).unwrap();
    assert_eq!(snapshot(&again), got, "opening again changes nothing");
}

/// A conversion that fails leaves the pod as it was: it refuses to open
/// (the host answers 503 for it) and keeps the old tables, so the next
/// build can try again.
#[test]
fn a_failed_conversion_keeps_the_old_pod() {
    let dir = pod_dir(&["old_pod.sql"]);
    {
        // A layout whose stored version is gone.
        let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DELETE FROM layout_blobs WHERE hash IN
                 (SELECT hash FROM app_versions WHERE app_id = 'c-r7jeifhpzm');",
        )
        .unwrap();
    }
    let err = match server::open_dir(dir.path()) {
        Ok(_) => panic!("the pod opened"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("c-r7jeifhpzm") || err.contains("layout"),
        "{err}"
    );
    let conn = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
    let kept: i64 = conn
        .query_row("SELECT COUNT(*) FROM custom_apps", [], |r| r.get(0))
        .unwrap();
    assert!(kept > 0);
    let switched: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pod_settings WHERE key = 'construct.switched'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(switched, 0);
}

/// `core://Ref` belonged to the old system: a write that uses it is
/// refused (only the one-time conversion reads it).
#[tokio::test]
async fn a_layout_may_not_use_the_old_blueprint_reference() {
    let pod = std::sync::Arc::new(server::state_in_memory());
    let r = pod_core::construct::write::mcp(
        &pod,
        "create_layout",
        &json!({"title": "Old", "layout": {"type": "core://Page", "title": "Old",
            "root": {"type": "core://Ref", "app": "recipes", "part": "layout"}}}),
    )
    .await
    .unwrap();
    let e = r.expect_err("refused");
    assert!(e.to_string().contains("core://Ref"), "{e}");
}
