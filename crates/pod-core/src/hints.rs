//! Reuse hints: advice a layout or component write answers beside its
//! other `warnings` (decision `element-roles-and-reuse`).
//!
//! An agent that writes a screen out node by node makes elements that are
//! large, shared by nothing and fetched again whole on every change. The
//! instructions tell it to look for what exists first; these hints say so
//! again at the point of writing, about the exact nodes:
//!
//! - **a component it could have used**: a subtree that is what a core or
//!   own component expands to. The match is exact: the component's
//!   parameters are wildcards, everything else must be equal.
//! - **a piece written out several times**: a subtree repeated three or
//!   more times in the write, equal but for the values in it.
//! - **a row written into a binding**: a source's entity
//!   (`<scheme>://<kind>/<id>`) with a literal, generated id.
//!
//! Hints are advice: the write has already succeeded. At most
//! [`MAX_HINTS`], the largest saving first, within [`BUDGET`].

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::Pod;

/// The most hints one write answers.
pub const MAX_HINTS: usize = 10;
/// How long a write may spend on its hints.
const BUDGET: Duration = Duration::from_millis(1500);
/// A repeated piece is worth a component from this many nodes.
const MIN_REPEAT_NODES: usize = 5;
/// ... written this many times.
const REPEATS: usize = 3;
/// A write that makes devices fetch more than this again says so.
pub const REFETCH_BYTES: i64 = 64 * 1024;

/// The first number standing for a number parameter in a pattern.
const NUMBER_ARG: i64 = 7_000_000;
/// The node standing for a component's children in a pattern.
const CHILDREN_ARG: i64 = 7_999_999;

/// A text parameter's placeholder in a pattern.
fn text_arg(name: &str) -> String {
    format!("\u{a7}{name}\u{a7}")
}

/// One hint and how many nodes acting on it saves.
struct Hint {
    saving: usize,
    text: String,
}

/// A component as a shape to look for.
struct Pattern {
    name: String,
    root_type: String,
    node: Value,
    /// Its number parameters, by placeholder.
    numbers: Vec<String>,
    defaults: Map<String, Value>,
    nodes: usize,
}

/// One node of the written document, in the order they are written.
struct Seen<'a> {
    pointer: String,
    node: &'a Value,
    shape: u64,
    /// Typed nodes in its subtree, itself included.
    size: usize,
    /// One past its last descendant in the list.
    end: usize,
}

