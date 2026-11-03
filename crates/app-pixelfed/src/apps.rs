//! The Pixelfed apps' layout files and the `pixelfed_demo` dev app.
//!
//! To add a layout version, add `layouts/<app>/NNNN.json` and one
//! `layout_file!` line to [`FILES`] (see `pod_core::layouts`).

use std::sync::{Arc, LazyLock};

use axum::{Json, Router, body::Bytes, response::IntoResponse, routing::post};
use pod_core::layouts::LayoutSource;
use pod_core::{AppInfo, AuthPolicy, Pod, StaticAdapter};
use protocol::WorkspaceSpec;
use serde_json::json;

/// `layout_file!("app", "NNNN")` embeds `layouts/app/NNNN.json`.
macro_rules! layout_file {
    ($app:literal, $name:literal) => {
        LayoutSource {
            app_id: $app,
            name: $name,
            text: include_str!(concat!("../../../layouts/", $app, "/", $name, ".json")),
        }
    };
}

/// `pixelfed_home`'s layout files, in number order.
pub static HOME_FILES: &[LayoutSource] = &[
    layout_file!("pixelfed_home", "0001"),
    layout_file!("pixelfed_home", "0002"),
    layout_file!("pixelfed_home", "0003"),
    layout_file!("pixelfed_home", "0004"),
    layout_file!("pixelfed_home", "0005"),
    layout_file!("pixelfed_home", "0006"),
    layout_file!("pixelfed_home", "0007"),
];

/// `pixelfed_demo`'s layout files, in number order.
pub static DEMO_FILES: &[LayoutSource] = &[
    layout_file!("pixelfed_demo", "0001"),
    layout_file!("pixelfed_demo", "0002"),
];

/// Every layout file of the Pixelfed apps (a test checks it matches
/// `layouts/`).
pub fn files() -> Vec<LayoutSource> {
    HOME_FILES.iter().chain(DEMO_FILES).copied().collect()
}

/// Files deployed with the `pixelfed://` widgets, which were later removed
/// (D36/D37). They still parse (as `UiNode::Unknown`) and are served to the
/// clients that draw them.
pub static HOME_LEGACY: &[(&str, u32)] = &[("pixelfed_home", 1), ("pixelfed_home", 2)];
pub static DEMO_LEGACY: &[(&str, u32)] = &[("pixelfed_demo", 1)];

/// The signed-in Pixelfed feed.
pub const HOME: AppInfo = AppInfo {
    id: crate::layout::WORKSPACE_ID,
    title: "Pixelfed",
    icon: "photo",
    dev: false,
    auth: AuthPolicy::SignIn(crate::adapter::SIGN_IN),
};

/// The static demo feed (fixture data, no upstream).
pub const DEMO: AppInfo = AppInfo {
    id: "pixelfed_demo",
    title: "Pixelfed demo",
    icon: "photo",
    dev: true,
    auth: AuthPolicy::Public,
};

static DEMO_JSON: &str = include_str!("../../../fixtures/pixelfed_home_bundle.json");

/// The `pixelfed_demo` bundle (entities and queries; its layout comes from
/// [`DEMO_FILES`]). Panics on first access if the fixture is invalid.
pub static DEMO_BUNDLE: LazyLock<WorkspaceSpec> = LazyLock::new(|| {
    serde_json::from_str(DEMO_JSON).expect("fixtures/pixelfed_home_bundle.json is invalid")
});

/// Force-parse the embedded fixture. Call at startup to fail fast.
pub fn validate_fixtures() {
    LazyLock::force(&DEMO_BUNDLE);
}

/// The `pixelfed_demo` app: a layout-only [`StaticAdapter`] over the fixture
/// bundle, plus `POST /v1/demo/ack` for its buttons.
pub fn demo_adapter() -> Arc<StaticAdapter> {
    Arc::new(
        StaticAdapter::new("pixelfed_demo", DEMO, DEMO_FILES, &DEMO_BUNDLE)
            .with_legacy_files(DEMO_LEGACY)
            .with_router(demo_router),
    )
}

fn demo_router() -> Router<Pod> {
    Router::new().route("/v1/demo/ack", post(demo_ack))
}

/// `POST /v1/demo/ack`: stateless 200 for the static `pixelfed_demo` fixture's
/// actions, so live-mode likes there are accepted (and not rolled back).
async fn demo_ack(body: Bytes) -> impl IntoResponse {
    tracing::info!(body = %String::from_utf8_lossy(&body), "demo ack");
    Json(json!({ "ok": true }))
}
