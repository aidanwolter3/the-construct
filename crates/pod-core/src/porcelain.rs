//! Workspace and Screen as porcelain (decision `plumbing-and-porcelain`).
//!
//! `core://Workspace` is still drawn by older apps. An app that draws the
//! plumbing it stands for ([`PLUMBING`]) gets it expanded instead: a
//! `core://Page` whose root is the server component `core:workspace` over
//! the slots, and whose screens are `core:screen` over each screen's content
//! and footer. Both components are ordinary data an agent can read, copy and
//! change, so page chrome needs no app build.

use serde_json::{Value, json};

use protocol::ClientCaps;

use crate::adapter::Catalog;
use crate::parts::{Mode, use_node};
use crate::store::Store;
use crate::validate::LayoutError;

/// The features an app must draw to get a workspace as plumbing.
pub const PLUMBING: [&str; 3] = [
    protocol::feature::PAGE,
    protocol::feature::SCROLL,
    protocol::feature::NAV_DATA,
];

/// The features of a `core://Workspace` itself, which a client that draws
/// [`PLUMBING`] never sees: the pod serves it the expansion instead. New
/// builds don't draw them (`features/removed/`, issue #126).
pub const WORKSPACE_FEATURES: [&str; 3] = protocol::EXPANDED_FEATURES;

/// The component a workspace's page is.
pub const WORKSPACE_COMPONENT: &str = "core:workspace";
/// The component each workspace screen is.
pub const SCREEN_COMPONENT: &str = "core:screen";

/// Whether `client` draws the plumbing a workspace expands to. A caller
/// that sent no features (a tool such as curl, [`ClientCaps::unfiltered`])
/// gets the layout as stored.
pub fn draws_plumbing(client: &ClientCaps) -> bool {
    PLUMBING.iter().all(|f| client.features.contains(*f)) && *client != ClientCaps::unfiltered()
}

/// Whether a workspace's one slot draws the whole page itself (a filling
/// `core://Box`, or an inline `core://Layout` shown always): no title.
fn fills_screen(slots: &[Value]) -> bool {
    let [slot] = slots else { return false };
    let c = &slot["content"];
    match c["type"].as_str() {
        Some("core://Box") => c["fill"].as_bool() == Some(true),
        Some("core://Layout") => {
            c["presentation"].as_str().is_none_or(|p| p == "Inline") && c["show_when"].is_null()
        }
        _ => false,
    }
}

/// The `core://Page` a `core://Workspace` (uses already expanded) stands
/// for, still holding the two component uses.
pub fn page_of(ws: &Value) -> Option<Value> {
    if ws["type"].as_str() != Some("core://Workspace") {
        return None;
    }
    let slots = ws["slots"].as_array().cloned().unwrap_or_default();
    let children: Vec<Value> = slots.iter().map(|s| s["content"].clone()).collect();
    let root = use_node(
        WORKSPACE_COMPONENT,
        None,
        &json!({
            "title": ws["title"],
            "title_scrolls": ws["title_scrolls"].as_bool().unwrap_or(false),
            "fills_screen": fills_screen(&slots),
        }),
        &Value::Array(children),
    );
    let screens: Vec<Value> = ws["screens"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let mut kids = vec![s["content"].clone()];
            if !s["footer"].is_null() {
                kids.push(s["footer"].clone());
            }
            let mut screen = json!({
                "id": s["id"], "route": s["route"], "title": s["title"],
                "root": use_node(SCREEN_COMPONENT, None, &json!({
                    "title": s["title"],
                    "data": s["data"].as_str().unwrap_or(""),
                    "has_footer": !s["footer"].is_null(),
                }), &Value::Array(kids)),
            });
            if !s["data"].is_null() {
                screen["data"] = s["data"].clone();
            }
            screen
        })
        .collect();
    let mut page =
        json!({ "type": "core://Page", "id": ws["id"], "title": ws["title"], "root": root });
    if !screens.is_empty() {
        page["screens"] = Value::Array(screens);
    }
    Some(page)
}

