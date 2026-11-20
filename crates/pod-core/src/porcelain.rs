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

use crate::store::Store;

/// A component use as a node: `{"type": "core://Use", "component": name,
/// "args", "children"}` (a write makes it a reference).
fn use_node(name: &str, args: &Value, children: &Value) -> Value {
    json!({ "type": crate::components::USE_TYPE, "component": name, "args": args, "children": children })
}

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
/// `core://Box`, an inline `core://Layout` shown always, or a
/// `core://Gesture` around one): no title.
pub fn fills_screen(slots: &[Value]) -> bool {
    let [slot] = slots else { return false };
    fills(&slot["content"])
}

fn fills(c: &Value) -> bool {
    match c["type"].as_str() {
        Some("core://Box") => c["fill"].as_bool() == Some(true),
        Some("core://Layout") => {
            c["presentation"].as_str().is_none_or(|p| p == "Inline") && c["show_when"].is_null()
        }
        Some("core://Gesture") => fills(&c["content"]),
        _ => false,
    }
}

/// The `core://Page` a `core://Workspace` (uses already expanded) stands
/// for, still holding the two component uses.
pub fn page_of(ws: &Value) -> Option<Value> {
    let slots = ws["slots"].as_array().cloned().unwrap_or_default();
    page_of_with(ws, fills_screen(&slots))
}

