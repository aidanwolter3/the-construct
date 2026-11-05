//! The `shell` blueprint (decision `blueprints-and-home`): the home layout
//! every pod starts with, a blank page saying "The Construct" and "Ask your
//! agent to build anything". The agent builds the rest into the pod's home.
//! It reads no data, so it has no feeds.
//!
//! Also the `settings` and `inbox` blueprints (decision `layouts-not-apps`):
//! Settings and the notification inbox as layouts over device data, which any
//! layout presents with `action.navigation.open {"layout": "settings"}`.
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

/// The Settings blueprint's id.
pub const SETTINGS: &str = "settings";
/// The inbox blueprint's id.
pub const INBOX: &str = "inbox";

/// Settings as a layout: connection, layouts and versions, sign-ins,
/// notification channels, the server, leaving the pod (device data only).
pub const SETTINGS_APP: AppInfo = AppInfo {
    id: SETTINGS,
    title: "Settings",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[],
};

/// The notification inbox as a layout (device data only).
pub const INBOX_APP: AppInfo = AppInfo {
    id: INBOX,
    title: "Notifications",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[],
};

fn empty() -> WorkspaceSpec {
    WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: protocol::UiNode::Spacer { size_dp: None },
        entities: Default::default(),
        queries: Default::default(),
    }
}

/// The Settings blueprint's adapter.
pub fn settings_adapter() -> Arc<dyn AppAdapter> {
    Arc::new(StaticAdapter::new(
        SETTINGS,
        SETTINGS_APP,
        SETTINGS_FILES,
        &empty(),
    ))
}

/// The inbox blueprint's adapter.
pub fn inbox_adapter() -> Arc<dyn AppAdapter> {
    Arc::new(StaticAdapter::new(INBOX, INBOX_APP, INBOX_FILES, &empty()))
}
