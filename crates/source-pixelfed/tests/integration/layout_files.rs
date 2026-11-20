//! The Pixelfed core layouts' layout files (this crate's `layouts/`), checked
//! with the core's loader (`pod_core::construct::core`), plus the generic
//! file rules exercised on them.

use std::sync::Arc;

use pod_core::construct::core::load_with;
use pod_core::layouts::*;
use pod_core::{Catalog, Source};
use serde_json::Value;
use source_pixelfed::{PixelfedAdapter, PixelfedConfig, apps};

/// The Pixelfed adapters, as the `server` binary registers them.
fn catalog() -> Catalog {
    let adapters: Vec<Arc<dyn Source>> = vec![
        Arc::new(PixelfedAdapter::new(PixelfedConfig::default())),
        apps::sample_adapter(),
    ];
    Catalog::new(adapters).unwrap()
}

fn files() -> Vec<LayoutSource> {
    apps::files()
}

/// An app's newest layout file.
fn newest(app_id: &str) -> LayoutSource {
    files()
        .into_iter()
        .filter(|f| f.app_id == app_id)
        .max_by_key(|f| f.name)
        .unwrap()
}
#[test]
fn every_file_in_layouts_is_embedded() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts");
    let mut on_disk = Vec::new();
    for app_dir in std::fs::read_dir(&root).unwrap() {
        let app_dir = app_dir.unwrap().path();
        let app_id = app_dir.file_name().unwrap().to_str().unwrap().to_owned();
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
        "build.rs embeds every file in layouts/ (and nothing else)"
    );
    for f in files() {
        let text =
            std::fs::read_to_string(root.join(f.app_id).join(format!("{}.json", f.name))).unwrap();
        assert_eq!(text, f.text);
    }
}

#[test]
fn every_file_parses_and_validates() {
    let core = load_with(&catalog(), &files()).unwrap();
    for a in catalog().core_layouts() {
        assert!(
            core.names.contains_key(&format!("@core/{}", a.id)),
            "{} has a layout file",
            a.id
        );
    }
}

fn src(app_id: &'static str, name: &'static str, text: &'static str) -> LayoutSource {
    LayoutSource {
        dir: "layouts",
        app_id,
        name,
        text,
    }
}

