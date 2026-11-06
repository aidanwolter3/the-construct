//! The Construct wire protocol: namespaced UI AST, action intents and the
//! normalized hydration bundle. Pure data types; no I/O.
//!
//! Deserialization is strict: an unknown node `"type"` or `"intent"` is an
//! error. Clients should catch that error and degrade gracefully (e.g. show
//! a placeholder or keep the last good layout); servers should validate
//! layouts before sending them.

pub mod ast;
pub mod bundle;
pub mod features;
pub mod intent;
pub mod notify;

pub use ast::{
    Align, ChoiceOption, DateFormat, DockTab, DrawerPage, EmbedPresentation, GroupBy, ICONS,
    ImageShape, Indent, ListBinding, ListFilter, ListFocus, ListWhere, NODE_TYPES, Orientation,
    PageScreen, Screen, SlotSpec, TextCount, TextFormat, TextStyle, Tone, UiNode, WhereClause,
};
pub use bundle::{DataBundle, EntityRecord, WorkspaceSpec};
pub use features::{
    ClientCaps, Compatibility, FEATURE_DOCS, FEATURES, FEATURES_HEADER, REMOVED_FEATURES, feature,
    features_header, parse_features, required_features,
};
pub use intent::{
    ActionIntent, Animation, DEVICE_COMMANDS, DEVICE_COMMANDS_WITH_APP, DEVICE_COMMANDS_WITH_ID,
    Edge, FieldUpdate, Position, PresentationMode,
};
pub use notify::{
    DEVICE_HEADER, Importance, Notification, NotificationChannel, NotificationEvent,
    NotificationList, PushPayload, ReadEvent, channel_key,
};
