//! The Construct wire protocol: namespaced UI AST, action intents and the
//! normalized hydration bundle. Pure data types; no I/O.
//!
//! Deserialization is strict: an unknown node `"type"` or `"intent"` is an
//! error. Clients should catch that error and degrade gracefully (e.g. show
//! a placeholder or keep the last good layout); servers should validate
//! layouts before sending them.

pub mod ast;
pub mod bundle;
pub mod intent;
pub mod schema;

pub use ast::{
    ICONS, ImageShape, Indent, ListBinding, NODE_TYPES, Orientation, Screen, SlotSpec, TextStyle,
    Tone, UiNode,
};
pub use bundle::{DataBundle, EntityRecord, WorkspaceSpec};
pub use intent::{ActionIntent, FieldUpdate, PresentationMode};
pub use schema::{SCHEMA_HEADER, SCHEMA_VERSION, required_schema};
