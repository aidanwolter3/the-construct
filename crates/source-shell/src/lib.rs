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
use pod_core::source::{EntityDoc, PathSpec, SourceManifest};
use pod_core::{AuthPolicy, BlueprintInfo, Source, StaticAdapter};
use protocol::WorkspaceSpec;

// `FILES`: every `layouts/shell/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The adapter name and the blueprint's app id ([`pod_core::HOME_BLUEPRINT`]).
pub const NAME: &str = pod_core::HOME_BLUEPRINT;

/// The blueprint. Never listed as an app; each pod's `home` app references it.
pub const SHELL: BlueprintInfo = BlueprintInfo {
    id: NAME,
    title: "Home",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[],
};

/// The blueprint's adapter: its layout files and no data.
pub fn adapter() -> Arc<dyn Source> {
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

/// Settings as a layout: connection, layouts and versions, sign-ins, the
/// pod's jacks, notification channels, the server, leaving the pod.
pub const SETTINGS_APP: BlueprintInfo = BlueprintInfo {
    id: SETTINGS,
    title: "Settings",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[],
};

/// The notification inbox as a layout: the pod's `notifications`
/// collection (decision `notifications-in-a-collection`), and the device's
/// own notification data for builds before it.
pub const INBOX_APP: BlueprintInfo = BlueprintInfo {
    id: INBOX,
    title: "Notifications",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[("collections", "notifications")],
};

fn empty() -> WorkspaceSpec {
    WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: protocol::UiNode::Spacer { size_dp: None },
        entities: Default::default(),
        queries: Default::default(),
    }
}

/// What Settings reads and writes on the pod: the jacks (decision
/// `jacks-in-settings`), served by `pod_core` (`GET /v1/jacks/screen`,
/// `POST /v1/jacks/plug`, `POST /v1/jacks/unplug`).
pub const SETTINGS_MANIFEST: SourceManifest = SourceManifest {
    feeds: pod_core::static_app::FEEDS,
    entity_schemes: &["jacks"],
    data_paths: &[
        PathSpec {
            pattern: "/v1/jacks/screen",
            query_keys: &[pod_core::jacks::SCREEN_QUERY],
            doc: "Every jack of the pod (jacks://<id>) and the jacks query.",
        },
        PathSpec {
            pattern: "/v1/jacks/screen?id={id}",
            query_keys: &[],
            doc: "One jack (jacks://<id>).",
        },
    ],
    actions: &[
        PathSpec {
            pattern: "/v1/jacks/plug",
            query_keys: &[],
            doc: "{id, config?}: plug a switch or config jack; blank values keep what is stored.",
        },
        PathSpec {
            pattern: "/v1/jacks/unplug",
            query_keys: &[],
            doc: "{id}: unplug a switch or config jack (a config jack forgets its values).",
        },
    ],
    entities: &[EntityDoc {
        uri: "jacks://{id}",
        doc: "A jack as Settings shows it; a secret shows only whether it is set.",
        fields: &[
            ("title", "its name"),
            ("summary", "what plugging it lets the pod do"),
            ("kind", "switch, config or account"),
            ("state", "plugged, unplugged or locked"),
            (
                "state_label",
                "the state in words, with its provider or account",
            ),
            ("plugged, locked, is_switch, is_config, is_account", "flags"),
            (
                "can_plug, can_unplug",
                "whether Settings offers Plug in / Unplug",
            ),
            ("how", "an account jack: where it is signed in"),
            (
                "has_<field>",
                "a config jack takes that field (provider, api_key, base_url)",
            ),
            ("<field>", "a config value now set (not a secret)"),
            (
                "<field>_set, <field>_hint",
                "a secret: whether it is set, in words",
            ),
        ],
    }],
    ..SourceManifest::new("Settings")
};

/// The Settings blueprint's adapter.
pub fn settings_adapter() -> Arc<dyn Source> {
    Arc::new(
        StaticAdapter::new(SETTINGS, SETTINGS_APP, SETTINGS_FILES, &empty())
            .with_manifest(SETTINGS_MANIFEST),
    )
}

/// The inbox blueprint's adapter.
pub fn inbox_adapter() -> Arc<dyn Source> {
    Arc::new(StaticAdapter::new(INBOX, INBOX_APP, INBOX_FILES, &empty()))
}
