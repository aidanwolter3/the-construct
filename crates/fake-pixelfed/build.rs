//! Embeds `fixtures/pixelfed/` (the two JSON fixtures and `media/`) into the
//! binary, so `fake-pixelfed` runs anywhere as a single file. Generates
//! `$OUT_DIR/embedded_fixtures.rs`: `static EMBEDDED: &[(&str, &[u8])]`,
//! paths relative to the fixture directory with `/` separators.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = manifest.join("../../fixtures/pixelfed");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded_fixtures.rs");

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for name in ["timeline_home.json", "account_verify_credentials.json"] {
        files.push((name.to_owned(), dir.join(name)));
    }
    let media = dir.join("media");
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("timeline_home.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("account_verify_credentials.json").display()
    );
    println!("cargo:rerun-if-changed={}", media.display());
    let mut media_files: Vec<PathBuf> = std::fs::read_dir(&media)
        .unwrap_or_else(|e| panic!("{}: {e}", media.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    media_files.sort();
    for p in media_files {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        println!("cargo:rerun-if-changed={}", p.display());
        files.push((format!("media/{name}"), p));
    }

    let mut src = String::from("static EMBEDDED: &[(&str, &[u8])] = &[\n");
    for (rel, path) in &files {
        let abs = canonical(path);
        writeln!(src, "    ({rel:?}, include_bytes!({abs:?})),").unwrap();
    }
    src.push_str("];\n");
    std::fs::write(out, src).unwrap();
}

fn canonical(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .to_string_lossy()
        .into_owned()
}
