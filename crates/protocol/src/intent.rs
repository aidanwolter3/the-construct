//! Unified intent model (docs/layouts.md "Intents").
//!
//! Actions are decoupled semantic intents; the `"intent"` field selects the
//! variant on the wire.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "intent")]
pub enum ActionIntent {
    #[serde(rename = "action.navigation.open")]
    /// Opens the screen whose route matches `target_uri`, or (feature
    /// `navigate_layout`, decision `layouts-not-apps`) presents `layout`
    /// on the back stack with `params` as its `$route.<name>` values, then
    /// opens its screen matching `target_uri`, if any.
    Navigate {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        target_uri: String,
        presentation: PresentationMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        params: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        animation: Option<Animation>,
    },
    #[serde(rename = "action.navigation.back")]
    NavigateBack,

    #[serde(rename = "action.mutation.optimistic")]
    OptimisticMutate {
        entity_uri: String,
        field: String,
        /// CEL expression, e.g. `"!entity.is_liked"`.
        value_expr: String,
        rollback_value: Value,
        endpoint: String,
        payload: Value,
        /// Extra fields of the same entity updated together with `field`.
        /// Every `value_expr` here (and the primary one) is evaluated against
        /// the PRE-mutation entity; all are applied and rolled back together.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        also: Vec<FieldUpdate>,
    },

    /// Start a sign-in. `app_path` and `token_path` are the pod's endpoints
    /// for registering the client and exchanging the code (docs/DECISIONS.md
    /// D36), e.g. `/v1/auth/pixelfed/app` and `/v1/auth/pixelfed/token`.
    #[serde(rename = "action.auth.login")]
    AuthLogin {
        instance_url: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        app_path: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        token_path: String,
    },

    /// Open a session (schema 7, D46): POST `payload` (form values bound) to
    /// the server path `path`; a 2xx answer's `session_token` becomes the
    /// client's session (like a finished sign-in) and the workspace is
    /// fetched again. How a device enters a pod code
    /// (`/v1/pods/open {token}`) or asks for a temporary pod.
    #[serde(rename = "action.auth.session")]
    OpenSession {
        path: String,
        #[serde(default)]
        payload: Value,
    },

    /// Load a pod path (a screen's `data`) whose answer is a
    /// [`crate::DataBundle`]: its entities are merged and its queries
    /// replace the same keys. The engine caches the result (shown offline)
    /// and reports progress in the snapshot's `loads_json`.
    #[serde(rename = "action.data.load")]
    LoadData { path: String },

    /// Optimistically add a new entity to a list and send it through the
    /// outbox: `entity` is stored at `entity_uri` (a temporary URI, e.g. with
    /// a `$nonce`) and appended to `queries[query_key]` until the pod answers;
    /// then every loaded path that produced `query_key` is reloaded (and the
    /// workspace too with `refresh_workspace`). A 4xx removes it again.
    ///
    /// Schema 9 (D49, D51): `attachments` maps a name to a form-bound list of
    /// staged attachment ids (`{"photos": "$form.photos"}`). The engine
    /// uploads them before the POST; `"$attachments.<name>"` is then the
    /// local attachment entity URIs inside `entity` and the upload ids inside
    /// `payload`. `position` puts the new entity at the start or end of its
    /// list.
    ///
    /// `dismiss` (feature `create_dismiss`) is removed
    /// (docs/decisions/named-vocabulary.md): layouts write `action.sequence
    /// [this create, action.navigation.back]`. The field stays only so the pod
    /// reads the old versions that use it and retires them.
    #[serde(rename = "action.mutation.create")]
    Create {
        query_key: String,
        entity_uri: String,
        entity: Value,
        endpoint: String,
        payload: Value,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refresh_workspace: bool,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        attachments: BTreeMap<String, Value>,
        #[serde(default, skip_serializing_if = "Position::is_end")]
        position: Position,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dismiss: bool,
    },

    /// Feature `action_sequence`: run `steps` in order, as one tap. Each
    /// step is any intent (another sequence too); one `$nonce` is bound for
    /// the whole sequence, so steps can refer to the same new entity.
    /// Navigation steps are resolved by the client, the rest by the engine.
    #[serde(rename = "action.sequence")]
    Sequence { steps: Vec<ActionIntent> },

    /// Feature `local_set`: set `field` (and any `also` fields) of
    /// `entity_uri` on this device only, from expressions evaluated against
    /// the entity as it is. Nothing is sent; the next load of the entity
    /// from the pod replaces it. For view state such as "added" or
    /// "expanded", usually as a step of an `action.sequence`.
    #[serde(rename = "action.local.set")]
    LocalSet {
        entity_uri: String,
        field: String,
        value_expr: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        also: Vec<FieldUpdate>,
    },

