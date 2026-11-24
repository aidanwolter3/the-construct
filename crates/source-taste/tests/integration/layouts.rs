//! The core layout's files: embedded, valid, and usable by reference.

use std::sync::Arc;

use pod_core::{Catalog, Source};
use serde_json::json;
use source_taste::{FILES, TasteAdapter};

use crate::fake::{pod, taste_layout, workspace};

#[test]
fn every_layout_file_is_embedded() {
    let mut on_disk = Vec::new();
    for layout in ["taste", "taste_item"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("layouts")
            .join(layout);
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| format!("{layout}/{}", e.unwrap().file_name().into_string().unwrap()))
            .collect();
        files.sort();
        on_disk.extend(files);
    }
    let embedded: Vec<String> = FILES
        .iter()
        .map(|f| format!("{}/{}.json", f.app_id, f.name))
        .collect();
    assert_eq!(on_disk, embedded);
}

#[test]
fn layout_files_parse_and_validate() {
    let adapters: Vec<Arc<dyn Source>> = vec![Arc::new(TasteAdapter::default())];
    let catalog = Catalog::new(adapters).unwrap();
    let core = pod_core::construct::core::load_with(&catalog, &FILES).unwrap();
    let taste = FILES.iter().filter(|f| f.app_id == "taste").count();
    assert_eq!(core.names["@core/taste"].len(), taste);
    assert_eq!(core.names["@core/taste_item"].len(), FILES.len() - taste);
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

/// The newest Taste layout's deck: no like and dislike buttons (the card is
/// swiped right or left) and a main view that doesn't scroll (no scrolling
/// screen scaffold, and no core://Scroll but the signed-out form's).
#[test]
fn the_deck_has_no_buttons_and_does_not_scroll() {
    let newest = FILES
        .iter()
        .filter(|f| f.app_id == "taste")
        .max_by_key(|f| f.name)
        .unwrap();
    let page: serde_json::Value = serde_json::from_str(newest.text).unwrap();
    let root = &page["layout"]["root"];
    let text = root.to_string();
    assert!(
        !text.contains("core:screen_scaffold"),
        "the main view scrolls"
    );
    assert!(
        !text.contains("Dislike $item.title"),
        "a dislike button on the deck"
    );
    assert!(
        !text.contains("\"Like $item.title"),
        "a like button on the deck"
    );
    let children = root["content"]["children"].as_array().unwrap();
    let signed_in = children
        .iter()
        .find(|c| c["equals"] == "true" && c["value"] == "${taste://status/pod#signed_in}")
        .expect("the signed-in view");
    assert!(
        !signed_in.to_string().contains("core://Scroll"),
        "the deck view scrolls"
    );
    assert!(
        signed_in.to_string().contains("\"stack\":true"),
        "the deck is still a stack"
    );
}
