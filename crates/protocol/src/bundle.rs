//! Normalized hydration wire bundle (DESIGN.md §2.2C).
//!
//! `WorkspaceSpec` is the Rust name for the hydration bundle: a layout tree
//! plus a flat, URI-keyed map of entities the layout binds against.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ast::UiNode;

/// The full payload served for a workspace (e.g. `GET /v1/workspaces/home`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorkspaceSpec {
    pub version: String,
    /// Root node; expected to be a `core://Workspace`.
    pub workspace: UiNode,
    /// Entities keyed by URI (e.g. `"pixelfed://posts/101"`).
    #[serde(default)]
    pub entities: BTreeMap<String, EntityRecord>,
}

/// A schemaless entity: an arbitrary JSON object whose fields round-trip
/// untouched. Domain-specific typed views can be layered on later.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(transparent)]
pub struct EntityRecord(pub Map<String, Value>);

impl EntityRecord {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, field: &str) -> Option<&Value> {
        self.0.get(field)
    }

    pub fn get_str(&self, field: &str) -> Option<&str> {
        self.0.get(field).and_then(Value::as_str)
    }

    pub fn get_bool(&self, field: &str) -> Option<bool> {
        self.0.get(field).and_then(Value::as_bool)
    }

    pub fn get_i64(&self, field: &str) -> Option<i64> {
        self.0.get(field).and_then(Value::as_i64)
    }

    pub fn get_f64(&self, field: &str) -> Option<f64> {
        self.0.get(field).and_then(Value::as_f64)
    }

    /// A field holding an array of strings; `None` if absent or any element
    /// is not a string.
    pub fn get_str_list(&self, field: &str) -> Option<Vec<&str>> {
        self.0
            .get(field)?
            .as_array()?
            .iter()
            .map(Value::as_str)
            .collect()
    }

    /// Set a field, returning the previous value if any.
    pub fn set(&mut self, field: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        self.0.insert(field.into(), value.into())
    }

    pub fn fields(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

impl From<Map<String, Value>> for EntityRecord {
    fn from(m: Map<String, Value>) -> Self {
        Self(m)
    }
}

impl TryFrom<Value> for EntityRecord {
    type Error = Value;

    /// Succeeds only for JSON objects; returns the input otherwise.
    fn try_from(v: Value) -> Result<Self, Self::Error> {
        match v {
            Value::Object(m) => Ok(Self(m)),
            other => Err(other),
        }
    }
}
