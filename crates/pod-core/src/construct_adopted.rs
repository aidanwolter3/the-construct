//! What an adopted element may reach on the adopter's pod (decision
//! `adoption-capabilities`).
//!
//! A shared element is code: its lists read data, its buttons call the
//! pod's actions. [`check`] walks a publication bound as the adopter binds
//! it, every element it embeds, opens or uses materialized with the inputs
//! its use site binds (so `${}`- and `$arg`-assembled strings are judged as
//! they draw), and sorts each thing it reaches:
//!
//! - **within the bind**: a query key, data path, entity or endpoint of a
//!   data set the adoption binds (the source says, [`crate::Source::adopted_scope`]);
//! - **refused**: data of a source that binds data sets, outside the bind
//!   (`collection:passwords:view:all` when only `chores` is bound), or
//!   anything no source of the pod owns;
//! - **accepted only when the adopter says so** (`accept_actions`): every
//!   other source's data and actions (`reads:<source>`, an endpoint), and
//!   the dangerous classes even within the bind: agent runs, jacks,
//!   sign-ins, deletes and the device commands that sign out, leave the
//!   pod, change the server or remove a layout.
//!
//! It also finds the URLs that send the adopter's data (decision
//! `adopt-shows-urls`) on the materialized tree, each keyed by the element
//! it is in, its pointer and what that element is bound to.

use std::collections::{BTreeMap, BTreeSet};

use construct_elements::Objects;
use serde_json::{Map, Value, json};

use crate::Pod;

/// The most element instances one adoption check materializes.
const MAX_INSTANCES: usize = 500;

/// Device commands an adopted element may run only when accepted.
pub const DANGEROUS_DEVICE: &[&str] = &[
    "leave_pod",
    "sign_out",
    "set_server",
    "sign_in",
    "delete_layout",
    "hide_layout",
    "unhide_layout",
];

/// One thing an adopted element reaches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reach {
    /// `reads`, `endpoint`, `device` or `auth`.
    pub kind: &'static str,
    pub value: String,
    /// The element's name (the publication's for its root) and the JSON
    /// pointer in it, materialized.
    pub element: String,
    pub at: String,
}

/// What a bound publication reaches, sorted.
#[derive(Debug, Default)]
pub struct Checked {
    /// Each element it draws (ids), for serving its data within the bind.
    pub elements: BTreeSet<String>,
    /// What needs the adopter's acceptance: `(class, accept value, reach)`.
    pub needs: Vec<(String, String, Reach)>,
    /// What can't be adopted at all.
    pub refused: Vec<(String, Reach)>,
    /// URLs that send what they read: `(key, flag)`; the key is what
    /// consent is recorded under.
    pub data_urls: Vec<(String, Value)>,
}

impl Checked {
    /// The accept values not in `accepted`, each once with its uses.
    pub fn unaccepted(&self, accepted: &BTreeSet<String>) -> Vec<Value> {
        let mut by: BTreeMap<&str, Value> = BTreeMap::new();
        for (class, value, r) in &self.needs {
            if accepted.contains(value) {
                continue;
            }
            let e = by.entry(value).or_insert_with(
                || json!({ "accept": value, "class": class, "kind": r.kind, "at": [] }),
            );
            if let Some(a) = e["at"].as_array_mut()
                && a.len() < 5
            {
                a.push(json!({ "element": r.element, "at": r.at }));
            }
        }
        by.into_values().collect()
    }

    /// The accept values this check needs, each once.
    pub fn needed(&self) -> BTreeSet<String> {
        self.needs.iter().map(|(_, v, _)| v.clone()).collect()
    }

    /// The data URLs not among `accepted` keys: their values, each once.
    pub fn unaccepted_urls(&self, accepted: &BTreeSet<String>) -> BTreeSet<String> {
        self.data_urls
            .iter()
            .filter(|(k, _)| !accepted.contains(k))
            .filter_map(|(_, f)| f["value"].as_str().map(str::to_owned))
            .collect()
    }