/// [`page_of`], with whether the workspace fills the screen worked out by the
/// caller (from its expansion: the construct converts a workspace whose
/// slots still hold uses, decision `content-addressed-construct`).
pub fn page_of_with(ws: &Value, fills: bool) -> Option<Value> {
    if ws["type"].as_str() != Some("core://Workspace") {
        return None;
    }
    let slots = ws["slots"].as_array().cloned().unwrap_or_default();
    let children: Vec<Value> = slots.iter().map(|s| s["content"].clone()).collect();
    let root = use_node(
        WORKSPACE_COMPONENT,
        &json!({
            "title": ws["title"],
            "title_scrolls": ws["title_scrolls"].as_bool().unwrap_or(false),
            "fills_screen": fills,
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
                "root": use_node(SCREEN_COMPONENT, &json!({
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
    (
        "card",
        "#FFFFFF",
        "#242428",
        "cards, rows and boxes on a page; dialogs and sheets",
    ),
    (
        "muted_text",
        "#6B6B76",
        "#A3A3AD",
        "secondary text, chevrons, quiet icons",
    ),
    (
        "success",
        "#137333",
        "#81C995",
        "text of good news (done, online)",
    ),
    (
        "success_surface",
        "#E6F4EA",
        "#1E3325",
        "tags and banners of good news",
    ),
    (
        "error_surface",
        "#FCE8E6",
        "#3D2321",
        "tags and banners of errors",
    ),
    (
        "highlight",
        "#FFF3CF",
        "#3B3320",
        "a focused or found item's tint",
    ),
    // Chart series, in an order whose neighbours stay apart for colour-blind
    // eyes, light and dark (decision `chart-colours`).
    (
        "chart_1",
        "#2A78D6",
        "#3987E5",
        "a chart's first series (blue)",
    ),
    (
        "chart_2",
        "#D95926",
        "#D95926",
        "a chart's second series (orange)",
    ),
    (
        "chart_3",
        "#199E70",
        "#199E70",
        "a chart's third series (aqua)",
    ),
    (
        "chart_4",
        "#C98500",
        "#C98500",
        "a chart's fourth series (yellow)",
    ),
    (
        "chart_5",
        "#D55181",
        "#D55181",
        "a chart's fifth series (magenta)",
    ),
    (
        "chart_6",
        "#008300",
        "#0CA30C",
        "a chart's sixth series (green)",
    ),
    (
        "chart_7",
        "#7A6BE0",
        "#9085E9",
        "a chart's seventh series (violet)",
    ),
    (
        "chart_8",
        "#E34948",
        "#E66767",
        "a chart's eighth series (red)",
    ),
    (
        "chart_other",
        "#9AA0A6",
        "#80858D",
        "a chart's other series (grey): the rest past eight, or unknown",
    ),
];

fn is_hex(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// At most this many tokens of the pod's own (decision
/// `custom-theme-tokens`).
pub const MAX_CUSTOM_TOKENS: usize = 40;

/// Whether `name` can be a token of the pod's own: `[a-z][a-z0-9_]{0,31}`,
/// not a built-in token and not ending in `_dark` (the flat form's key for
/// a token's dark value).
pub fn custom_token_name(name: &str) -> Result<(), String> {
    let ok = name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !ok {
        return Err(format!(
            "'{name}' isn't a token name: a lowercase letter, then up to 31 lowercase letters, digits or _"
        ));
    }
    if name.ends_with("_dark") {
        return Err(format!(
            "'{name}' ends in _dark, which names a token's dark value; set {{\"{}\": {{\"dark\": ...}}}} instead",
            name.trim_end_matches("_dark")
        ));
    }
    Ok(())
}

fn is_builtin(token: &str) -> bool {
    THEME_TOKENS.iter().any(|t| t.0 == token)
}

/// The pod's theme: every built-in token with its light and dark colour
/// (defaults where the pod set none), then the pod's own tokens, each with
/// both values (one set mode stands in for the other). Blocking.
pub fn theme(store: &Store) -> rusqlite::Result<serde_json::Map<String, Value>> {
    let set: Value = store
        .pod_setting(THEME_SETTING)?
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let colour = |t: &str, mode: &str| {
        set[t][mode]
            .as_str()
            .filter(|c| is_hex(c))
            .map(str::to_owned)
    };
    let mut out: serde_json::Map<String, Value> = THEME_TOKENS
        .iter()
        .map(|(t, light, dark, _)| {
            let light = colour(t, "light").unwrap_or_else(|| (*light).to_owned());
            let dark = colour(t, "dark").unwrap_or_else(|| (*dark).to_owned());
            ((*t).to_owned(), json!({ "light": light, "dark": dark }))
        })
        .collect();
    if let Some(m) = set.as_object() {
        for t in m.keys().filter(|t| !is_builtin(t)) {
            let (light, dark) = (colour(t, "light"), colour(t, "dark"));
            if let Some(either) = light.clone().or_else(|| dark.clone()) {
                let light = light.unwrap_or_else(|| either.clone());
                let dark = dark.unwrap_or(either);
                out.insert(t.clone(), json!({ "light": light, "dark": dark }));
            }
        }
    }
    Ok(out)
}

/// The pod's own tokens' names, in name order. Blocking.
pub fn custom_tokens(store: &Store) -> rusqlite::Result<Vec<String>> {
    Ok(theme(store)?
        .keys()
        .filter(|t| !is_builtin(t))
        .cloned()
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

/// The tokens the pod set itself (`{token: {light?, dark?}}`); the rest
/// follow the defaults, so a later default reaches the pod. Blocking.
fn theme_set(store: &Store) -> rusqlite::Result<serde_json::Map<String, Value>> {
    Ok(store
        .pod_setting(THEME_SETTING)?
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default())
}

/// Change tokens: `{token: {light?, dark?}}`, or the flat form the bundle
/// carries (`{token: "#RRGGBB", token_dark: "#RRGGBB"}`); `{token: null}`
/// removes one of the pod's own tokens or puts a built-in back to its
/// defaults. A name that isn't built in adds a token of the pod's own
/// (decision `custom-theme-tokens`). `Err`: what is wrong. Blocking.
pub fn set_theme(store: &Store, changes: &Value) -> Result<serde_json::Map<String, Value>, String> {
    let Some(changes) = changes.as_object() else {
        return Err("\"theme\" is {token: {light?, dark?}}".into());
    };
    let mut set = theme_set(store).map_err(|e| e.to_string())?;
    for (key, v) in changes {
        let (token, flat_mode) = match key.strip_suffix("_dark") {
            Some(t) if v.is_string() => (t, Some("dark")),
            _ if v.is_string() => (key.as_str(), Some("light")),
            _ => (key.as_str(), None),
        };
        if v.is_null() {
            set.remove(key.as_str());
            continue;
        }
        if !is_builtin(token) {
            custom_token_name(token)?;
            if !set.contains_key(token)
                && set.keys().filter(|t| !is_builtin(t)).count() >= MAX_CUSTOM_TOKENS
            {
                return Err(format!(
                    "a pod has at most {MAX_CUSTOM_TOKENS} tokens of its own; remove one with {{\"<token>\": null}}"
                ));
            }
        }
        let slot = set.entry(token.to_owned()).or_insert_with(|| json!({}));
        if !slot.is_object() {
            *slot = json!({});
        }
        let modes: Vec<(&str, &Value)> = match flat_mode {
            Some(m) => vec![(m, v)],
            None => ["light", "dark"]
                .into_iter()
                .filter_map(|m| v.get(m).map(|c| (m, c)))
                .collect(),
        };
        if flat_mode.is_none() && !v.is_object() {
            return Err(format!("{token} is {{light?, dark?}} or #RRGGBB"));
        }
        for (mode, c) in modes {
            match c {
                Value::Null => {}
                Value::String(c) if is_hex(c) => slot[mode] = json!(c),
                _ => return Err(format!("{token}.{mode} is #RRGGBB")),
            }
        }
    }
    // A token of the pod's own with neither colour isn't kept.
    set.retain(|t, v| {
        is_builtin(t)
            || ["light", "dark"]
                .iter()
                .any(|m| v.get(*m).is_some_and(Value::is_string))
    });
    store
        .set_pod_setting(THEME_SETTING, &Value::Object(set).to_string())
        .map_err(|e| e.to_string())?;
    theme(store).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_theme_keeps_only_what_was_set() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("server.db")).unwrap();
        // The nested form and the flat form the bundle carries.
        set_theme(&store, &json!({"accent": {"light": "#112233"}})).unwrap();
        let t = set_theme(&store, &json!({"card_dark": "#101012", "page": "#FAFAFA"})).unwrap();
        assert_eq!(t["accent"], json!({"light": "#112233", "dark": "#D9C8FF"}));
        assert_eq!(t["card"], json!({"light": "#FFFFFF", "dark": "#101012"}));
        assert_eq!(t["page"]["light"], "#FAFAFA");
        // Unset tokens aren't stored, so they follow the defaults.
        let stored = theme_set(&store).unwrap();
        assert_eq!(stored.len(), 3, "{stored:?}");
        assert!(stored["accent"].get("dark").is_none());
        for bad in [
            json!({"Nope": "#101010"}),
            json!({"9lives": "#101010"}),
            json!({"card_dark": "white"}),
            json!({"card": 3}),
        ] {
            assert!(set_theme(&store, &bad).is_err(), "{bad}");
        }
        let e = theme_entity(&store).unwrap();
        assert_eq!(e.get_str("card_dark"), Some("#101012"));
        assert_eq!(e.get_str("highlight"), Some("#FFF3CF"));
    }

    #[test]
    fn a_pod_defines_its_own_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("server.db")).unwrap();
        let t = set_theme(
            &store,
            &json!({"dock": {"light": "#1C1B19", "dark": "#2A2B27"}, "eat": "#4F7F3B", "move_dark": "#8DB4E6"}),
        )
        .unwrap();
        assert_eq!(t["dock"], json!({"light": "#1C1B19", "dark": "#2A2B27"}));
        // One mode set: it stands in for the other.
        assert_eq!(t["eat"], json!({"light": "#4F7F3B", "dark": "#4F7F3B"}));
        assert_eq!(t["move"], json!({"light": "#8DB4E6", "dark": "#8DB4E6"}));
        assert_eq!(custom_tokens(&store).unwrap(), vec!["dock", "eat", "move"]);
        let e = theme_entity(&store).unwrap();
        assert_eq!(e.get_str("dock"), Some("#1C1B19"));
        assert_eq!(e.get_str("dock_dark"), Some("#2A2B27"));
        assert_eq!(e.get_str("eat_dark"), Some("#4F7F3B"));
        // A name ending in _dark is the flat form's dark key, never a token.
        assert!(set_theme(&store, &json!({"shade_dark": {"light": "#101010"}})).is_err());
        // null removes a token of the pod's own, and resets a built-in.
        set_theme(&store, &json!({"accent": "#112233"})).unwrap();
        let t = set_theme(&store, &json!({"eat": null, "accent": null})).unwrap();
        assert!(t.get("eat").is_none());
        assert_eq!(t["accent"]["light"], "#4A2C82");
        // The cap counts only the pod's own tokens.
        let many: serde_json::Map<String, Value> = (0..MAX_CUSTOM_TOKENS)
            .map(|i| (format!("t{i}"), json!("#101010")))
            .collect();
        assert!(set_theme(&store, &Value::Object(many)).is_err());
        // Existing ones can still change at the cap.
        let fill: serde_json::Map<String, Value> = (0..MAX_CUSTOM_TOKENS - 2)
            .map(|i| (format!("t{i}"), json!("#101010")))
            .collect();
        set_theme(&store, &Value::Object(fill)).unwrap();
        assert!(set_theme(&store, &json!({"dock": "#000000"})).is_ok());
        assert!(set_theme(&store, &json!({"one_more": "#000000"})).is_err());
    }

    #[test]
    fn a_gesture_around_a_filling_node_fills_the_screen() {
        let slot = |content: Value| vec![json!({"slot_id": "s", "content": content})];
        let filling =
            json!({"type": "core://Box", "fill": true, "content": {"type": "core://Divider"}});
        assert!(fills_screen(&slot(filling.clone())));
        let gesture = |content: Value| {
            json!({"type": "core://Gesture", "content": content,
            "on_swipe_down": {"intent": "action.navigation.back"}})
        };
        assert!(fills_screen(&slot(gesture(filling))));
        assert!(!fills_screen(&slot(gesture(
            json!({"type": "core://Divider"})
        ))));
    }
}
