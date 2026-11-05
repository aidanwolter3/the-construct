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
/// Most options in one `core://ChoiceChips`.
pub const MAX_CHOICES: usize = 50;
/// Most columns of a `core://List` grid.
pub const MAX_COLUMNS: u16 = 6;
/// Most items a `core://MediaPicker` may hold, and most lines of a
/// multi-line `core://TextField` (schema 9, D51).
pub const MAX_MEDIA_ITEMS: u32 = 10;
pub const MAX_TEXT_LINES: u32 = 12;

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
    check_sticky(&node, None)?;
    for intent in intents(&node) {
        let path = match intent {
            ActionIntent::OptimisticMutate { endpoint, .. }
            | ActionIntent::Create { endpoint, .. } => {
                check_attachments(intent)?;
                endpoint
            }
            ActionIntent::LoadData { path } => path,
            ActionIntent::Device {
                command, app, id, ..
            } => {
                let c = command.as_str();
                if !protocol::DEVICE_COMMANDS.contains(&c) {
                    return Err(format!(
                        "unknown device command '{command}' (known: {})",
                        protocol::DEVICE_COMMANDS.join(", ")
                    ));
                }
                let blank = |v: &Option<String>| v.as_deref().is_none_or(|a| a.trim().is_empty());
                if protocol::DEVICE_COMMANDS_WITH_APP.contains(&c) && blank(app) {
                    return Err(format!("action.device {command} needs an app"));
                }
                if protocol::DEVICE_COMMANDS_WITH_ID.contains(&c) && blank(id) {
                    return Err(format!("action.device {command} needs an id"));
                }
                continue;
            }
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

/// Tabs a `core://Dock` holds at most (fixed ones; and its query items).
const MAX_DOCK_TABS: usize = 6;

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

/// A `core://Box` colour: `#RRGGBB`, or a binding (`$item.<field>`,
/// `${<uri>#<field>}`) the client resolves.
fn check_color(c: &str) -> Result<(), String> {
    if c.contains("$item.") || c.contains("${") {
        Ok(())
    } else {
        check_tint(c).map_err(|_| format!("colour '{c}' must be #RRGGBB or a binding"))
    }
}

/// Numeric fields the renderers cannot draw sensibly (a non-finite `f32`
/// would even serialize as `null` and make the stored layout unreadable), and
/// names clients must know (icons, tints).
fn check_values(node: &UiNode) -> Result<(), String> {
    match node {
        UiNode::Dock {
            field,
            before,
            query_key,
            item,
            max_items,
            after,
        } => {
            if field.trim().is_empty() {
                return Err("a dock needs a field".into());
            }
            if query_key.is_some() != item.is_some() {
                return Err("a dock's query_key and item go together".into());
            }
            if before.len() + after.len() > MAX_DOCK_TABS {
                return Err(format!("a dock has at most {MAX_DOCK_TABS} fixed tabs"));
            }
            if max_items.is_some_and(|m| m == 0 || usize::from(m) > MAX_DOCK_TABS) {
                return Err(format!("dock max_items is 1..={MAX_DOCK_TABS}"));
            }
            if before.is_empty() && after.is_empty() && item.is_none() {
                return Err("a dock needs at least one tab".into());
            }
            Ok(())
        }
        UiNode::Drawer {
            field,
            before,
            query_key,
            item,
            after,
            ..
        } => {
            if field.trim().is_empty() {
                return Err("a drawer needs a field".into());
            }
            if query_key.is_some() != item.is_some() {
                return Err("a drawer's query_key and item go together".into());
            }
            if before.is_empty() && after.is_empty() && item.is_none() {
                return Err("a drawer needs at least one page".into());
            }
            Ok(())
        }
        UiNode::AppFrame { app } => {
            if app.trim().is_empty() {
                Err("an app frame needs an app".into())
            } else {
                Ok(())
            }
        }
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
        UiNode::Progress { size_dp: Some(s) } => check_dp("progress size_dp", *s, MAX_SIZE_DP),
        UiNode::Text {
            value_expr,
            count: Some(count),
            ..
        } => check_count(value_expr, count),
        UiNode::List {
            indent,
            filter,
            group_by,
            where_,
            columns,
            spacing_dp,
            padding_h_dp,
            focus,
            ..
        } => {
            if let Some(f) = focus {
                if f.uri.trim().is_empty() {
                    return Err("list focus needs a uri".into());
                }
                for c in [&f.background, &f.content_color].into_iter().flatten() {
                    check_tint(c)
                        .map_err(|_| format!("list focus colour '{c}' must be #RRGGBB"))?;
                }
            }
            if *columns > MAX_COLUMNS {
                return Err(format!("list columns {columns} (max {MAX_COLUMNS})"));
            }
            check_dp("list spacing_dp", *spacing_dp, MAX_PADDING_DP)?;
            check_dp("list padding_h_dp", *padding_h_dp, MAX_PADDING_DP)?;
            if let Some(i) = indent
                && (i.step_dp > MAX_PADDING_DP || i.max.unwrap_or(4) > 8)
            {
                return Err(format!(
                    "indent step_dp {} / max {:?} too large (max {MAX_PADDING_DP} dp, 8 steps)",
                    i.step_dp, i.max
                ));
            }
            if let Some(f) = filter
                && (f.value.trim().is_empty()
                    || f.fields.is_empty()
                    || f.fields.iter().any(|x| x.trim().is_empty()))
            {
                return Err("list filter needs a value and at least one field".into());
            }
            if group_by.as_ref().is_some_and(|g| g.field.trim().is_empty()) {
                return Err("list group_by needs a field".into());
            }
            if where_
                .as_ref()
                .is_some_and(|w| w.field.trim().is_empty() || w.equals.trim().is_empty())
            {
                return Err("list where needs a field and a value".into());
            }
            Ok(())
        }
        UiNode::Box {
            background,
            content_color,
            border_color,
            corner_dp,
            padding_h_dp,
            padding_v_dp,
            width_dp,
            height_dp,
            ..
        } => {
            for c in [background, content_color, border_color]
                .into_iter()
                .flatten()
            {
                check_color(c)?;
            }
            check_dp("corner_dp", *corner_dp, MAX_SIZE_DP)?;
            check_dp("padding_h_dp", *padding_h_dp, MAX_PADDING_DP)?;
            check_dp("padding_v_dp", *padding_v_dp, MAX_PADDING_DP)?;
            for s in [width_dp, height_dp].into_iter().flatten() {
                if *s == 0 || *s > MAX_SIZE_DP {
                    return Err(format!("box width/height {s} (1..={MAX_SIZE_DP})"));
                }
            }
            Ok(())
        }
        UiNode::ChoiceChips {
            field,
            options,
            padding_h_dp,
            ..
        } => {
            if field.trim().is_empty() {
                return Err("choice chips need a field".into());
            }
            if options.is_empty() || options.len() > MAX_CHOICES {
                return Err(format!("choice chips need 1..={MAX_CHOICES} options"));
            }
            let mut values = std::collections::HashSet::new();
            for o in options {
                if o.label.trim().is_empty() {
                    return Err("every choice needs a label".into());
                }
                if !values.insert(o.value.as_str()) {
                    return Err(format!("choice value '{}' is used twice", o.value));
                }
            }
            check_dp("padding_h_dp", *padding_h_dp, MAX_PADDING_DP)
        }
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
        UiNode::MediaPicker {
            field, max_items, ..
        } => {
            if field.trim().is_empty() {
                return Err("media picker needs a field".into());
            }
            if *max_items == 0 || *max_items > MAX_MEDIA_ITEMS {
                return Err(format!(
                    "media picker max_items {max_items} (1..={MAX_MEDIA_ITEMS})"
                ));
            }
            Ok(())
        }
        UiNode::TextField { lines: Some(l), .. } if *l == 0 || *l > MAX_TEXT_LINES => {
            Err(format!("text field lines {l} (1..={MAX_TEXT_LINES})"))
        }
        _ => Ok(()),
    }
}

/// A `core://Text` `count` (feature `count`) names a query, its `where`
/// fields, and is shown: the text has a `$count` (or a `zero_text`).
fn check_count(value_expr: &str, count: &protocol::TextCount) -> Result<(), String> {
    if count.query_key.trim().is_empty() {
        return Err("text count needs a query_key".into());
    }
    if count.where_.iter().any(|w| w.field.trim().is_empty()) {
        return Err("text count where needs a field".into());
    }
    if !value_expr.contains("$count") && count.zero_text.is_none() {
        return Err("text count is set but value_expr has no $count (e.g. \"$count left\")".into());
    }
    Ok(())
}

/// `core://Sticky` (schema 11) pins to the top of a scrolling column, so it
/// must not sit inside a `core://List` or `core://Carousel` template (each
/// item would pin within itself) or inside another `core://Sticky`.
/// `inside` names the enclosing node that rules it out.
fn check_sticky(node: &UiNode, inside: Option<&str>) -> Result<(), String> {
    if let (UiNode::Sticky { .. }, Some(outer)) = (node, inside) {
        return Err(format!("core://Sticky cannot be inside a {outer}"));
    }
    let inside = match node {
        UiNode::List { .. } => Some("core://List"),
        UiNode::Carousel { .. } => Some("core://Carousel"),
        UiNode::Sticky { .. } => Some("core://Sticky"),
        _ => inside,
    };
    node.children()
        .into_iter()
        .try_for_each(|c| check_sticky(c, inside))
}

/// A create's `attachments` (schema 9, D49): each name `[a-z_]+` maps to a
/// `$form.<field>` binding, and every `$attachments.<name>` the intent uses is
/// one of them.
fn check_attachments(intent: &ActionIntent) -> Result<(), String> {
    let ActionIntent::Create {
        attachments,
        entity,
        payload,
        ..
    } = intent
    else {
        return Ok(());
    };
    for (name, binding) in attachments {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return Err(format!("attachment name '{name}' must be [a-z_]+"));
        }
        let ok = binding
            .as_str()
            .and_then(|b| b.strip_prefix("$form."))
            .is_some_and(|f| !f.is_empty());
        if !ok {
            return Err(format!(
                "attachment '{name}' must be bound to \"$form.<field>\""
            ));
        }
    }
    let text = format!("{entity}{payload}");
    for used in text.split("$attachments.").skip(1) {
        let name: String = used
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || *c == '_')
            .collect();
        if !attachments.contains_key(&name) {
            return Err(format!(
                "'$attachments.{name}' is not one of the create's attachments"
            ));
        }
    }
    Ok(())
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

/// Why a custom app's layout was refused ([`custom_layout`]): where (a JSON
/// pointer into the layout), what is wrong, and what is allowed instead, so
/// whoever wrote it (often Claude) can fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutError {
    pub pointer: String,
    pub message: String,
    pub allowed: Vec<String>,
}

