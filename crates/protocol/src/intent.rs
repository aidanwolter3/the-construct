//! Unified intent model (DESIGN.md §2.2B).
//!
//! Actions are decoupled semantic intents; the `"intent"` field selects the
//! variant on the wire.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

    #[serde(rename = "action.agent.prompt")]
    PromptAgent { prompt_text: String },
}

/// One extra field update carried by `ActionIntent::OptimisticMutate.also`.
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
}