/// A small valid layout file for `pixelfed_sample_no_login` (schema 1: a title and
/// one action), the base the validation tests mutate.
const MINIMAL: &str = r#"{
  "note": "Minimal",
  "schema": 1,
  "layout": {
"type": "core://Workspace",
"id": "pixelfed_sample_no_login",
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
            "endpoint": "/v1/pixelfed_sample_no_login/ack",
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
    let cases: Vec<(LayoutSource, &str)> = vec![
        (
            src("pixelfed_sample_no_login", "0001", "{"),
            "layouts/pixelfed_sample_no_login/0001.json: not valid JSON",
        ),
        (
            src("pixelfed_sample_no_login", "1", ok),
            "layouts/pixelfed_sample_no_login/1.json: file names are 4-digit",
        ),
        (
            src("nope", "0001", ok),
            "layouts/nope/0001.json: 'nope' is not a core layout",
        ),
        (
            src("pixelfed_sample_no_login", "0001", r#"{"layout": {}}"#),
            "\"note\" must be a non-empty string",
        ),
        (
            src("pixelfed_sample_no_login", "0001", r#"{"note": "x"}"#),
            "\"layout\" is missing",
        ),
        (
            src(
                "pixelfed_sample_no_login",
                "0001",
                r#"{"note": "x", "layout": {}, "extra": 1}"#,
            ),
            "unknown key 'extra'",
        ),
        (
            src(
                "pixelfed_sample_no_login",
                "0001",
                r#"{"note": "x", "layout": {"type": "core://Text", "value_expr": "", "style": "Body"}}"#,
            ),
            "layouts/pixelfed_sample_no_login/0001.json: invalid layout: root must be a core://Workspace or a core://Page",
        ),
        (
            src("pixelfed", "0001", ok),
            "invalid layout: workspace id changed from 'pixelfed' to 'pixelfed_sample_no_login'",
        ),
    ];
    for (s, want) in cases {
        let err = load_with(&catalog(), &[s]).unwrap_err();
        assert!(err.contains(want), "{err} does not contain {want}");
    }
    // Gaps and duplicates.
    let err = load_with(
        &catalog(),
        &[
            src("pixelfed_sample_no_login", "0001", ok),
            src("pixelfed_sample_no_login", "0003", ok),
        ],
    )
    .unwrap_err();
    assert_eq!(
        err,
        "layouts/pixelfed_sample_no_login/0003.json: layout files are numbered without gaps, expected 0002.json next"
    );
    // An app's files may start above 0001: the older ones were retired
    // (decision `retired-layout-files`).
    let core = load_with(&catalog(), &[src("pixelfed_sample_no_login", "0002", ok)]).unwrap();
    assert!(
        core.names["@core/pixelfed_sample_no_login"][0]
            .1
            .contains("file 0002")
    );
    let err = load_with(
        &catalog(),
        &[
            src("pixelfed_sample_no_login", "0001", ok),
            src("pixelfed_sample_no_login", "0001", ok),
        ],
    )
    .unwrap_err();
    assert!(err.contains("listed twice"), "{err}");
    // Structural rules from the agent validator (endpoint prefix).
    let bad = ok.replace("/v1/pixelfed_sample_no_login/ack", "https://evil/ack");
    let bad: &'static str = Box::leak(bad.into_boxed_str());
    let err = load_with(&catalog(), &[src("pixelfed_sample_no_login", "0001", bad)]).unwrap_err();
    assert!(
        err.contains("must be a pod path starting with /v1/"),
        "{err}"
    );
    // An endpoint the app's data sources don't declare is refused (decision
    // `custom-apps-and-data-sources`), naming what they offer.
    let undeclared = ok.replace("/v1/pixelfed_sample_no_login/ack", "/v1/sample/other");
    let undeclared: &'static str = Box::leak(undeclared.into_boxed_str());
    let err = load_with(
        &catalog(),
        &[src("pixelfed_sample_no_login", "0001", undeclared)],
    )
    .unwrap_err();
    assert!(
        err.contains(
            "a mutation's endpoint '/v1/sample/other' is not an action of its data sources \
             (pixelfed_sample_no_login); they offer: /v1/pixelfed_sample_no_login/ack"
        ),
        "{err}"
    );
}

#[test]
fn unknown_node_types_are_refused() {
    // A node type removed from the protocol (D37) is refused in any file.
    let removed = leak(MINIMAL.replacen("\"core://Button\"", "\"pixelfed://FeedList\"", 1));
    let err = load_with(
        &catalog(),
        &[src("pixelfed_sample_no_login", "0003", removed)],
    )
    .unwrap_err();
    assert!(
        err.contains("unknown node type 'pixelfed://FeedList'"),
        "{err}"
    );
    // So is a typo'd type in a generic file.
    let typo = leak(MINIMAL.replacen("\"core://Button\"", "\"core://Buton\"", 1));
    let err = load_with(&catalog(), &[src("pixelfed_sample_no_login", "0003", typo)]).unwrap_err();
    assert!(err.contains("unknown node type 'core://Buton'"), "{err}");
    // An unknown icon is refused too.
    let feed = newest("pixelfed");
    let icon = leak(feed.text.replacen("\"chat_bubble\"", "\"chat_bubbles\"", 1));
    let err = load_with(&catalog(), &[src("pixelfed", feed.name, icon)]).unwrap_err();
    assert!(err.contains("unknown icon 'chat_bubbles'"), "{err}");
    // An icon given as its drawing is taken, path data that isn't is refused.
    let drawn = leak(feed.text.replacen(
        "\"chat_bubble\"",
        "\"path:M20,2H4c-1.1,0 -2,0.9 -2,2v18l4,-4h14c1.1,0 2,-0.9 2,-2V4c0,-1.1 -0.9,-2 -2,-2z\"",
        1,
    ));
    load_with(&catalog(), &[src("pixelfed", feed.name, drawn)]).unwrap();
    let bad = leak(feed.text.replacen("\"chat_bubble\"", "\"path:<svg/>\"", 1));
    let err = load_with(&catalog(), &[src("pixelfed", feed.name, bad)]).unwrap_err();
    assert!(err.contains("icon path data must start with M"), "{err}");
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn unknown_intent_tags_are_refused_in_files() {
    // The newest feed file: generic nodes, so its intents are typed.
    let feed = newest("pixelfed");
    let (name, ok) = (feed.name, feed.text);
    // A typo'd tag (would deserialize as ActionIntent::Unknown).
    let typo = leak(ok.replacen(
        "\"action.navigation.open\"",
        "\"action.navigation.opne\"",
        1,
    ));
    let err = load_with(&catalog(), &[src("pixelfed", name, typo)]).unwrap_err();
    assert!(
        err.starts_with(&format!(
            "layouts/pixelfed/{name}.json: invalid layout: unknown action intent \
             'action.navigation.opne' at /layout/"
        )),
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
        let err = load_with(&catalog(), &[src("pixelfed", name, bad)]).unwrap_err();
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
fn rfc3339_formats_utc() {
    for secs in [0, 951_782_400, 1_793_620_960, 4_102_444_799] {
        let s = rfc3339(secs);
        assert_eq!(source_pixelfed::parse_rfc3339(&s), Some(secs), "{s}");
    }
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
}