impl LayoutError {
    fn new(pointer: &str, message: impl Into<String>) -> Self {
        Self {
            pointer: pointer.to_owned(),
            message: message.into(),
            allowed: Vec::new(),
        }
    }

    fn allowing(mut self, allowed: impl IntoIterator<Item = String>) -> Self {
        self.allowed = allowed.into_iter().collect();
        self
    }

    /// `{"pointer", "message", "allowed"}`.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "pointer": self.pointer,
            "message": self.message,
            "allowed": self.allowed,
        })
    }
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.pointer.is_empty() {
            write!(f, "{}", self.message)?;
        } else {
            write!(f, "{}: {}", self.pointer, self.message)?;
        }
        if !self.allowed.is_empty() {
            write!(f, " (allowed: {})", self.allowed.join(", "))?;
        }
        Ok(())
    }
}

/// The validation of a custom app's layout (decision
/// `custom-apps-and-data-sources`), stricter than a layout file's:
///
/// 1. [`validate_structure`] (the root id must be `id`);
/// 2. no node type or intent this build doesn't know;
/// 3. capabilities, against `sources` (the app's feeds' sources): screen
///    `data` and `action.data.load` paths fit a declared data path;
///    mutation and create endpoints fit a declared action; no
///    `action.auth.*` (sign-ins start from Settings); every `query_key`
///    fits a key the sources fill; entity URIs (`entity_uri`, `${uri#field}`
///    bindings) use a declared scheme or `core://sources/<id>`; no other
///    pod path (`/v1/...`) appears anywhere.
///
/// `Err` names the JSON pointer and the allowed alternatives.
pub fn custom_layout(
    id: &str,
    doc: &Value,
    sources: &[(&str, &crate::source::SourceManifest)],
) -> Result<UiNode, LayoutError> {
    let node = validate_structure(id, doc).map_err(|e| LayoutError::new("", e))?;
    if let Some(t) = unknown_node_types(&node).first() {
        let at = find_pointer(doc, "", &|k, v| {
            k == "type" && v.as_str() == Some(t.as_str())
        })
        .unwrap_or_default();
        return Err(LayoutError::new(&at, format!("unknown node type '{t}'"))
            .allowing(protocol::NODE_TYPES.iter().map(|t| t.to_string())));
    }
    let mut found = Vec::new();
    crate::layouts::unknown_intent_tags(doc, "", &mut found);
    if let Some((at, tag)) = found.first() {
        return Err(LayoutError::new(
            at,
            format!("unknown action intent '{tag}'"),
        ));
    }
    // Vocabulary current clients no longer draw (decision
    // `named-vocabulary`) is never written again.
    if let Some(f) = protocol::required_features(&node)
        .into_iter()
        .find(|f| protocol::REMOVED_FEATURES.contains(f))
    {
        let (key, hint) = match f {
            "create_dismiss" => (
                "dismiss",
                "; to close the screen after a create, put it in an action.sequence followed by action.navigation.back",
            ),
            _ => ("", ""),
        };
        let at = find_pointer(doc, "", &|k, _| k == key).unwrap_or_default();
        return Err(LayoutError::new(
            &at,
            format!("'{f}' was removed from the layout vocabulary{hint}"),
        ));
    }
    let caps = Caps::new(sources);
    caps.walk(doc, "")?;
    Ok(node)
}

