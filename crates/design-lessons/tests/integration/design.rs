//! docs/design-lessons.json is docs/design.md read by `design_lessons`. A pod's
//! fillers fetch the JSON from `main`, so a change to the document that
//! doesn't regenerate it would leave the course behind.

use std::path::PathBuf;

fn docs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

#[test]
fn lessons_json_matches_the_design_document() {
    let markdown = std::fs::read_to_string(docs().join("design.md")).expect("docs/design.md");
    let course = design_lessons::parse(&markdown).unwrap_or_else(|e| panic!("docs/design.md: {e}"));
    let json = design_lessons::to_json(&course);
    let path = docs().join("design-lessons.json");
    if std::env::var_os("UPDATE_DESIGN_LESSONS").is_some() {
        std::fs::write(&path, &json).expect("write docs/design-lessons.json");
        return;
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        stored == json,
        "docs/design-lessons.json is out of date with docs/design.md; regenerate it with \
         UPDATE_DESIGN_LESSONS=1 cargo test -p design-lessons"
    );
}

#[test]
fn every_glossary_term_is_defined_once() {
    let markdown = std::fs::read_to_string(docs().join("design.md")).expect("docs/design.md");
    let course = design_lessons::parse(&markdown).expect("parses");
    let mut seen = std::collections::BTreeSet::new();
    for t in &course.glossary {
        assert!(
            !t.definition.is_empty(),
            "glossary `{}` has no definition",
            t.term
        );
        assert!(
            seen.insert(t.term.to_lowercase()),
            "glossary `{}` twice",
            t.term
        );
    }
}
