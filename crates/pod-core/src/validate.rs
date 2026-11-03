//! Validation of developer-authored layout files (`layouts/<app>/NNNN.json`,
//! see [`crate::layouts`]): structure, bounds and the rules every client relies
//! on. Generic: nothing here knows an app.

use protocol::{ActionIntent, ICONS, UiNode};
use serde_json::Value;

/// Validation limits.
pub const MAX_SLOTS: usize = 50;
pub const MAX_LAYOUT_BYTES: usize = 256 * 1024;
/// Deepest JSON nesting (objects + arrays) a layout may have. Clients parse and
/// render recursively (serde's default limit is 128), so a deep tree must never
/// reach them.
pub const MAX_LAYOUT_DEPTH: usize = 48;
/// Aspect ratio range (width / height) for images and carousels.
pub const ASPECT_RATIO_RANGE: std::ops::RangeInclusive<f32> = 0.2..=5.0;
/// Largest padding / spacing / indent step.
pub const MAX_PADDING_DP: u16 = 64;
/// Largest image / icon size.
pub const MAX_SIZE_DP: u16 = 512;

/// Nesting depth of objects/arrays (a scalar root is 0). Iterative, so a deep
/// document cannot overflow the stack here.
fn json_depth(v: &Value) -> usize {
    let mut max = 0;
    let mut stack = vec![(v, 1usize)];
    while let Some((v, d)) = stack.pop() {
        let children: Box<dyn Iterator<Item = &Value>> = match v {
            Value::Array(a) => Box::new(a.iter()),
            Value::Object(m) => Box::new(m.values()),
            _ => continue,
        };
        max = max.max(d);
        if d > MAX_LAYOUT_DEPTH {
            break; // deep enough to reject; don't walk the rest
        }
        stack.extend(children.map(|c| (c, d + 1)));
    }
    max
}

/// Depth (first: everything after it recurses) and size limits.
fn check_bounds(doc: &Value) -> Result<(), String> {
    if json_depth(doc) > MAX_LAYOUT_DEPTH {
        return Err(format!(
            "layout nests deeper than {MAX_LAYOUT_DEPTH} levels"
        ));
    }
    let size = serde_json::to_vec(doc).map_or(usize::MAX, |b| b.len());
    if size > MAX_LAYOUT_BYTES {
        return Err(format!("layout is {size} bytes (max {MAX_LAYOUT_BYTES})"));
    }
    Ok(())
}

/// Structural validation of a workspace layout: depth and size bounds,
/// deserializes as a [`UiNode`], the root is a `core://Workspace` with id
/// `expected_id`, 1..=[`MAX_SLOTS`] slots with unique `slot_id`s, numeric
/// fields in range, known icon names, valid screens, and every endpoint and
/// data path a pod path (`/v1/...`). Unknown node types are not rejected here
/// (see [`unknown_node_types`]).
pub fn validate_structure(expected_id: &str, doc: &Value) -> Result<UiNode, String> {
    check_bounds(doc)?;
    let node: UiNode = serde_json::from_value(doc.clone())
        .map_err(|e| format!("layout does not deserialize as a UiNode: {e}"))?;
    let UiNode::Workspace { id, slots, .. } = &node else {
        return Err("root must stay a core://Workspace".to_owned());
    };
    if id != expected_id {
        return Err(format!(
            "workspace id changed from '{expected_id}' to '{id}'"
        ));
    }
    if slots.is_empty() {
        return Err("workspace must keep at least one slot".to_owned());
    }
    if slots.len() > MAX_SLOTS {
        return Err(format!("{} slots (max {MAX_SLOTS})", slots.len()));
    }
    let mut ids = std::collections::HashSet::new();
    if let Some(dup) = slots.iter().find(|s| !ids.insert(s.slot_id.as_str())) {
        return Err(format!("duplicate slot_id '{}'", dup.slot_id));
    }
    for n in node.walk() {
        check_values(n)?;
    }
    check_screens(&node)?;
    for intent in intents(&node) {
        let path = match intent {
            ActionIntent::OptimisticMutate { endpoint, .. }
            | ActionIntent::Create { endpoint, .. } => endpoint,
            ActionIntent::LoadData { path } => path,
            ActionIntent::AuthLogin {
                app_path,
                token_path,
                ..
            } => {
                for p in [app_path, token_path] {
                    if !p.starts_with("/v1/") {
                        return Err(format!(
                            "sign-in path '{p}' must be a pod path starting with /v1/"
                        ));
                    }
                }
                continue;
            }
            _ => continue,
        };
        if !path.starts_with("/v1/") {
            return Err(format!(
                "endpoint '{path}' must be a pod path starting with /v1/"
            ));
        }
    }
    Ok(node)
}

