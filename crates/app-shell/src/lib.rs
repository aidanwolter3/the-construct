//! The `shell` blueprint (decision `blueprints-and-home`): the home layout
//! every pod starts with, a blank page saying "The Construct" and "Ask your
//! agent to build anything". The agent builds the rest into the pod's home.
//! It reads no data, so it has no feeds.
//!
//! To add a layout version, add `layouts/shell/NNNN.json` in this crate (see
//! `pod_core::layouts`); `build.rs` embeds every file there.

use std::sync::Arc;

use pod_core::layouts::LayoutSource;
use pod_core::{AppAdapter, AppInfo, AuthPolicy, StaticAdapter};
use protocol::WorkspaceSpec;

// `FILES`: every `layouts/shell/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The adapter name and the blueprint's app id ([`pod_core::HOME_BLUEPRINT`]).
pub const NAME: &str = pod_core::HOME_BLUEPRINT;

/// The blueprint. Never listed as an app; each pod's `home` app references it.
pub const SHELL: AppInfo = AppInfo {
    id: NAME,
    title: "Home",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[],
};

/// The blueprint's adapter: its layout files and no data.
pub fn adapter() -> Arc<dyn AppAdapter> {
    let empty = WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: protocol::UiNode::Spacer { size_dp: None },
        entities: Default::default(),
        queries: Default::default(),
    };
    Arc::new(StaticAdapter::new(NAME, SHELL, FILES, &empty))
}
