//! Public links (decisions `public-web-links`,
//! `public-links-share-the-closure`): a layout, the layouts it opens and
//! embeds, and the rows they show, copied to the host for anyone with the
//! link to look at in a browser, read-only, without a pod. Only when the
//! host allows it (`CONSTRUCT_PUBLIC_WEB=1`).
//!
//! Privacy is storage (decision `sharing`): a visitor never reaches the pod.
//! Sharing copies the layout as the owner's device draws it (its plumbing,
//! components expanded) with its data, and so each layout it opens or
//! embeds whose data may go out, then takes out everything that could reach
//! further: what writes, what loads more, links to layouts it doesn't
//! carry, screens. Only Collections data goes, and of it only the rows the
//! copy's layouts show (each list's `where` and each condition applied,
//! with the `$route` and `$item` of each place it is drawn; a condition
//! that can't be decided keeps its rows home) and the fields they read
//! where they show. The owner reviews the copy (every text and value a
//! visitor would read) before it goes out, and the pod copies it again when
//! its Collections data changes, so the page stays current: new rows wait
//! for the next review unless the link lets them out. Revoking takes the
//! copy off the host, and what it showed off the owner's list.

use axum::http::HeaderMap;
use protocol::{DataBundle, EntityRecord};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::Pod;
use crate::construct::share::Shelf;

/// What data of a source a public link may carry ([`crate::Source::public_data`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicData {
    /// Entity URI prefixes.
    pub uris: Vec<&'static str>,
    /// Query key prefixes.
    pub queries: Vec<&'static str>,
}

impl PublicData {
    /// What every source of the pod lets out, and their names.
    fn of(pod: &Pod) -> (PublicData, Vec<&'static str>) {
        let mut all = PublicData::default();
        let mut names = Vec::new();
        for a in pod.catalog().adapters() {
            let d = a.public_data();
            if !d.uris.is_empty() || !d.queries.is_empty() {
                names.push(a.name());
            }
            all.uris.extend(d.uris);
            all.queries.extend(d.queries);
        }
        (all, names)
    }
}

/// The largest copy (its layouts and data, as JSON): a whole recipe book,
/// every recipe with its lines and steps (decision `public-link-size`).
pub const MAX_COPY_BYTES: usize = 2 * 1024 * 1024;
/// Most public links a pod keeps live.
pub const MAX_LIVE: usize = 20;
/// Most revoked public links a pod's list keeps; older ones are deleted.
pub const MAX_REVOKED: usize = 20;
/// Wait this long after a data change before copying again, so a burst of
/// writes makes one copy.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// A public copy on the host.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicCopy {
    /// `pub-` and 32 base32 characters: unguessable; the page is `/s/<link>`.
    pub link: String,
    pub owner_pod: String,
    /// The layout it copies (`home`, `c-…`).
    pub layout: String,
    /// What it was asked for ([`Ask::key`]): the rows it shows (entity
    /// URIs; none: every row the layout shows), `item:<uri>`,
    /// `param:<name>=<value>` and `leave_out:<layout>`.
    pub rows: Vec<String>,
    pub title: String,
    /// `{format, layout, title, node, data, item?, params?, layouts?}`:
    /// what a visitor's page draws (format 1 had only `title`, `node`,
    /// `data` and `item`).
    pub copy: Value,
    /// A chosen short path, `/<owner's handle>/<slug>`.
    pub slug: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub revoked_at: Option<i64>,
}

/// A new public link.
pub fn new_link() -> String {
    let mut buf = [0u8; 20];
    getrandom::fill(&mut buf).expect("OS random number generator");
    format!("pub-{}", construct_elements::canonical::base32(&buf))
}

/// Whether `s` looks like a public link.
pub fn is_link(s: &str) -> bool {
    s.strip_prefix("pub-").is_some_and(|r| {
        (16..=64).contains(&r.len())
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// The intents a public page keeps: nothing that writes, loads or signs
/// in; an open only of a layout the copy carries.
const KEPT_INTENTS: [&str; 4] = [
    "action.navigation.back",
    "action.local.set",
    "action.link.open",
    "action.form.clear",
];
const OPEN: &str = "action.navigation.open";

/// Nodes whose job is an intent: without it they go (a button that would
/// write), or their content stays (a tappable card).
const CONTROLS: [&str; 5] = [
    "core://Button",
    "core://IconButton",
    "core://Switch",
    "core://MediaPicker",
    "core://TextField",
];
const WRAPPERS: [&str; 3] = ["core://Tappable", "core://Gesture", "core://SwipeCard"];

/// The layout an open (`action.navigation.open`) or embed names, when it
/// names one plainly (no `$`, no screen of it).
fn target(o: &Map<String, Value>) -> Option<&str> {
    let layout = o.get("layout")?.as_str()?;
    let plain = !layout.is_empty() && !layout.contains('$');
    let screen = o
        .get("target_uri")
        .and_then(Value::as_str)
        .is_some_and(|t| !t.is_empty());
    (plain && !screen).then_some(layout)
}

/// Whether an intent (a node's `on_click`, …) may stay: it writes nothing
/// and goes nowhere outside the copy (`carried`: the layouts in it).
fn kept(intent: &Value, carried: &BTreeSet<String>) -> bool {
    match intent["intent"].as_str() {
        Some(i) if KEPT_INTENTS.contains(&i) => true,
        Some(OPEN) => intent
            .as_object()
            .and_then(target)
            .is_some_and(|t| carried.contains(t)),
        Some("action.sequence") => intent["steps"]
            .as_array()
            .is_some_and(|s| s.iter().all(|x| kept(x, carried))),
        _ => false,
    }
}

/// Whether a node carries an intent that may not stay.
fn has_dropped_intent(o: &Map<String, Value>, carried: &BTreeSet<String>) -> bool {
    o.iter().any(|(k, v)| {
        k != "item_template" && v.get("intent").is_some_and(|_| !kept(v, carried))
            || v.as_array().is_some_and(|a| {
                a.iter()
                    .any(|x| x.get("intent").is_some_and(|_| !kept(x, carried)))
            })
    })
}

/// What was taken out of a copy, for the review.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Removed {
    pub controls: usize,
    pub embeds: usize,
    pub screens: usize,
}

/// The copy of a plumbing node a visitor gets: controls that would write or
/// go outside the copy taken out (a tappable keeps its content), embeds of
/// layouts it doesn't carry and screens dropped. `carried`: the layouts the
/// copy carries, whose opens and embeds stay. `None`: the node goes.
pub fn strip(v: &Value, carried: &BTreeSet<String>, removed: &mut Removed) -> Option<Value> {
    match v {
        Value::Array(a) => Some(Value::Array(
            a.iter()
                .filter_map(|x| strip(x, carried, removed))
                .collect(),
        )),
        Value::Object(o) => {
            let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
            if ty == "core://Layout" && !target(o).is_some_and(|t| carried.contains(t)) {
                removed.embeds += 1;
                return None;
            }
            if has_dropped_intent(o, carried) {
                if WRAPPERS.contains(&ty) {
                    removed.controls += 1;
                    return o.get("content").and_then(|c| strip(c, carried, removed));
                }
                if CONTROLS.contains(&ty) || o.contains_key("on_click") {
                    removed.controls += 1;
                    return None;
                }
            }
            let mut out = Map::new();
            for (k, x) in o {
                if k == "screens" {
                    removed.screens += x.as_array().map_or(0, Vec::len);
                    continue;
                }
                // Intents left in other places (a field's on_submit, a
                // chip's) go; kept ones stay.
                if x.get("intent").is_some() && !kept(x, carried) {
                    removed.controls += 1;
                    continue;
                }
                match strip(x, carried, removed) {
                    Some(y) => {
                        out.insert(k.clone(), y);
                    }
                    // A required child that went: an empty space.
                    None => {
                        out.insert(k.clone(), json!({ "type": "core://Spacer", "size_dp": 0 }));
                    }
                }
            }
            Some(Value::Object(out))
        }
        other => Some(other.clone()),
    }
}

/// The field names a node reads: `$item.<f>` and `${<uri>#<f>}` in its
/// strings, and the names under the keys that hold a field name (a list's
/// `where`, `filter` and `group_by`, a count's `where`, `indent`, a
/// carousel's `aspect_field`). Other keys and values of the plumbing
/// (`"label"`, `"title"`, `"style"`…) are not field names: a row field named
/// like one stays out unless something reads it.
fn fields_read(node: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    read_fields(node, &mut out);
    out.remove("");
    out
}

fn read_fields(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => {
            for (i, _) in s.match_indices("$item.") {
                out.insert(word(&s[i + 6..]).to_owned());
            }
            for (i, _) in s.match_indices('#') {
                let w = word(&s[i + 1..]);
                if s[i + 1 + w.len()..].starts_with('}') {
                    out.insert(w.to_owned());
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| read_fields(x, out)),
        Value::Object(o) => {
            for (k, x) in o {
                names_under(k, x, out);
                read_fields(x, out);
            }
        }
        _ => {}
    }
}

/// The field names a key that holds them names (a `where`, …).
fn names_under(k: &str, x: &Value, out: &mut BTreeSet<String>) {
    match k {
        // One condition or a list of them: each names its field.
        "where" => {
            let all = x.as_array().map_or(std::slice::from_ref(x), Vec::as_slice);
            for c in all {
                names(&c["field"], out);
            }
        }
        "filter" => names(&x["fields"], out),
        "group_by" | "indent" => names(&x["field"], out),
        "aspect_field" => names(x, out),
        _ => {}
    }
}

/// A field name, or a list of them, as written.
fn names(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => {
            out.insert(s.clone());
        }
        Value::Array(a) => a.iter().for_each(|x| names(x, out)),
        _ => {}
    }
}

/// The name at the start of `s` (letters, digits, `_`).
fn word(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// The pod's theme (its colours): a copy carries it whole, so the page looks
/// as the owner's does.
const THEME: &str = "core://theme";

/// Whether every piece of data is one its source lets out ([`PublicData`]);
/// else the kinds that aren't.
fn shareable(data: &DataBundle, allowed: &PublicData) -> Result<(), Vec<String>> {
    let mut other: BTreeSet<String> = BTreeSet::new();
    for uri in data.entities.keys() {
        let ok = allowed.uris.iter().any(|p| uri.starts_with(p))
            || uri.starts_with("core://sources/")
            || uri == THEME;
        if !ok {
            other.insert(uri.split("://").next().unwrap_or("").to_owned());
        }
    }
    for key in data.queries.keys() {
        if !allowed.queries.iter().any(|p| key.starts_with(p)) {
            other.insert(key.split(':').next().unwrap_or("").to_owned());
        }
    }
    if other.is_empty() {
        Ok(())
    } else {
        Err(other.into_iter().collect())
    }
}

/// One use of a layout in a copy: its `$route` and `$item` there.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Use {
    pub route: BTreeMap<String, String>,
    pub item: Option<String>,
}

/// Most uses of layouts one copy walks (a list of 300 meals each pushing
/// its recipe is 301).
const MAX_USES: usize = 2000;

/// What a copy's layouts show, found by drawing them in the head: for each
/// use of each layout (the root with its `params` and `item`, a layout
/// pushed from a row with that row's params), the rows each list keeps
/// after its `where`, the rows a `${<uri>#<f>}` names, and the fields read
/// of each.
struct Shows<'a> {
    pool: &'a DataBundle,
    /// The layouts the copy carries (their stripped nodes).
    nodes: &'a BTreeMap<String, Value>,
    root: &'a str,
    /// The root's lists hold only these rows (`rows`), when given.
    only: &'a BTreeSet<String>,
    /// Shown rows and the fields read of each.
    rows: BTreeMap<String, BTreeSet<String>>,
    /// Shown rows by query key (every key a list reads, even when empty).
    queries: BTreeMap<String, BTreeSet<String>>,
    /// Shown rows by layout (for the review and the size).
    by_layout: BTreeMap<String, BTreeSet<String>>,
    /// Rows of each query key a list's template draws, and those it hides
    /// (its root a condition that is false for the row, or can't be
    /// decided): a hidden row goes nowhere through that key, not even
    /// through a count of it.
    drawn: BTreeMap<String, BTreeSet<String>>,
    hidden: BTreeMap<String, BTreeSet<String>>,
    /// Rows counts read (key → row → (layout, fields)), added once every
    /// list is walked, unless a list of that key hides them.
    counted: BTreeMap<String, BTreeMap<String, (String, BTreeSet<String>)>>,
    /// Row fields a visitor's own page sets (a kept `action.local.set` of
    /// a row), each with the sets that write it: a condition on them is the
    /// visitor's to change, as far as those sets reach.
    visitor_fields: BTreeMap<String, Vec<VisitorSet>>,
    /// The pod's clock when the copy is made: its zone and the minute it is
    /// there (decision `public-links-decide-ages`). `None`: an age can't be
    /// decided, and its rows stay out.
    clock: Option<Clock>,
    seen: BTreeSet<(String, Use)>,
    todo: Vec<(String, Use)>,
}

/// A moment on the pod's wall clock, as the renderer reads a list's ages.
struct Clock {
    zone: jiff::tz::TimeZone,
    /// Local minutes since the epoch.
    now: i64,
}

impl Clock {
    /// Now, in `zone`.
    fn now(zone: jiff::tz::TimeZone) -> Option<Clock> {
        let local = jiff::Timestamp::now().to_zoned(zone.clone()).datetime();
        let text = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            local.year(),
            local.month(),
            local.day(),
            local.hour(),
            local.minute()
        );
        let m = construct_render::dates::moment(&text, "", Some(&zone))?;
        Some(Clock {
            now: m.day * 1_440 + m.minute.unwrap_or(0),
            zone,
        })
    }

    /// Whether `value` is more than `older_than` ago, as the renderer's
    /// `Where` tests it: an empty or unreadable value, or a duration that
    /// doesn't read, never is.
    fn older(&self, value: Option<&Value>, older_than: &str) -> bool {
        let Some(secs) = protocol::ast::duration_secs(older_than) else {
            return false;
        };
        value
            .and_then(Value::as_str)
            .and_then(|t| construct_render::dates::moment(t, "", Some(&self.zone)))
            .map(|m| m.day * 1_440 + m.minute.unwrap_or(0))
            .is_some_and(|at| at + (secs + 59) / 60 <= self.now)
    }
}

