//! Golden expansion (decision `plumbing-and-porcelain`): a blueprint file
//! rewritten onto server components records exactly the layout of the file
//! it replaces, so nothing visible changed.

use pod_core::layouts::{node_value, parse_file_with};

/// `(app, file)`: files written with components whose layout must equal the
/// file before them.
const REWRITTEN: &[(&str, u32)] = &[
    ("events", 5),
    ("settings", 4),
    ("inbox", 2),
    ("shell", 6),
    ("recipes", 6),
    ("pixelfed", 12),
    ("pixelfed", 13),
];

#[test]
fn rewritten_blueprints_expand_to_the_layout_they_replace() {
    // The components as they were when the files were rewritten (version
    // 1); later versions change what the files expand to (the theme's
    // colours, decision `dark-palette`).
    let catalog = server::catalog();
    let first = pod_core::parts::Pending {
        defs: catalog
            .component_defs()
            .defs
            .iter()
            .filter(|(_, v, _)| *v == 1)
            .cloned()
            .collect(),
    };
    let files: Vec<_> = server::layout_files()
        .iter()
        .map(|s| parse_file_with(&catalog, s, &first).unwrap())
        .collect();
    let file = |app: &str, n: u32| {
        files
            .iter()
            .find(|f| f.app_id == app && f.number == n)
            .unwrap_or_else(|| panic!("{app}/{n:04}.json"))
    };
    for (app, n) in REWRITTEN {
        let (new, old) = (file(app, *n), file(app, n - 1));
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(new.path.trim_start_matches("crates/")),
        )
        .unwrap();
        assert!(
            text.contains("core://Use"),
            "{} uses no component",
            new.path
        );
        assert_eq!(
            node_value(&new.layout),
            node_value(&old.layout),
            "{} doesn't expand to {}",
            new.path,
            old.path
        );
        let parts = |f: &pod_core::layouts::LayoutFile| {
            f.parts
                .iter()
                .map(|(k, v)| (k.clone(), node_value(v)))
                .collect::<Vec<_>>()
        };
        assert_eq!(parts(new), parts(old), "{}: parts", new.path);
    }
}