/// The reuse hints for `doc` (a layout or a component's template), each
/// `"<JSON pointer>: <advice>"`. `writing`: the component being written,
/// which is not suggested to itself.
pub fn reuse(pod: &Pod, doc: &Value, writing: Option<&str>) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut seen = Vec::new();
    walk(doc, String::new(), &mut seen);
    let mut covered = vec![false; seen.len()];
    let mut hints: Vec<Hint> = Vec::new();
    let mut late = false;

    // A component it could have used.
    let patterns = patterns(pod, writing, deadline);
    for i in 0..seen.len() {
        if covered[i] {
            continue;
        }
        if Instant::now() > deadline {
            late = true;
            break;
        }
        let ty = seen[i].node["type"].as_str().unwrap_or("");
        for p in patterns.iter().filter(|p| p.root_type == ty) {
            let mut bound = Map::new();
            if !matches(&p.node, seen[i].node, p, &mut bound) {
                continue;
            }
            let children = bound.remove("children");
            bound.retain(|k, v| p.defaults.get(k) != Some(v));
            let mut uses = json!({ "type": "core://Use", "ref": p.name });
            if !bound.is_empty() {
                uses["args"] = Value::Object(bound);
            }
            let mut text = short(&uses.to_string(), 260);
            if let Some(Value::Array(c)) = children
                && !c.is_empty()
            {
                text.push_str(&format!(
                    ", with the {} inside as its children",
                    nodes(c.len())
                ));
            }
            hints.push(Hint {
                saving: seen[i].size.saturating_sub(1).max(1),
                text: format!(
                    "{}: this is what {} draws: use {text}",
                    pointer(&seen[i].pointer),
                    p.name
                ),
            });
            covered[i..seen[i].end].fill(true);
            break;
        }
    }

    // A piece written out several times.
    let mut by_shape: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (i, s) in seen.iter().enumerate() {
        if s.size >= MIN_REPEAT_NODES {
            by_shape.entry(s.shape).or_default().push(i);
        }
    }
    for i in 0..seen.len() {
        if covered[i] || seen[i].size < MIN_REPEAT_NODES {
            continue;
        }
        // The copies not inside something already reported.
        let copies: Vec<usize> = by_shape[&seen[i].shape]
            .iter()
            .copied()
            .filter(|j| !covered[*j])
            .collect();
        if copies.len() < REPEATS {
            continue;
        }
        let others: Vec<String> = copies[1..]
            .iter()
            .take(2)
            .map(|j| pointer(&seen[*j].pointer).to_owned())
            .collect();
        let more = copies.len() - 1 - others.len();
        hints.push(Hint {
            saving: (copies.len() - 1) * seen[i].size,
            text: format!(
                "{}: this piece ({}) is written {} times here (also at {}{}), differing only in its values: define a component for it (define_component, the values as parameters) and use it {} times",
                pointer(&seen[i].pointer),
                nodes(seen[i].size),
                copies.len(),
                others.join(", "),
                if more > 0 { format!(" and {more} more") } else { String::new() },
                copies.len(),
            ),
        });
        for j in copies {
            covered[j..seen[j].end].fill(true);
        }
    }

    // A row written into a binding.
    let mut rows: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let schemes: Vec<String> = pod
        .catalog()
        .adapters()
        .iter()
        .flat_map(|a| {
            a.manifest()
                .entity_schemes
                .iter()
                .map(|s| format!("{s}://"))
                .collect::<Vec<_>>()
        })
        .collect();
    literal_rows(doc, String::new(), &schemes, &mut rows);
    for (row, (at, count)) in rows {
        hints.push(Hint {
            saving: count,
            text: format!(
                "{}: the row {row} is written into this {}: an element that names one row can't be reused or shared. Pass what it shows as an input ($in.<name>, bound where the layout is opened), read it through a list with where, or, for a value only this device needs (the tab that is open, a filter), keep it on the device (action.local.set with keep) instead of in a row",
                pointer(&at),
                if count == 1 { "element".to_owned() } else { format!("element {count} times") },
            ),
        });
    }

    hints.sort_by_key(|a| std::cmp::Reverse(a.saving));
    let extra = hints.len().saturating_sub(MAX_HINTS);
    let mut out: Vec<String> = hints.into_iter().take(MAX_HINTS).map(|h| h.text).collect();
    if extra > 0 {
        out.push(format!("and {extra} more places like these"));
    }
    if late {
        out.push("reuse hints were cut short: this element is too large to check in time, which is itself a reason to split it".into());
    }
    out
}

/// What a write's `refetch` costs, when it is a lot.
pub fn refetch(answer: &Value) -> Option<String> {
    let bytes = answer["refetch"]["bytes"].as_i64()?;
    let objects = answer["refetch"]["objects"].as_i64().unwrap_or(1);
    (bytes > REFETCH_BYTES).then(|| {
        format!(
            "this write makes every device fetch {} KB again ({objects} element{}): an element is fetched whole whenever it or anything it references changes, so keep the ones near home small (embeds, links and component uses; list_layouts gives each layout's bytes)",
            bytes / 1024,
            if objects == 1 { "" } else { "s" },
        )
    })
}

fn nodes(n: usize) -> String {
    if n == 1 {
        "1 node".into()
    } else {
        format!("{n} nodes")
    }
}