/// One kept `action.local.set` of a field: which row it writes and what.
#[derive(Debug, Clone, PartialEq)]
struct VisitorSet {
    /// The one row it writes (an `entity_uri` without bindings); `None`:
    /// whichever row it is bound to, so any.
    target: Option<String>,
    /// The values it writes (a JSON literal `value_expr`); `None`: one
    /// computed from the row (`!entity.open`) or a binding, so any.
    values: Option<Value>,
}

impl VisitorSet {
    fn reaches(&self, item: Option<&str>) -> bool {
        self.target.as_deref().is_none_or(|t| item == Some(t))
    }
}

/// What a condition comes to for a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Test {
    /// True or false, from the data the owner's screen draws.
    Known(bool),
    /// The visitor's own choice (`$form`, `local://`, a field their page
    /// sets): either way can be drawn, as on the owner's screen a tap away.
    Visitor,
    /// Not decided here (a date or an age, an unbound name): fail closed.
    Unknown,
}

impl Test {
    /// Whether what it guards goes in the copy.
    fn shows(self) -> bool {
        matches!(self, Test::Known(true) | Test::Visitor)
    }
    fn not(self, not: bool) -> Test {
        match self {
            Test::Known(b) => Test::Known(b != not),
            t => t,
        }
    }
}

/// A condition's value as the head can read it.
enum Read {
    Known(Value),
    Visitor,
    Unknown,
}

impl<'a> Shows<'a> {
    fn new(
        pool: &'a DataBundle,
        nodes: &'a BTreeMap<String, Value>,
        root: &'a str,
        only: &'a BTreeSet<String>,
    ) -> Self {
        Shows {
            pool,
            nodes,
            root,
            only,
            rows: BTreeMap::new(),
            queries: BTreeMap::new(),
            by_layout: BTreeMap::new(),
            drawn: BTreeMap::new(),
            hidden: BTreeMap::new(),
            counted: BTreeMap::new(),
            visitor_fields: nodes.values().fold(BTreeMap::new(), |mut out, n| {
                visitor_set(n, &mut out);
                out
            }),
            clock: None,
            seen: BTreeSet::new(),
            todo: Vec::new(),
        }
    }

    /// Walks every use reached from the root's; false past [`MAX_USES`].
    fn run(&mut self, first: Use) -> bool {
        self.todo.push((self.root.to_owned(), first));
        while let Some((id, u)) = self.todo.pop() {
            if !self.seen.insert((id.clone(), u.clone())) {
                continue;
            }
            if self.seen.len() > MAX_USES {
                return false;
            }
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            if let Some(i) = &u.item {
                let mut read = BTreeSet::new();
                self.read_shown(node, &u, Some(i), &mut read);
                read.remove("");
                self.show(&id, i, read);
            }
            let item = u.item.clone();
            self.walk(&id, node, &u, item.as_deref());
        }
        self.add_counted();
        true
    }

    /// The rows counts read, but none a list of the same key hides.
    fn add_counted(&mut self) {
        for (key, rows) in std::mem::take(&mut self.counted) {
            let hid = |r: &String| {
                self.hidden.get(&key).is_some_and(|h| h.contains(r))
                    && !self.drawn.get(&key).is_some_and(|d| d.contains(r))
            };
            let keep: Vec<_> = rows.into_iter().filter(|(r, _)| !hid(r)).collect();
            for (r, (layout, fields)) in keep {
                self.queries
                    .entry(key.clone())
                    .or_default()
                    .insert(r.clone());
                self.show(&layout, &r, fields);
            }
        }
    }

    fn show(&mut self, layout: &str, uri: &str, fields: BTreeSet<String>) {
        if !self.pool.entities.contains_key(uri) {
            return;
        }
        self.rows.entry(uri.to_owned()).or_default().extend(fields);
        self.by_layout
            .entry(layout.to_owned())
            .or_default()
            .insert(uri.to_owned());
    }