    /// The keys of the data URLs whose value is in `values`.
    pub fn url_keys(&self, values: &BTreeSet<String>) -> BTreeSet<String> {
        self.data_urls
            .iter()
            .filter(|(_, f)| f["value"].as_str().is_some_and(|v| values.contains(v)))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// The refusal of what can't be adopted, if anything.
    pub fn refusal(&self, name: &str) -> Option<Value> {
        let (why, r) = self.refused.first()?;
        Some(json!({
            "error": "reaches_unbound",
            "detail": format!("{name} reaches {:?} ({why}) at {} of '{}': an adopted element \
                reaches only the data it is bound to, so it can't be adopted as it is",
                r.value, r.at, r.element),
            "refused": self.refused.iter().take(20).map(|(why, r)| json!({
                "value": r.value, "kind": r.kind, "why": why, "element": r.element, "at": r.at,
            })).collect::<Vec<_>>(),
        }))
    }
}

/// The refusal of capabilities the adopter hasn't accepted.
pub fn needs_refusal(name: &str, unaccepted: &[Value]) -> Value {
    json!({
        "error": "needs_acceptance",
        "detail": format!("{name} reaches actions or data outside what it is bound to (each \
            listed with its class: agents, jacks, auth, device and delete are dangerous). \
            Nothing was adopted: show the user each one and, only if they agree, call again \
            with accept_actions listing the accept values exactly."),
        "needs_acceptance": unaccepted,
    })
}

/// The class of a pod path an adopted element may call only when
/// accepted, whatever it binds.
fn dangerous_endpoint(path: &str) -> Option<&'static str> {
    let p = path.trim();
    let first = p
        .strip_prefix("/v1/")
        .and_then(|r| r.split(['/', '?']).next())
        .unwrap_or("");
    match first {
        "agents" => return Some("agents"),
        "jacks" => return Some("jacks"),
        "auth" | "oauth" | "sessions" | "pods" | "devices" | "sign-in" | "signin" => {
            return Some("auth");
        }
        _ => {}
    }
    let segs = p.split('?').next().unwrap_or("").split('/');
    if segs
        .clone()
        .any(|s| s.contains("delete") || s.contains("remove") || s == "leave")
    {
        return Some("delete");
    }
    None
}

fn add(out: &mut Vec<Reach>, kind: &'static str, value: &str, element: &str, at: String) {
    out.push(Reach {
        kind,
        value: value.to_owned(),
        element: element.to_owned(),
        at,
    });
}

/// The things `node` (one materialized element) reaches.
fn reaches(v: &Value, element: &str, at: &str, out: &mut Vec<Reach>) {
    match v {
        Value::Object(o) => {
            if let Some(tag) = o.get("intent").and_then(Value::as_str) {
                let s = |k: &str| o.get(k).and_then(Value::as_str);
                if let Some(e) = s("endpoint") {
                    add(out, "endpoint", e, element, format!("{at}/endpoint"));
                }
                if let Some(p) = s("path") {
                    add(out, "reads", p, element, format!("{at}/path"));
                }
                // A literal entity (`$item.uri` is the list's own row).
                if let Some(u) = s("entity_uri").filter(|u| u.contains("://")) {
                    add(out, "reads", u, element, format!("{at}/entity_uri"));
                }
                if tag == "action.device"
                    && let Some(c) = s("command")
                {
                    add(out, "device", c, element, format!("{at}/command"));
                }
                if tag.starts_with("action.auth.") {
                    add(out, "auth", tag, element, format!("{at}/intent"));
                }
            }
            for (k, c) in o {
                let here = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
                match (k.as_str(), c) {
                    ("query_key", Value::String(q)) => add(out, "reads", q, element, here),
                    ("data", Value::String(p)) if p.starts_with("/v1/") => {
                        add(out, "reads", p, element, here);
                    }
                    ("endpoint" | "path" | "entity_uri", Value::String(_))
                        if o.contains_key("intent") => {}
                    _ => reaches(c, element, &here, out),
                }
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                reaches(c, element, &format!("{at}/{i}"), out);
            }
        }
        Value::String(s) => {
            // `${<uri>#<field>}` bindings: entities it reads.
            for b in s.split("${").skip(1) {
                let inner = b.split('}').next().unwrap_or("");
                let uri = inner.split('#').next().unwrap_or("");
                if !inner.contains('#') {
                    add(out, "reads", s, element, at.to_owned());
                } else if uri.contains("://") {
                    add(out, "reads", uri, element, at.to_owned());
                }
            }
        }
        _ => {}
    }
}

/// Whether `s` has a `${…}` naming no field (`${}` draws as nothing).
fn has_empty_expr(s: &str) -> bool {
    s.split("${")
        .skip(1)
        .any(|b| !b.split('}').next().unwrap_or("").contains('#'))
}

