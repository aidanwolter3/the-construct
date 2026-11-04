include!("../pod-core/build_support/layout_files.rs");

fn main() {
    generate_layout_files(&[
        ("pixelfed", "PIXELFED_FILES"),
        ("pixelfed_sample_no_login", "SAMPLE_FILES"),
    ]);
}