    /// Feature `open_link`: open `url` (an `http(s)://` address, usually a
    /// binding such as `$item.tickets_url`) in the device's browser.
    /// Nothing is sent to the pod. A URL of any other scheme does nothing.
    #[serde(rename = "action.link.open")]
    OpenLink { url: String },

    /// Feature `device_action`: something the device does itself, named by
    /// `command` ([`DEVICE_COMMANDS`]): `open_settings` and `open_inbox`
    /// (present the `settings` and `inbox` layouts), `refresh`, `leave_pod`
    /// (forget the pod on this device), `open_notification` (`id`),
    /// `mark_all_read`, `toggle_channel` (`app`, `id`: a notification
    /// channel), `sign_in` (`id`: a sign-in provider; `value`: another
    /// server), `sign_out`, `set_server` (`id`: a server option),
    /// `show_versions` (`app`, a layout: load its versions as device data),
    /// `pin_version` (`id`: a version of that app, shown on this device),
    /// `follow_latest` (that app follows its newest version again).
    /// Nothing is sent to the pod unless the command itself talks to
    /// it. Any layout may use any command (decision `blueprints-and-home`).
    #[serde(rename = "action.device")]
    Device {
        command: String,
        /// The layout a command names (`layout` on the wire is accepted too).
        #[serde(default, alias = "layout", skip_serializing_if = "Option::is_none")]
        app: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// A command's free value: for `sign_in`, the server to sign in to
        /// (usually `$form?.server`; blank: the provider's default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },

    /// Any intent tag this build does not know, e.g. the removed
    /// `action.agent.prompt` (D32), which AI edits could put into layouts
    /// that legacy app versions still hold. Deserializing one drops its
    /// fields instead of failing the whole layout (so such a version still
    /// serves and renders; the action does nothing). Serializes as
    /// `{"intent": "action.unknown"}`. Never author it.
    #[serde(rename = "action.unknown", other)]
    Unknown,
}

impl ActionIntent {
    /// `action.navigation.open` of the screen matching `target_uri`.
    pub fn open(target_uri: impl Into<String>, presentation: PresentationMode) -> Self {
        ActionIntent::Navigate {
            target_uri: target_uri.into(),
            presentation,
            layout: None,
            params: BTreeMap::new(),
            animation: None,
        }
    }

    /// This intent and, for a sequence, every step inside it (depth first).
    pub fn flatten(&self) -> Vec<&ActionIntent> {
        let mut out = vec![self];
        if let ActionIntent::Sequence { steps } = self {
            out.extend(steps.iter().flat_map(ActionIntent::flatten));
        }
        out
    }
}

/// The `action.device` commands this build knows.
pub const DEVICE_COMMANDS: [&str; 13] = [
    "open_settings",
    "open_inbox",
    "refresh",
    "leave_pod",
    "open_notification",
    "mark_all_read",
    "toggle_channel",
    "sign_in",
    "sign_out",
    "set_server",
    "show_versions",
    "pin_version",
    "follow_latest",
];

/// The `action.device` commands that need an `app`, and those that need an
/// `id`.
pub const DEVICE_COMMANDS_WITH_APP: [&str; 2] = ["toggle_channel", "show_versions"];
pub const DEVICE_COMMANDS_WITH_ID: [&str; 5] = [
    "open_notification",
    "toggle_channel",
    "sign_in",
    "set_server",
    "pin_version",
];

/// Commands retired by decision `layouts-not-apps` (feature
/// `device_open_app`, removed): old layouts still parse, they do nothing.
pub const RETIRED_DEVICE_COMMANDS: [&str; 2] = ["open_app", "show"];

/// One extra field update carried by `ActionIntent::OptimisticMutate.also`
/// (and `ActionIntent::LocalSet.also`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FieldUpdate {
    pub field: String,
    /// Same expression subset as the primary `value_expr`.
    pub value_expr: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationMode {
    Push,
    Modal,
    Replace,
    /// A layout as a bottom sheet (feature `navigate_layout`).
    Sheet,
    /// A layout as a side panel from the start edge (feature `navigate_layout`).
    Panel,
}

/// How a presented layout comes in and goes out (decision
/// `layouts-not-apps`): named only; the platform picks durations.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Animation {
    None,
    Fade,
    Slide,
    SheetUp,
}

/// Where `action.mutation.create` puts its new entity in the list (schema 9).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Start,
    #[default]
    End,
}

impl Position {
    fn is_end(&self) -> bool {
        *self == Position::End
    }
}