/// Which source owns `r` (for reads and endpoints), if any; `Ok(None)`:
/// the device's own data or the pod's own descriptions, free to read.
fn owner<'a>(pod: &'a Pod, r: &Reach) -> Result<Option<&'a str>, ()> {
    let v = r.value.trim();
    if r.kind == "reads" {
        if v.starts_with(crate::validate::LOCAL_QUERY_PREFIX)
            || v.starts_with("local://")
            || v.starts_with("core://sources/")
            || v.starts_with("core://theme")
        {
            return Ok(None);
        }
        if let Some((scheme, _)) = v.split_once("://") {
            return pod
                .catalog()
                .sources()
                .find(|(_, m)| m.entity_schemes.contains(&scheme))
                .map(|(n, _)| Some(n))
                .ok_or(());
        }
    }
    for (name, m) in pod.catalog().sources() {
        let hit = if r.kind == "endpoint" {
            m.actions
                .iter()
                .any(|p| crate::source::path_matches(p.pattern, v))
        } else if v.starts_with("/v1/") {
            m.data_paths
                .iter()
                .any(|p| crate::source::path_matches(p.pattern, v))
        } else {
            m.query_keys().any(|p| crate::source::key_matches(p, v))
        };
        if hit {
            return Ok(Some(name));
        }
    }
    Err(())
}

/// Sort what `node` reaches into `out`, given the bound feeds `scope`.
fn classify(pod: &Pod, scope: &[(String, String)], found: Vec<Reach>, out: &mut Checked) {
    let feeds_of = |source: &str| -> Vec<String> {
        scope
            .iter()
            .filter(|(s, _)| s == source)
            .map(|(_, f)| f.clone())
            .collect()
    };
    for r in found {
        match r.kind {
            "device" => {
                if DANGEROUS_DEVICE.contains(&r.value.as_str()) {
                    out.needs
                        .push(("device".into(), format!("device:{}", r.value), r));
                }
            }
            "auth" => out.needs.push(("auth".into(), r.value.clone(), r)),
            _ => {
                if has_empty_expr(&r.value) {
                    out.refused
                        .push(("an empty ${} expression hides what it names".into(), r));
                    continue;
                }
                let source = match owner(pod, &r) {
                    Ok(None) => continue,
                    Ok(Some(s)) => s,
                    Err(()) => {
                        out.refused
                            .push(("no data source of this pod owns it".into(), r));
                        continue;
                    }
                };
                let Some((adapter, _)) = pod.catalog().source(source) else {
                    continue;
                };
                let within = adapter.adopted_scope(&feeds_of(source), &r.value);
                if within == Some(false) {
                    out.refused.push((
                        format!("{source} data the adoption doesn't bind: bind it as an input"),
                        r,
                    ));
                    continue;
                }
                let danger = (r.kind == "endpoint")
                    .then(|| dangerous_endpoint(&r.value))
                    .flatten();
                match (danger, within) {
                    (Some(class), _) => out.needs.push((class.into(), r.value.clone(), r)),
                    (None, Some(true)) => {}
                    (None, None) if r.kind == "reads" => {
                        out.needs
                            .push(("source".into(), format!("reads:{source}"), r));
                    }
                    (None, _) => out.needs.push(("source".into(), r.value.clone(), r)),
                }
            }
        }
    }
}

/// One element instance: its name, element, bound inputs, materialized node.
type Instance = (String, String, Map<String, Value>, Value);

/// Every element instance `root` reaches bound with `bind`: `(name, el,
/// inputs, materialized node)`, the root first.
fn instances(
    objects: &dyn Objects,
    name: &str,
    root: &str,
    bind: &Map<String, Value>,
) -> Result<Vec<Instance>, Value> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue =
        std::collections::VecDeque::from([(name.to_owned(), root.to_owned(), bind.clone())]);
    while let Some((n, el, inputs)) = queue.pop_front() {
        let id = construct_elements::instance_id(&el, &inputs);
        if !seen.insert(id) || objects.object(&el).is_none() {
            continue;
        }
        if out.len() >= MAX_INSTANCES {
            return Err(json!({ "error": "too_large",
                "detail": format!("an adoption reaches at most {MAX_INSTANCES} element instances") }));
        }
        let m = construct_elements::materialize_with(objects, &el, &inputs)
            .map_err(|e| json!({ "error": "invalid_layout", "detail": e.to_string() }))?;
        for e in &m.edges {
            if let Some(t) = &e.el {
                queue.push_back((e.name.clone(), t.clone(), e.bind.clone()));
            }
        }
        out.push((n, el, inputs, m.node));
    }
    Ok(out)
}

/// The key a data URL's consent is recorded under: the URL, the element
/// it is in, its pointer there, and what that element is bound to.
fn url_key(value: &str, element: &str, at: &str, inputs: &Map<String, Value>) -> String {
    format!(
        "{value}\u{1f}{element}\u{1f}{at}\u{1f}{}",
        construct_elements::canonical(&Value::Object(inputs.clone()))
    )
}

