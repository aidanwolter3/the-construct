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
pub mod notify;
pub mod schema;

pub use ast::{
    Align, ChoiceOption, DateFormat, GroupBy, ICONS, ImageShape, Indent, ListBinding, ListFilter,
    ListFocus, ListWhere, NODE_TYPES, Orientation, Screen, SlotSpec, TextCount, TextFormat,
    TextStyle, Tone, UiNode,
};
pub use bundle::{DataBundle, EntityRecord, WorkspaceSpec};
pub use intent::{ActionIntent, FieldUpdate, Position, PresentationMode};
pub use notify::{
    DEVICE_HEADER, Importance, Notification, NotificationChannel, NotificationEvent,
    NotificationList, PushPayload, ReadEvent, channel_key,
};
pub use schema::{
    BASE_FEATURES, ClientCaps, Compatibility, FEATURES, FEATURES_HEADER, LAYOUT_SCHEMA,
    NAMED_SCHEMA, RETIRED, SCHEMA_HEADER, SCHEMA_VERSION, compatibility, drawn_features, feature,
    features_header, implied_features, oldest_layout_schema, parse_features, required_features,
    required_schema,
};