/// The `"type"`s of the unknown nodes in the tree (layout files may only hold
/// the removed types that deployed versions already use).
pub fn unknown_node_types(node: &UiNode) -> Vec<String> {
    node.walk()
        .into_iter()
        .filter(|n| matches!(n, UiNode::Unknown(_)))
        .map(UiNode::type_tag)
        .collect()
}

fn check_aspect(what: &str, a: f32) -> Result<(), String> {
    if ASPECT_RATIO_RANGE.contains(&a) {
        Ok(())
    } else {
        Err(format!(
            "{what} {a} outside {}..={}",
            ASPECT_RATIO_RANGE.start(),
            ASPECT_RATIO_RANGE.end()
        ))
    }
}

fn check_dp(what: &str, v: u16, max: u16) -> Result<(), String> {
    if v > max {
        Err(format!("{what} {v} (max {max})"))
    } else {
        Ok(())
    }
}

fn check_icon(icon: &str) -> Result<(), String> {
    if ICONS.contains(&icon) {
        Ok(())
    } else {
        Err(format!(
            "unknown icon '{icon}' (known: {})",
            ICONS.join(", ")
        ))
    }
}

fn check_tint(tint: &str) -> Result<(), String> {
    let hex = tint.strip_prefix('#').unwrap_or("");
    if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("tint '{tint}' must be #RRGGBB"))
    }
}

/// Numeric fields the renderers cannot draw sensibly (a non-finite `f32`
/// would even serialize as `null` and make the stored layout unreadable), and
/// names clients must know (icons, tints).
fn check_values(node: &UiNode) -> Result<(), String> {
    match node {
        UiNode::Container { padding_dp, .. } => check_dp("padding_dp", *padding_dp, MAX_PADDING_DP),
        UiNode::Row {
            spacing_dp,
            padding_h_dp,
            padding_v_dp,
            ..
        }
        | UiNode::Column {
            spacing_dp,
            padding_h_dp,
            padding_v_dp,
            ..
        } => {
            check_dp("spacing_dp", *spacing_dp, MAX_PADDING_DP)?;
            check_dp("padding_h_dp", *padding_h_dp, MAX_PADDING_DP)?;
            check_dp("padding_v_dp", *padding_v_dp, MAX_PADDING_DP)
        }
        UiNode::Spacer { size_dp: Some(s) } => check_dp("spacer size_dp", *s, MAX_SIZE_DP),
        UiNode::List { indent, .. } => match indent {
            Some(i) if i.step_dp > MAX_PADDING_DP || i.max.unwrap_or(4) > 8 => Err(format!(
                "indent step_dp {} / max {:?} too large (max {MAX_PADDING_DP} dp, 8 steps)",
                i.step_dp, i.max
            )),
            _ => Ok(()),
        },
        UiNode::Image {
            size_dp,
            aspect_ratio,
            ..
        } => {
            if let Some(a) = aspect_ratio {
                check_aspect("image aspect_ratio", *a)?;
            }
            if size_dp.is_some_and(|s| s == 0 || s > MAX_SIZE_DP) {
                return Err(format!("image size_dp {size_dp:?} (1..={MAX_SIZE_DP})"));
            }
            Ok(())
        }
        UiNode::Carousel {
            aspect_ratio,
            min_aspect,
            max_aspect,
            ..
        } => {
            check_aspect("carousel aspect_ratio", *aspect_ratio as f32)?;
            for a in [min_aspect, max_aspect].into_iter().flatten() {
                check_aspect("carousel min/max aspect", *a as f32)?;
            }
            if let (Some(lo), Some(hi)) = (min_aspect, max_aspect)
                && lo > hi
            {
                return Err(format!("carousel min_aspect {lo} > max_aspect {hi}"));
            }
            Ok(())
        }
        UiNode::IconButton {
            icon,
            active_icon,
            active_tint,
            size_dp,
            ..
        } => {
            check_icon(icon)?;
            if let Some(i) = active_icon {
                check_icon(i)?;
            }
            if let Some(t) = active_tint {
                check_tint(t)?;
            }
            if let Some(s) = size_dp {
                check_dp("icon size_dp", *s, MAX_SIZE_DP)?;
            }
            Ok(())
        }
        UiNode::Chip { icon: Some(i), .. } => check_icon(i),
        UiNode::Badge { icon, .. } => check_icon(icon),
        _ => Ok(()),
    }
}