    /// `s` with `$route.<name>` and `$item.<field>` filled in (others left).
    fn bind(&self, s: &str, u: &Use, item: Option<&str>) -> String {
        let entity = item.and_then(|i| self.pool.entities.get(i));
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            let after = &rest[i..];
            if let Some(r) = after.strip_prefix("$route.") {
                let name = word(r);
                match u.route.get(name) {
                    Some(v) if !name.is_empty() => {
                        out.push_str(v);
                        rest = &r[name.len()..];
                        continue;
                    }
                    _ => {}
                }
            } else if let Some(r) = after.strip_prefix("$item.")
                && let Some(e) = entity
            {
                let name = word(r);
                if !name.is_empty() {
                    out.push_str(&e.get(name).map(render).unwrap_or_default());
                    rest = &r[name.len()..];
                    continue;
                }
            }
            out.push('$');
            rest = &after[1..];
        }
        out.push_str(rest);
        out
    }

    /// The rows `${<uri>#<field>}` in `s` names.
    fn lookups(&mut self, layout: &str, s: &str, u: &Use, item: Option<&str>) {
        let mut rest = s;
        while let Some(i) = rest.find("${") {
            let after = &rest[i + 2..];
            let Some(end) = after.find('}') else { break };
            let inner = self.bind(&after[..end], u, item);
            rest = &after[end + 1..];
            let (uri, field) = match inner.split_once('#') {
                Some((u, f)) => (u, Some(f)),
                None => (inner.as_str(), None),
            };
            if uri.contains('$')
                || !uri.contains("://")
                || uri.starts_with("local://")
                || uri.starts_with("core://")
            {
                continue;
            }
            let fields = field.map(|f| word(f).to_owned()).into_iter().collect();
            self.show(layout, uri, fields);
        }
    }

    /// The rows of `key` (bound) a list keeps: its `where` holds, and, in
    /// the root, only `rows` when given.
    fn kept_rows(
        &self,
        layout: &str,
        o: &Map<String, Value>,
        u: &Use,
        item: Option<&str>,
    ) -> (String, Vec<String>) {
        let key = self.bind(o["query_key"].as_str().unwrap_or(""), u, item);
        let conds: Vec<&Value> = match o.get("where") {
            Some(Value::Array(a)) => a.iter().collect(),
            Some(w @ Value::Object(_)) => vec![w],
            _ => Vec::new(),
        };
        let rows = self
            .pool
            .queries
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|r| layout != self.root || self.only.is_empty() || self.only.contains(*r))
            .filter(|r| {
                let e = self.pool.entities.get(*r);
                conds
                    .iter()
                    .all(|c| self.holds(c, e, u, item) == Some(true))
            })
            .cloned()
            .collect();
        (key, rows)
    }

    /// Whether a `where` condition holds for a row, as the renderer tests
    /// it (`layout.rs` `Where`). `None`: it can't be decided here (an
    /// unbound name, or an age without the pod's clock), and the row stays
    /// out. An age is decided on the pod's clock as the copy is made
    /// (decision `public-links-decide-ages`): time only moves one way, so a
    /// row an age hides now never shows later, and one it shows now that
    /// ages out is hidden by the visitor's page as by the owner's. An
    /// `equals` on what the visitor picks (`$form`, `local://`) holds:
    /// their page starts blank, which matches every row, as the owner's
    /// does.
    fn holds(
        &self,
        c: &Value,
        e: Option<&EntityRecord>,
        u: &Use,
        item: Option<&str>,
    ) -> Option<bool> {
        if let Some(age) = c.get("older_than").filter(|v| !v.is_null()) {
            let clock = self.clock.as_ref()?;
            let field = c["field"].as_str().unwrap_or("");
            let older = clock.older(e.and_then(|e| e.get(field)), age.as_str().unwrap_or(""));
            return Some(older != (c["not"] == true));
        }
        let raw = c["equals"].as_str().unwrap_or("");
        if self.visitor(raw, item) {
            return Some(true);
        }
        let want = self.bind(raw, u, item);
        if want.trim().is_empty() {
            return Some(true);
        }
        if want.contains('$') {
            return None;
        }
        let field = c["field"].as_str().unwrap_or("");
        let hit = e.and_then(|e| e.get(field)).is_some_and(|v| match v {
            Value::String(s) => *s == want,
            Value::Bool(b) => want == if *b { "true" } else { "false" },
            other => render(other) == want,
        });
        Some(hit != (c["not"] == true))
    }

    /// Whether `expr` reads what the visitor's own page decides: a form
    /// value, a `local://` entity, a field of row `item` their page sets.
    fn visitor(&self, expr: &str, item: Option<&str>) -> bool {
        expr.contains("$form.")
            || expr.contains("local://")
            || expr.match_indices("$item.").any(|(i, _)| {
                self.visitor_fields
                    .get(word(&expr[i + "$item.".len()..]))
                    .is_some_and(|sets| sets.iter().any(|s| s.reaches(item)))
            })
    }

    /// The values `$item.<field>` of row `item` can have on the visitor's
    /// page when `expr` is that and only fixed values are set on it: the
    /// row's own, then each a set writes. `None`: it isn't such a read
    /// (it is the row's alone, or a set computes what it writes).
    fn visitor_values(&self, expr: &str, item: Option<&str>) -> Option<Vec<Value>> {
        let f = expr.trim().strip_prefix("$item.")?;
        if f.is_empty() || word(f) != f {
            return None;
        }
        let sets: Vec<&VisitorSet> = self
            .visitor_fields
            .get(f)?
            .iter()
            .filter(|s| s.reaches(item))
            .collect();
        if sets.is_empty() {
            return None;
        }
        let row = item.and_then(|i| self.pool.entities.get(i));
        let mut out = vec![row.and_then(|r| r.get(f)).cloned().unwrap_or(Value::Null)];
        for s in sets {
            out.push(s.values.clone()?);
        }
        Some(out)
    }

    /// A condition's value (`core://Conditional` `value`, an embed's
    /// `show_when`), as the renderer reads it (`bind.rs` `value`).
    fn read(&self, expr: &str, u: &Use, item: Option<&str>) -> Read {
        if self.visitor(expr, item) {
            return Read::Visitor;
        }
        let e = expr.trim();
        if let Some(f) = e.strip_prefix("$item.")
            && !f.is_empty()
            && word(f) == f
        {
            let row = item.and_then(|i| self.pool.entities.get(i));
            return Read::Known(row.and_then(|r| r.get(f)).cloned().unwrap_or(Value::Null));
        }
        match self.text(e, u, item) {
            Some(t) => Read::Known(Value::String(t)),
            None => Read::Unknown,
        }
    }

    /// `s` with `$route`, `$item` and `${<uri>#<field>}` filled in from
    /// the data; `None` when something is left unbound.
    fn text(&self, s: &str, u: &Use, item: Option<&str>) -> Option<String> {
        let bound = self.bind(s, u, item);
        let mut out = String::new();
        let mut rest = bound.as_str();
        while let Some(i) = rest.find("${") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let end = after.find('}')?;
            let (uri, field) = after[..end].split_once('#')?;
            if uri.contains('$') {
                return None;
            }
            let v = self.pool.entities.get(uri).and_then(|e| e.get(field));
            out.push_str(&v.map(render).unwrap_or_default());
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        (!out.contains('$')).then_some(out)
    }

    /// Whether a `core://Conditional` shows its content, or an embed with
    /// `show_when` draws, for this use and row (`bind.rs` `shows`).
    fn test(&self, o: &Map<String, Value>, u: &Use, item: Option<&str>) -> Test {
        let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
        let not = o.get("not") == Some(&Value::Bool(true));
        if ty == "core://Layout" {
            let Some(when) = o.get("show_when").and_then(Value::as_str) else {
                return Test::Known(true);
            };
            return self.compare(when, o, u, item);
        }
        if ty != "core://Conditional" {
            return Test::Known(true);
        }
        if o.get("date").is_some_and(|d| !d.is_null()) {
            // The visitor's clock decides, after the copy is made.
            return Test::Unknown;
        }
        if let Some(c) = o.get("count").and_then(Value::as_object) {
            let key = self.bind(
                c.get("query_key").and_then(Value::as_str).unwrap_or(""),
                u,
                item,
            );
            if key.contains('$') {
                return Test::Unknown;
            }
            let conds: Vec<&Value> = match c.get("where") {
                Some(Value::Array(a)) => a.iter().collect(),
                Some(w @ Value::Object(_)) => vec![w],
                _ => Vec::new(),
            };
            let mut t = Test::Known(false);
            for r in self.pool.queries.get(&key).into_iter().flatten() {
                let e = self.pool.entities.get(r);
                let all = conds.iter().map(|c| self.holds(c, e, u, item));
                let all: Option<Vec<bool>> = all.collect();
                match all {
                    Some(v) if v.iter().all(|b| *b) => {
                        t = Test::Known(true);
                        break;
                    }
                    Some(_) => {}
                    None => t = Test::Unknown,
                }
            }
            return t.not(not);
        }
        let value = o.get("value").and_then(Value::as_str).unwrap_or("");
        self.compare(value, o, u, item).not(not)
    }

    /// A value tested against `equals`, or for truthiness.
    /// A row field the visitor's page sets only to fixed values is decided
    /// for each value it can have: the visitor decides only when they
    /// differ (a "Hide" that sets `private` to true never shows what
    /// `private` hides).
    fn compare(&self, value: &str, o: &Map<String, Value>, u: &Use, item: Option<&str>) -> Test {
        if let Some(values) = self.visitor_values(value, item) {
            let mut tests = values.iter().map(|v| self.against(v, o, u, item));
            let first = tests.next().unwrap_or(Test::Unknown);
            return if tests.all(|t| t == first) {
                first
            } else {
                Test::Visitor
            };
        }
        match self.read(value, u, item) {
            Read::Known(v) => self.against(&v, o, u, item),
            Read::Visitor => Test::Visitor,
            Read::Unknown => Test::Unknown,
        }
    }

    /// `v` tested against `o`'s `equals`, or for truthiness.
    fn against(&self, v: &Value, o: &Map<String, Value>, u: &Use, item: Option<&str>) -> Test {
        match o.get("equals").and_then(Value::as_str) {
            None => Test::Known(truthy(v)),
            Some(e) if self.visitor(e, item) => Test::Visitor,
            Some(e) => match self.text(e, u, item) {
                Some(want) => Test::Known(render(v) == want),
                None => Test::Unknown,
            },
        }
    }

    /// Whether a template draws anything for a row: false when its root
    /// (through conditions) is a condition that doesn't show for it.
    fn draws(&self, t: &Value, u: &Use, item: Option<&str>) -> bool {
        let mut n = t;
        loop {
            let Some(o) = n.as_object() else { return true };
            if !self.test(o, u, item).shows() {
                return false;
            }
            match (o.get("type").and_then(Value::as_str), o.get("content")) {
                (Some("core://Conditional"), Some(c)) => n = c,
                _ => return true,
            }
        }
    }

    /// The fields a node reads of `item` where it draws: as
    /// [`fields_read`], but not under a condition that doesn't show, nor in
    /// a nested list's template (that reads its own rows).
    fn read_shown(&self, v: &Value, u: &Use, item: Option<&str>, out: &mut BTreeSet<String>) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| self.read_shown(x, u, item, out)),
            Value::Object(o) => {
                let shows = self.test(o, u, item).shows();
                let list = o.get("query_key").is_some_and(Value::is_string);
                let conditional =
                    o.get("type").and_then(Value::as_str) == Some("core://Conditional");
                for (k, x) in o {
                    let skip = (conditional && k == "content" && !shows)
                        || (list && (k == "item_template" || k == "item"));
                    if skip {
                        continue;
                    }
                    names_under(k, x, out);
                    self.read_shown(x, u, item, out);
                }
            }
            other => read_fields(other, out),
        }
    }

    fn walk(&mut self, layout: &str, v: &Value, u: &Use, item: Option<&str>) {
        match v {
            Value::String(s) => self.lookups(layout, s, u, item),
            Value::Array(a) => a.iter().for_each(|x| self.walk(layout, x, u, item)),
            Value::Object(o) => {
                let mut templates: &[&str] = &[];
                if o.get("query_key").is_some_and(Value::is_string) {
                    // A list (or a dock, chips): the rows it keeps that its
                    // template draws, each with the fields read of it there
                    // (the list's own, its template's where it shows for
                    // that row), and its template per row.
                    let (key, rows) = self.kept_rows(layout, o, u, item);
                    templates = &["item_template", "item"];
                    let mut own = o.clone();
                    for t in templates {
                        own.remove(*t);
                    }
                    let read = fields_read(&Value::Object(own));
                    let shown: Vec<&Value> = templates
                        .iter()
                        .filter_map(|t| o.get(*t).filter(|t| t.is_object()))
                        .collect();
                    self.queries.entry(key.clone()).or_default();
                    for r in &rows {
                        if !shown.iter().all(|t| self.draws(t, u, Some(r))) {
                            self.hidden
                                .entry(key.clone())
                                .or_default()
                                .insert(r.clone());
                            continue;
                        }
                        let mut fields = read.clone();
                        for t in &shown {
                            self.read_shown(t, u, Some(r), &mut fields);
                        }
                        fields.remove("");
                        self.drawn.entry(key.clone()).or_default().insert(r.clone());
                        self.queries
                            .entry(key.clone())
                            .or_default()
                            .insert(r.clone());
                        self.show(layout, r, fields);
                        for t in &shown {
                            self.walk(layout, t, u, Some(r));
                        }
                    }
                }
                if let Some(c) = o.get("count").and_then(Value::as_object)
                    && c.get("query_key").is_some_and(Value::is_string)
                {
                    // A count: its rows, with only the fields its `where`
                    // reads, once the lists are walked (a row a list of the
                    // same key hides isn't counted out).
                    let (key, rows) = self.kept_rows(layout, c, u, item);
                    let read = fields_read(&o["count"]);
                    self.queries.entry(key.clone()).or_default();
                    let counted = self.counted.entry(key).or_default();
                    for r in rows {
                        let e = counted
                            .entry(r)
                            .or_insert_with(|| (layout.to_owned(), BTreeSet::new()));
                        e.1.extend(read.iter().cloned());
                    }
                }
                if !self.test(o, u, item).shows() {
                    // A condition that doesn't show (or can't be decided):
                    // what it guards stays home; only what it tests goes.
                    for (k, x) in o {
                        if !matches!(k.as_str(), "content" | "layout" | "params" | "count") {
                            self.walk(layout, x, u, item);
                        }
                    }
                    return;
                }
                let is_open = o.get("intent").and_then(Value::as_str) == Some(OPEN);
                let is_embed = o.get("type").and_then(Value::as_str) == Some("core://Layout");
                if (is_open || is_embed)
                    && let Some(t) = target(o)
                    && self.nodes.contains_key(t)
                {
                    let params: BTreeMap<String, String> = o
                        .get("params")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flatten()
                        .map(|(k, x)| (k.clone(), self.bind(&render(x), u, item)))
                        .collect();
                    // An embed draws in its host: its route unless it sets one.
                    let next = if is_embed {
                        Use {
                            route: if params.is_empty() {
                                u.route.clone()
                            } else {
                                params
                            },
                            item: item.map(str::to_owned),
                        }
                    } else {
                        Use {
                            route: params,
                            item: None,
                        }
                    };
                    self.todo.push((t.to_owned(), next));
                }
                for (k, x) in o {
                    // A count's rows are counted above, not drawn.
                    let count = k == "count" && x.get("query_key").is_some_and(Value::is_string);
                    if !templates.contains(&k.as_str()) && !count {
                        self.walk(layout, x, u, item);
                    }
                }
            }
            _ => {}
        }
    }

    /// The data a visitor gets: the shown rows, each with only the fields
    /// read of it, the lists holding only them, and the theme.
    fn data(&self) -> DataBundle {
        let mut out = DataBundle::default();
        for (key, shown) in &self.queries {
            let kept = self
                .pool
                .queries
                .get(key)
                .into_iter()
                .flatten()
                .filter(|u| shown.contains(*u))
                .cloned()
                .collect();
            out.queries.insert(key.clone(), kept);
        }
        if let Some(t) = self.pool.entities.get(THEME) {
            out.entities.insert(THEME.to_owned(), t.clone());
        }
        for (uri, fields) in &self.rows {
            let Some(e) = self.pool.entities.get(uri) else {
                continue;
            };
            let mut m = Map::new();
            if let Ok(Value::Object(all)) = serde_json::to_value(e) {
                for (f, v) in all {
                    if f == "id" || fields.contains(&f) {
                        m.insert(f, v);
                    }
                }
            }
            if let Ok(rec) = serde_json::from_value::<EntityRecord>(Value::Object(m)) {
                out.entities.insert(uri.clone(), rec);
            }
        }
        out
    }
}

