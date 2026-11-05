//! Embeds the server components (`components/<name>/NNNN.json`, decision
//! `plumbing-and-porcelain`) as `$OUT_DIR/component_files.rs`: adding a
//! component version is adding a file, with no list to edit.

use std::fmt::Write as _;

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("components");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut out =
        String::from("/// `crates/pod-core/components/<name>/NNNN.json`, sorted (generated).\n");
    out.push_str("pub static CORE_COMPONENT_FILES: &[ComponentSource] = &[\n");
    // Missing only in a dependency-only build (cargo-chef's cook step).
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .map(|d| {
            d.map(|e| e.unwrap().path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    for dir in dirs {
        println!("cargo:rerun-if-changed={}", dir.display());
        let name = dir.file_name().unwrap().to_str().unwrap().to_owned();
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
            .collect();
        files.sort();
        for f in files {
            let path = dir.join(format!("{f}.json"));
            writeln!(
                out,
                "    ComponentSource {{ name: {name:?}, file: {f:?}, text: include_str!({:?}) }},",
                path.display().to_string()
            )
            .unwrap();
        }
    }
    out.push_str("];\n");
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("component_files.rs");
    std::fs::write(dest, out).unwrap();
}
