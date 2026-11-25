//! The `shell` core layout (decision `blueprints-and-home`): the home layout
//! every pod starts with, a blank page saying "The Construct" and "Ask your
//! agent to build anything". The agent builds the rest into the pod's home.
//! It reads no data, so it has no feeds.
//!
//! Also the `settings` and `inbox` core layouts (decision `layouts-not-apps`):
//! Settings and the notification inbox as layouts over device data, which any
//! layout presents with `action.navigation.open {"layout": "settings"}`.
//!
//! To add a layout version, add `layouts/shell/NNNN.json` in this crate (see
//! `pod_core::layouts`); `build.rs` embeds every file there.

use std::sync::Arc;

use pod_core::layouts::LayoutSource;
use pod_core::source::{EntityDoc, PathSpec, SourceManifest};
use pod_core::{AuthPolicy, CoreLayoutInfo, Source, StaticAdapter};
use protocol::WorkspaceSpec;

// `FILES`: every `layouts/shell/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The adapter name and the core layout's app id ([`pod_core::HOME_CORE_LAYOUT`]).
pub const NAME: &str = pod_core::HOME_CORE_LAYOUT;

/// The core layout. Never listed as an app; each pod's `home` app references it.
pub const SHELL: CoreLayoutInfo = CoreLayoutInfo {
    id: NAME,
    title: "Home",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[],
};

/// The core layout's adapter: its layout files and no data.
pub fn adapter() -> Arc<dyn Source> {
    let empty = WorkspaceSpec {
        version: pod_core::BUNDLE_VERSION.into(),
        workspace: protocol::UiNode::Spacer { size_dp: None },
        entities: Default::default(),
        queries: Default::default(),
    };
    Arc::new(StaticAdapter::new(NAME, SHELL, FILES, &empty).with_catalog(CATALOG))
}

/// What `find_elements` says about this crate's layouts.
const CATALOG: &str = include_str!("../catalog.json");

/// The Settings core layout's id.
pub const SETTINGS: &str = "settings";
/// The inbox core layout's id.
pub const INBOX: &str = "inbox";

/// Settings as a layout: connection, layouts and versions, sign-ins, the
/// pod's jacks, notification channels, the server, leaving the pod.
pub const SETTINGS_APP: CoreLayoutInfo = CoreLayoutInfo {
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
pub const INBOX_APP: CoreLayoutInfo = CoreLayoutInfo {
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
/// `POST /v1/jacks/plug`, `POST /v1/jacks/unplug`), and the connected apps
/// (decision `connected-apps`), which the pod host answers itself (`GET
/// /v1/connected-apps/screen`, `POST /v1/connected-apps/revoke` and
/// `/v1/connected-apps/access`, decision `mcp-access-levels`).
pub const SETTINGS_MANIFEST: SourceManifest = SourceManifest {
    feeds: pod_core::static_app::FEEDS,
    entity_schemes: &["jacks", "connected_apps", "edit_groups", "checkpoints"],
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
        PathSpec {
            pattern: "/v1/connected-apps/screen",
            query_keys: &["connected_apps"],
            doc: "Every app connected through MCP (connected_apps://<id>) and the connected_apps query.",
        },
        PathSpec {
            pattern: "/v1/connected-apps/screen?id={id}",
            query_keys: &[],
            doc: "One connected app (connected_apps://<id>).",
        },
        PathSpec {
            pattern: pod_core::construct::edit_groups::SCREEN_PATH,
            query_keys: &[
                pod_core::construct::edit_groups::GROUPS_QUERY,
                pod_core::construct::edit_groups::CHECKPOINTS_QUERY,
            ],
            doc: "The edit groups of the last 30 days (edit_groups://<id>) and the checkpoints kept (checkpoints://<id>), newest first (decision edit-groups).",
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
        PathSpec {
            pattern: "/v1/connected-apps/revoke",
            query_keys: &[],
            doc: "{id}: disconnect that app (its MCP credential stops working at once).",
        },
        PathSpec {
            pattern: "/v1/connected-apps/access",
            query_keys: &[],
            doc: "{id, access}: what that app may do: read, edit or outward (decision mcp-access-levels).",
        },
        PathSpec {
            pattern: pod_core::construct::edit_groups::APPLY_PATH,
            query_keys: &[],
            doc: "{id}: make a proposed edit group live, exactly as it was staged (refused if its screens moved since).",
        },
        PathSpec {
            pattern: pod_core::construct::edit_groups::REVERT_PATH,
            query_keys: &[],
            doc: "{id} or {since}: undo an applied edit group, or every one since a time (your own edits stay).",
        },
        PathSpec {
            pattern: pod_core::construct::edit_groups::RESTORE_PATH,
            query_keys: &[],
            doc: "{id}: put every screen back as it was at that checkpoint (core layouts stay current; rows untouched). Confirm first.",
        },
    ],
    entities: &[
        EntityDoc {
            uri: "connected_apps://{id}",
            doc: "An app connected to the pod through MCP (OAuth or `server pods mcp`).",
            fields: &[
                ("title", "the name the app gave itself"),
                ("host", "where its OAuth sign-in sent the code"),
                (
                    "detail",
                    "host, when it connected and when it was last used, in words",
                ),
                ("expiry", "when it ends by itself if unused, in words"),
                ("connected", "true until it is disconnected"),
                ("access", "what it may do: read, edit or outward"),
                ("access_label", "its access and what that allows, in words"),
                (
                    "grant_text",
                    "a scoped credential's limits in words (decision mcp-grants); empty without",
                ),
            ],
        },
        EntityDoc {
            uri: "edit_groups://{id}",
            doc: "A change an agent staged or made to the owner's screens, undoable in one step (decision edit-groups).",
            fields: &[
                ("title", "what it does, in a line"),
                ("why", "why it was made"),
                ("state", "proposed, applied, reverted or conflict"),
                ("state_text", "the state in words (\"Changed 3 h ago\")"),
                ("can_undo", "true while it is applied"),
                ("can_apply", "true while it is proposed and additive"),
                ("names_text", "the layouts and components it changes"),
            ],
        },
        EntityDoc {
            uri: "checkpoints://{id}",
            doc: "Home as it was before an applied edit group, to go back to.",
            fields: &[
                ("note", "what came after it"),
                ("when_text", "when, in words"),
                ("restore_text", "the restore button's line"),
            ],
        },
        EntityDoc {
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
        },
    ],
    ..SourceManifest::new("Settings")
};

/// The Settings core layout's adapter.
pub fn settings_adapter() -> Arc<dyn Source> {
    Arc::new(
        StaticAdapter::new(SETTINGS, SETTINGS_APP, SETTINGS_FILES, &empty())
            .with_manifest(SETTINGS_MANIFEST)
            .with_catalog(CATALOG),
    )
}

/// The inbox core layout's adapter.
pub fn inbox_adapter() -> Arc<dyn Source> {
    Arc::new(StaticAdapter::new(INBOX, INBOX_APP, INBOX_FILES, &empty()).with_catalog(CATALOG))
}