fn pointer(p: &str) -> &str {
    if p.is_empty() { "/" } else { p }
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn is_node(v: &Value) -> bool {
    v["type"].as_str().is_some_and(|t| t.starts_with("core://"))
}

/// Every typed node under `v`, in order, with its shape and size. Answers
/// `(shape, size)` of `v`.
fn walk<'a>(v: &'a Value, at: String, out: &mut Vec<Seen<'a>>) -> (u64, usize) {
    let mut h = DefaultHasher::new();
    match v {
        Value::Object(o) => {
            let mine = is_node(v).then(|| {
                out.push(Seen {
                    pointer: at.clone(),
                    node: v,
                    shape: 0,
                    size: 0,
                    end: 0,
                });
                out.len() - 1
            });
            let mut size = usize::from(mine.is_some());
            "o".hash(&mut h);
            for (k, c) in o {
                k.hash(&mut h);
                // What kind of node or intent it is belongs to the shape;
                // every other value is what a parameter would carry.
                if matches!(k.as_str(), "type" | "intent")
                    && let Some(s) = c.as_str()
                {
                    s.hash(&mut h);
                    continue;
                }
                let (ch, cs) = walk(c, format!("{at}/{}", escape(k)), out);
                ch.hash(&mut h);
                size += cs;
            }
            let shape = h.finish();
            if let Some(i) = mine {
                let end = out.len();
                let s = &mut out[i];
                (s.shape, s.size, s.end) = (shape, size, end);
            }
            (shape, size)
        }
        Value::Array(a) => {
            "a".hash(&mut h);
            let mut size = 0;
            for (i, c) in a.iter().enumerate() {
                let (ch, cs) = walk(c, format!("{at}/{i}"), out);
                ch.hash(&mut h);
                size += cs;
            }
            (h.finish(), size)
        }
        Value::String(_) => (1, 0),
        Value::Number(_) => (2, 0),
        Value::Bool(_) => (3, 0),
        Value::Null => (4, 0),
    }
}

/// Every `<scheme>://<kind>/<id>` with a literal, generated-looking id in a
/// string of `v`, for the entity schemes of the pod's sources: the entity,
/// where it is first written, and how often.
fn literal_rows(
    v: &Value,
    at: String,
    schemes: &[String],
    out: &mut BTreeMap<String, (String, usize)>,
) {
    match v {
        Value::Object(o) => {
            for (k, c) in o {
                literal_rows(c, format!("{at}/{}", escape(k)), schemes, out);
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                literal_rows(c, format!("{at}/{i}"), schemes, out);
            }
        }
        Value::String(s) => {
            for scheme in schemes {
                let mut rest = s.as_str();
                while let Some(i) = rest.find(scheme.as_str()) {
                    rest = &rest[i + scheme.len()..];
                    let word = |t: &str| -> String {
                        t.chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                            .collect()
                    };
                    let kind = word(rest);
                    let Some(after) = rest[kind.len()..].strip_prefix('/') else {
                        continue;
                    };
                    let id = word(after);
                    let next = after[id.len()..].chars().next();
                    // A longer path names something else (a collection's
                    // filler), `$item.id` is no literal, and a short word
                    // (`status`, `server`) is a fixed entity, not a row.
                    let generated = id.len() >= 8
                        && id.chars().any(|c| c.is_ascii_digit())
                        && id.chars().any(|c| c.is_ascii_alphabetic());
                    if kind.is_empty() || !generated || matches!(next, Some('/' | '$' | '.')) {
                        continue;
                    }
                    let e = out
                        .entry(format!("{scheme}{kind}/{id}"))
                        .or_insert_with(|| (at.clone(), 0));
                    e.1 += 1;
                }
            }
        }
        _ => {}
    }
}