/// The row fields a copy's own `action.local.set` sets on the visitor's
/// device (a row's "expanded"), with the row and value each set writes:
/// what the visitor's page decides.
fn visitor_set(v: &Value, out: &mut BTreeMap<String, Vec<VisitorSet>>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| visitor_set(x, out)),
        Value::Object(o) => {
            let uri = o.get("entity_uri").and_then(Value::as_str).unwrap_or("");
            if o.get("intent").and_then(Value::as_str) == Some("action.local.set")
                && !uri.starts_with("local://")
            {
                let target = (!uri.contains('$')).then(|| uri.to_owned());
                let also = o.get("also").and_then(Value::as_array);
                for w in std::iter::once(v).chain(also.into_iter().flatten()) {
                    let mut fields = BTreeSet::new();
                    names(&w["field"], &mut fields);
                    let values = w["value_expr"]
                        .as_str()
                        .filter(|e| !e.contains('$'))
                        .and_then(|e| serde_json::from_str::<Value>(e.trim()).ok());
                    for f in fields {
                        out.entry(f).or_default().push(VisitorSet {
                            target: target.clone(),
                            values: values.clone(),
                        });
                    }
                }
            }
            o.values().for_each(|x| visitor_set(x, out));
        }
        _ => {}
    }
}

/// Whether a value is truthy as the renderer tests it (`bind.rs`
/// `truthy`): not missing, null, false, 0, "", "0", "false" or [].
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => {
            let s = s.trim();
            !(s.is_empty() || s == "0" || s == "false" || s.parse::<f64>() == Ok(0.0))
        }
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// A value as the renderer writes it.
fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The data a visitor gets of one layout drawn with `u` (only `rows` in
/// its lists when given): see [`Shows`].
pub fn minimize(data: &DataBundle, node: &Value, rows: &[String], u: Use) -> DataBundle {
    let nodes = BTreeMap::from([("root".to_owned(), node.clone())]);
    let only = rows.iter().cloned().collect();
    let mut s = Shows::new(data, &nodes, "root", &only);
    s.run(u);
    s.data()
}

/// Keys of a node whose strings a person reads.
const TEXT_KEYS: [&str; 9] = [
    "value_expr",
    "label",
    "title",
    "text",
    "placeholder",
    "empty_text",
    "no_match_text",
    "zero_text",
    "content_description",
];

/// What `s` says once expressions (`$item.x`, `${…}`) are taken out.
fn literal(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = match after.strip_prefix('{') {
            Some(inner) => inner.find('}').map_or(after.len(), |e| e + 2),
            None => after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(after.len()),
        };
        rest = &after[end..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// What a visitor would read: the copy's own texts (expressions taken out)
/// and its rows' values, and every URL.
fn review_texts(copy: &Value) -> (Vec<String>, Vec<String>) {
    let mut texts = BTreeSet::new();
    let mut urls = BTreeSet::new();
    fn walk(v: &Value, key: &str, texts: &mut BTreeSet<String>, urls: &mut BTreeSet<String>) {
        match v {
            Value::String(s) if s.starts_with("http://") || s.starts_with("https://") => {
                urls.insert(s.clone());
            }
            Value::String(s) if TEXT_KEYS.contains(&key) => {
                let t = literal(s);
                if t.chars().any(char::is_alphanumeric) {
                    texts.insert(t);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, key, texts, urls)),
            Value::Object(o) => o.iter().for_each(|(k, x)| walk(x, k, texts, urls)),
            _ => {}
        }
    }
    walk(&copy["node"], "", &mut texts, &mut urls);
    for (uri, e) in copy["data"]["entities"].as_object().into_iter().flatten() {
        if uri.starts_with("core://") {
            continue;
        }
        for (f, v) in e.as_object().into_iter().flatten() {
            match v {
                Value::String(s) if s.starts_with("http") => {
                    urls.insert(s.clone());
                }
                Value::String(s) if f != "id" && !s.is_empty() => {
                    texts.insert(s.clone());
                }
                Value::Number(n) if f != "id" => {
                    texts.insert(format!("{f}: {n}"));
                }
                _ => {}
            }
        }
    }
    (texts.into_iter().collect(), urls.into_iter().collect())
}

/// The review id of a copy and how it is shared: what `confirm` must
/// repeat. It covers the ask (rows, item, params, leave_out and
/// `new_rows`) and the slug as well as the copy, so a confirm can't change
/// what the review said, e.g. let new rows out without a review.
fn review_id(copy: &Value, ask: &Ask, slug: Option<&str>) -> String {
    let all = json!({ "copy": copy, "key": ask.key(), "slug": slug });
    let h = Sha256::digest(all.to_string().as_bytes());
    format!("review-{}", crate::util::hex(&h[..8]))
}

/// The copy's format (`format` in it; a copy without one is format 1, a
/// single layout). Clients draw every format they have served
/// (`construct_render::public`); a change to it is a new number.
pub const FORMAT: u64 = 2;

/// Most layouts one copy carries (the root and what it reaches).
pub const MAX_LAYOUTS: usize = 16;

/// What to copy: a layout and how it is drawn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ask {
    pub layout: String,
    /// Only these rows in the root's lists (empty: every row they show).
    pub rows: Vec<String>,
    /// The row the root draws as `$item`.
    pub item: Option<String>,
    /// The root's `$route.<name>`.
    pub params: BTreeMap<String, String>,
    /// Layouts not to carry: their opens and embeds go.
    pub leave_out: BTreeSet<String>,
    /// New rows go out without a review when the data changes (the owner
    /// chose it, `new_rows: "publish"`); else they wait for the next share.
    pub publish_new_rows: bool,
}

/// The key entry of [`Ask::publish_new_rows`].
const PUBLISH_NEW_ROWS: &str = "new_rows:publish";

impl Ask {
    /// What a stored copy's key (`PublicCopy::rows`) asked for: rows, and
    /// `item:<uri>`, `param:<name>=<value>`, `leave_out:<layout>` and
    /// `new_rows:publish`.
    pub fn from_key(layout: &str, key: &[String]) -> Ask {
        let mut a = Ask {
            layout: layout.to_owned(),
            ..Ask::default()
        };
        for r in key {
            if let Some(i) = r.strip_prefix("item:") {
                a.item = Some(i.to_owned());
            } else if let Some(p) = r.strip_prefix("param:") {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                a.params.insert(k.to_owned(), v.to_owned());
            } else if let Some(l) = r.strip_prefix("leave_out:") {
                a.leave_out.insert(l.to_owned());
            } else if r == PUBLISH_NEW_ROWS {
                a.publish_new_rows = true;
            } else {
                a.rows.push(r.clone());
            }
        }
        a
    }

    /// The stored key: what makes the link (rows, item, params), then what
    /// it leaves out.
    pub fn key(&self) -> Vec<String> {
        let mut k = self.identity();
        k.extend(self.leave_out.iter().map(|l| format!("leave_out:{l}")));
        if self.publish_new_rows {
            k.push(PUBLISH_NEW_ROWS.to_owned());
        }
        k
    }

    /// What makes a link: the same layout with other rows, item or params
    /// is another link.
    fn identity(&self) -> Vec<String> {
        let mut k = self.rows.clone();
        k.extend(self.item.iter().map(|i| format!("item:{i}")));
        k.extend(self.params.iter().map(|(n, v)| format!("param:{n}={v}")));
        k
    }
}

/// A layout copied for the public, with what was taken out.
pub struct Made {
    pub copy: Value,
    pub removed: Removed,
    /// The layouts it carries: id, title, the rows shown there.
    pub layouts: Vec<(String, String, BTreeSet<String>)>,
    /// Layouts it reaches but doesn't carry, and why.
    pub left_out: Vec<(String, String)>,
}

fn refuse(error: &str, detail: impl Into<String>) -> Value {
    json!({ "error": error, "detail": detail.into() })
}

/// Copies `ask.layout` (as the owner's device draws it) for the public,
/// with the layouts it opens and embeds (decision
/// `public-links-share-the-closure`).
pub async fn make(pod: &Pod, ask: &Ask) -> Result<Made, Value> {
    let (allowed, names) = PublicData::of(pod);
    let spec = crate::preview::fetch(pod, &HeaderMap::new(), &ask.layout).await?;
    let mut pool = DataBundle {
        entities: spec.entities,
        queries: spec.queries,
    };
    if let Err(other) = shareable(&pool, &allowed) {
        return Err(refuse(
            "data_not_shareable",
            format!(
                "a public link carries only data its source lets out ({}); this layout draws {}",
                names.join(", "),
                other.join(", ")
            ),
        ));
    }
    let root =
        serde_json::to_value(&spec.workspace).map_err(|e| refuse("copy_failed", e.to_string()))?;
    let title = root["title"].as_str().unwrap_or(&ask.layout).to_owned();

    // The closure: what the root opens and embeds, and what those do, each
    // layout once, as long as its data may go out.
    let mut full: BTreeMap<String, Value> = BTreeMap::from([(ask.layout.clone(), root)]);
    let mut left_out: Vec<(String, String)> = Vec::new();
    let mut todo = vec![ask.layout.clone()];
    while let Some(id) = todo.pop() {
        let mut found = BTreeSet::new();
        targets(&full[&id], &mut found);
        for t in found {
            if full.contains_key(&t) || left_out.iter().any(|(l, _)| *l == t) {
                continue;
            }
            let why = if ask.leave_out.contains(&t) {
                Some("left out (leave_out)".to_owned())
            } else if pod.catalog().core_layout(&t).is_some() {
                Some("a built-in layout (Settings, the inbox, …)".to_owned())
            } else if full.len() >= MAX_LAYOUTS {
                Some(format!("a copy carries at most {MAX_LAYOUTS} layouts"))
            } else {
                match crate::preview::fetch(pod, &HeaderMap::new(), &t).await {
                    Err(e) => Some(format!("not loaded: {}", e["detail"])),
                    Ok(s) => {
                        let data = DataBundle {
                            entities: s.entities,
                            queries: s.queries,
                        };
                        match (
                            shareable(&data, &allowed),
                            serde_json::to_value(&s.workspace),
                        ) {
                            (Err(other), _) => Some(format!("it draws {}", other.join(", "))),
                            (_, Err(e)) => Some(e.to_string()),
                            (Ok(()), Ok(node)) => {
                                pool.entities.extend(data.entities);
                                for (k, v) in data.queries {
                                    pool.queries.entry(k).or_insert(v);
                                }
                                full.insert(t.clone(), node);
                                todo.push(t.clone());
                                None
                            }
                        }
                    }
                }
            };
            if let Some(w) = why {
                left_out.push((t, w));
            }
        }
    }
    let carried: BTreeSet<String> = full.keys().cloned().collect();
    let mut removed = Removed::default();
    let nodes: BTreeMap<String, Value> = full
        .iter()
        .map(|(id, n)| {
            (
                id.clone(),
                strip(n, &carried, &mut removed).unwrap_or(Value::Null),
            )
        })
        .collect();

    for r in ask.rows.iter().chain(&ask.item) {
        if !pool.entities.contains_key(r) {
            return Err(refuse(
                "unknown_row",
                format!(
                    "{r} is not a row this layout shows (rows are the entity URIs the layout's lists hold)"
                ),
            ));
        }
    }
    let only: BTreeSet<String> = ask.rows.iter().cloned().collect();
    // Ages read as the owner's devices read them: in the pod's zone.
    let zone = crate::blocking(pod, |s| Ok(crate::tz::time_zone(s)))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(jiff::tz::TimeZone::UTC);
    let mut shows = Shows::new(&pool, &nodes, &ask.layout, &only);
    shows.clock = Clock::now(zone);
    let first = Use {
        route: ask.params.clone(),
        item: ask.item.clone(),
    };
    if !shows.run(first) {
        return Err(refuse(
            "too_large",
            format!(
                "the copy draws its layouts more than {MAX_USES} times (one per row that opens one): share fewer rows or leave_out a layout"
            ),
        ));
    }
    let data = shows.data();
    // Opens and embeds of a layout carried: the root too, when something
    // goes back to it.
    let mut reached = BTreeSet::new();
    for n in nodes.values() {
        targets(n, &mut reached);
    }
    let layouts: Map<String, Value> = nodes
        .iter()
        .filter(|(id, _)| **id != ask.layout || reached.contains(*id))
        .map(|(id, n)| (id.clone(), n.clone()))
        .collect();
    let mut copy = json!({ "format": FORMAT, "layout": ask.layout, "title": title,
        "node": nodes[&ask.layout], "data": data });
    if let Some(i) = &ask.item {
        copy["item"] = json!(i);
    }
    if !ask.params.is_empty() {
        copy["params"] = json!(ask.params);
    }
    if !layouts.is_empty() {
        copy["layouts"] = Value::Object(layouts);
    }
    let made_layouts: Vec<(String, String, BTreeSet<String>)> = nodes
        .iter()
        .map(|(id, n)| {
            (
                id.clone(),
                n["title"].as_str().unwrap_or(id).to_owned(),
                shows.by_layout.get(id).cloned().unwrap_or_default(),
            )
        })
        .collect();
    if copy.to_string().len() > MAX_COPY_BYTES {
        let entity_size = |u: &String| {
            data.entities
                .get(u)
                .and_then(|e| serde_json::to_string(e).ok())
                .map_or(0, |s| s.len())
        };
        let mut sizes: Vec<(usize, &String)> = made_layouts
            .iter()
            .map(|(id, _, rows)| {
                (
                    nodes[id].to_string().len() + rows.iter().map(entity_size).sum::<usize>(),
                    id,
                )
            })
            .collect();
        sizes.sort_by(|a, b| b.cmp(a));
        let largest: Vec<String> = sizes
            .iter()
            .take(3)
            .map(|(n, id)| format!("{id} ({} KiB)", n.div_ceil(1024)))
            .collect();
        return Err(refuse(
            "too_large",
            format!(
                "the copy is over {} KiB; the largest layouts in it: {}. Share fewer rows or leave_out a layout",
                MAX_COPY_BYTES / 1024,
                largest.join(", ")
            ),
        ));
    }
    Ok(Made {
        copy,
        removed,
        layouts: made_layouts,
        left_out,
    })
}

/// The layouts a node opens or embeds plainly.
fn targets(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| targets(x, out)),
        Value::Object(o) => {
            let opens = o.get("intent").and_then(Value::as_str) == Some(OPEN)
                || o.get("type").and_then(Value::as_str) == Some("core://Layout");
            if opens && let Some(t) = target(o) {
                out.insert(t.to_owned());
            }
            o.values().for_each(|x| targets(x, out));
        }
        _ => {}
    }
}

