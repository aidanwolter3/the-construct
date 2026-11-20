include!("../pod-core/build_support/layout_files.rs");

fn main() {
    // The kits' layouts (decision `element-roles-and-reuse`).
    generate_layout_files(&[("checklist", "CHECKLIST_FILES")]);
}
