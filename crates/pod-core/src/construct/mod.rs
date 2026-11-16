//! The content-addressed construct (decision `content-addressed-construct`):
//! immutable elements by hash, names that move, opt-in per pod.
//!
//! A pod is on the old layout system ([`crate::layouts`], [`crate::custom`])
//! until it is switched ([`switch`]); nothing here touches a pod that isn't:
//! its tables are created by the first switch, and every caller checks
//! [`is_switched`] first.
//!
//! - [`store`]: the tables (`objects`, `names`, `name_history`, `follows`)
//!   and their reads and writes.
//! - [`convert`]: the old system's layouts and components as elements and
//!   names (the switch and its dry run).
//! - [`seal`]: the construct for a client's features, and an element
//!   materialized as the renderer draws it.
//! - [`write`]: the MCP tools' writes on a switched pod.
//! - [`http`]: `GET /v1/construct`, `/v1/objects`, `/v1/construct/data`.
//! - [`switch`]: the switch, its dry run and the revert.

pub mod convert;
pub mod http;
pub mod seal;
pub mod store;
pub mod switch;
pub mod write;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::Pod;
use crate::store::Store;

/// The pod setting that says a pod is switched (`"1"`).
pub const SWITCHED_SETTING: &str = "construct.switched";
/// The pod setting counting the construct's writes: what a sealed root is
/// cached under.
pub const GENERATION_SETTING: &str = "construct.generation";
/// The prefix of what ships with the server.
pub const CORE_PREFIX: &str = "@core/";
/// The prefix of the pod's own components.
pub const ME_PREFIX: &str = "@me/";
/// The `LayoutEvent::workspace_id` a move of any name is announced with:
/// home's streams send `event: construct` for it.
pub const CONSTRUCT_EVENT_ID: &str = "@construct";
/// How long [`is_switched`] trusts what it read.
pub const RECHECK_SECS: i64 = 5;
/// History entries kept per name.
pub const MAX_HISTORY: i64 = 500;

/// A pod's construct state, per pod ([`Pod::local`]): whether it is
/// switched (read once) and caches of objects and sealed roots.
#[derive(Default)]
pub struct State {
    /// 0: not read yet, 1: not switched, 2: switched.
    switched: AtomicU8,
    /// When `switched` was read (seconds since the epoch): a switch made by
    /// another process (`server pods switch`) is seen within [`RECHECK_SECS`].
    read_at: std::sync::atomic::AtomicI64,
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

/// Whether the pod is switched to the construct (blocking the first time).
pub fn is_switched(pod: &Pod) -> bool {
    if let Some(known) = known_switched(pod) {
        return known;
    }
    let st = state(pod);
    let on = read_switched(pod.store());
    st.switched.store(if on { 2 } else { 1 }, Ordering::SeqCst);
    st.read_at.store(crate::util::now_secs(), Ordering::SeqCst);
    on
}

/// [`is_switched`] when it is already known (no store read).
pub fn known_switched(pod: &Pod) -> Option<bool> {
    let st = state(pod);
    if crate::util::now_secs() - st.read_at.load(Ordering::SeqCst) > RECHECK_SECS {
        return None;
    }
    match st.switched.load(Ordering::SeqCst) {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// Whether the store says the pod is switched.
pub fn read_switched(store: &Store) -> bool {
    store
        .pod_setting(SWITCHED_SETTING)
        .ok()
        .flatten()
        .is_some_and(|v| v == "1")
}

/// Forget what [`is_switched`] read (after a switch or a revert).
pub(crate) fn reset(pod: &Pod) {
    let st = state(pod);
    st.switched.store(0, Ordering::SeqCst);
    st.roots.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The name a pod layout id has (its id), or a blueprint's (`@core/<id>`).
pub fn blueprint_name(id: &str) -> String {
    format!("{CORE_PREFIX}{id}")
}

/// Whether `name` is the pod's own layout (`home`, `c-…`).
pub fn is_layout_name(name: &str) -> bool {
    crate::custom::is_custom_id(name)
}

/// The `@me/` or `@core/` name of an old component name (`pod:x`, `core:x`).
pub fn component_name(old: &str) -> Option<String> {
    if let Some(b) = old.strip_prefix(crate::components::POD) {
        return Some(format!("{ME_PREFIX}{b}"));
    }
    old.strip_prefix(crate::components::CORE)
        .map(|b| format!("{CORE_PREFIX}{b}"))
}

/// The old component name of a `@me/` or `@core/` name.
pub fn old_component_name(name: &str) -> Option<String> {
    if let Some(b) = name.strip_prefix(ME_PREFIX) {
        return Some(format!("{}{b}", crate::components::POD));
    }
    name.strip_prefix(CORE_PREFIX)
        .map(|b| format!("{}{b}", crate::components::CORE))
}

/// 409 `construct_switched`: an old-system write on a switched pod.
pub fn switched_refusal(what: &str) -> Value {
    serde_json::json!({
        "error": "construct_switched",
        "detail": format!(
            "{what} is part of the old layout system; this pod is switched to the construct \
             (decision content-addressed-construct): use the MCP layout tools"
        ),
    })
}
