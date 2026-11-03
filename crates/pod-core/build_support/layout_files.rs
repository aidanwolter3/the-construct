// Shared by the adapters' build scripts (`include!`d from their build.rs):
// generates each app's list of embedded layout files from `layouts/<app>/`,
// so adding a layout version is adding a file, with no list to edit.

/// Writes `$OUT_DIR/layout_files.rs`: for each `(app_id, CONST)`, a
/// `pub static CONST: &[LayoutSource]` of every `layouts/<app_id>/*.json`, in
/// name order (`LayoutSource` must be in scope where it is included). File
/// names are checked by `pod_core::layouts::parse_files`.
fn generate_layout_files(apps: &[(&str, &str)]) {
    use std::fmt::Write as _;
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../../layouts");
    let mut out = String::new();
    for (app, konst) in apps {
        let dir = root.join(app);
        println!("cargo:rerun-if-changed={}", dir.display());
        // Missing only in a dependency-only build (cargo-chef's cook step,
        // deploy/pod.Dockerfile); a real build always has it.
        let Ok(entries) = std::fs::read_dir(&dir) else {
            println!("cargo:warning=no {}: embedding no layout files", dir.display());
            writeln!(out, "pub static {konst}: &[LayoutSource] = &[];").unwrap();
            continue;
        };
        let mut names: Vec<String> = entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
            .collect();
        names.sort();
        writeln!(out, "/// `layouts/{app}/*.json`, in name order (generated).").unwrap();
        writeln!(out, "pub static {konst}: &[LayoutSource] = &[").unwrap();
        for n in names {
            let path = dir.join(format!("{n}.json"));
            writeln!(
                out,
                "    LayoutSource {{ app_id: {app:?}, name: {n:?}, text: include_str!({:?}) }},",
                path.display().to_string()
            )
            .unwrap();
        }
        writeln!(out, "];").unwrap();
    }
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("layout_files.rs");
    std::fs::write(dest, out).unwrap();
}