/// Check a publication's root (`name`, element `root`, in `objects`) bound
/// with `bind` over the feeds `scope` (see the module docs). A component
/// is checked as written: its use site, the adopter's own, binds its data.
pub fn check(
    pod: &Pod,
    objects: &dyn Objects,
    name: &str,
    root: &str,
    bind: &Map<String, Value>,
    scope: &[(String, String)],
    component: bool,
) -> Result<Checked, Value> {
    let mut out = Checked::default();
    for (n, el, inputs, node) in instances(objects, name, root, bind)? {
        out.elements.insert(el.clone());
        let mut found = Vec::new();
        reaches(&node, &n, "", &mut found);
        if component {
            // Its data comes through its arguments: only what it calls.
            found.retain(|r| r.kind != "reads");
        }
        classify(pod, scope, found, &mut out);
        let mut flags = Vec::new();
        super::share::flags_of(&node, &n, &mut flags);
        for f in flags.into_iter().filter(|f| f["reads_data"] == true) {
            let key = url_key(
                f["value"].as_str().unwrap_or(""),
                &n,
                f["at"].as_str().unwrap_or(""),
                &inputs,
            );
            out.data_urls.push((key, f));
        }
    }
    out.needs.sort();
    out.needs.dedup();
    Ok(out)
}

/// The feeds an adopted element `id` may load (decision
/// `adoption-capabilities`): `None` when `id` is no adoption's, else the
/// feeds its adoption binds and the home feeds of the sources the adopter
/// accepted reading. Read on `conn`.
pub fn scope_of(
    conn: &rusqlite::Connection,
    objects: &dyn Objects,
    id: &str,
) -> Option<Vec<(String, String)>> {
    let home = crate::layouts::home_feeds(conn);
    for a in super::share::adoption_scopes(conn) {
        let reached = if a.elements.is_empty() {
            // Adopted before this was recorded: its references' closure.
            let Ok(Some(root)) = super::store::name(conn, &a.name) else {
                continue;
            };
            closure(objects, &root)
        } else {
            a.elements.clone()
        };
        if !reached.contains(id) {
            continue;
        }
        let mut feeds = a.feeds.clone();
        if a.elements.is_empty() {
            // The data sets its bindings name, as adopting bound them.
            let named: BTreeSet<&str> = a
                .bind
                .iter()
                .filter(|(k, _)| !k.contains(super::share::FIELD_SEP))
                .filter_map(|(_, v)| v.as_str())
                .collect();
            feeds.extend(
                home.iter()
                    .filter(|(_, f)| named.contains(f.as_str()))
                    .cloned(),
            );
        }
        for acc in &a.accepted_actions {
            if let Some(src) = acc.strip_prefix("reads:") {
                feeds.extend(home.iter().filter(|(s, _)| s == src).cloned());
            }
        }
        feeds.sort();
        feeds.dedup();
        return Some(feeds);
    }
    None
}

/// Every element id `root`'s references reach (any site), `root` too.
fn closure(objects: &dyn Objects, root: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_owned()];
    while let Some(id) = stack.pop() {
        if seen.len() >= MAX_INSTANCES || !seen.insert(id.clone()) {
            continue;
        }
        if let Some(v) = objects.object(&id) {
            construct_elements::refs::visit(&v["node"], &mut |r, _| {
                if let Some(el) = &r.el {
                    stack.push(el.clone());
                }
            });
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_endpoints_are_classed() {
        assert_eq!(dangerous_endpoint("/v1/agents/tasks/start"), Some("agents"));
        assert_eq!(dangerous_endpoint("/v1/jacks/plug"), Some("jacks"));
        assert_eq!(
            dangerous_endpoint("/v1/collections/chores/delete"),
            Some("delete")
        );
        assert_eq!(dangerous_endpoint("/v1/collections/chores/update"), None);
    }

    #[test]
    fn reaches_finds_endpoints_keys_commands_and_entities() {
        let node = json!({"type": "core://Column", "children": [
            {"type": "core://List", "query_key": "collection:secret:view:all", "item_template":
                {"type": "core://Text", "value_expr": "${collections://other/$item.id#title}"}},
            {"type": "core://Button", "label": "Go", "on_click": {"intent": "action.sequence",
                "steps": [
                    {"intent": "action.mutation.create", "endpoint": "/v1/agents/tasks/start",
                     "payload": {"prompt": "x"}},
                    {"intent": "action.device", "command": "leave_pod"}]}}]});
        let mut out = Vec::new();
        reaches(&node, "c-x", "", &mut out);
        let got: BTreeSet<(&str, &str)> = out.iter().map(|r| (r.kind, r.value.as_str())).collect();
        for want in [
            ("reads", "collection:secret:view:all"),
            ("reads", "collections://other/$item.id"),
            ("endpoint", "/v1/agents/tasks/start"),
            ("device", "leave_pod"),
        ] {
            assert!(got.contains(&want), "{want:?} in {got:?}");
        }
    }
}