/// The first pointer under `at` whose `(key, value)` satisfies `pred`.
fn find_pointer(v: &Value, at: &str, pred: &dyn Fn(&str, &Value) -> bool) -> Option<String> {
    match v {
        Value::Object(map) => map.iter().find_map(|(k, child)| {
            let here = format!("{at}/{}", escape_pointer(k));
            if pred(k, child) {
                Some(here)
            } else {
                find_pointer(child, &here, pred)
            }
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, child)| find_pointer(child, &format!("{at}/{i}"), pred)),
        _ => None,
    }
}

fn escape_pointer(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

/// The entity scheme of data the device provides (feature `local_data`).
pub const LOCAL_SCHEME: &str = "local";
/// The query-key prefix of data the device provides.
pub const LOCAL_QUERY_PREFIX: &str = "local:";

/// What a custom layout may use, from its sources' manifests.
struct Caps<'a> {
    sources: &'a [(&'a str, &'a crate::source::SourceManifest)],
}

impl<'a> Caps<'a> {
    fn new(sources: &'a [(&'a str, &'a crate::source::SourceManifest)]) -> Self {
        Self { sources }
    }

    fn data_paths(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sources
            .iter()
            .flat_map(|(_, m)| m.data_paths.iter().map(|p| p.pattern))
    }