fn shelf(pod: &Pod) -> Result<(String, std::sync::Arc<dyn Shelf>), Value> {
    let Some((id, shelf)) = pod.shelf() else {
        return Err(refuse(
            "no_shelf",
            "this server runs the pod without a host",
        ));
    };
    if !shelf.public_links_enabled() {
        return Err(refuse(
            "public_links_off",
            "this pod can't share public links: the server doesn't serve them (CONSTRUCT_PUBLIC_WEB), or the pod is a temporary one",
        ));
    }
    Ok((id.to_owned(), shelf.clone()))
}

fn answer(c: &PublicCopy, base: Option<&str>, handle: Option<&str>) -> Value {
    let url = |path: String| {
        base.map_or(path.clone(), |b| {
            format!("{}{path}", b.trim_end_matches('/'))
        })
    };
    let ask = Ask::from_key(&c.layout, &c.rows);
    let mut out = json!({
        "link": c.link,
        "layout": c.layout,
        "rows": ask.rows,
        "title": c.title,
        "updated_at": c.updated_at,
        "revoked": c.revoked_at.is_some(),
    });
    // The host keeps only a hash of a link (decision `link-tokens-hashed`):
    // its url is known when the link is made, not afterwards.
    if is_link(&c.link) {
        out["url"] = json!(url(format!("/s/{}", c.link)));
    } else if c.revoked_at.is_none() {
        out["url_note"] = json!(
            "the url was shown when this link was made and the server doesn't keep it; unshare_publicly takes this link id, and sharing again makes a new url"
        );
    }
    if let Some(i) = ask.item {
        out["item"] = json!(i);
    }
    if !ask.params.is_empty() {
        out["params"] = json!(ask.params);
    }
    if !ask.leave_out.is_empty() {
        out["leave_out"] = json!(ask.leave_out);
    }
    if c.revoked_at.is_none() {
        out["new_rows"] = json!(if ask.publish_new_rows {
            "publish"
        } else {
            "hold"
        });
    }
    if let (Some(s), Some(h)) = (&c.slug, handle) {
        out["slug"] = json!(s);
        out["short_url"] = json!(url(format!("/{h}/{s}")));
    }
    out
}

/// Whether `s` may be a chosen short path (`/<handle>/<slug>`): 1 to 48
/// lowercase letters, digits and `-`, not starting or ending with `-`.
pub fn is_slug(s: &str) -> bool {
    (1..=48).contains(&s.len())
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The MCP tools (only listed while the host serves public links).
pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({ "name": "share_publicly",
            "description": "Make a page anyone can open in a browser, without a pod: a copy of one of the pod's layouts, the layouts it opens and embeds (a list's rows push their screen: those come too, each with the rows it shows for each row), and only the rows they show, read-only (decisions public-web-links, public-links-share-the-closure). Without confirm nothing is shared: the answer is a review of everything a visitor would read (every text and value, every URL, per layout) and what was taken out (buttons that write, links to layouts not carried, screens); show it to the user and ask before confirming. With confirm (the review_id) it goes out and answers the page's url. It carries only rows and fields the owner's screen shows: a row or field under a condition that is false for it (core://Conditional, an embed's show_when) or can't be decided (a Conditional's date test) stays out; a list's age (older_than) is decided on the pod's clock as the copy is made (decision public-links-decide-ages). The copy is refreshed when the data changes: edits and removals of its rows go out, but new rows wait until it is shared again (a new review), unless new_rows is \"publish\"; say so. An edit to its layouts holds the page at its last copy until it is shared again (a new review). params draws a screen that reads $route.<name> (a recipe screen: {\"id\": \"<meal id>\"}); item draws the layout with that row as $item; rows limits the root's lists to some rows; leave_out keeps layouts out (their links go). slug also answers it at /<handle>/<slug> (the pod needs a handle: set_handle). Sharing the same layout, rows, item and params again updates the same link.",
            "inputSchema": { "type": "object", "properties": {
                "layout": { "type": "string", "description": "A layout id: home or a c-... id from list_layouts" },
                "params": { "type": "object", "description": "{name: value} for $route.<name> of the layout (as render_layout_preview)" },
                "rows": { "type": "array", "items": { "type": "string" }, "description": "Only these rows in the layout's lists (entity URIs they hold)" },
                "item": { "type": "string", "description": "A row (entity URI) the layout draws as $item" },
                "leave_out": { "type": "array", "items": { "type": "string" }, "description": "Layout ids the copy doesn't carry (the review lists the ones it does)" },
                "slug": { "type": "string", "description": "A short path: /<handle>/<slug> (lowercase letters, digits, -)" },
                "new_rows": { "type": "string", "enum": ["hold", "publish"], "description": "When the data changes, rows the review didn't show: hold (the default) waits for the next share; publish lets them out without a review" },
                "confirm": { "type": "string", "description": "The review_id of the review the user approved" } },
                "required": ["layout"] } }),
        json!({ "name": "list_public_links",
            "description": "The pod's public links (share_publicly): each link's url (and short_url), layout, params, rows, when it was last copied, and new_rows_waiting (rows held for a review: share it again to let them out); revoked ones too.",
            "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "unshare_publicly",
            "description": "Revoke a public link: the page stops working at once and its copy leaves the server.",
            "inputSchema": { "type": "object", "properties": {
                "link": { "type": "string", "description": "The link (pub-...) from list_public_links" } },
                "required": ["link"] } }),
    ]
}

/// The base URL visitors use: the host's chosen one (`CONSTRUCT_PUBLIC_URL`),
/// else the MCP request's.
pub fn base_url(headers: &HeaderMap) -> Option<String> {
    if let Some(u) = crate::public_url_from_env() {
        return Some(u);
    }
    let host = headers.get("host")?.to_str().ok()?;
    let https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p == "https")
        || !(host.starts_with("127.") || host.starts_with("localhost"));
    Some(format!("{}://{host}", if https { "https" } else { "http" }))
}

/// Runs a public-link tool; `None` when `name` isn't one.
pub async fn tool(
    pod: &Pod,
    headers: &HeaderMap,
    name: &str,
    args: &Value,
) -> Option<Result<Value, Value>> {
    if !tool_specs().iter().any(|t| t["name"] == name) {
        return None;
    }
    let (pod_id, shelf) = match shelf(pod) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let base = base_url(headers);
    let handle = shelf.handle(&pod_id).ok().flatten();
    Some(match name {
        "share_publicly" => {
            share(
                pod,
                &pod_id,
                shelf.as_ref(),
                args,
                base.as_deref(),
                handle.as_deref(),
            )
            .await
        }
        "list_public_links" => match shelf.public_of_pod(&pod_id) {
            Ok(v) => {
                let mut links = Vec::new();
                for c in &v {
                    let mut a = answer(c, base.as_deref(), handle.as_deref());
                    let ask = Ask::from_key(&c.layout, &c.rows);
                    if c.revoked_at.is_none()
                        && !ask.publish_new_rows
                        && let Ok(made) = make(pod, &ask).await
                    {
                        let had = copy_rows(&c.copy);
                        a["new_rows_waiting"] =
                            json!(copy_rows(&made.copy).difference(&had).count());
                    }
                    links.push(a);
                }
                Ok(json!({ "links": links }))
            }
            Err(e) => Err(refuse("shelf_error", format!("{e:?}"))),
        },
        "unshare_publicly" => {
            let link = args["link"].as_str().unwrap_or("");
            match shelf.revoke_public(&pod_id, link) {
                Ok(true) => Ok(json!({ "ok": true, "link": link,
                    "detail": "the page no longer opens and its copy left the server" })),
                Ok(false) => Err(refuse(
                    "unknown_link",
                    "no live public link of this pod has that link",
                )),
                Err(e) => Err(refuse("shelf_error", format!("{e:?}"))),
            }
        }
        _ => return None,
    })
}

/// The `share_publicly` arguments.
fn ask_of(args: &Value) -> Result<Ask, Value> {
    let strings = |k: &str| -> Vec<String> {
        args[k]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str().map(str::to_owned))
            .collect()
    };
    let mut rows = strings("rows");
    rows.sort();
    rows.dedup();
    let params = match &args["params"] {
        Value::Null => BTreeMap::new(),
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), render(v))).collect(),
        _ => return Err(refuse("invalid_arguments", "params: {name: value}")),
    };
    Ok(Ask {
        layout: args["layout"].as_str().unwrap_or("").to_owned(),
        rows,
        item: args["item"].as_str().map(str::to_owned),
        params,
        leave_out: strings("leave_out").into_iter().collect(),
        publish_new_rows: match args["new_rows"].as_str() {
            None | Some("hold") => false,
            Some("publish") => true,
            Some(_) => {
                return Err(refuse(
                    "invalid_arguments",
                    "new_rows: \"hold\" (the default) or \"publish\"",
                ));
            }
        },
    })
}

