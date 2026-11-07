//! The blueprint's files: embedded, valid, and usable by reference.

use std::sync::Arc;

use app_agents::{AgentsAdapter, FILES};
use pod_core::layouts::parse_files;
use pod_core::{AppAdapter, Catalog};
use serde_json::json;

use crate::runner::{agents_layout, pod, workspace};

#[test]
fn every_layout_file_is_embedded() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts/agents");
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
    let adapters: Vec<Arc<dyn AppAdapter>> = vec![Arc::new(AgentsAdapter::default())];
    let catalog = Catalog::new(adapters).unwrap();
    let files = parse_files(&catalog, FILES).unwrap();
    assert_eq!(files.len(), FILES.len());
}

#[test]
fn the_manifest_is_valid() {
    pod_core::source::check_manifest(app_agents::NAME, &app_agents::MANIFEST).unwrap();
}

#[tokio::test]
async fn a_signed_out_pod_shows_the_sign_in() {
    let pod = pod();
    let app = agents_layout(&pod).await;
    let ws = workspace(&pod, &app).await;
    let status = &ws["entities"]["agents://runner/status"];
    assert_eq!(status["signed_in"], false, "{ws}");
    assert_eq!(ws["queries"]["agents:sessions"], json!([]));
    assert!(
        ws.to_string().contains("Sign in to your agent runner"),
        "the sign-in form is in the layout"
    );
}
