//! The Pixelfed apps' layout files (`layouts/pixelfed_*`), checked with the
//! core's parser and sync, plus the generic file rules exercised on them.

use std::sync::Arc;

use app_pixelfed::{PixelfedAdapter, PixelfedConfig, apps};
use pod_core::layouts::*;
use pod_core::store::{Store, SyncReport};
use pod_core::{AppAdapter, Catalog};
use serde_json::Value;

/// The Pixelfed adapters, as the `server` binary registers them.
fn catalog() -> Catalog {
    let adapters: Vec<Arc<dyn AppAdapter>> = vec![
        Arc::new(PixelfedAdapter::new(PixelfedConfig::default())),
        apps::demo_adapter(),
    ];
    Catalog::new(adapters).unwrap()
}

fn files() -> Vec<LayoutSource> {
    apps::files()
}
#[test]
fn every_file_in_layouts_is_embedded() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../layouts");
    let mut on_disk = Vec::new();
    for app_dir in std::fs::read_dir(&root).unwrap() {
        let app_dir = app_dir.unwrap().path();
        let app_id = app_dir.file_name().unwrap().to_str().unwrap().to_owned();
        // Other adapters' apps are checked by their own crates.
        if !app_id.starts_with("pixelfed_") {
            continue;
        }
        for f in std::fs::read_dir(&app_dir).unwrap() {
            let name = f.unwrap().file_name().into_string().unwrap();
            on_disk.push(format!("{app_id}/{name}"));
        }
    }
    on_disk.sort();
    let mut embedded: Vec<String> = files()
        .iter()
        .map(|f| format!("{}/{}.json", f.app_id, f.name))
        .collect();
    embedded.sort();
    assert_eq!(
        on_disk, embedded,
        "every file in layouts/pixelfed_* must be listed in app_pixelfed::apps (and vice versa)"
    );
    for f in files() {
        let text =
            std::fs::read_to_string(root.join(f.app_id).join(format!("{}.json", f.name))).unwrap();
        assert_eq!(text, f.text);
    }
}

#[test]
fn every_file_parses_and_validates() {
    let files = parse_files(&catalog(), &files()).unwrap();
    for a in catalog().apps() {
        assert!(
            files.iter().any(|f| f.app_id == a.id && f.number == 1),
            "{} has a 0001.json",
            a.id
        );
    }
}

#[test]
fn first_files_are_the_original_builtin_layouts() {
    let files = parse_files(&catalog(), &files()).unwrap();
    for (app_id, note) in [
        ("pixelfed_home", "Classic feed"),
        ("pixelfed_demo", "Classic feed"),
    ] {
        let f = files
            .iter()
            .find(|f| f.app_id == app_id && f.number == 1)
            .unwrap();
        assert_eq!(
            Some(f.layout.clone()),
            builtin(&catalog(), app_id),
            "{app_id}"
        );
        assert_eq!(f.note, note);
    }
}

fn src(app_id: &'static str, name: &'static str, text: &'static str) -> LayoutSource {
    LayoutSource { app_id, name, text }
}

/// A small valid layout file for `pixelfed_demo` (schema 1: a title and
/// one action), the base the validation tests mutate.
const MINIMAL: &str = r#"{
  "note": "Minimal",
  "schema": 1,
  "layout": {
"type": "core://Workspace",
"id": "pixelfed_demo",
"title": "Demo",
"slots": [
  {
    "slot_id": "main",
    "provider_uri": "core://test",
    "content": {
      "type": "core://Container",
      "orientation": "Vertical",
      "padding_dp": 16,
      "children": [
        {
          "type": "core://Text",
          "style": "Title",
          "value_expr": "Minimal demo"
        },
        {
          "type": "core://Button",
          "label": "Ack",
          "on_click": {
            "intent": "action.mutation.optimistic",
            "endpoint": "/v1/demo/ack",
            "entity_uri": "core://test/x",
            "field": "n",
            "payload": {},
            "rollback_value": 0,
            "value_expr": "0"
          }
        }
      ]
    }
  }
]
  }
}"#;