/// Every component a layout of this pod may use, as patterns: each one
/// expanded with placeholders for its arguments, once with every parameter
/// given and once with only those that have no default.
fn patterns(pod: &Pod, writing: Option<&str>, deadline: Instant) -> Vec<Pattern> {
    let defs: Vec<(String, Value)> = pod.store().with_conn(|conn| {
        let mut out = Vec::new();
        for (name, hash) in crate::construct::store::names(conn).unwrap_or_default() {
            if crate::construct::is_layout_name(&name) || Some(name.as_str()) == writing {
                continue;
            }
            if let Ok(Some((el, _))) = crate::construct::store::object(conn, &hash)
                && el.get("params").is_some()
            {
                out.push((name, el["params"].clone()));
            }
        }
        out
    });
    let mut out = Vec::new();
    for (name, params) in defs {
        if Instant::now() > deadline {
            break;
        }
        let params = params.as_array().cloned().unwrap_or_default();
        let defaults: Map<String, Value> = params
            .iter()
            .filter_map(|p| Some((p["name"].as_str()?.to_owned(), p.get("default")?.clone())))
            .collect();
        for full in [true, false] {
            let mut args = Map::new();
            let mut numbers = Vec::new();
            for p in &params {
                let Some(n) = p["name"].as_str() else {
                    continue;
                };
                if !full && p.get("default").is_some() {
                    continue;
                }
                let v = match p["type"].as_str().unwrap_or("text") {
                    "number" => {
                        numbers.push(n.to_owned());
                        json!(NUMBER_ARG + numbers.len() as i64 - 1)
                    }
                    "bool" => json!(true),
                    "json" => json!({ "\u{a7}arg": n }),
                    _ => json!(text_arg(n)),
                };
                args.insert(n.to_owned(), v);
            }
            let mut bools = Map::new();
            for p in &params {
                if p["type"] == json!("bool")
                    && let Some(n) = p["name"].as_str()
                    && args.contains_key(n)
                {
                    bools.insert(n.to_owned(), json!(true));
                }
            }
            let uses = json!({ "type": "core://Use", "ref": name, "args": args,
                "children": [{ "type": "core://Spacer", "size_dp": CHILDREN_ARG }] });
            let Ok(node) = crate::construct::write::draft_plumbing(pod, &uses) else {
                continue;
            };
            let Some(root_type) = node["type"].as_str().map(str::to_owned) else {
                continue;
            };
            let (count, fixed) = weigh(&node, false);
            // A pattern of one node, or one with nothing fixed in it, is
            // any node of its type: no advice.
            if count < 2 || fixed == 0 {
                continue;
            }
            let mut defaults = defaults.clone();
            // A bool the pattern was drawn with is part of what matched.
            for (k, v) in &bools {
                if defaults.get(k) != Some(v) {
                    defaults.remove(k);
                }
            }
            out.push(Pattern {
                name: name.clone(),
                root_type,
                node,
                numbers,
                defaults,
                nodes: count,
            });
        }
    }
    // The larger pattern first: a row before the icon inside it.
    out.sort_by_key(|a| std::cmp::Reverse(a.nodes));
    out
}

/// `(typed nodes, fixed values)` of a pattern: the values that are not
/// placeholders and not a node's `type`.
fn weigh(v: &Value, is_type: bool) -> (usize, usize) {
    match v {
        Value::Object(o) => {
            if o.contains_key("\u{a7}arg") {
                return (0, 0);
            }
            let mut sum = (
                usize::from(is_node(v) && v["size_dp"] != json!(CHILDREN_ARG)),
                0,
            );
            for (k, c) in o {
                let (n, f) = weigh(c, k == "type");
                sum = (sum.0 + n, sum.1 + f);
            }
            sum
        }
        Value::Array(a) => a.iter().fold((0, 0), |s, c| {
            let (n, f) = weigh(c, false);
            (s.0 + n, s.1 + f)
        }),
        Value::String(s) => (0, usize::from(!is_type && !s.contains('\u{a7}'))),
        Value::Number(n) => (0, usize::from(n.as_i64().is_none_or(|i| i < NUMBER_ARG))),
        Value::Bool(_) => (0, 1),
        Value::Null => (0, 0),
    }
}

fn bind(bound: &mut Map<String, Value>, name: &str, v: &Value) -> bool {
    match bound.get(name) {
        Some(had) => had == v,
        None => {
            bound.insert(name.to_owned(), v.clone());
            true
        }
    }
}

fn is_children(p: &Value) -> bool {
    p["type"] == json!("core://Spacer") && p["size_dp"] == json!(CHILDREN_ARG)
}

