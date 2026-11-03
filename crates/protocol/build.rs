//! Collects the layout features (`features/<name>.md`, one file each) into
//! `protocol::FEATURES` and one constant per feature in `protocol::feature`.
//! One file per feature means parallel changes never edit the same line.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("features");
    println!("cargo:rerun-if-changed={}", dir.display());
    // Missing only in a dependency-only build (cargo-chef's cook step,
    // deploy/pod.Dockerfile); a real build always has it.
    let entries = std::fs::read_dir(&dir).ok().into_iter().flatten();
    let mut names: Vec<String> = entries
        .map(|e| e.expect("a directory entry").file_name())
        .filter_map(|n| n.to_str().map(str::to_owned))
        .filter(|n| n != "README.md")
        .map(|n| {
            let name = n
                .strip_suffix(".md")
                .unwrap_or_else(|| panic!("features/{n}: feature files are <name>.md"));
            let valid = name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            assert!(
                valid,
                "features/{n}: a feature name is [a-z][a-z0-9_]* (it goes in a header)"
            );
            name.to_owned()
        })
        .collect();
    names.sort();

    let mut out = String::new();
    out.push_str("/// Every layout feature this build draws, sorted (`features/*.md`).\n");
    out.push_str("pub const FEATURES: &[&str] = &[");
    for n in &names {
        write!(out, "{n:?}, ").unwrap();
    }
    out.push_str("];\n\n");
    out.push_str("/// One constant per layout feature (`features/<name>.md`).\n");
    out.push_str("pub mod feature {\n");
    for n in &names {
        let doc = std::fs::read_to_string(dir.join(format!("{n}.md"))).unwrap_or_default();
        for line in doc.lines() {
            writeln!(out, "    /// {line}").unwrap();
        }
        writeln!(
            out,
            "    pub const {}: &str = {n:?};",
            n.to_ascii_uppercase()
        )
        .unwrap();
    }
    out.push_str("}\n");
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("features.rs");
    std::fs::write(dest, out).expect("writing features.rs");
}
