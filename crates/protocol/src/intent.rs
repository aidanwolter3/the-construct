//! Unified intent model (DESIGN.md §2.2B).
//!
//! Actions are decoupled semantic intents; the `"intent"` field selects the
//! variant on the wire.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "intent")]
pub enum ActionIntent {
    #[serde(rename = "action.navigation.open")]
    Navigate {
        target_uri: String,
        presentation: PresentationMode,
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

    #[serde(rename = "action.auth.login")]
    AuthLogin { instance_url: String },

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
    #[serde(rename = "action.mutation.create")]
    Create {
        query_key: String,
        entity_uri: String,
        entity: Value,
        endpoint: String,
        payload: Value,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refresh_workspace: bool,
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

/// One extra field update carried by `ActionIntent::OptimisticMutate.also`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FieldUpdate {
    pub field: String,
    /// Same expression subset as the primary `value_expr`.
    pub value_expr: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationMode {
    Push,
    Modal,
    Replace,
}
