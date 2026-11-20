//! Kits: a layout and the collection it is over, ready to use (decision
//! `element-roles-and-reuse`).
//!
//! A kit's layout is a core layout that reads its collection through
//! inputs (`$in.collection`: the collection's name; `$in.title`: the
//! page's title), so one layout serves any collection of the kit's shape,
//! by reference: a deploy that improves it reaches every pod that uses it.
//! MCP `use_kit` makes the collection (or checks one that exists has the
//! kit's fields), adds its feed to home and answers the embed and the link
//! that show it, with the inputs bound.

use pod_core::layouts::LayoutSource;
use pod_core::{AuthPolicy, CoreLayoutInfo};
use serde_json::{Value, json};

include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// One kit.
pub struct Kit {
    /// Its layout, `@core/<id>` in every pod.
    pub layout: CoreLayoutInfo,
    /// One line: what it is for.
    pub summary: &'static str,
    /// The collection's name when the caller gives none.
    pub collection: &'static str,
    /// The collection it is over: `define_collection`'s `fields`.
    pub fields: fn() -> Value,
}

const fn layout(id: &'static str, title: &'static str) -> CoreLayoutInfo {
    CoreLayoutInfo {
        id,
        title,
        icon: "check_box",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[],
    }
}

/// Every kit the server ships.
pub static KITS: &[Kit] = &[Kit {
    layout: layout("checklist", "Checklist"),
    summary: "Things to tick off: to-dos, a packing list, chores. Add at the top, tap to tick, \
              swipe left to delete; done items move under Done.",
    collection: "checklist",
    fields: || {
        json!([
            { "name": "title", "type": "text" },
            { "name": "done", "type": "bool", "default": false },
            { "name": "notes", "type": "text", "optional": true },
        ])
    },
}];

pub fn kit(id: &str) -> Option<&'static Kit> {
    KITS.iter().find(|k| k.layout.id == id)
}

/// The kits' layout files.
pub fn layout_files() -> &'static [LayoutSource] {
    CHECKLIST_FILES
}