#[test]
fn invalid_files_are_refused_with_their_path() {
    let ok = MINIMAL;
    assert_eq!(
        (files()[1].app_id, files()[1].name),
        ("pixelfed_home", "0002")
    );
    let cases: [(LayoutSource, &str); 12] = [
        (
            src("pixelfed_demo", "0001", "{"),
            "layouts/pixelfed_demo/0001.json: not valid JSON",
        ),
        (
            src("pixelfed_demo", "1", ok),
            "layouts/pixelfed_demo/1.json: file names are 4-digit",
        ),
        (
            src("nope", "0001", ok),
            "layouts/nope/0001.json: 'nope' is not an app",
        ),
        (
            src("pixelfed_demo", "0001", r#"{"layout": {}}"#),
            "\"note\" must be a non-empty string",
        ),
        (
            src("pixelfed_demo", "0001", r#"{"note": "x"}"#),
            "\"layout\" is missing",
        ),
        (
            src(
                "pixelfed_demo",
                "0001",
                r#"{"note": "x", "layout": {}, "extra": 1}"#,
            ),
            "unknown key 'extra'",
        ),
        (
            src(
                "pixelfed_demo",
                "0001",
                r#"{"note": "x", "layout": {"type": "core://Text", "value_expr": "", "style": "Body"}}"#,
            ),
            "layouts/pixelfed_demo/0001.json: invalid layout: root must stay a core://Workspace",
        ),
        (
            src("pixelfed_home", "0001", ok),
            "invalid layout: workspace id changed from 'pixelfed_home' to 'pixelfed_demo'",
        ),
        (
            src(
                "pixelfed_demo",
                "0001",
                leak(ok.replacen("  \"schema\": 1,\n", "", 1)),
            ),
            "layouts/pixelfed_demo/0001.json: \"schema\" must be the client schema",
        ),
        (
            src(
                "pixelfed_demo",
                "0001",
                leak(ok.replacen("\"schema\": 1", "\"schema\": 0", 1)),
            ),
            "\"schema\" must be the client schema the layout is written for, 1..=",
        ),
        (
            src(
                "pixelfed_demo",
                "0001",
                leak(ok.replacen("\"schema\": 1", "\"schema\": 999", 1)),
            ),
            "\"schema\" must be",
        ),
        (
            src(
                "pixelfed_home",
                "0001",
                leak(
                    files()[1]
                        .text
                        .replacen("\"schema\": 2", "\"schema\": 1", 1),
                ),
            ),
            "\"schema\" is 1 but the layout uses vocabulary from schema 2",
        ),
    ];
    for (s, want) in cases {
        let err = parse_files(&catalog(), &[s]).unwrap_err();
        assert!(err.contains(want), "{err} does not contain {want}");
    }
    // Gaps and duplicates.
    let err = parse_files(
        &catalog(),
        &[
            src("pixelfed_demo", "0001", ok),
            src("pixelfed_demo", "0003", ok),
        ],
    )
    .unwrap_err();
    assert_eq!(
        err,
        "layouts/pixelfed_demo/0003.json: layout files are numbered without gaps, expected 0002.json next"
    );
    let err = parse_files(&catalog(), &[src("pixelfed_demo", "0002", ok)]).unwrap_err();
    assert!(err.contains("expected 0001.json next"), "{err}");
    let err = parse_files(
        &catalog(),
        &[
            src("pixelfed_demo", "0001", ok),
            src("pixelfed_demo", "0001", ok),
        ],
    )
    .unwrap_err();
    assert!(err.contains("listed twice"), "{err}");
    // Structural rules from the agent validator (endpoint prefix).
    let bad = ok.replace("/v1/demo/ack", "https://evil/ack");
    let bad: &'static str = Box::leak(bad.into_boxed_str());
    let err = parse_files(&catalog(), &[src("pixelfed_demo", "0001", bad)]).unwrap_err();
    assert!(
        err.contains("must be a pod path starting with /v1/"),
        "{err}"
    );
    // New intents are fine in developer-authored files.
    let new_intent = ok.replace("/v1/demo/ack", "/v1/demo/other");
    let new_intent: &'static str = Box::leak(new_intent.into_boxed_str());
    parse_files(&catalog(), &[src("pixelfed_demo", "0001", new_intent)]).unwrap();
}

#[test]
fn removed_node_types_only_parse_in_their_deployed_files() {
    let legacy = files()
        .into_iter()
        .find(|f| f.app_id == "pixelfed_home" && f.name == "0001")
        .unwrap();
    // The deployed file still parses: its widgets are opaque unknown nodes.
    let parsed = parse_file(&catalog(), &legacy).unwrap();
    assert_eq!(
        pod_core::validate::unknown_node_types(&parsed.layout),
        ["pixelfed://FeedList"]
    );
    // The same content as a new file is refused.
    let err = parse_file(&catalog(), &src("pixelfed_home", "0004", legacy.text)).unwrap_err();
    assert!(
        err.contains("unknown node type 'pixelfed://FeedList'"),
        "{err}"
    );
    // So is a typo'd type in a generic file.
    let typo = leak(MINIMAL.replacen("\"core://Button\"", "\"core://Buton\"", 1));
    let err = parse_file(&catalog(), &src("pixelfed_demo", "0003", typo)).unwrap_err();
    assert!(err.contains("unknown node type 'core://Buton'"), "{err}");
    // An unknown icon is refused too.
    let feed = files()
        .into_iter()
        .find(|f| f.app_id == "pixelfed_home" && f.name == "0003")
        .unwrap()
        .text;
    let icon = leak(feed.replacen("\"chat_bubble\"", "\"chat_bubbles\"", 1));
    let err = parse_file(&catalog(), &src("pixelfed_home", "0003", icon)).unwrap_err();
    assert!(err.contains("unknown icon 'chat_bubbles'"), "{err}");
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn unknown_intent_tags_are_refused_in_files() {
    // The newest feed file: generic nodes, so its intents are typed.
    let ok = files()
        .into_iter()
        .find(|f| f.app_id == "pixelfed_home" && f.name == "0003")
        .unwrap()
        .text;
    // A typo'd tag (would deserialize as ActionIntent::Unknown).
    let typo = leak(ok.replacen(
        "\"action.navigation.open\"",
        "\"action.navigation.opne\"",
        1,
    ));
    let err = parse_files(&catalog(), &[src("pixelfed_home", "0003", typo)]).unwrap_err();
    assert!(
        err.starts_with(
            "layouts/pixelfed_home/0003.json: invalid layout: unknown action intent \
             'action.navigation.opne' at /layout/"
        ),
        "{err}"
    );
    // The pointer leads to the object holding the tag.
    let at = err
        .split(" at ")
        .nth(1)
        .and_then(|r| r.split(' ').next())
        .unwrap();
    let doc: Value = serde_json::from_str(typo).unwrap();
    assert_eq!(doc.pointer(at).unwrap()["intent"], "action.navigation.opne");
    // The removed agent intent and the fallback tag itself.
    for tag in ["action.agent.prompt", "action.unknown"] {
        let bad = leak(ok.replacen("\"action.mutation.optimistic\"", &format!("\"{tag}\""), 1));
        let err = parse_files(&catalog(), &[src("pixelfed_home", "0003", bad)]).unwrap_err();
        assert!(
            err.contains(&format!("unknown action intent '{tag}'")),
            "{err}"
        );
    }
    // Every tag in the shipped files is known.
    for f in files() {
        let mut found = Vec::new();
        unknown_intent_tags(&serde_json::from_str(f.text).unwrap(), "", &mut found);
        assert!(found.is_empty(), "{}/{}: {found:?}", f.app_id, f.name);
    }
}

#[test]
fn file_hash_ignores_formatting_and_key_order_but_not_content() {
    let one = MINIMAL;
    let parsed = parse_file(&catalog(), &src("pixelfed_demo", "0001", one)).unwrap();
    // Compact, keys sorted (serde_json's map), top-level keys reversed.
    let v: Value = serde_json::from_str(one).unwrap();
    let compact = leak(format!(
        "{{\"layout\":{},\"schema\":1,\"note\":{}}}",
        serde_json::to_string(&v["layout"]).unwrap(),
        v["note"]
    ));
    let pretty = leak(serde_json::to_string_pretty(&v).unwrap());
    for text in [compact, pretty] {
        assert_eq!(
            parse_file(&catalog(), &src("pixelfed_demo", "0001", text))
                .unwrap()
                .file_hash,
            parsed.file_hash
        );
    }
    // Note whitespace is trimmed before hashing (the served note is trimmed too).
    let padded = leak(one.replacen("\"Minimal\"", "\"  Minimal \"", 1));
    assert_eq!(
        parse_file(&catalog(), &src("pixelfed_demo", "0001", padded))
            .unwrap()
            .file_hash,
        parsed.file_hash
    );
    // Content changes change it: the note, a value, an added explicit default.
    let renoted = leak(one.replacen("\"Minimal\"", "\"Minimal!\"", 1));
    let retitled = leak(one.replace("Minimal demo", "Minimal v2"));
    for text in [renoted, retitled] {
        assert_ne!(
            parse_file(&catalog(), &src("pixelfed_demo", "0001", text))
                .unwrap()
                .file_hash,
            parsed.file_hash
        );
    }
    assert_eq!(parsed.file_hash.len(), 64);
    // `schema` is not hashed: it was added to files after they deployed
    // (it is recorded and append-only on its own, see the store).
    let v2 = leak(one.replacen("\"schema\": 1", "\"schema\": 2", 1));
    let p2 = parse_file(&catalog(), &src("pixelfed_demo", "0001", v2)).unwrap();
    assert_eq!((p2.file_hash, p2.schema), (parsed.file_hash, 2));
}

#[test]
fn sync_appends_new_files_once_and_refuses_changes() {
    let store = Store::open_in_memory().unwrap();
    let report = sync(&store, &catalog()).unwrap();
    assert_eq!(report.appended.len(), files().len());
    assert!(report.adopted.is_empty() && report.missing.is_empty());
    assert_eq!(
        sync(&store, &catalog()).unwrap(),
        SyncReport::default(),
        "idempotent"
    );
    let (v, layout) = latest(&store, &catalog(), "pixelfed_home")
        .unwrap()
        .unwrap();
    assert_eq!(
        v, 7,
        "0007: comments under their post, notifications open the thread"
    );
    assert!(pod_core::validate::unknown_node_types(&layout).is_empty());
    // Each client gets the newest version it can draw.
    for (schema, version) in [
        (1, 1),
        (2, 2),
        (7, 3),
        (8, 4),
        (9, 5),
        (10, 6),
        (11, 6),
        (protocol::SCHEMA_VERSION, 7),
    ] {
        let served = active_for(&store, &catalog(), "pixelfed_home", schema)
            .unwrap()
            .unwrap();
        assert_eq!(served.version, version, "schema {schema}");
        assert!(protocol::required_schema(&served.layout) <= schema);
    }
    // A third pixelfed_demo file.
    let one = src("pixelfed_demo", "0003", MINIMAL);
    let two_text = MINIMAL.replace("Minimal demo", "Minimal v2");
    let two_text: &'static str = Box::leak(two_text.into_boxed_str());
    let two = src("pixelfed_demo", "0003", two_text);
    let mut sources = files();
    sources.push(two);
    assert_eq!(
        sync_sources(&store, &catalog(), &sources).unwrap().appended,
        [("layouts/pixelfed_demo/0003.json".to_owned(), 3)]
    );
    // Changing a deployed file stops the start, naming it.
    let mut changed = files();
    changed.push(src("pixelfed_demo", "0003", one.text));
    assert_eq!(
        sync_sources(&store, &catalog(), &changed).unwrap_err(),
        "layouts/pixelfed_demo/0003.json changed after it was deployed as version 3; layout files are \
         append-only. To recover, restore the deployed content of layouts/pixelfed_demo/0003.json \
         (git log -p -- layouts/pixelfed_demo/0003.json) and put the change in a new file, 0004.json"
    );
    // Reformatting a file (same layout and note) is not a change.
    let reformatted: Value = serde_json::from_str(two_text).unwrap();
    let reformatted: &'static str = Box::leak(
        serde_json::to_string(&reformatted)
            .unwrap()
            .into_boxed_str(),
    );
    let mut same = files();
    same.push(src("pixelfed_demo", "0003", reformatted));
    assert_eq!(
        sync_sources(&store, &catalog(), &same).unwrap(),
        SyncReport::default()
    );
    // A rollback deploy without 0003.json starts, serving v3 unchanged.
    let report = sync(&store, &catalog()).unwrap();
    assert_eq!(
        report.missing,
        [
            "layouts/pixelfed_demo/0003.json was deployed as version 3 but is missing from this build; \
          serving the recorded history unchanged"
        ]
    );
    assert!(report.appended.is_empty());
    assert_eq!(
        latest(&store, &catalog(), "pixelfed_demo")
            .unwrap()
            .unwrap()
            .0,
        3
    );
}

#[test]
fn rfc3339_formats_utc() {
    for secs in [0, 951_782_400, 1_793_620_960, 4_102_444_799] {
        let s = rfc3339(secs);
        assert_eq!(app_pixelfed::parse_rfc3339(&s), Some(secs), "{s}");
    }
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
}
