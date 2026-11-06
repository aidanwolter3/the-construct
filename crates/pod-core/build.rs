//! Embeds the server components (`components/<name>/NNNN.json`, decision
//! `plumbing-and-porcelain`) as `$OUT_DIR/component_files.rs`.

include!("build_support/component_files.rs");

fn main() {
    generate_component_files("CORE_COMPONENT_FILES");
}
