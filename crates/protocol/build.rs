//! Collects the layout features (`features/<name>.md`, one file each) into
//! `protocol::FEATURES`, the removed ones (`features/removed/<name>.md`) into
//! `protocol::REMOVED_FEATURES`, and one constant per feature in
//! `protocol::feature`. One file per feature means parallel changes never
//! edit the same line.

use std::fmt::Write as _;
use std::path::Path;

/// The sorted feature names of the `*.md` files in `dir` (README excluded).
fn names(dir: &Path) -> Vec<String> {
    let entries = std::fs::read_dir(dir).ok().into_iter().flatten();
    let mut names: Vec<String> = entries
        .map(|e| e.expect("a directory entry"))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
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
    names
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("features");
    let removed_dir = dir.join("removed");
    println!("cargo:rerun-if-changed={}", dir.display());
    println!("cargo:rerun-if-changed={}", removed_dir.display());
    // Missing only in a dependency-only build (cargo-chef's cook step,
    // deploy/pod.Dockerfile); a real build always has it.
    let drawn = names(&dir);
    let removed = names(&removed_dir);
    for n in &removed {
        assert!(
            !drawn.contains(n),
            "features/removed/{n}.md: a removed feature is not also drawn"
        );
    }

    let mut out = String::new();
    out.push_str("/// Every layout feature this build draws, sorted (`features/*.md`).\n");
    out.push_str("pub const FEATURES: &[&str] = &[");
    for n in &drawn {
        write!(out, "{n:?}, ").unwrap();
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// Features no client draws any more, sorted (`features/removed/*.md`): a\n\
         /// version that uses one is retired.\n",
    );
    out.push_str("pub const REMOVED_FEATURES: &[&str] = &[");
    for n in &removed {
        write!(out, "{n:?}, ").unwrap();
    }
    out.push_str("];\n\n");
    out.push_str("/// `(name, description)` of every drawn feature, sorted (`features/*.md`).\n");
    out.push_str("pub const FEATURE_DOCS: &[(&str, &str)] = &[\n");
    for n in &drawn {
        let doc = std::fs::read_to_string(dir.join(format!("{n}.md"))).unwrap_or_default();
        writeln!(out, "    ({n:?}, {:?}),", doc.trim()).unwrap();
    }
    out.push_str("];\n\n");
    out.push_str("/// One constant per layout feature (`features/<name>.md`, `features/removed/<name>.md`).\n");
    out.push_str("pub mod feature {\n");
    for (n, d) in drawn
        .iter()
        .map(|n| (n, &dir))
        .chain(removed.iter().map(|n| (n, &removed_dir)))
    {
        let doc = std::fs::read_to_string(d.join(format!("{n}.md"))).unwrap_or_default();
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