    fn actions(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sources
            .iter()
            .flat_map(|(_, m)| m.actions.iter().map(|p| p.pattern))
    }

    fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sources.iter().flat_map(|(_, m)| m.query_keys())
    }

    fn schemes(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sources
            .iter()
            .flat_map(|(_, m)| m.entity_schemes.iter().copied())
    }

    fn data_path(&self, at: &str, path: &str) -> Result<(), LayoutError> {
        if self
            .data_paths()
            .any(|p| crate::source::path_matches(p, path))
        {
            return Ok(());
        }
        Err(LayoutError::new(
            at,
            format!("'{path}' is not a data path of this app's data sources"),
        )
        .allowing(self.data_paths().map(str::to_owned)))
    }

    fn action(&self, at: &str, path: &str) -> Result<(), LayoutError> {
        if self.actions().any(|p| crate::source::path_matches(p, path)) {
            return Ok(());
        }
        Err(LayoutError::new(
            at,
            format!("'{path}' is not an action of this app's data sources"),
        )
        .allowing(self.actions().map(str::to_owned)))
    }

    fn query_key(&self, at: &str, key: &str) -> Result<(), LayoutError> {
        // Data the device provides (feature `local_data`).
        if key.starts_with(LOCAL_QUERY_PREFIX) {
            return Ok(());
        }
        if self.keys().any(|p| crate::source::key_matches(p, key)) {
            return Ok(());
        }
        Err(LayoutError::new(
            at,
            format!("query key '{key}' is not one this app's data sources fill"),
        )
        .allowing(self.keys().map(str::to_owned)))
    }

    /// An entity URI (only checked when it names a scheme: `$item.uri`
    /// and other bindings are the client's business).
    fn entity(&self, at: &str, uri: &str) -> Result<(), LayoutError> {
        let Some((scheme, rest)) = uri.split_once("://") else {
            return Ok(());
        };
        if scheme.contains('$') {
            return Ok(());
        }
        if scheme == "core" {
            if rest.starts_with("sources/") {
                return Ok(());
            }
        } else if scheme == LOCAL_SCHEME || self.schemes().any(|s| s == scheme) {
            return Ok(());
        }
        Err(LayoutError::new(
            at,
            format!("entity URI '{uri}' is not from this app's data sources"),
        )
        .allowing(
            self.schemes()
                .map(|s| format!("{s}://..."))
                .chain(["core://sources/<source>".to_owned()]),
        ))
    }

    fn intent(&self, at: &str, map: &serde_json::Map<String, Value>) -> Result<(), LayoutError> {
        let tag = map.get("intent").and_then(Value::as_str).unwrap_or("");
        let field = |k: &str| map.get(k).and_then(Value::as_str);
        match tag {
            "action.mutation.optimistic" | "action.mutation.create" => {
                if let Some(e) = field("endpoint") {
                    self.action(&format!("{at}/endpoint"), e)?;
                }
                if let Some(u) = field("entity_uri") {
                    self.entity(&format!("{at}/entity_uri"), u)?;
                }
            }
            "action.local.set" => {
                if let Some(u) = field("entity_uri") {
                    self.entity(&format!("{at}/entity_uri"), u)?;
                }
            }
            "action.link.open" => {
                // A literal link must be http(s); a binding is checked by the client.
                if let Some(u) = field("url") {
                    let u = u.trim();
                    if !(u.starts_with('$')
                        || u.starts_with("https://")
                        || u.starts_with("http://"))
                    {
                        return Err(LayoutError::new(
                            &format!("{at}/url"),
                            "a link is an http(s):// address or a binding such as $item.url"
                                .to_owned(),
                        ));
                    }
                }
            }
            "action.data.load" => {
                if let Some(p) = field("path") {
                    self.data_path(&format!("{at}/path"), p)?;
                }
            }
            t if t.starts_with("action.auth.") => {
                return Err(LayoutError::new(
                    at,
                    format!(
                        "'{t}' is not allowed in a custom app: sign-ins start from Settings \
                         (show a hint with core://sources/<source>#signed_in instead)"
                    ),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Walk the layout.
    fn walk(&self, v: &Value, at: &str) -> Result<(), LayoutError> {
        match v {
            Value::Object(map) => {
                if map.get("intent").is_some_and(Value::is_string) {
                    self.intent(at, map)?;
                }
                for (k, child) in map {
                    let here = format!("{at}/{}", escape_pointer(k));
                    match (k.as_str(), child) {
                        ("query_key", Value::String(q)) => self.query_key(&here, q)?,
                        // Checked with their intent above.
                        ("endpoint" | "path" | "entity_uri", Value::String(_))
                            if map.contains_key("intent") => {}
                        (_, Value::String(s)) => self.string(&here, s)?,
                        ("screens", Value::Array(items)) => {
                            for (i, item) in items.iter().enumerate() {
                                self.walk_screen(item, &format!("{here}/{i}"))?;
                            }
                        }
                        _ => self.walk(child, &here)?,
                    }
                }
                Ok(())
            }
            Value::Array(items) => items
                .iter()
                .enumerate()
                .try_for_each(|(i, child)| self.walk(child, &format!("{at}/{i}"))),
            Value::String(s) => self.string(at, s),
            _ => Ok(()),
        }
    }

    fn walk_screen(&self, v: &Value, at: &str) -> Result<(), LayoutError> {
        let Value::Object(map) = v else {
            return self.walk(v, at);
        };
        for (k, child) in map {
            let here = format!("{at}/{}", escape_pointer(k));
            match (k.as_str(), child) {
                ("data", Value::String(p)) => self.data_path(&here, p)?,
                // A screen route is local navigation (any scheme).
                ("route", Value::String(_)) => {}
                (_, Value::String(s)) => self.string(&here, s)?,
                _ => self.walk(child, &here)?,
            }
        }
        Ok(())
    }

    /// Any other string: no pod path, and `${uri#field}` bindings name
    /// entities of the sources.
    fn string(&self, at: &str, s: &str) -> Result<(), LayoutError> {
        if s.trim_start().starts_with("/v1/") {
            return Err(LayoutError::new(
                at,
                format!(
                    "'{s}' is a pod path; a custom layout may only use its sources' data paths (screen \"data\", action.data.load) and actions (mutation endpoints)"
                ),
            ));
        }
        for binding in s.split("${").skip(1) {
            let inner = binding.split('}').next().unwrap_or("");
            let uri = inner.split('#').next().unwrap_or("");
            self.entity(at, uri)?;
        }
        Ok(())
    }
}

/// Every action intent in the tree (screens included).
pub fn intents(node: &UiNode) -> Vec<&ActionIntent> {
    node.walk()
        .into_iter()
        .flat_map(UiNode::own_intents)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list(extra: Value) -> UiNode {
        let mut v = json!({"type": "core://List", "query_key": "q",
            "item_template": {"type": "core://Divider"}});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn list_filter_and_group_by_need_their_fields() {
        let ok = list(json!({"filter": {"value": "$form.q", "fields": ["title"]},
            "group_by": {"field": "category", "header": {"type": "core://Divider"}}}));
        assert_eq!(check_values(&ok), Ok(()));
        for bad in [
            json!({"filter": {"value": " ", "fields": ["title"]}}),
            json!({"filter": {"value": "$form.q", "fields": []}}),
            json!({"filter": {"value": "$form.q", "fields": [""]}}),
            json!({"group_by": {"field": "", "header": {"type": "core://Divider"}}}),
        ] {
            assert!(check_values(&list(bad.clone())).is_err(), "{bad}");
        }
    }

    fn node(v: Value) -> UiNode {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn sticky_only_pins_in_a_scrolling_column() {
        let sticky = json!({"type": "core://Sticky", "content": {"type": "core://Divider"}});
        let ok = node(json!({"type": "core://Column", "children": [sticky.clone()]}));
        assert_eq!(check_sticky(&ok, None), Ok(()));
        for bad in [
            json!({"type": "core://List", "query_key": "q", "item_template": sticky.clone()}),
            json!({"type": "core://List", "query_key": "q", "item_template": {"type": "core://Divider"},
                "group_by": {"field": "c", "header": sticky.clone()}}),
            json!({"type": "core://Carousel", "items": "$item.m", "item_template": sticky.clone()}),
            json!({"type": "core://Sticky", "content": {"type": "core://Column",
                "children": [sticky.clone()]}}),
        ] {
            assert!(check_sticky(&node(bad.clone()), None).is_err(), "{bad}");
        }
    }

    #[test]
    fn text_counts_are_checked() {
        let text = |count: Value, expr: &str| {
            node(
                json!({"type": "core://Text", "style": "Body", "value_expr": expr, "count": count}),
            )
        };
        let ok = json!({"query_key": "collection:todos", "where": [{"field": "done", "equals": "false"}]});
        assert_eq!(check_values(&text(ok.clone(), "$count left")), Ok(()));
        assert!(check_values(&text(ok, "left")).is_err(), "no $count");
        let no_key = json!({"query_key": " "});
        assert!(check_values(&text(no_key, "$count")).is_err());
        let no_field = json!({"query_key": "q", "where": [{"field": "", "equals": "x"}]});
        assert!(check_values(&text(no_field, "$count")).is_err());
        let zero_only = json!({"query_key": "q", "zero_text": "All done"});
        assert_eq!(check_values(&text(zero_only, "Not done yet")), Ok(()));
        // A date format needs no check beyond its type.
        let dated = json!({"type": "core://Text", "style": "Body", "value_expr": "Due $item.due",
            "format": {"date": "Relative", "past": "Overdue"}});
        assert_eq!(check_values(&node(dated)), Ok(()));
    }

    #[test]
    fn progress_size_is_bounded() {
        let p = |v: Value| node(v);
        assert_eq!(check_values(&p(json!({"type": "core://Progress"}))), Ok(()));
        let sized = json!({"type": "core://Progress", "size_dp": 24});
        assert_eq!(check_values(&p(sized)), Ok(()));
        let huge = json!({"type": "core://Progress", "size_dp": 9999});
        assert!(check_values(&p(huge)).is_err());
    }

    #[test]
    fn schema_6_nodes_are_checked() {
        let ok = list(json!({"where": {"field": "category", "equals": "$form.cat"}}));
        assert_eq!(check_values(&ok), Ok(()));
        assert!(check_values(&list(json!({"where": {"field": "", "equals": "x"}}))).is_err());
        let boxed = |extra: Value| {
            let mut v = json!({"type": "core://Box", "content": {"type": "core://Divider"}});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            node(v)
        };
        for good in [
            json!({"background": "#FBE6DF", "content_color": "$item.ink"}),
            json!({"border_color": "${x://a#line}", "width_dp": 44, "height_dp": 44}),
        ] {
            assert_eq!(check_values(&boxed(good.clone())), Ok(()), "{good}");
        }
        for bad in [
            json!({"background": "red"}),
            json!({"width_dp": 0}),
            json!({"corner_dp": 9999}),
        ] {
            assert!(check_values(&boxed(bad.clone())).is_err(), "{bad}");
        }
        let chips = |options: Value| {
            node(json!({"type": "core://ChoiceChips", "field": "cat", "options": options}))
        };
        assert_eq!(
            check_values(&chips(
                json!([{"label": "All"}, {"label": "A", "value": "a"}])
            )),
            Ok(())
        );
        assert!(check_values(&chips(json!([]))).is_err());
        assert!(check_values(&chips(json!([{"label": "A"}, {"label": "B"}]))).is_err());
        assert!(check_values(&chips(json!([{"label": " ", "value": "a"}]))).is_err());
    }

    #[test]
    fn schema_9_nodes_and_attachments_are_checked() {
        let picker =
            |n: u32| node(json!({"type": "core://MediaPicker", "field": "p", "max_items": n}));
        assert_eq!(check_values(&picker(4)), Ok(()));
        assert!(check_values(&picker(0)).is_err());
        assert!(check_values(&picker(11)).is_err());
        let lines = |n: u32| node(json!({"type": "core://TextField", "field": "c", "lines": n}));
        assert_eq!(check_values(&lines(4)), Ok(()));
        assert!(check_values(&lines(0)).is_err());
        let create = |attachments: Value, payload: Value| -> ActionIntent {
            serde_json::from_value(json!({"intent": "action.mutation.create", "query_key": "q",
                "entity_uri": "x://p/$nonce", "entity": {}, "endpoint": "/v1/x",
                "payload": payload, "attachments": attachments}))
            .unwrap()
        };
        let ok = create(
            json!({"photos": "$form.photos"}),
            json!({"m": "$attachments.photos"}),
        );
        assert_eq!(check_attachments(&ok), Ok(()));
        for (a, p) in [
            (json!({"photos": "photos"}), json!({})),
            (json!({"Photos": "$form.p"}), json!({})),
            (
                json!({"photos": "$form.photos"}),
                json!({"m": "$attachments.other"}),
            ),
            (json!({}), json!({"m": "$attachments.photos"})),
        ] {
            assert!(
                check_attachments(&create(a.clone(), p.clone())).is_err(),
                "{a} {p}"
            );
        }
    }

    #[test]
    fn list_grids_are_checked() {
        let ok = list(json!({"columns": 2, "spacing_dp": 12, "padding_h_dp": 12}));
        assert_eq!(check_values(&ok), Ok(()));
        for bad in [
            json!({"columns": 40}),
            json!({"spacing_dp": 500}),
            json!({"padding_h_dp": 500}),
        ] {
            assert!(check_values(&list(bad.clone())).is_err(), "{bad}");
        }
    }
}