/// Whether `n` is what pattern `p` draws, filling `bound` with the
/// arguments that make it so.
fn matches(p: &Value, n: &Value, pat: &Pattern, bound: &mut Map<String, Value>) -> bool {
    match p {
        Value::String(s) => {
            let Some(start) = s.find('\u{a7}') else {
                return p == n;
            };
            let rest = &s[start + '\u{a7}'.len_utf8()..];
            let Some(len) = rest.find('\u{a7}') else {
                return p == n;
            };
            let (name, after) = (&rest[..len], &rest[len + '\u{a7}'.len_utf8()..]);
            let before = &s[..start];
            if before.is_empty() && after.is_empty() {
                return bind(bound, name, n);
            }
            // A placeholder inside other text: the text around it must be
            // there, and what is between is the argument.
            let Some(text) = n.as_str() else {
                return false;
            };
            if after.contains('\u{a7}') {
                return false;
            }
            text.strip_prefix(before)
                .and_then(|t| t.strip_suffix(after))
                .is_some_and(|mid| bind(bound, name, &json!(mid)))
        }
        Value::Number(x) => {
            if let Some(i) = x.as_i64()
                && i >= NUMBER_ARG
                && let Some(name) = pat.numbers.get((i - NUMBER_ARG) as usize)
            {
                return n.is_number() && bind(bound, name, n);
            }
            n.is_number() && x.as_f64() == n.as_f64()
        }
        Value::Object(po) => {
            if let Some(name) = po.get("\u{a7}arg").and_then(Value::as_str) {
                return bind(bound, name, n);
            }
            let Some(no) = n.as_object() else {
                return false;
            };
            po.len() == no.len()
                && po
                    .iter()
                    .all(|(k, pv)| no.get(k).is_some_and(|nv| matches(pv, nv, pat, bound)))
        }
        Value::Array(pa) => {
            let Some(na) = n.as_array() else {
                return false;
            };
            match pa.iter().position(is_children) {
                None => {
                    pa.len() == na.len()
                        && pa
                            .iter()
                            .zip(na)
                            .all(|(pv, nv)| matches(pv, nv, pat, bound))
                }
                Some(k) => {
                    let tail = pa.len() - k - 1;
                    na.len() >= k + tail
                        && pa[..k]
                            .iter()
                            .zip(na)
                            .all(|(pv, nv)| matches(pv, nv, pat, bound))
                        && pa[k + 1..]
                            .iter()
                            .zip(&na[na.len() - tail..])
                            .all(|(pv, nv)| matches(pv, nv, pat, bound))
                        && bind(bound, "children", &json!(na[k..na.len() - tail]))
                }
            }
        }
        _ => p == n,
    }
}

/// The pod setting that holds what agents said was missing: a JSON list of
/// `{at, what}`, newest last.
pub const GAPS_SETTING: &str = "construct.gaps";
/// The most gaps a pod keeps, and the longest one may be.
const MAX_GAPS: usize = 100;
const MAX_GAP_CHARS: usize = 300;

/// MCP `note_gap {what}`: an agent found nothing that fitted and wrote
/// plumbing; it says in a line what it needed. The operator reads the
/// lines (`server pods gaps`) to learn what the core set lacks, so nobody
/// has to upstream a component by hand.
pub fn note_gap(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let what: String = args["what"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if what.is_empty() {
        return Err(json!({ "error": "invalid_arguments",
            "detail": "pass \"what\": one line naming the shape that was missing (\"a row with a leading checkbox and a trailing tag\"), not the user's content" }));
    }
    let what: String = what.chars().take(MAX_GAP_CHARS).collect();
    let mut gaps = gaps(pod.store());
    gaps.push(json!({ "at": crate::util::now_secs(), "what": what }));
    let drop = gaps.len().saturating_sub(MAX_GAPS);
    gaps.drain(..drop);
    pod.store()
        .set_pod_setting(GAPS_SETTING, &Value::Array(gaps.clone()).to_string())
        .map_err(|e| json!({ "error": "internal", "detail": e.to_string() }))?;
    Ok(json!({ "ok": true, "noted": gaps.len() }))
}

/// What agents of this pod said was missing, oldest first.
pub fn gaps(store: &crate::store::Store) -> Vec<Value> {
    store
        .pod_setting(GAPS_SETTING)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str::<Vec<Value>>(&v).ok())
        .unwrap_or_default()
}
