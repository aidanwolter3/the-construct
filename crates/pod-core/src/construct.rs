//! The content-addressed construct (decision `content-addressed-construct`):
//! immutable elements by hash, names that move. Every pod is on it: a pod
//! the old layout system still held is converted when it opens
//! ([`crate::migrations::layout_system`]).
//!
//! - [`store`]: the tables (`objects`, `names`, `name_history`, `follows`)
//!   and their reads and writes.
//! - [`core`]: what ships with the server (`@core/` names), from this
//!   build's files, and a new pod's home.
//! - [`seal`]: the construct for a client's features, and an element
//!   materialized as the renderer draws it.
//! - [`write`]: the MCP tools' writes.
//! - [`http`]: `GET /v1/construct`, `/v1/objects`, `/v1/construct/data`,
//!   `/v1/construct/names`.
//! - [`events`]: `GET /v1/construct/events`.
//! - [`share`]: publishing to the host's shelf and adopting from it
//!   (decision `sharing`).
//! - [`adopted`]: what an adopted element may reach (decision
//!   `adoption-capabilities`).

#[path = "construct_adopted.rs"]
pub mod adopted;
#[path = "construct_core.rs"]
pub mod core;
#[path = "construct_events.rs"]
pub mod events;
#[path = "construct_http.rs"]
pub mod http;
#[path = "construct_seal.rs"]
pub mod seal;
#[path = "construct_share.rs"]
pub mod share;
#[path = "construct_store.rs"]
pub mod store;
#[path = "construct_trace.rs"]
pub mod trace;
#[path = "construct_write.rs"]
pub mod write;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::Pod;
use crate::store::Store;

/// The pod setting that says a pod is on the construct (`"1"`): set by the
/// conversion of an old pod, and by every open after it.
pub const SWITCHED_SETTING: &str = "construct.switched";
/// The pod setting counting the construct's writes: what a sealed root is
/// cached under.
pub const GENERATION_SETTING: &str = "construct.generation";
/// The prefix of what ships with the server.
pub const CORE_PREFIX: &str = "@core/";
/// The prefix of the pod's own components.
pub const ME_PREFIX: &str = "@me/";
/// History entries kept per name.
pub const MAX_HISTORY: i64 = 500;

/// A pod's construct state, per pod ([`Pod::local`]): caches of objects and
/// sealed roots.
#[derive(Default)]
pub struct State {
    /// Objects read from the store, by id.
    pub(crate) objects: Mutex<HashMap<String, Arc<Value>>>,
    /// Sealed roots by `(generation, features)`.
    pub(crate) roots: Mutex<HashMap<(i64, String), String>>,
    /// Serializes the construct's writes.
    pub(crate) write_lock: Mutex<()>,
}

/// This pod's construct state.
pub fn state(pod: &Pod) -> Arc<State> {
    pod.local::<State>()
}

/// Whether the store says the pod is on the construct.
pub fn read_switched(store: &Store) -> bool {
    store
        .pod_setting(SWITCHED_SETTING)
        .ok()
        .flatten()
        .is_some_and(|v| v == "1")
}

/// A core layout's name (`@core/<id>`).
pub fn core_layout_name(id: &str) -> String {
    format!("{CORE_PREFIX}{id}")
}

/// Whether `name` is the pod's own layout (`home`, `c-…`).
pub fn is_layout_name(name: &str) -> bool {
    crate::layouts::is_layout_id(name)
}

/// The `@me/` or `@core/` name of a component's other spelling (`pod:x`,
/// `core:x`), which a write accepts.
pub fn component_name(old: &str) -> Option<String> {
    if let Some(b) = old.strip_prefix(crate::components::POD) {
        return Some(format!("{ME_PREFIX}{b}"));
    }
    old.strip_prefix(crate::components::CORE)
        .map(|b| format!("{CORE_PREFIX}{b}"))
}

/// Open the construct of a pod (every open, before it serves): convert a
/// pod the old layout system still held (with its dry run logged), move
/// the `@core/` names to this build's files, give a new pod its home, and
/// drop the old tables. Blocking. `Err` stops the pod from opening.
pub fn open(
    store: &Store,
    catalog: &crate::Catalog,
) -> Result<Option<crate::migrations::layout_system::Report>, String> {
    let built = core::load(catalog)?;
    let report = crate::migrations::layout_system::open(store, catalog).map_err(|e| {
        tracing::error!(error = %e, "the pod could not be moved to the construct; it stays closed and its old tables are kept");
        e
    })?;
    let moved = core::refresh(store, catalog, &built)
        .map_err(|e| format!("moving the construct's @core names to this build: {e}"))?;
    if !moved.is_empty() {
        tracing::info!(moved = moved.len(), "the construct follows this build");
    }
    core::seed_home(store, catalog)?;
    store
        .set_pod_setting(SWITCHED_SETTING, "1")
        .map_err(|e| e.to_string())?;
    store
        .migrate_adapter(
            crate::migrations::layout_system::SCOPE,
            crate::migrations::layout_system::STEPS,
        )
        .map_err(|e| format!("dropping the old layout tables: {e}"))?;
    Ok(report)
}
