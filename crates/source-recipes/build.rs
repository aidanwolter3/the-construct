include!("../pod-core/build_support/layout_files.rs");

fn main() {
    generate_layout_files(&[("recipes", "FILES")]);
}
