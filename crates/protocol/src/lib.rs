//! The Construct wire protocol: namespaced UI AST, action intents and the
//! normalized hydration bundle. Pure data types; no I/O.
//!
//! Deserialization is strict: an unknown node `"type"` or `"intent"` is an
//! error. Clients should catch that error and degrade gracefully (e.g. show
//! a placeholder or keep the last good layout); servers should validate
//! layouts before sending them.

pub mod ast;
pub mod build;
pub mod bundle;
pub mod expr;
pub mod features;
pub mod intent;
pub mod notify;
pub mod public_rows;

pub use ast::{
    Align, BorderStyle, ButtonSize, ButtonStyle, CAPS_TRACKING, ChoiceOption, DateFormat, DateTest,
    DockTab, DrawerPage, EmbedPresentation, GroupBy, ICON_PATH_PREFIX, ICONS, ImageShape, Indent,
    Justify, ListBinding, ListFilter, ListFocus, ListWhere, MAX_ICON_PATH, MAX_TEXT_LINES,
    NODE_TYPES, Orientation, PageScreen, QueryCount, Screen, SlotSpec, TextCount, TextFormat,
    TextStyle, TextWeight, Tone, UiNode, WhereClause, icon_path,
};
pub use build::{
    BUILD_HEADER, BuildId, DEVICE_NAME_HEADER, PLATFORMS, build, build_header, parse_build_header,
    platform,
};
pub use bundle::{DataBundle, EntityRecord, WorkspaceSpec};
pub use features::{
    ClientCaps, Compatibility, EXPANDED_FEATURES, FEATURE_DOCS, FEATURES, FEATURES_HEADER,
    REMOVED_FEATURES, feature, features_header, is_retired, parse_features, required_features,
};
pub use intent::{
    ActionIntent, Animation, DEVICE_COMMANDS, DEVICE_COMMANDS_WITH_APP, DEVICE_COMMANDS_WITH_ID,
    DEVICE_OWNED_LOCAL, Edge, FieldUpdate, Position, PresentationMode, is_device_owned,
};
pub use notify::{
    DEVICE_HEADER, Importance, Notification, NotificationChannel, NotificationEvent,
    NotificationList, PushPayload, ROOT_HEADER, ROOT_MANIFEST, ReadEvent, channel_key,
    channel_layout_in, notification_layout_in,
};