async fn share(
    pod: &Pod,
    pod_id: &str,
    shelf: &dyn Shelf,
    args: &Value,
    base: Option<&str>,
    handle: Option<&str>,
) -> Result<Value, Value> {
    let ask = ask_of(args)?;
    let slug = args["slug"].as_str().filter(|s| !s.is_empty());
    if let Some(s) = slug {
        if !is_slug(s) {
            return Err(refuse(
                "invalid_slug",
                "a slug is 1 to 48 lowercase letters, digits and -",
            ));
        }
        if handle.is_none() {
            return Err(refuse(
                "no_handle",
                "a short path is /<handle>/<slug>: give the pod a handle first (set_handle)",
            ));
        }
    }
    let made = make(pod, &ask).await?;
    let id = review_id(&made.copy, &ask, slug);
    if args["confirm"].as_str() != Some(id.as_str()) {
        let entities = made.copy["data"]["entities"].as_object();
        let real = |u: &&String| !u.starts_with("core://");
        let fields_of = |rows: &mut dyn Iterator<Item = &String>| -> BTreeSet<String> {
            rows.filter_map(|u| entities.and_then(|e| e.get(u)))
                .flat_map(|e| e.as_object().into_iter().flatten().map(|(k, _)| k.clone()))
                .collect()
        };
        let (texts, urls) = review_texts(&made.copy);
        let all: Vec<&String> = entities
            .into_iter()
            .flatten()
            .map(|(u, _)| u)
            .filter(real)
            .collect();
        let fields = fields_of(&mut all.iter().copied());
        let layouts: Vec<Value> = made
            .layouts
            .iter()
            .map(|(lid, title, rows)| {
                let node = if *lid == ask.layout {
                    &made.copy["node"]
                } else {
                    &made.copy["layouts"][lid]
                };
                let data: Map<String, Value> = rows
                    .iter()
                    .filter_map(|u| {
                        entities
                            .and_then(|e| e.get(u))
                            .map(|e| (u.clone(), e.clone()))
                    })
                    .collect();
                let (texts, urls) =
                    review_texts(&json!({ "node": node, "data": { "entities": data } }));
                json!({ "layout": lid, "title": title, "rows": rows.len(),
                    "fields": fields_of(&mut rows.iter()), "texts": texts, "urls": urls })
            })
            .collect();
        return Ok(json!({
            "review_id": id,
            "title": made.copy["title"],
            "shared": "nothing yet: confirm with this review_id once the user approves",
            "anyone_with_the_link_reads": {
                "rows": all.len(),
                "fields": fields,
                "texts": texts,
                "urls": urls,
                "layouts": layouts,
            },
            "taken_out": {
                "controls_that_write_or_go_elsewhere": made.removed.controls,
                "embedded_layouts": made.removed.embeds,
                "screens": made.removed.screens,
                "layouts_not_carried": made.left_out.iter()
                    .map(|(l, why)| json!({ "layout": l, "why": why })).collect::<Vec<_>>(),
            },
            "updates": if ask.publish_new_rows {
                "the copy is made again whenever the data changes: new rows these layouts show will appear on the page too, without a review; after an edit to these layouts the page keeps its last copy until it is shared again"
            } else {
                "the copy is made again whenever the data changes: edits and removals of these rows go out, new rows wait until it is shared again (new_rows: \"publish\" lets them out without a review); after an edit to these layouts the page keeps its last copy until it is shared again"
            },
        }));
    }
    let live = shelf
        .public_of_pod(pod_id)
        .map_err(|e| refuse("shelf_error", format!("{e:?}")))?;
    let existing = live.iter().find(|c| {
        c.revoked_at.is_none()
            && c.layout == ask.layout
            && Ask::from_key(&c.layout, &c.rows).identity() == ask.identity()
    });
    // The shelf holds the cap (MAX_LIVE): it counts and inserts in one
    // transaction, so two shares at once can't both pass.
    let title = made.copy["title"]
        .as_str()
        .unwrap_or(&ask.layout)
        .to_owned();
    let c = shelf
        .put_public(PublicCopy {
            link: existing.map_or_else(new_link, |c| c.link.clone()),
            owner_pod: pod_id.to_owned(),
            layout: ask.layout.clone(),
            rows: ask.key(),
            title,
            copy: made.copy,
            slug: slug
                .map(str::to_owned)
                .or_else(|| existing.and_then(|c| c.slug.clone())),
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        })
        .map_err(|e| match e {
            crate::construct::share::ShelfError::SlugTaken => refuse(
                "slug_taken",
                "another live public link of this pod has that slug: pick another",
            ),
            crate::construct::share::ShelfError::TooManyLinks => refuse(
                "too_many_links",
                format!("a pod keeps at most {MAX_LIVE} public links: unshare one first"),
            ),
            e => refuse("shelf_error", format!("{e:?}")),
        })?;
    tracing::info!("a public link was shared");
    let mut out = answer(&c, base, handle);
    out["detail"] =
        json!("anyone with this url can read the page, without a pod; unshare_publicly stops it");
    Ok(out)
}

fn same_format(a: &Value, b: &Value) -> bool {
    a["format"] == b["format"]
}

/// Whether two copies draw the same layouts (everything but their rows),
/// so they read the same fields and carry the same screens.
fn same_shape(a: &Value, b: &Value) -> bool {
    let shape = |c: &Value| {
        let mut c = c.clone();
        if let Some(o) = c.as_object_mut() {
            o.remove("data");
        }
        c
    };
    shape(a) == shape(b)
}

/// The rows (entity URIs) of a copy's data, not the theme or sources.
fn copy_rows(copy: &Value) -> BTreeSet<String> {
    copy["data"]["entities"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(u, _)| u)
        .filter(|u| !u.starts_with("core://"))
        .cloned()
        .collect()
}

/// `new` holding only what `old` (the copy the owner last reviewed, or
/// its refresh) shows: rows it didn't carry wait for the next review, and
/// the rows it keeps carry only fields it carried. The rows held back.
pub fn hold_new_rows(new: &Value, old: &Value) -> (Value, usize) {
    let had = copy_rows(old);
    let fields: BTreeSet<String> = old["data"]["entities"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(u, _)| !u.starts_with("core://"))
        .flat_map(|(_, e)| e.as_object().into_iter().flatten().map(|(f, _)| f.clone()))
        .collect();
    let mut out = new.clone();
    let mut held = 0;
    if let Some(es) = out["data"]["entities"].as_object_mut() {
        es.retain(|u, e| {
            if u.starts_with("core://") {
                return true;
            }
            if !had.contains(u) {
                held += 1;
                return false;
            }
            if let Some(m) = e.as_object_mut() {
                m.retain(|f, _| f == "id" || fields.contains(f));
            }
            true
        });
    }
    if let Some(qs) = out["data"]["queries"].as_object_mut() {
        for list in qs.values_mut() {
            if let Some(a) = list.as_array_mut() {
                a.retain(|u| u.as_str().is_some_and(|u| had.contains(u)));
            }
        }
    }
    (out, held)
}

/// Copies every live public link of the pod again (its data changed).
pub async fn refresh(pod: &Pod) {
    let Ok((pod_id, shelf)) = shelf(pod) else {
        return;
    };
    let Ok(live) = shelf.public_of_pod(&pod_id) else {
        return;
    };
    for c in live.into_iter().filter(|c| c.revoked_at.is_none()) {
        let ask = Ask::from_key(&c.layout, &c.rows);
        match make(pod, &ask).await {
            Ok(made) if made.copy == c.copy => {}
            // Only rows go out without a review: a layout edited since (a new
            // field, a screen it now opens) holds the page at its last copy
            // until the owner shares it again. A copy of an older format is
            // rewritten (decision `public-links-share-the-closure`).
            Ok(made) if same_format(&made.copy, &c.copy) && !same_shape(&made.copy, &c.copy) => {
                tracing::warn!(
                    "a public link's layouts changed since its review: it keeps its last copy"
                );
            }
            Ok(made) => {
                // Rows the owner hasn't reviewed wait for the next share,
                // unless the link publishes new rows (issue #446).
                let copy = if ask.publish_new_rows {
                    made.copy
                } else {
                    let (copy, held) = hold_new_rows(&made.copy, &c.copy);
                    if held > 0 {
                        tracing::info!(
                            held,
                            "a public link holds new rows until it is shared again"
                        );
                    }
                    copy
                };
                if copy != c.copy {
                    let _ = shelf.put_public(PublicCopy { copy, ..c });
                }
            }
            // The layout went or changed shape: the page keeps its last copy.
            Err(e) => tracing::warn!(error = %e["error"], "a public link couldn't be copied again"),
        }
    }
}

/// Keeps the pod's public links current: after its Collections data
/// changes (and settles), copies them again. Started with the pod.
pub fn watch(pod: &Pod) {
    let Some((_, shelf)) = pod.shelf() else {
        return;
    };
    if !shelf.public_links_enabled() {
        return;
    }
    // The sources whose data a copy may carry: their changes copy again.
    let sources = PublicData::of(pod).1;
    if sources.is_empty() {
        return;
    }
    let mut rx = pod.subscribe_data();
    // Weak: a pod the host closes is not kept open by this.
    let weak = std::sync::Arc::downgrade(pod);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(e) if sources.contains(&e.source.as_str()) => {}
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return,
            }
            tokio::time::sleep(SETTLE).await;
            while rx.try_recv().is_ok() {}
            let Some(pod) = weak.upgrade() else { return };
            refresh(&pod).await;
        }
    });
}