/// A `core://Workspace` (uses already expanded) as plumbing; anything else
/// as it is.
pub fn to_plumbing(store: &Store, catalog: &Catalog, doc: &Value) -> Result<Value, LayoutError> {
    match page_of(doc) {
        Some(page) => crate::parts::expand(store, catalog, &page, None, Mode::Lenient),
        None => Ok(doc.clone()),
    }
}

// ---------- theme ----------

/// The pod setting holding the theme: `{token: {"light", "dark"}}`.
pub const THEME_SETTING: &str = "theme";
/// The entity a custom layout's bundle carries the theme in; the app turns
/// it into `local://theme` for the device's light or dark mode.
pub const THEME_ENTITY: &str = "core://theme";

/// The theme tokens and their defaults `(token, light, dark, what for)`;
/// the app has the same defaults built in.
pub const THEME_TOKENS: &[(&str, &str, &str, &str)] = &[
    ("card_border", "#E4E4EA", "#3A3A42", "card and box borders"),
    (
        "accent",
        "#4A2C82",
        "#D9C8FF",
        "text on accent surfaces, highlights",
    ),
    (
        "accent_surface",
        "#EFE9FA",
        "#2E2645",
        "tinted tags and banners",
    ),
    ("muted_surface", "#EEF0F4", "#2A2C31", "quiet backgrounds"),
    ("page", "#F7F5F0", "#1A1A1C", "a page's own background"),
    ("like", "#ED4956", "#FF6B7A", "likes, favourites"),
    ("unread", "#1E66D0", "#7AB0FF", "unread dots"),
    ("error", "#B3261E", "#F2B8B5", "error text"),
];

fn is_hex(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// The pod's theme: every token with its light and dark colour (defaults
/// where the pod set none). Blocking.
pub fn theme(store: &Store) -> rusqlite::Result<serde_json::Map<String, Value>> {
    let set: Value = store
        .pod_setting(THEME_SETTING)?
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    Ok(THEME_TOKENS
        .iter()
        .map(|(t, light, dark, _)| {
            let pick = |mode: &str, d: &str| {
                set[*t][mode]
                    .as_str()
                    .filter(|c| is_hex(c))
                    .unwrap_or(d)
                    .to_owned()
            };
            (
                (*t).to_owned(),
                json!({ "light": pick("light", light), "dark": pick("dark", dark) }),
            )
        })
        .collect())
}

/// The [`THEME_ENTITY`]: `<token>` light and `<token>_dark`. Blocking.
pub fn theme_entity(store: &Store) -> rusqlite::Result<protocol::EntityRecord> {
    let mut e = protocol::EntityRecord::default();
    for (t, v) in theme(store)? {
        e.set(&t, v["light"].as_str().unwrap_or(""));
        e.set(format!("{t}_dark"), v["dark"].as_str().unwrap_or(""));
    }
    Ok(e)
}

/// Change tokens (`{token: {light?, dark?}}`); `Err`: what is wrong.
/// Blocking.
pub fn set_theme(store: &Store, changes: &Value) -> Result<serde_json::Map<String, Value>, String> {
    let Some(changes) = changes.as_object() else {
        return Err("\"theme\" is {token: {light?, dark?}}".into());
    };
    let mut current = theme(store).map_err(|e| e.to_string())?;
    for (token, v) in changes {
        let Some(slot) = current.get_mut(token) else {
            return Err(format!(
                "no token '{token}' (tokens: {})",
                THEME_TOKENS
                    .iter()
                    .map(|t| t.0)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        for mode in ["light", "dark"] {
            match v.get(mode) {
                None | Some(Value::Null) => {}
                Some(Value::String(c)) if is_hex(c) => slot[mode] = json!(c),
                Some(_) => return Err(format!("{token}.{mode} is #RRGGBB")),
            }
        }
    }
    store
        .set_pod_setting(THEME_SETTING, &Value::Object(current.clone()).to_string())
        .map_err(|e| e.to_string())?;
    Ok(current)
}
