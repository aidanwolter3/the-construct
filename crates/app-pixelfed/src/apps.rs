//! The Pixelfed apps' layout files and the `pixelfed_sample_no_login` dev
//! app: fixed sample posts that need no sign-in.
//!
//! To add a layout version, add `layouts/<app>/NNNN.json` in this crate (see
//! `pod_core::layouts`); `build.rs` embeds every file there.

use std::sync::{Arc, LazyLock};

use axum::{Json, Router, body::Bytes, response::IntoResponse, routing::post};
use pod_core::layouts::LayoutSource;
use pod_core::{AppInfo, AuthPolicy, FeedSpec, PathSpec, Pod, SourceManifest, StaticAdapter};
use protocol::WorkspaceSpec;
use serde_json::json;

// `PIXELFED_FILES` and `SAMPLE_FILES`: every `layouts/pixelfed/*.json` and
// `layouts/pixelfed_sample_no_login/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// Every layout file of the Pixelfed apps (a test checks it matches the
/// crate's `layouts/`).
pub fn files() -> Vec<LayoutSource> {
    PIXELFED_FILES.iter().chain(SAMPLE_FILES).copied().collect()
}

/// The signed-in Pixelfed feed.
pub const PIXELFED: AppInfo = AppInfo {
    id: crate::layout::WORKSPACE_ID,
    title: "Pixelfed",
    icon: "photo",
    dev: false,
    auth: AuthPolicy::SignIn(crate::adapter::SIGN_IN),
    feeds: &[(crate::adapter::NAME, crate::adapter::HOME_FEED)],
};

/// The sample feed (fixed data, no upstream, no sign-in).
pub const SAMPLE: AppInfo = AppInfo {
    id: SAMPLE_NAME,
    title: "Pixelfed sample",
    icon: "photo",
    dev: true,
    auth: AuthPolicy::Public,
    feeds: &[(SAMPLE_NAME, pod_core::static_app::FEED)],
};

/// The sample's adapter name: its app id and its data source id.
pub const SAMPLE_NAME: &str = "pixelfed_sample_no_login";

/// The sample's one action: accepts anything, changes nothing.
pub const SAMPLE_ACK: &str = "/v1/pixelfed_sample_no_login/ack";

/// What the sample's layouts use: its fixed data and the stateless ack. Its
/// data reuses Pixelfed's shapes (`pixelfed://` URIs, `timeline:home`), so
/// it declares no entity scheme or query key of its own: those belong to
/// the Pixelfed source.
const SAMPLE_MANIFEST: SourceManifest = SourceManifest {
    feeds: &[FeedSpec {
        name: pod_core::static_app::FEED,
        query_keys: &[],
        doc: "Fixed sample posts (crates/app-pixelfed/sample/bundle.json).",
    }],
    actions: &[PathSpec {
        pattern: SAMPLE_ACK,
        query_keys: &[],
        doc: "Accepts any action and changes nothing.",
    }],
    dev: true,
    ..SourceManifest::new("Pixelfed sample")
};

static SAMPLE_JSON: &str = include_str!("../sample/bundle.json");

/// The sample bundle (entities and queries; its layout comes from
/// [`SAMPLE_FILES`]). Panics on first access if the file is invalid.
pub static SAMPLE_BUNDLE: LazyLock<WorkspaceSpec> = LazyLock::new(|| {
    serde_json::from_str(SAMPLE_JSON).expect("crates/app-pixelfed/sample/bundle.json is invalid")
});

/// Force-parse the embedded sample. Call at startup to fail fast.
pub fn validate_fixtures() {
    LazyLock::force(&SAMPLE_BUNDLE);
}

/// The `pixelfed_sample_no_login` app: a layout-only [`StaticAdapter`] over
/// the sample bundle, plus [`SAMPLE_ACK`] for its buttons.
pub fn sample_adapter() -> Arc<StaticAdapter> {
    Arc::new(
        StaticAdapter::new(SAMPLE_NAME, SAMPLE, SAMPLE_FILES, &SAMPLE_BUNDLE)
            .with_manifest(SAMPLE_MANIFEST)
            .with_router(sample_router),
    )
}

fn sample_router() -> Router<Pod> {
    Router::new().route(SAMPLE_ACK, post(sample_ack))
}

/// [`SAMPLE_ACK`]: a stateless 200 for the sample's actions, so live-mode
/// likes there are accepted (and not rolled back).
async fn sample_ack(body: Bytes) -> impl IntoResponse {
    tracing::info!(body = %String::from_utf8_lossy(&body), "sample ack");
    Json(json!({ "ok": true }))
}