/// `BTreeMap` of a copy's rows by URI (tests).
#[cfg(test)]
fn rows_of(d: &DataBundle) -> std::collections::BTreeMap<String, Value> {
    d.entities
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_unguessable_and_recognized() {
        let l = new_link();
        assert!(is_link(&l), "{l}");
        assert_ne!(l, new_link());
        assert!(!is_link("pub-short") && !is_link("share-abcdefghijklmnopqrstuvwxyz234567"));
    }

    #[test]
    fn writes_and_ways_out_are_taken_out() {
        let node = json!({ "type": "core://Page", "id": "c-x", "title": "Recipes",
            "screens": [{ "id": "s", "route": "x://{id}", "root": { "type": "core://Divider" } }],
            "root": { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Title", "value_expr": "Recipes" },
                { "type": "core://Button", "label": "Delete all",
                  "on_click": { "intent": "action.mutation.create", "query_key": "q", "entity_uri": "x", "entity": {}, "endpoint": "/v1/x", "payload": {} } },
                { "type": "core://Tappable", "on_click": { "intent": "action.navigation.open", "layout": "c-secret" },
                  "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.title" } },
                { "type": "core://Tappable", "on_click": { "intent": "action.link.open", "url": "https://shop.example/x" },
                  "content": { "type": "core://Text", "style": "Body", "value_expr": "Buy" } },
                { "type": "core://Layout", "layout": "c-private" } ] } });
        let mut removed = Removed::default();
        let out = strip(&node, &BTreeSet::new(), &mut removed).unwrap();
        let s = out.to_string();
        assert!(
            !s.contains("Delete all") && !s.contains("c-secret") && !s.contains("c-private"),
            "{s}"
        );
        assert!(s.contains("$item.title"), "a tappable keeps its content");
        assert!(s.contains("shop.example"), "a link out stays");
        assert!(out.get("screens").is_none());
        assert_eq!(
            removed,
            Removed {
                controls: 2,
                embeds: 1,
                screens: 1
            }
        );
        // What's left still parses as plumbing.
        let _: protocol::UiNode = serde_json::from_value(out).unwrap();
    }

    #[test]
    fn only_listed_rows_and_read_fields_go() {
        let data: DataBundle = serde_json::from_value(json!({
            "entities": {
                "notes://recipes/1": { "id": "1", "title": "Lentil soup", "notes": "for Dana's birthday", "minutes": 40 },
                "notes://recipes/2": { "id": "2", "title": "Shakshuka", "notes": "secret", "minutes": 30 },
                "notes://diary/9": { "id": "9", "title": "Dear diary" } },
            "queries": { "note:recipes": ["notes://recipes/1", "notes://recipes/2"] } })).unwrap();
        let node = json!({ "type": "core://List", "query_key": "note:recipes",
            "item_template": { "type": "core://Text", "style": "Body", "value_expr": "$item.title · $item.minutes min" } });
        let all = rows_of(&minimize(&data, &node, &[], Use::default()));
        assert_eq!(all.len(), 2, "the diary isn't listed: it doesn't go");
        assert_eq!(
            all["notes://recipes/1"],
            json!({ "id": "1", "title": "Lentil soup", "minutes": 40 })
        );
        let one = minimize(&data, &node, &["notes://recipes/2".into()], Use::default());
        assert_eq!(
            one.queries["note:recipes"],
            vec!["notes://recipes/2".to_owned()]
        );
        assert_eq!(rows_of(&one).len(), 1);
    }

    #[test]
    fn a_field_named_like_a_layout_key_stays_out_unless_read() {
        let data: DataBundle = serde_json::from_value(json!({
            "entities": {
                "notes://people/1": { "id": "1", "name": "Rowan", "label": "owes me money",
                    "title": "private", "kind": "friend", "city": "Northvale", "bio": "x" } },
            "queries": { "note:people": ["notes://people/1"] } }))
        .unwrap();
        // Plumbing keys and values (`label`, `title`, `style`, `Body`) are
        // not field names: only what the page reads goes.
        let node = json!({ "type": "core://List", "query_key": "note:people",
            "item_template": { "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Body", "value_expr": "$item.name" },
                { "type": "core://Button", "label": "Say hi",
                  "on_click": { "intent": "action.navigation.back" } } ] } });
        let rows = rows_of(&minimize(&data, &node, &[], Use::default()));
        assert_eq!(
            rows["notes://people/1"],
            json!({ "id": "1", "name": "Rowan" })
        );
        // Names under a field-name key are read: `where`, `filter`, `group_by`.
        let node = json!({ "type": "core://List", "query_key": "note:people",
            "where": { "field": "kind", "equals": "$form.kind" },
            "filter": { "value": "$form.q", "fields": ["city"] },
            "group_by": { "field": "title", "header": { "type": "core://Divider" } },
            "item_template": { "type": "core://Text", "value_expr": "${notes://people/1#bio}" } });
        let rows = rows_of(&minimize(&data, &node, &[], Use::default()));
        assert_eq!(
            rows["notes://people/1"],
            json!({ "id": "1", "kind": "friend", "city": "Northvale", "title": "private", "bio": "x" })
        );
    }

    #[test]
    fn only_data_a_source_lets_out_is_shared() {
        let ok: DataBundle = serde_json::from_value(
            json!({ "entities": { "notes://a/1": {}, "core://theme": {} },
            "queries": { "note:a": [] } }),
        )
        .unwrap();
        let allowed = PublicData {
            uris: vec!["notes://"],
            queries: vec!["note:"],
        };
        assert!(shareable(&ok, &allowed).is_ok());
        let no: DataBundle =
            serde_json::from_value(json!({ "entities": { "photos://posts/1": {} },
            "queries": { "timeline:home": [] } }))
            .unwrap();
        assert_eq!(
            shareable(&no, &allowed),
            Err(vec!["photos".into(), "timeline".into()])
        );
    }

    #[test]
    fn the_review_lists_what_a_visitor_reads() {
        let copy = json!({ "title": "Recipes", "node": { "type": "core://Text", "style": "Body",
            "value_expr": "Our family recipes · $item.minutes min", "url": "https://img.example/a.png" },
            "data": { "entities": { "notes://r/1": { "id": "1", "title": "Lentil soup" } } } });
        let (texts, urls) = review_texts(&copy);
        assert!(
            texts.contains(&"Our family recipes ·  min".to_owned()),
            "{texts:?}"
        );
        assert!(texts.contains(&"Lentil soup".to_owned()) && !texts.contains(&"Body".to_owned()));
        assert_eq!(urls, vec!["https://img.example/a.png".to_owned()]);
        let ask = Ask::default();
        assert_eq!(
            review_id(&copy, &ask, None),
            review_id(&copy.clone(), &ask, None)
        );
        let publish = Ask {
            publish_new_rows: true,
            ..Ask::default()
        };
        assert_ne!(
            review_id(&copy, &ask, None),
            review_id(&copy, &publish, None),
            "new_rows is part of the review"
        );
        assert_ne!(
            review_id(&copy, &ask, None),
            review_id(&copy, &ask, Some("recipes")),
            "so is the slug"
        );
    }

    /// The made-up recipe screen of the issue: it reads `$route.id`, its
    /// lists keep one meal's lines and steps with `where`, and a lookup
    /// names each line's ingredient.
    fn recipe_screen() -> Value {
        json!({ "type": "core://Page", "id": "c-recipe", "title": "Recipe", "root": { "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Title", "value_expr": "${notes://meals/$route.id#name}" },
            { "type": "core://IconButton", "icon": "favorite", "content_description": "Favorite",
              "active": "${notes://meals/$route.id#favorite}",
              "on_click": { "intent": "action.mutation.optimistic", "endpoint": "/v1/notes/meals/update",
                "entity_uri": "notes://meals/$route.id", "field": "favorite", "payload": {} } },
            { "type": "core://Text", "style": "Title", "value_expr": "$count",
              "count": { "query_key": "note:recipe_lines",
                "where": [{ "field": "deleted", "equals": "false" }, { "field": "meal_id", "equals": "$route.id" }] } },
            { "type": "core://List", "query_key": "note:recipe_lines",
              "where": [{ "field": "deleted", "equals": "false" }, { "field": "meal_id", "equals": "$route.id" }],
              "item_template": { "type": "core://Row", "children": [
                { "type": "core://Text", "style": "Body", "value_expr": "$item.thing" },
                { "type": "core://Conditional", "value": "${notes://ingredients/$item.ingredient#staple}", "equals": "true",
                  "content": { "type": "core://Text", "style": "Caption", "value_expr": "Staple" } } ] } },
            { "type": "core://List", "query_key": "note:recipe_steps",
              "where": [{ "field": "meal_id", "equals": "$route.id" }],
              "item_template": { "type": "core://Text", "style": "Body", "value_expr": "$item.text" } } ] } })
    }

    /// Two meals, each with lines and steps, ingredients, and a Pixelfed
    /// post: made up.
    fn kitchen() -> DataBundle {
        let mut entities = Map::new();
        let mut lines = Vec::new();
        let mut steps = Vec::new();
        for (m, name) in [("m1", "Lentil soup"), ("m2", "Shakshuka")] {
            entities.insert(
                format!("notes://meals/{m}"),
                json!({ "id": m, "name": name, "favorite": true, "notes": "only for me" }),
            );
            for i in 0..3 {
                let id = format!("{m}-l{i}");
                entities.insert(format!("notes://recipe_lines/{id}"), json!({ "id": id, "meal_id": m,
                    "deleted": false, "thing": format!("{name} thing {i}"), "ingredient": format!("i{i}"), "cost": 3 }));
                lines.push(format!("notes://recipe_lines/{id}"));
                let sid = format!("{m}-s{i}");
                entities.insert(
                    format!("notes://recipe_steps/{sid}"),
                    json!({ "id": sid, "meal_id": m, "text": format!("{name} step {i}") }),
                );
                steps.push(format!("notes://recipe_steps/{sid}"));
            }
        }
        // A deleted line of m1: its list hides it.
        entities.insert(
            "notes://recipe_lines/m1-gone".into(),
            json!({ "id": "m1-gone",
            "meal_id": "m1", "deleted": true, "thing": "Old thing" }),
        );
        lines.push("notes://recipe_lines/m1-gone".into());
        for i in 0..3 {
            entities.insert(
                format!("notes://ingredients/i{i}"),
                json!({ "id": format!("i{i}"), "staple": i == 0, "price": 9 }),
            );
        }
        serde_json::from_value(json!({ "entities": entities, "queries": {
            "note:meals": ["notes://meals/m1", "notes://meals/m2"],
            "note:recipe_lines": lines, "note:recipe_steps": steps } }))
        .unwrap()
    }

    fn route(id: &str) -> Use {
        Use {
            route: BTreeMap::from([("id".to_owned(), id.to_owned())]),
            item: None,
        }
    }

    /// Issue #363: a `$route.id` screen shared with params carries its
    /// meal, that meal's lines and steps (its lists' `where` applied), and
    /// the ingredients its lines look up, each with only the fields read.
    #[test]
    fn a_route_screen_carries_only_its_rows() {
        let mut removed = Removed::default();
        let node = strip(&recipe_screen(), &BTreeSet::new(), &mut removed).unwrap();
        let rows = rows_of(&minimize(&kitchen(), &node, &[], route("m1")));
        let keys: Vec<&str> = rows.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "notes://ingredients/i0",
                "notes://ingredients/i1",
                "notes://ingredients/i2",
                "notes://meals/m1",
                "notes://recipe_lines/m1-l0",
                "notes://recipe_lines/m1-l1",
                "notes://recipe_lines/m1-l2",
                "notes://recipe_steps/m1-s0",
                "notes://recipe_steps/m1-s1",
                "notes://recipe_steps/m1-s2",
            ]
        );
        assert_eq!(
            rows["notes://meals/m1"],
            json!({ "id": "m1", "name": "Lentil soup" }),
            "the favorite button went, and notes aren't shown"
        );
        assert_eq!(
            rows["notes://ingredients/i0"],
            json!({ "id": "i0", "staple": true })
        );
        assert!(rows["notes://recipe_lines/m1-l0"].get("cost").is_none());
    }

    /// A list with `where` carries only matching rows; one on what the
    /// visitor picks (a form value, blank at first) keeps them all, as the
    /// owner's screen does; an age can't be decided and keeps none.
    #[test]
    fn a_list_where_keeps_only_matching_rows() {
        let node = json!({ "type": "core://List", "query_key": "note:recipe_steps",
            "where": { "field": "meal_id", "equals": "m2" },
            "item_template": { "type": "core::Text", "value_expr": "$item.text" } });
        let d = minimize(&kitchen(), &node, &[], Use::default());
        assert_eq!(d.queries["note:recipe_steps"].len(), 3);
        assert!(
            d.queries["note:recipe_steps"]
                .iter()
                .all(|u| u.contains("/m2-"))
        );
        let not = json!({ "type": "core://List", "query_key": "note:recipe_steps",
            "where": [{ "field": "meal_id", "equals": "m2", "not": true }],
            "item_template": { "type": "core::Text", "value_expr": "$item.text" } });
        let d = minimize(&kitchen(), &not, &[], Use::default());
        assert!(
            d.queries["note:recipe_steps"]
                .iter()
                .all(|u| u.contains("/m1-"))
        );
        let form = json!({ "type": "core://List", "query_key": "note:recipe_steps",
            "where": [{ "field": "meal_id", "equals": "$form.meal" }],
            "item_template": { "type": "core::Text", "value_expr": "$item.text" } });
        assert_eq!(
            minimize(&kitchen(), &form, &[], Use::default()).queries["note:recipe_steps"].len(),
            6
        );
        let age = json!({ "type": "core://List", "query_key": "note:recipe_steps",
            "where": [{ "field": "meal_id", "older_than": "7d" }],
            "item_template": { "type": "core::Text", "value_expr": "$item.text" } });
        let d = minimize(&kitchen(), &age, &[], Use::default());
        assert!(d.queries["note:recipe_steps"].is_empty() && rows_of(&d).is_empty());
    }

    /// Decision `public-links-decide-ages`: with the pod's clock, a list's
    /// age keeps the rows it shows now (upcoming events, an unreadable
    /// date) and leaves out the ones it hides, both ways round.
    #[test]
    fn ages_are_decided_on_the_pods_clock() {
        let at = |hours: i64| {
            let t = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(hours);
            let d = t.to_zoned(jiff::tz::TimeZone::UTC).datetime();
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}",
                d.year(),
                d.month(),
                d.day(),
                d.hour(),
                d.minute()
            )
        };
        let data: DataBundle = serde_json::from_value(json!({
            "entities": {
                "c://events/past": { "id": "past", "title": "Last week", "start": at(-24 * 7) },
                "c://events/today": { "id": "today", "title": "An hour ago", "start": at(-1) },
                "c://events/soon": { "id": "soon", "title": "Tomorrow", "start": at(24) },
                "c://events/tbd": { "id": "tbd", "title": "Date to come", "start": "" } },
            "queries": { "c:events": ["c://events/past", "c://events/today", "c://events/soon", "c://events/tbd"] } }))
        .unwrap();
        let kept = |cond: Value, clock: bool| {
            let node = json!({ "type": "core://List", "query_key": "c:events", "where": [cond],
                "item_template": { "type": "core://Text", "value_expr": "$item.title" } });
            let nodes = BTreeMap::from([("root".to_owned(), node)]);
            let only = BTreeSet::new();
            let mut s = Shows::new(&data, &nodes, "root", &only);
            if clock {
                s.clock = Clock::now(jiff::tz::TimeZone::UTC);
            }
            s.run(Use::default());
            let d = s.data();
            let mut ids: Vec<String> = d.queries["c:events"]
                .iter()
                .map(|u| u.rsplit('/').next().unwrap().to_owned())
                .collect();
            ids.sort();
            ids
        };
        let upcoming = json!({ "field": "start", "older_than": "6h", "not": true });
        assert_eq!(kept(upcoming.clone(), true), ["soon", "tbd", "today"]);
        let old = json!({ "field": "start", "older_than": "2d" });
        assert_eq!(kept(old, true), ["past"]);
        // Without the clock it fails closed, as before.
        assert!(kept(upcoming, false).is_empty());
    }

    fn diary() -> DataBundle {
        serde_json::from_value(json!({
            "entities": {
                "notes://diary/1": { "id": "1", "title": "Picnic", "private": false, "body": "sunny", "secret": "" },
                "notes://diary/2": { "id": "2", "title": "Therapy", "private": true, "body": "hard day", "secret": "x" } },
            "queries": { "note:diary": ["notes://diary/1", "notes://diary/2"] } }))
        .unwrap()
    }

    /// Issue #446: a row the owner's screen hides (its template under a
    /// condition false for it) is not in the copy, not even through a count
    /// of the same list; a field read only under a hidden condition stays
    /// home.
    #[test]
    fn rows_and_fields_a_condition_hides_stay_home() {
        let node = json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": "",
              "count": { "query_key": "note:diary", "where": [{ "field": "title", "equals": "Therapy" }] } },
            { "type": "core://List", "query_key": "note:diary",
              "item_template": { "type": "core://Conditional", "value": "$item.private", "not": true,
                "content": { "type": "core://Column", "children": [
                  { "type": "core://Text", "value_expr": "$item.title" },
                  { "type": "core://Conditional", "value": "$item.secret",
                    "content": { "type": "core://Text", "value_expr": "$item.body" } } ] } } } ] });
        let d = minimize(&diary(), &node, &[], Use::default());
        let rows = rows_of(&d);
        assert_eq!(d.queries["note:diary"], vec!["notes://diary/1".to_owned()]);
        assert!(!rows.contains_key("notes://diary/2"), "{rows:?}");
        assert_eq!(
            rows["notes://diary/1"],
            json!({ "id": "1", "title": "Picnic", "private": false, "secret": "" }),
            "the body is read only where `secret` is truthy"
        );
        // An embed's show_when is a condition too.
        let embed = json!({ "type": "core://List", "query_key": "note:diary",
            "item_template": { "type": "core://Layout", "layout": "root", "show_when": "$item.private",
              "equals": "false" } });
        let d = minimize(&diary(), &embed, &[], Use::default());
        assert_eq!(d.queries["note:diary"], vec!["notes://diary/1".to_owned()]);
    }

    /// Issue #446: a condition that can't be decided when the copy is made
    /// (a date, an unbound name) fails closed; one on what the visitor picks
    /// (a form value, the device) shows, as on the owner's screen a tap away.
    #[test]
    fn an_undecidable_condition_drops_the_row() {
        let with = |cond: Value| {
            let mut c = cond;
            c["type"] = json!("core://Conditional");
            c["content"] = json!({ "type": "core://Text", "value_expr": "$item.body" });
            let node =
                json!({ "type": "core://List", "query_key": "note:diary", "item_template": c });
            minimize(&diary(), &node, &[], Use::default())
        };
        let dated = with(json!({ "value": "$item.title", "date": "Future" }));
        assert!(dated.queries["note:diary"].is_empty() && rows_of(&dated).is_empty());
        let unbound = with(json!({ "value": "$route.who", "equals": "me" }));
        assert!(unbound.queries["note:diary"].is_empty());
        let form = with(json!({ "value": "$form.open" }));
        assert_eq!(form.queries["note:diary"].len(), 2);
        let device = with(json!({ "value": "${local://device#size_class}", "equals": "expanded" }));
        assert_eq!(rows_of(&device)["notes://diary/2"]["body"], "hard day");
        // A row field the copy's own local.set changes is the visitor's too.
        let node = json!({ "type": "core://List", "query_key": "note:diary", "item_template":
            { "type": "core://Column", "children": [
              { "type": "core://Tappable", "content": { "type": "core://Text", "value_expr": "$item.title" },
                "on_click": { "intent": "action.local.set", "entity_uri": "notes://diary/$item.id",
                  "field": "open", "value_expr": "true" } },
              { "type": "core://Conditional", "value": "$item.open",
                "content": { "type": "core://Text", "value_expr": "$item.body" } } ] } });
        let d = minimize(&diary(), &node, &[], Use::default());
        assert_eq!(rows_of(&d)["notes://diary/1"]["body"], "sunny");
    }

    /// A local.set the visitor can tap reaches only the row it writes and
    /// the value it writes: a "Hide" that sets `private` to true never
    /// shows what `private` hides, and a set of one fixed row leaves the
    /// others' `private` theirs.
    #[test]
    fn a_visitor_set_shows_only_what_its_value_reaches() {
        let list = |set: Value| {
            json!({ "type": "core://List", "query_key": "note:diary", "item_template":
                { "type": "core://Column", "children": [
                  { "type": "core://Tappable", "content": { "type": "core://Text", "value_expr": "$item.title" },
                    "on_click": set },
                  { "type": "core://Conditional", "value": "$item.private", "not": true,
                    "content": { "type": "core://Text", "value_expr": "$item.body" } } ] } })
        };
        let hide = list(
            json!({ "intent": "action.local.set", "entity_uri": "notes://diary/$item.id",
            "field": "private", "value_expr": "true" }),
        );
        let d = minimize(&diary(), &hide, &[], Use::default());
        let rows = rows_of(&d);
        assert_eq!(rows["notes://diary/1"]["body"], "sunny");
        assert!(rows["notes://diary/2"].get("body").is_none(), "{rows:?}");
        // A set that can write false (a toggle, or a "Show") is the visitor's.
        let show = list(
            json!({ "intent": "action.local.set", "entity_uri": "notes://diary/$item.id",
            "field": "private", "value_expr": "false" }),
        );
        let d = minimize(&diary(), &show, &[], Use::default());
        assert_eq!(rows_of(&d)["notes://diary/2"]["body"], "hard day");
        let toggle = list(
            json!({ "intent": "action.local.set", "entity_uri": "notes://diary/$item.id",
            "field": "private", "value_expr": "!entity.private" }),
        );
        let d = minimize(&diary(), &toggle, &[], Use::default());
        assert_eq!(rows_of(&d)["notes://diary/2"]["body"], "hard day");
        // One fixed row's set: only that row is the visitor's.
        let one = list(
            json!({ "intent": "action.local.set", "entity_uri": "notes://diary/9",
            "field": "private", "value_expr": "false" }),
        );
        let d = minimize(&diary(), &one, &[], Use::default());
        assert!(rows_of(&d)["notes://diary/2"].get("body").is_none());
    }

    /// Issue #446: a refresh lets out edits and removals of the rows the
    /// owner reviewed, but new rows (and fields no reviewed row had) wait.
    #[test]
    fn a_refresh_holds_new_rows() {
        let old = json!({ "format": 2, "node": {}, "data": {
            "entities": { "core://theme": { "id": "theme" },
                "notes://diary/1": { "id": "1", "title": "Picnic" },
                "notes://diary/3": { "id": "3", "title": "Gone" } },
            "queries": { "note:diary": ["notes://diary/1", "notes://diary/3"] } } });
        let new = json!({ "format": 2, "node": {}, "data": {
            "entities": { "core://theme": { "id": "theme" },
                "notes://diary/1": { "id": "1", "title": "Picnic!", "body": "new field" },
                "notes://diary/2": { "id": "2", "title": "Therapy" } },
            "queries": { "note:diary": ["notes://diary/1", "notes://diary/2"] } } });
        let (out, held) = hold_new_rows(&new, &old);
        assert_eq!(held, 1);
        assert_eq!(
            out["data"],
            json!({ "entities": { "core://theme": { "id": "theme" },
                "notes://diary/1": { "id": "1", "title": "Picnic!" } },
                "queries": { "note:diary": ["notes://diary/1"] } })
        );
    }

    /// A list whose rows push an item screen: the copy keeps the push to a
    /// layout it carries, the item screen's rows for each listed row, and
    /// strips a push to a layout it doesn't (a Pixelfed one).
    #[test]
    fn a_list_carries_the_screens_its_rows_push() {
        let meals = json!({ "type": "core://Page", "id": "c-meals", "title": "Meals", "root": { "type": "core://Column", "children": [
            { "type": "core://List", "query_key": "note:meals",
              "where": [{ "field": "name", "equals": "Shakshuka" }],
              "item_template": { "type": "core://Tappable",
                "on_click": { "intent": "action.navigation.open", "layout": "c-recipe", "params": { "id": "$item.id" }, "presentation": "Push" },
                "content": { "type": "core://Text", "style": "Body", "value_expr": "$item.name" } } },
            { "type": "core://Tappable", "on_click": { "intent": "action.navigation.open", "layout": "c-photos", "presentation": "Push" },
              "content": { "type": "core://Text", "style": "Body", "value_expr": "Photos" } } ] } });
        let carried = BTreeSet::from(["c-meals".to_owned(), "c-recipe".to_owned()]);
        let mut removed = Removed::default();
        let nodes: BTreeMap<String, Value> = [("c-meals", meals), ("c-recipe", recipe_screen())]
            .into_iter()
            .map(|(id, n)| (id.to_owned(), strip(&n, &carried, &mut removed).unwrap()))
            .collect();
        let s = nodes["c-meals"].to_string();
        assert!(
            s.contains("c-recipe") && !s.contains("c-photos") && s.contains("Photos"),
            "{s}"
        );
        let pool = kitchen();
        let only = BTreeSet::new();
        let mut shows = Shows::new(&pool, &nodes, "c-meals", &only);
        assert!(shows.run(Use::default()));
        let rows = rows_of(&shows.data());
        assert!(rows.contains_key("notes://recipe_steps/m2-s1"), "{rows:?}");
        assert!(
            !rows.keys().any(|u| u.contains("m1")),
            "the meal the list hides, and its recipe, stay home: {rows:?}"
        );
        assert_eq!(shows.by_layout["c-recipe"].len(), 1 + 3 + 3 + 3);
    }

    /// What a stored link asked for reads back from its key.
    #[test]
    fn a_link_key_keeps_what_it_asked_for() {
        let ask = Ask {
            layout: "c-recipe".into(),
            rows: vec!["notes://meals/m1".into()],
            item: Some("notes://meals/m1".into()),
            params: BTreeMap::from([("id".to_owned(), "m1".to_owned())]),
            leave_out: BTreeSet::from(["c-photos".to_owned()]),
            publish_new_rows: true,
        };
        assert_eq!(Ask::from_key("c-recipe", &ask.key()), ask);
        assert_eq!(
            Ask::from_key("c-recipe", &ask.key()).identity(),
            Ask {
                publish_new_rows: false,
                ..ask.clone()
            }
            .identity(),
            "publishing new rows or not is the same link"
        );
        // A key stored before params (rows and item only) reads as it did.
        let old = Ask::from_key("c-x", &["notes://a/1".into(), "item:notes://a/2".into()]);
        assert_eq!(old.rows, vec!["notes://a/1".to_owned()]);
        assert_eq!(old.item.as_deref(), Some("notes://a/2"));
        assert!(is_slug("lentil-soup") && !is_slug("-x") && !is_slug("A") && !is_slug(""));
    }
}