/// Screens (docs/DECISIONS.md D34): unique ids, a route like
/// `scheme://a/{param}/b` (each `{param}` a whole segment, named `[a-z_]+`,
/// used once), `data` a pod path, and every `$route.<name>` used by the screen
/// naming one of its route's parameters.
fn check_screens(node: &UiNode) -> Result<(), String> {
    let UiNode::Workspace { screens, .. } = node else {
        return Ok(());
    };
    let mut ids = std::collections::HashSet::new();
    for screen in screens {
        let fail = |e: String| Err(format!("screen '{}': {e}", screen.id));
        if screen.id.is_empty() || !ids.insert(screen.id.as_str()) {
            return fail("ids must be unique and non-empty".into());
        }
        let params = match route_params(&screen.route) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        if let Some(data) = &screen.data
            && !data.starts_with("/v1/")
        {
            return fail(format!(
                "data '{data}' must be a pod path starting with /v1/"
            ));
        }
        let text = serde_json::to_string(screen).map_err(|e| e.to_string())?;
        for used in text.split("$route.").skip(1) {
            let name: String = used
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !params.contains(&name) {
                return fail(format!(
                    "'$route.{name}' is not a parameter of route '{}'",
                    screen.route
                ));
            }
        }
    }
    Ok(())
}

/// The `{param}` names of a screen route; `Err` if the route is malformed.
pub fn route_params(route: &str) -> Result<Vec<String>, String> {
    let Some((scheme, rest)) = route.split_once("://") else {
        return Err(format!("route '{route}' must look like scheme://path"));
    };
    if scheme.is_empty() || rest.is_empty() {
        return Err(format!("route '{route}' must look like scheme://path"));
    }
    let mut params = Vec::new();
    for seg in rest.split('/') {
        if seg.is_empty() {
            return Err(format!("route '{route}' has an empty segment"));
        }
        if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                return Err(format!(
                    "route parameter '{{{name}}}' must be named [a-z_]+"
                ));
            }
            if params.iter().any(|p| p == name) {
                return Err(format!("route parameter '{{{name}}}' is used twice"));
            }
            params.push(name.to_owned());
        } else if seg.contains('{') || seg.contains('}') {
            return Err(format!("route segment '{seg}' mixes text and a parameter"));
        }
    }
    Ok(params)
}

/// Every action intent in the tree (screens included).
pub fn intents(node: &UiNode) -> Vec<&ActionIntent> {
    node.walk()
        .into_iter()
        .flat_map(UiNode::own_intents)
        .collect()
}
