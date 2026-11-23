//! The core layout's files: embedded, valid, and usable by reference.

use std::sync::Arc;

use pod_core::{Catalog, Source};
use serde_json::json;
use source_taste::{FILES, TasteAdapter};

use crate::fake::{pod, taste_layout, workspace};

#[test]
fn every_layout_file_is_embedded() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts/taste");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    on_disk.sort();
    let embedded: Vec<String> = FILES.iter().map(|f| format!("{}.json", f.name)).collect();
    assert_eq!(on_disk, embedded);
}

#[test]
fn layout_files_parse_and_validate() {
    let adapters: Vec<Arc<dyn Source>> = vec![Arc::new(TasteAdapter::default())];
    let catalog = Catalog::new(adapters).unwrap();
    let core = pod_core::construct::core::load_with(&catalog, FILES).unwrap();
    assert_eq!(core.names["@core/taste"].len(), FILES.len());
}

#[test]
fn the_manifest_is_valid() {
    pod_core::source::check_manifest(source_taste::NAME, &source_taste::MANIFEST).unwrap();
}

#[tokio::test]
async fn a_signed_out_pod_shows_the_sign_in() {
    let pod = pod();
    let app = taste_layout(&pod).await;
    let ws = workspace(&pod, &app).await;
    assert_eq!(
        ws["entities"]["taste://status/pod"]["signed_in"], false,
        "{ws}"
    );
    assert_eq!(ws["queries"]["taste:feed"], json!([]));
    assert!(
        ws.to_string().contains("Sign in to your taste service"),
        "the sign-in form is in the layout"
    );
}
