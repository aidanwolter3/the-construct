//! Edit groups and checkpoints (decision `edit-groups`): how an unattended
//! agent (Anticipate, docs/anticipate.md) changes the owner's screens so
//! that every change can be judged before it goes live and undone in one tap
//! after.
//!
//! An **edit group** is a set of writes to the pod's own layouts and `@me/`
//! components, each named by its authored name and the version it was based
//! on (`base_seq`, what `get_layout` calls the version):
//!
//! - **staged** ([`stage`]): the writes are made inside a savepoint that is
//!   rolled back, so nothing goes live; what they would make is kept (the
//!   staged names and elements, which `walk_journey {group}` reads), with a
//!   report: the expanded diff, the names they re-seal, and whether the
//!   change is `additive` (only adds, plus a binding or target replaced
//!   inside the failing subtree of a stored walk, with added nodes that call
//!   only what the screen already calls, link to no new host and show no
//!   injected text);
//! - **applied** ([`apply`]): the same writes, for real, in one transaction
//!   through [`super::write::put`], only when every authored name is still
//!   at its `base_seq` and home is still where it was when staged, and,
//!   under a grant, only when the group is additive and touches no protected
//!   name nor a component one uses;
//! - **reverted** ([`revert`]): each authored name back to the element it
//!   had, its references sealed again to what they name now (so an embedded
//!   component edited since keeps its edit), only when its newest version
//!   that isn't just a re-seal is the one the group wrote; otherwise nothing
//!   moves and the group is `conflict`.
//!
//! A **checkpoint** is home's element before an applied group: one hash that
//! pins every element home reached then. [`restore_checkpoint`] puts every
//! name in its closure back (`@core/` names stay current), after a dry run
//! that says what it moves, which of the owner's own edits it undoes and what
//! home would no longer reach. Checkpoints, groups and the elements they need
//! are kept from the pod's garbage collection ([`pinned`]), for 90 days or the
//! newest 30 checkpoints, whichever keeps more.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use construct_elements::refs::Site;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

use super::store as cstore;
use super::write::{self, ConnObjects};
use super::{CORE_PREFIX, ME_PREFIX};
use crate::Pod;
use crate::grant::Grant;
use crate::layouts::{AUTHOR_CLAUDE, AUTHOR_POD};
use crate::util::now_secs;

/// How long checkpoints are kept, at least.
pub const KEEP_SECS: i64 = 90 * 86_400;
/// How many checkpoints are kept, at least.
pub const KEEP_COUNT: i64 = 30;
/// The most writes one group makes.
pub const MAX_ENTRIES: usize = 4;
/// The note prefix of a group's writes in each name's history.
pub const NOTE_PREFIX: &str = "Anticipate:";
/// The note prefix of a group's revert.
pub const REVERT_PREFIX: &str = "Reverted:";

fn refuse(error: &str, detail: impl Into<String>) -> Value {
    json!({ "error": error, "detail": detail.into() })
}

fn store_err(e: rusqlite::Error) -> Value {
    refuse("store_error", e.to_string())
}

/// Create the tables if they aren't there.
pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS edit_groups (
             id          TEXT PRIMARY KEY,
             title       TEXT NOT NULL,
             why         TEXT NOT NULL DEFAULT '',
             author      TEXT NOT NULL,
             walk_id     TEXT,
             state       TEXT NOT NULL,
             created_at  INTEGER NOT NULL,
             applied_at  INTEGER,
             reverted_at INTEGER,
             entries     TEXT NOT NULL,
             staged      TEXT NOT NULL DEFAULT '{}',
             report      TEXT NOT NULL DEFAULT '{}',
             staged_root TEXT,
             root_before TEXT,
             root_after  TEXT,
             checkpoint  TEXT
         );
         CREATE TABLE IF NOT EXISTS construct_checkpoints (
             id    TEXT PRIMARY KEY,
             at    INTEGER NOT NULL,
             root  TEXT NOT NULL,
             note  TEXT NOT NULL DEFAULT '',
             grp   TEXT,
             names TEXT NOT NULL DEFAULT '{}'
         );
         CREATE TABLE IF NOT EXISTS construct_pins (
             hash  TEXT NOT NULL,
             owner TEXT NOT NULL,
             PRIMARY KEY (hash, owner)
         );",
    )?;
    // A group applied as a trial (the owner keeps it, or it is undone).
    let has_trial: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('edit_groups') WHERE name = 'trial')",
        [],
        |r| r.get(0),
    )?;
    if !has_trial {
        conn.execute_batch("ALTER TABLE edit_groups ADD COLUMN trial INTEGER NOT NULL DEFAULT 0;")?;
    }
    Ok(())
}

/// The element hashes groups and checkpoints keep from garbage collection
/// ([`cstore::collect_garbage`] starts from them as well as from the names'
/// histories). Empty when the tables don't exist yet.
pub fn pinned(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'construct_pins')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached("SELECT DISTINCT hash FROM construct_pins")?;
    stmt.query_map([], |r| r.get(0))?.collect()
}

/// How long a group's screens are listed in the manifest's swaps.
pub const SWAP_SECS: i64 = 7 * 86_400;
/// The most groups the manifest's swaps list.
pub const SWAP_GROUPS: usize = 8;

/// The manifest's swaps (decision `instant-swap`): for each group applied in
/// the last week (a trial, kept or undone; newest [`SWAP_GROUPS`]), each name
/// it wrote with the element it had before and the one the group wrote, both
/// as a manifest's devices are sent them ([`construct_elements::stripped`],
/// stored). A device holds both and swaps at once on Undo or Redo. Empty
/// when there are none or the tables don't exist yet.
pub fn swaps(conn: &Connection) -> rusqlite::Result<Map<String, Value>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'edit_groups')",
        [],
        |r| r.get(0),
    )?;
    let mut out = Map::new();
    if !exists {
        return Ok(out);
    }
    let rows: Vec<(String, String)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT id, entries FROM edit_groups
             WHERE state IN ('trial', 'applied', 'reverted') AND applied_at >= ?1
             ORDER BY applied_at DESC LIMIT ?2",
        )?;
        stmt.query_map(params![now_secs() - SWAP_SECS, SWAP_GROUPS as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let sent = |hash: &str| -> rusqlite::Result<Option<String>> {
        let Some((el, features)) = cstore::object(conn, hash)? else {
            return Ok(None);
        };
        let v = construct_elements::stripped(&el);
        let id = construct_elements::object_id(&v);
        cstore::put_object(conn, &id, &v, &features)?;
        Ok(Some(id))
    };
    for (id, entries) in rows {
        let mut names = Map::new();
        for e in entries_of(&entries) {
            let (Some(b), Some(a)) = (&e.before, &e.after) else {
                continue;
            };
            if let (Some(b), Some(a)) = (sent(b)?, sent(a)?) {
                names.insert(e.name.clone(), json!([b, a]));
            }
        }
        if !names.is_empty() {
            out.insert(id, Value::Object(names));
        }
    }
    Ok(out)
}

fn pin(conn: &Connection, hash: &str, owner: &str) -> rusqlite::Result<()> {
    if hash.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO construct_pins (hash, owner) VALUES (?1, ?2)",
        params![hash, owner],
    )?;
    Ok(())
}

fn new_id(prefix: &str) -> String {
    let mut buf = [0u8; 8];
    let _ = getrandom::fill(&mut buf);
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}_{hex}")
}

/// The newest seq of `name` that isn't only a re-seal (0: none).
pub fn newest_written_seq(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    Ok(conn
        .query_row(
            "SELECT seq FROM name_history WHERE name = ?1
             AND NOT (author = ?2 AND COALESCE(note, '') LIKE ?3)
             ORDER BY seq DESC LIMIT 1",
            params![name, AUTHOR_POD, format!("{}%", cstore::RESEAL_NOTE)],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn home_hash(conn: &Connection) -> Result<String, Value> {
    cstore::name(conn, crate::HOME_APP)
        .map_err(store_err)?
        .ok_or_else(|| refuse("no_home", "the pod has no home yet"))
}

fn element(conn: &Connection, hash: &str) -> Result<Arc<Value>, Value> {
    cstore::object(conn, hash)
        .map_err(store_err)?
        .map(|(v, _)| v)
        .ok_or_else(|| refuse("not_found", format!("element {hash} is gone")))
}

/// What clients draw for element `hash` (components expanded), or null.
fn expanded(conn: &Connection, hash: &str) -> Value {
    let objects = ConnObjects::new(conn);
    construct_elements::materialize(&objects, hash)
        .map(|m| m.node)
        .unwrap_or(Value::Null)
}

// ---------- the writes ----------

/// One write of a group, as the caller gives it and as it is stored.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    kind: Kind,
    base_seq: i64,
    /// `{layout}` or `{json_patch}` for a layout, `{component}` for one.
    change: Value,
    before: Option<String>,
    after: Option<String>,
    applied_seq: Option<i64>,
    /// The seq its revert wrote: a redo needs the name still there.
    reverted_seq: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Layout,
    Component,
}

impl Entry {
    fn from_args(conn: &Connection, v: &Value) -> Result<Entry, Value> {
        let (name, kind) = if let Some(n) = v["layout_id"].as_str().or(v["id"].as_str()) {
            (n.to_owned(), Kind::Layout)
        } else if let Some(n) = v["component"].as_str() {
            let n = if n.starts_with(ME_PREFIX) {
                n.to_owned()
            } else {
                format!("{ME_PREFIX}{n}")
            };
            (n, Kind::Component)
        } else {
            return Err(refuse(
                "invalid_arguments",
                "each change names \"id\" (a layout) or \"component\" (an @me/ component)",
            ));
        };
        if name.starts_with(CORE_PREFIX) {
            return Err(refuse(
                "read_only",
                "core layouts and components ship with the server",
            ));
        }
        if kind == Kind::Layout && !super::is_layout_name(&name) {
            return Err(refuse(
                "not_a_layout",
                format!("'{name}' isn't one of the pod's layouts"),
            ));
        }
        let base_seq = v["base_version"].as_i64().ok_or_else(|| {
            refuse(
                "invalid_arguments",
                "each change gives \"base_version\": the version get_layout or get_component showed",
            )
        })?;
        let mut change = Map::new();
        for k in ["layout", "json_patch", "definition"] {
            if let Some(x) = v.get(k).filter(|x| !x.is_null()) {
                change.insert(k.into(), x.clone());
            }
        }
        if change.is_empty() {
            return Err(refuse(
                "invalid_arguments",
                "each change gives \"layout\" or \"json_patch\" (a layout), or \"definition\" (a component)",
            ));
        }
        let before = cstore::name(conn, &name).map_err(store_err)?;
        if before.is_none() {
            return Err(refuse("not_found", format!("nothing is named '{name}'")));
        }
        Ok(Entry {
            name,
            kind,
            base_seq,
            change: Value::Object(change),
            before,
            after: None,
            applied_seq: None,
            reverted_seq: None,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "kind": match self.kind { Kind::Layout => "layout", Kind::Component => "component" },
            "base_seq": self.base_seq,
            "change": self.change,
            "before": self.before,
            "after": self.after,
            "applied_seq": self.applied_seq,
            "reverted_seq": self.reverted_seq,
        })
    }

    fn from_json(v: &Value) -> Option<Entry> {
        Some(Entry {
            name: v["name"].as_str()?.to_owned(),
            kind: if v["kind"] == "component" {
                Kind::Component
            } else {
                Kind::Layout
            },
            base_seq: v["base_seq"].as_i64()?,
            change: v["change"].clone(),
            before: v["before"].as_str().map(str::to_owned),
            after: v["after"].as_str().map(str::to_owned),
            applied_seq: v["applied_seq"].as_i64(),
            reverted_seq: v["reverted_seq"].as_i64(),
        })
    }

    /// Make the write in `conn`'s transaction; the names it moved.
    fn write(&self, pod: &Pod, conn: &Connection, note: &str) -> Result<Vec<(String, i64)>, Value> {
        match self.kind {
            Kind::Layout => {
                let mut args =
                    json!({ "app": self.name, "base_version": self.base_seq, "note": note });
                if let Some(l) = self.change.get("layout") {
                    args["layout"] = l.clone();
                }
                if let Some(p) = self.change.get("json_patch") {
                    args["json_patch"] = p.clone();
                }
                write::update_layout(pod, conn, &args).map(|(_, m)| m)
            }
            Kind::Component => {
                let args = json!({ "name": self.name, "base_version": self.base_seq,
                    "component": self.change["definition"], "note": note });
                write::put_component(pod, conn, &args, true).map(|(_, m)| m)
            }
        }
    }
}

fn entries_of(v: &str) -> Vec<Entry> {
    serde_json::from_str::<Vec<Value>>(v)
        .unwrap_or_default()
        .iter()
        .filter_map(Entry::from_json)
        .collect()
}

// ---------- protection and the additive check ----------

/// The names a grant protects: those it lists, and every component one of
/// them uses (`core://Use`, transitively): a component Today uses changes
/// Today. Layouts a protected one embeds or links to are their own names.
pub fn protected_closure(conn: &Connection, grant: &Grant) -> rusqlite::Result<BTreeSet<String>> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = grant.protected_names.iter().cloned().collect();
    while let Some(n) = queue.pop_front() {
        if !out.insert(n.clone()) {
            continue;
        }
        let Some(h) = cstore::name(conn, &n)? else {
            continue;
        };
        let Some((el, _)) = cstore::object(conn, &h)? else {
            continue;
        };
        for (r, site) in construct_elements::refs::all(&el["node"]) {
            if site == Site::Use && !r.name.starts_with(CORE_PREFIX) {
                queue.push_back(r.name.clone());
            }
        }
    }
    Ok(out)
}

/// Keys whose value may be replaced inside the failing subtree: what binds
/// a node to data or says where a tap goes.
const BINDING_KEYS: &[&str] = &[
    "value_expr",
    "query_key",
    "ref",
    "target_uri",
    "endpoint",
    "field",
    "params",
    "payload",
    "entity_uri",
    "presentation",
    "where",
];

/// What the walk a group fixes says (`journeys` decision): the layout and
/// node path of the failing step, and the endpoint its `expect-write`
/// names.
#[derive(Debug, Clone, Default)]
pub struct Failing {
    pub layout: Option<String>,
    pub pointer: Option<String>,
    /// The data set the journey's `expect-write` names: a fix may newly call
    /// an endpoint with that name as a path segment, among `known` (the
    /// journey's `expect-write` names), among `known`.
    pub endpoint: Option<String>,
    /// The endpoints some layout of the pod already calls: a fix reuses one,
    /// never invents one.
    pub known: BTreeSet<String>,
}

/// The facts of `before` the additive check compares added nodes with: the
/// endpoints it calls and the web hosts it links to.
#[derive(Default)]
struct Calls {
    endpoints: BTreeSet<String>,
    hosts: BTreeSet<String>,
    /// Endpoints a fix may add although the screen didn't call them.
    allowed: BTreeSet<String>,
}

fn collect_calls(v: &Value, out: &mut Calls) {
    match v {
        Value::Object(o) => {
            if let Some(e) = o.get("endpoint").and_then(Value::as_str) {
                out.endpoints.insert(e.to_owned());
            }
            for x in o.values() {
                collect_calls(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_calls(x, out)),
        // Any address, whichever field holds it: an image's `url`, a
        // link's, a fetch's, or one inside a text (Markdown links).
        Value::String(s) => out.hosts.extend(crate::grant::url_hosts(s)),
        _ => {}
    }
}

/// The web hosts the pod's live screens use (every layout as clients draw
/// it, components expanded): the only ones a granted credential's rows may
/// name and the pod fetches from for it (`crate::grant::for_call`).
pub fn screen_hosts(conn: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    let mut calls = Calls::default();
    for l in write::live_layouts(conn)? {
        collect_calls(&l.node, &mut calls);
    }
    Ok(calls.hosts)
}

/// Where the pod's live screens go (`crate::grant::Nav`): the row fields a
/// screen opens as a `target_uri` bound to `$item.<field>`, and every
/// screen's route.
pub fn screen_nav(conn: &Connection) -> rusqlite::Result<crate::grant::Nav> {
    fn walk(v: &Value, nav: &mut crate::grant::Nav) {
        match v {
            Value::Object(o) => {
                if let Some(f) = o
                    .get("target_uri")
                    .and_then(Value::as_str)
                    .and_then(|t| t.strip_prefix("$item."))
                    .filter(|f| f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                {
                    nav.fields.insert(f.to_owned());
                }
                if let Some(screens) = o.get("screens").and_then(Value::as_array) {
                    for s in screens {
                        if let Some(r) = s["route"].as_str() {
                            nav.routes.insert(r.to_owned());
                        }
                    }
                }
                o.values().for_each(|x| walk(x, nav));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, nav)),
            _ => {}
        }
    }
    let mut nav = crate::grant::Nav::default();
    for l in write::live_layouts(conn)? {
        walk(&l.node, &mut nav);
    }
    Ok(nav)
}

/// The web hosts `after` names that `before` didn't.
fn new_hosts(before: &Value, after: &Value) -> BTreeSet<String> {
    let (mut b, mut a) = (Calls::default(), Calls::default());
    collect_calls(before, &mut b);
    collect_calls(after, &mut a);
    a.hosts.difference(&b.hosts).cloned().collect()
}

/// Why an added (or replaced) value does something the screen didn't:
/// a new endpoint, a delete, a link to a new host, or text bound to what an
/// agent wrote. `None`: nothing new.
fn new_behaviour(v: &Value, before: &Calls) -> Option<String> {
    match v {
        Value::Object(o) => {
            if let Some(e) = o.get("endpoint").and_then(Value::as_str)
                && !before.endpoints.contains(e)
                && !before.allowed.contains(e)
            {
                return Some(format!("it calls {e}, which the screen didn't call"));
            }
            if let Some(i) = o.get("intent").and_then(Value::as_str)
                && i.contains("delete")
            {
                return Some(format!("it adds a delete ({i})"));
            }
            for (k, x) in o {
                if let Some(s) = x.as_str() {
                    if let Some(h) = crate::grant::url_hosts(s)
                        .into_iter()
                        .find(|h| !before.hosts.contains(h))
                    {
                        return Some(format!(
                            "it reaches {h} (an image, link or fetch), which the screen didn't"
                        ));
                    }
                    if s.contains("anticipate_needs") || s.contains("walk://") {
                        return Some("it shows text an agent wrote".into());
                    }
                    if k == "endpoint" && s.ends_with("/delete") && !before.endpoints.contains(s) {
                        return Some(format!("it adds a delete ({s})"));
                    }
                }
                if let Some(r) = new_behaviour(x, before) {
                    return Some(r);
                }
            }
            None
        }
        Value::Array(a) => a.iter().find_map(|x| new_behaviour(x, before)),
        // A string in a list (an object's are read above).
        Value::String(s) => crate::grant::url_hosts(s)
            .into_iter()
            .find(|h| !before.hosts.contains(h))
            .map(|h| format!("it reaches {h} (an image, link or fetch), which the screen didn't")),
        _ => None,
    }
}

/// The additive check's findings over one layout's expanded tree.
#[derive(Default, Debug)]
pub struct Diff {
    /// JSON pointers of what was added.
    pub added: Vec<String>,
    /// JSON pointers of values replaced (allowed or not).
    pub replaced: Vec<String>,
    /// Why it isn't additive; empty: it is.
    pub violations: Vec<String>,
}

fn ptr(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// Compare `before` with `after` (expanded trees) at `path`.
fn diff(
    before: &Value,
    after: &Value,
    path: &str,
    failing: Option<&str>,
    calls: &Calls,
    out: &mut Diff,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(b), Value::Object(a)) => {
            for (k, bv) in b {
                // A reference's sealed element moves with any re-seal: it is
                // the same reference.
                if k == "el" {
                    continue;
                }
                match a.get(k) {
                    None => out.violations.push(format!("{} was removed", ptr(path, k))),
                    Some(av) => {
                        let at = ptr(path, k);
                        let scalar = !bv.is_object() && !bv.is_array();
                        if scalar || (k == "ref" || k == "params" || k == "payload" || k == "where")
                        {
                            if bv != av {
                                replaced(k, bv, av, &at, failing, calls, out);
                            }
                        } else {
                            diff(bv, av, &at, failing, calls, out);
                        }
                    }
                }
            }
            for (k, av) in a {
                if !b.contains_key(k) {
                    let at = ptr(path, k);
                    if let Some(why) = new_behaviour(&json!({ k.clone(): av }), calls) {
                        out.violations.push(format!("{at}: {why}"));
                    }
                    out.added.push(at);
                }
            }
        }
        // As many children as before: nothing was added there, so each
        // child is compared with the one in its place.
        (Value::Array(b), Value::Array(a)) if a.len() == b.len() => {
            for (i, (bv, av)) in b.iter().zip(a).enumerate() {
                diff(bv, av, &format!("{path}/{i}"), failing, calls, out);
            }
        }
        (Value::Array(b), Value::Array(a)) => {
            // Keep the order: every child before must appear after, in order;
            // a child that changed in place is matched by position among the
            // ones of its type.
            let mut j = 0;
            for (i, bv) in b.iter().enumerate() {
                let found = a[j..].iter().position(|x| x == bv);
                match found {
                    Some(off) => {
                        for (k, extra) in a[j..j + off].iter().enumerate() {
                            note_added(extra, &format!("{path}/{}", j + k), calls, out);
                        }
                        j += off + 1;
                    }
                    None => {
                        // The next child of the same type stands for it.
                        let same = a[j..]
                            .iter()
                            .position(|x| x["type"] == bv["type"] && !b[i + 1..].contains(x));
                        match same {
                            Some(off) => {
                                for (k, extra) in a[j..j + off].iter().enumerate() {
                                    note_added(extra, &format!("{path}/{}", j + k), calls, out);
                                }
                                diff(
                                    bv,
                                    &a[j + off],
                                    &format!("{path}/{}", j + off),
                                    failing,
                                    calls,
                                    out,
                                );
                                j += off + 1;
                            }
                            None => out.violations.push(format!("{path}/{i} was removed")),
                        }
                    }
                }
            }
            for (k, extra) in a[j..].iter().enumerate() {
                note_added(extra, &format!("{path}/{}", j + k), calls, out);
            }
        }
        _ => replaced("", before, after, path, failing, calls, out),
    }
}

fn note_added(v: &Value, at: &str, calls: &Calls, out: &mut Diff) {
    if let Some(why) = new_behaviour(v, calls) {
        out.violations.push(format!("{at}: {why}"));
    }
    out.added.push(at.to_owned());
}

#[allow(clippy::too_many_arguments)]
fn replaced(
    key: &str,
    before: &Value,
    after: &Value,
    at: &str,
    failing: Option<&str>,
    calls: &Calls,
    out: &mut Diff,
) {
    // A reference that names the same thing is the same reference.
    if key == "ref" && before["name"] == after["name"] && before["pin"] == after["pin"] {
        return;
    }
    out.replaced.push(at.to_owned());
    let inside = failing.is_some_and(|f| at == f || at.starts_with(&format!("{f}/")));
    if !BINDING_KEYS.contains(&key) {
        out.violations.push(format!(
            "{at} changed (only a binding or target may change)"
        ));
    } else if !inside {
        out.violations.push(format!(
            "{at} changed outside the failing step's node{}",
            failing.map(|f| format!(" ({f})")).unwrap_or_default()
        ));
    } else if let Some(why) = new_behaviour(after, calls) {
        out.violations.push(format!("{at}: {why}"));
    }
}

/// The additive check of `before` → `after` (expanded trees of one layout).
pub fn additive(before: &Value, after: &Value, failing: &Failing) -> Diff {
    let mut calls = Calls::default();
    collect_calls(before, &mut calls);
    if let Some(prefix) = &failing.endpoint {
        calls.allowed = failing
            .known
            .iter()
            .filter(|e| {
                e.split(['/', '?']).any(|seg| seg == prefix.as_str()) && !e.ends_with("/delete")
            })
            .cloned()
            .collect();
    }
    let mut out = Diff::default();
    diff(
        before,
        after,
        "",
        failing.pointer.as_deref(),
        &calls,
        &mut out,
    );
    out
}

/// What a screen lets the owner see and do: every endpoint it calls, every
/// layout or screen it opens, every list it shows and every value it binds
/// from their data (theme colours aside). A restyle may move, regroup or
/// restyle anything, but must keep all of these.
fn affordances(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                match (k.as_str(), x) {
                    ("endpoint", Value::String(e)) => {
                        out.insert(format!("calls {e}"));
                    }
                    ("ref", Value::Object(r)) => {
                        if let Some(n) = r.get("name").and_then(Value::as_str) {
                            out.insert(format!("opens {n}"));
                        }
                        continue;
                    }
                    ("target_uri", Value::String(t)) => {
                        out.insert(format!("opens {t}"));
                    }
                    ("query_key", Value::String(q)) => {
                        out.insert(format!("lists {q}"));
                    }
                    (_, Value::String(t))
                        if !t.starts_with("${local://theme")
                            && (t.contains("$item.")
                                || t.contains("${")
                                || t.contains("$route.")
                                || t.contains("$form.")) =>
                    {
                        out.insert(format!("shows {t}"));
                    }
                    _ => {}
                }
                affordances(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| affordances(x, out)),
        _ => {}
    }
}

/// The restyle check of `before` → `after` (expanded trees of one layout or
/// component), decision `edit-groups`: anything may move, regroup or change
/// style, but nothing the screen showed from the owner's data, no button,
/// link or list it had may go, and nothing new may be done (no endpoint the
/// pod's screens don't already call, no link to a new host, no delete, no
/// text bound to an agent's rows). The reasons it isn't safe; empty: it is.
pub fn safe(before: &Value, after: &Value, failing: &Failing) -> Vec<String> {
    let mut out = Vec::new();
    let (mut had, mut has) = (BTreeSet::new(), BTreeSet::new());
    affordances(before, &mut had);
    affordances(after, &mut has);
    for lost in had.difference(&has) {
        out.push(format!("it {lost} no more"));
    }
    let mut calls = Calls::default();
    collect_calls(before, &mut calls);
    if let Some(prefix) = &failing.endpoint {
        calls.allowed = failing
            .known
            .iter()
            .filter(|e| {
                e.split(['/', '?']).any(|seg| seg == prefix.as_str()) && !e.ends_with("/delete")
            })
            .cloned()
            .collect();
    }
    if let Some(why) = new_behaviour(after, &calls) {
        out.push(why);
    }
    out
}

/// The failing step of a stored walk (`journey_walks`, written by
/// `walk_journey`); default when there is none.
fn failing_of(conn: &Connection, walk_id: Option<&str>) -> rusqlite::Result<Failing> {
    let Some(id) = walk_id else {
        return Ok(Failing::default());
    };
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'journey_walks')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Failing::default());
    }
    let row: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT failing_layout, failing_pointer, expect_endpoint FROM journey_walks WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row
        .map(|(layout, pointer, endpoint)| Failing {
            layout,
            pointer,
            endpoint,
            known: BTreeSet::new(),
        })
        .unwrap_or_default())
}

// ---------- staging ----------

/// `stage_edit_group {title, why?, walk_id?, changes: [{id | component,
/// base_version, layout | json_patch | definition}]}`: make the writes in a
/// savepoint rolled back, keep what they would make, and answer the report.
pub fn stage(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
    grant: Option<&Grant>,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    ensure(conn).map_err(store_err)?;
    let title: String = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            refuse(
                "invalid_arguments",
                "\"title\": what the change does, in a line",
            )
        })?
        .chars()
        .take(120)
        .collect();
    let why: String = args["why"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    let walk_id = args["walk_id"].as_str().map(str::to_owned);
    let changes = args["changes"]
        .as_array()
        .filter(|c| !c.is_empty() && c.len() <= MAX_ENTRIES)
        .ok_or_else(|| {
            refuse(
                "invalid_arguments",
                format!("\"changes\": 1 to {MAX_ENTRIES} writes"),
            )
        })?;
    let mut entries = changes
        .iter()
        .map(|c| Entry::from_args(conn, c))
        .collect::<Result<Vec<_>, _>>()?;
    let names: BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    if names.len() != entries.len() {
        return Err(refuse("invalid_arguments", "each name is changed once"));
    }
    let protected = match grant {
        Some(g) => protected_closure(conn, g).map_err(store_err)?,
        None => BTreeSet::new(),
    };
    let mut failing = failing_of(conn, walk_id.as_deref()).map_err(store_err)?;
    if failing.endpoint.is_some() {
        let mut known = Calls::default();
        for l in write::live_layouts(conn).map_err(store_err)? {
            collect_calls(&l.node, &mut known);
        }
        failing.known = known.endpoints;
    }
    let root = home_hash(conn)?;

    // Before, as clients draw it.
    let befores: BTreeMap<String, Value> = entries
        .iter()
        .map(|e| {
            (
                e.name.clone(),
                expanded(conn, e.before.as_deref().unwrap_or("")),
            )
        })
        .collect();

    conn.execute_batch("SAVEPOINT edit_group_stage")
        .map_err(store_err)?;
    let staged = (|| -> Result<_, Value> {
        let mut moved: Vec<(String, i64)> = Vec::new();
        for e in &entries {
            moved.extend(e.write(pod, conn, &format!("{NOTE_PREFIX} {title}"))?);
        }
        let mut names_now = Map::new();
        let mut objects = Map::new();
        let mut afters: BTreeMap<String, Value> = BTreeMap::new();
        let moved_names: BTreeSet<String> = moved.iter().map(|(n, _)| n.clone()).collect();
        for n in &moved_names {
            if let Some(h) = cstore::name(conn, n).map_err(store_err)? {
                let el = element(conn, &h)?;
                objects.insert(h.clone(), (*el).clone());
                names_now.insert(n.clone(), json!(h));
            }
        }
        for e in &entries {
            let h = cstore::name(conn, &e.name)
                .map_err(store_err)?
                .unwrap_or_default();
            afters.insert(e.name.clone(), expanded(conn, &h));
        }
        Ok((moved_names, names_now, objects, afters))
    })();
    conn.execute_batch("ROLLBACK TO edit_group_stage; RELEASE edit_group_stage")
        .map_err(store_err)?;
    let (moved_names, staged_names, staged_objects, afters) = staged?;

    // The report: the additive check per authored layout, what re-seals,
    // and what the grant protects.
    let mut violations: Vec<String> = Vec::new();
    let mut safe_violations: Vec<String> = Vec::new();
    let mut diffs = Map::new();
    for e in &entries {
        let f = if failing.layout.as_deref() == Some(e.name.as_str()) {
            failing.clone()
        } else {
            Failing {
                endpoint: failing.endpoint.clone(),
                known: failing.known.clone(),
                ..Failing::default()
            }
        };
        let d = additive(&befores[&e.name], &afters[&e.name], &f);
        let unsafe_why = safe(&befores[&e.name], &afters[&e.name], &f);
        safe_violations.extend(unsafe_why.iter().map(|v| format!("{}: {v}", e.name)));
        violations.extend(d.violations.iter().map(|v| format!("{}: {v}", e.name)));
        diffs.insert(
            e.name.clone(),
            json!({ "added": d.added, "replaced": d.replaced, "violations": d.violations }),
        );
        if protected.contains(&e.name) {
            violations.push(format!("{} is protected", e.name));
            safe_violations.push(format!("{} is protected", e.name));
        }
    }
    // Under a grant a new web host is refused here, not only at apply: a
    // staged group is previewed and walked, and nothing it names should be
    // fetched (design: the pod's own fetches).
    if grant.is_some() {
        let mut reached: BTreeSet<String> = BTreeSet::new();
        for e in &entries {
            reached.extend(new_hosts(&befores[&e.name], &afters[&e.name]));
        }
        if !reached.is_empty() {
            return Err(refuse(
                "new_host",
                format!(
                    "the group reaches {} (an image, link or fetch), which the screen didn't; \
                     this connection's grant stages only changes that reach the hosts a screen \
                     already uses",
                    reached.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }
    let followers: Vec<&String> = moved_names
        .iter()
        .filter(|n| !names.contains(n.as_str()))
        .collect();
    let report = json!({
        "additive": violations.is_empty(),
        "violations": violations,
        "safe": safe_violations.is_empty(),
        "safe_violations": safe_violations,
        "diff": diffs,
        "followers": followers,
        "staged_root": staged_names.get(crate::HOME_APP),
    });
    for e in &mut entries {
        e.after = staged_names
            .get(&e.name)
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    let id = new_id("grp");
    let entries_json = Value::Array(entries.iter().map(Entry::to_json).collect()).to_string();
    conn.execute(
        "INSERT INTO edit_groups (id, title, why, author, walk_id, state, created_at, entries,
                                  staged, report, staged_root)
         VALUES (?1, ?2, ?3, ?4, ?5, 'proposed', ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            title,
            why,
            AUTHOR_CLAUDE,
            walk_id,
            now_secs(),
            entries_json,
            json!({ "names": staged_names, "objects": staged_objects }).to_string(),
            report.to_string(),
            root,
        ],
    )
    .map_err(store_err)?;
    for e in &entries {
        if let Some(b) = &e.before {
            pin(conn, b, &id).map_err(store_err)?;
        }
    }
    Ok((
        json!({ "ok": true, "id": id, "state": "proposed", "report": report,
                "note": "Nothing is live yet: walk_journey {group} walks the staged screens; apply_edit_group makes them live." }),
        Vec::new(),
    ))
}

/// A staged group's names (to element hashes) and those elements.
pub type Staged = (BTreeMap<String, String>, BTreeMap<String, Value>);

/// What a staged group would make: its names and their elements, for a walk
/// of the staged screens (`walk_journey {group}`).
pub fn staged(conn: &Connection, id: &str) -> rusqlite::Result<Option<Staged>> {
    ensure(conn)?;
    let s: Option<String> = conn
        .query_row("SELECT staged FROM edit_groups WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(s.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| {
            let names = v["names"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, h)| Some((k.clone(), h.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            let objects = v["objects"]
                .as_object()
                .map(|m| m.iter().map(|(k, o)| (k.clone(), o.clone())).collect())
                .unwrap_or_default();
            (names, objects)
        }))
}

struct Group {
    id: String,
    title: String,
    state: String,
    entries: Vec<Entry>,
    report: Value,
    staged_root: Option<String>,
    trial: bool,
}

fn group(conn: &Connection, id: &str) -> Result<Group, Value> {
    ensure(conn).map_err(store_err)?;
    conn.query_row(
        "SELECT id, title, state, entries, report, staged_root, trial FROM edit_groups WHERE id = ?1",
        [id],
        |r| {
            Ok(Group {
                id: r.get(0)?,
                title: r.get(1)?,
                state: r.get(2)?,
                entries: entries_of(&r.get::<_, String>(3)?),
                report: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
                staged_root: r.get(5)?,
                trial: r.get::<_, i64>(6)? != 0,
            })
        },
    )
    .optional()
    .map_err(store_err)?
    .ok_or_else(|| refuse("group_not_found", format!("no edit group '{id}'")))
}

/// Record a checkpoint of home as it is now; its id.
fn checkpoint(conn: &Connection, note: &str, grp: Option<&str>) -> Result<String, Value> {
    ensure(conn).map_err(store_err)?;
    let root = home_hash(conn)?;
    let id = new_id("ckpt");
    // Every pod name's element now, so a restore knows each exactly (its
    // history's times are seconds).
    let names: Map<String, Value> = cstore::names(conn)
        .map_err(store_err)?
        .into_iter()
        .filter(|(n, _)| !n.starts_with(CORE_PREFIX))
        .map(|(n, h)| (n, json!(h)))
        .collect();
    conn.execute(
        "INSERT INTO construct_checkpoints (id, at, root, note, grp, names) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, now_secs(), root, note, grp, Value::Object(names.clone()).to_string()],
    )
    .map_err(store_err)?;
    pin(conn, &root, &id).map_err(store_err)?;
    for h in names.values().filter_map(Value::as_str) {
        pin(conn, h, &id).map_err(store_err)?;
    }
    prune(conn).map_err(store_err)?;
    Ok(id)
}

/// Forget checkpoints older than [`KEEP_SECS`] beyond the newest
/// [`KEEP_COUNT`], and groups ended that long ago, with their pins.
fn prune(conn: &Connection) -> rusqlite::Result<()> {
    let cutoff = now_secs() - KEEP_SECS;
    conn.execute(
        "DELETE FROM construct_checkpoints WHERE at < ?1 AND id NOT IN
             (SELECT id FROM construct_checkpoints ORDER BY at DESC LIMIT ?2)",
        params![cutoff, KEEP_COUNT],
    )?;
    conn.execute(
        "DELETE FROM edit_groups WHERE created_at < ?1 AND state != 'applied'",
        [cutoff],
    )?;
    conn.execute(
        "DELETE FROM construct_pins WHERE owner NOT IN (SELECT id FROM construct_checkpoints)
             AND owner NOT IN (SELECT id FROM edit_groups)",
        [],
    )?;
    Ok(())
}

/// Refuse `g` under `grant` if it writes a name the grant protects.
fn refuse_protected(conn: &Connection, grant: &Grant, g: &Group) -> Result<(), Value> {
    let protected = protected_closure(conn, grant).map_err(store_err)?;
    match g.entries.iter().find(|e| protected.contains(&e.name)) {
        Some(e) => Err(refuse("protected", format!("{} is protected", e.name))),
        None => Ok(()),
    }
}

/// `apply_edit_group {id}`: make a staged group live.
pub fn apply(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
    grant: Option<&Grant>,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| refuse("invalid_arguments", "\"id\" is required"))?;
    let mut g = group(conn, id)?;
    // Under a grant the server holds the gate (decision `mcp-grants`) for
    // every way a group goes live: applied or redone, it is safe (restyles
    // allowed, nothing lost, nothing new done) and touches nothing
    // protected; and only the owner keeps a trial.
    if let Some(gr) = grant {
        match g.state.as_str() {
            "proposed" | "reverted" => {
                refuse_protected(conn, gr, &g)?;
                if g.report["safe"] != true {
                    return Err(refuse(
                        "not_safe",
                        format!(
                            "this connection's grant applies only safe groups: {}",
                            g.report["safe_violations"]
                        ),
                    ));
                }
            }
            "trial" => {
                return Err(refuse(
                    "owner_keeps",
                    "only the owner keeps a trial (Keep in the app); this connection's grant can't",
                ));
            }
            _ => {}
        }
    }
    match g.state.as_str() {
        "proposed" => {}
        // Keep: a trial becomes the owner's.
        "trial" => {
            conn.execute(
                "UPDATE edit_groups SET state = 'applied', trial = 0 WHERE id = ?1",
                [&g.id],
            )
            .map_err(store_err)?;
            return Ok((
                json!({ "ok": true, "id": g.id, "state": "applied", "kept": true }),
                Vec::new(),
            ));
        }
        // Redo: the same elements again.
        "reverted" => return redo(pod, conn, g),
        // Already the owner's: a second Keep changes nothing.
        "applied" => {
            return Ok((
                json!({ "ok": true, "id": g.id, "state": "applied", "already": true }),
                Vec::new(),
            ));
        }
        other => {
            return Err(refuse(
                "not_proposed",
                format!(
                    "the group is {other}: a proposed group is applied, a trial kept, an undone one redone"
                ),
            ));
        }
    }
    let trial = args["trial"] == true;
    let root = home_hash(conn)?;
    if g.staged_root.as_deref().is_some_and(|s| s != root) {
        return Err(refuse(
            "stale_stage",
            "the screens changed since this group was staged: stage it again and walk it again",
        ));
    }
    for e in &g.entries {
        let seq = cstore::last_seq(conn, &e.name).map_err(store_err)?;
        if seq != e.base_seq {
            return Err(
                json!({ "error": "stale_base", "name": e.name, "latest_version": seq,
                "detail": "it changed since the group was staged: stage it again" }),
            );
        }
    }
    let ckpt = checkpoint(conn, &format!("Before: {}", g.title), Some(&g.id))?;
    let mut moved = Vec::new();
    for e in &mut g.entries {
        moved.extend(e.write(pod, conn, &format!("{NOTE_PREFIX} {}", g.title))?);
        e.applied_seq = Some(cstore::last_seq(conn, &e.name).map_err(store_err)?);
        e.after = cstore::name(conn, &e.name).map_err(store_err)?;
        if let Some(a) = &e.after {
            pin(conn, a, &g.id).map_err(store_err)?;
        }
    }
    let after = home_hash(conn)?;
    let state = if trial { "trial" } else { "applied" };
    conn.execute(
        "UPDATE edit_groups SET state = ?7, trial = ?8, applied_at = ?2, entries = ?3,
             root_before = ?4, root_after = ?5, checkpoint = ?6 WHERE id = ?1",
        params![
            g.id,
            now_secs(),
            Value::Array(g.entries.iter().map(Entry::to_json).collect()).to_string(),
            root,
            after,
            ckpt,
            state,
            trial as i64
        ],
    )
    .map_err(store_err)?;
    pin(conn, &after, &g.id).map_err(store_err)?;
    Ok((
        json!({ "ok": true, "id": g.id, "state": state, "checkpoint": ckpt,
                "names": g.entries.iter().map(|e| json!({ "name": e.name, "version": e.applied_seq })).collect::<Vec<_>>() }),
        moved,
    ))
}

/// Redo an undone group: each authored name to the element the group wrote,
/// its references sealed again to what they name now. The elements are the
/// ones devices already hold, so swapping back and forth is fast. Only while
/// each name is still where the undo left it.
fn redo(pod: &Pod, conn: &Connection, mut g: Group) -> Result<(Value, Vec<(String, i64)>), Value> {
    for e in &g.entries {
        let written = newest_written_seq(conn, &e.name).map_err(store_err)?;
        if Some(written) != e.reverted_seq {
            conn.execute(
                "UPDATE edit_groups SET state = 'conflict' WHERE id = ?1",
                [&g.id],
            )
            .map_err(store_err)?;
            return Ok((
                json!({ "ok": false, "id": g.id, "state": "conflict", "name": e.name,
                        "detail": format!("{} was changed since the undo", e.name) }),
                Vec::new(),
            ));
        }
    }
    let mut moved = Vec::new();
    for e in &mut g.entries {
        let Some(after) = e.after.clone() else {
            continue;
        };
        moved.extend(put_back(
            pod,
            conn,
            &e.name,
            &after,
            &format!("{NOTE_PREFIX} {}", g.title),
        )?);
        e.applied_seq = Some(newest_written_seq(conn, &e.name).map_err(store_err)?);
    }
    let state = if g.trial { "trial" } else { "applied" };
    conn.execute(
        "UPDATE edit_groups SET state = ?2, entries = ?3, applied_at = ?4 WHERE id = ?1",
        params![
            g.id,
            state,
            Value::Array(g.entries.iter().map(Entry::to_json).collect()).to_string(),
            now_secs()
        ],
    )
    .map_err(store_err)?;
    Ok((
        json!({ "ok": true, "id": g.id, "state": state, "redone": true }),
        moved,
    ))
}

/// Put `name` back to element `hash`, its references sealed again to what
/// they name now.
fn put_back(
    pod: &Pod,
    conn: &Connection,
    name: &str,
    hash: &str,
    note: &str,
) -> Result<Vec<(String, i64)>, Value> {
    let el = element(conn, hash)?;
    let mut el = (*el).clone();
    el["node"] = write::normalize(conn, pod.catalog(), &el["node"])?;
    let layout = !name.starts_with(ME_PREFIX);
    write::put(conn, pod.catalog(), name, el, layout, AUTHOR_CLAUDE, note)
}

/// `revert_edit_group {id}`: undo an applied group.
pub fn revert(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
    grant: Option<&Grant>,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| refuse("invalid_arguments", "\"id\" is required"))?;
    let mut g = group(conn, id)?;
    if g.state != "applied" && g.state != "trial" {
        return Err(refuse(
            "not_applied",
            format!("the group is {}: only an applied group is undone", g.state),
        ));
    }
    // An undo writes the group's names too: under a grant, no protected one.
    if let Some(gr) = grant {
        refuse_protected(conn, gr, &g)?;
    }
    for e in &g.entries {
        let written = newest_written_seq(conn, &e.name).map_err(store_err)?;
        if Some(written) != e.applied_seq {
            conn.execute(
                "UPDATE edit_groups SET state = 'conflict' WHERE id = ?1",
                [&g.id],
            )
            .map_err(store_err)?;
            return Ok((
                json!({ "ok": false, "id": g.id, "state": "conflict", "name": e.name,
                        "detail": format!("{} was changed since; undo it from its history (restore_version)", e.name) }),
                Vec::new(),
            ));
        }
    }
    let mut moved = Vec::new();
    for e in g.entries.iter_mut().rev() {
        let Some(before) = e.before.clone() else {
            continue;
        };
        moved.extend(put_back(
            pod,
            conn,
            &e.name,
            &before,
            &format!("{REVERT_PREFIX} {}", g.title),
        )?);
        // Where the undo left it: a redo needs it still there.
        e.reverted_seq = Some(newest_written_seq(conn, &e.name).map_err(store_err)?);
    }
    conn.execute(
        "UPDATE edit_groups SET state = 'reverted', reverted_at = ?2, entries = ?3 WHERE id = ?1",
        params![
            g.id,
            now_secs(),
            Value::Array(g.entries.iter().map(Entry::to_json).collect()).to_string()
        ],
    )
    .map_err(store_err)?;
    Ok((
        json!({ "ok": true, "id": g.id, "state": "reverted" }),
        moved,
    ))
}

/// `revert_since {since}`: undo every group applied since a time, newest
/// first, keeping the owner's own edits; stops at the first conflict.
pub fn revert_since(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
    grant: Option<&Grant>,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    ensure(conn).map_err(store_err)?;
    let since = args["since"].as_i64().unwrap_or(0);
    let ids: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT id FROM edit_groups WHERE state = 'applied' AND applied_at >= ?1 ORDER BY applied_at DESC")
            .map_err(store_err)?;
        stmt.query_map([since], |r| r.get(0))
            .map_err(store_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(store_err)?
    };
    // Under a grant, refused whole if any of them is protected.
    if let Some(gr) = grant {
        for id in &ids {
            refuse_protected(conn, gr, &group(conn, id)?)?;
        }
    }
    let mut moved = Vec::new();
    let mut done = Vec::new();
    for id in ids {
        let (out, m) = revert(pod, conn, &json!({ "id": id }), grant)?;
        moved.extend(m);
        let ok = out["ok"] == true;
        done.push(out);
        if !ok {
            break;
        }
    }
    Ok((
        json!({ "ok": done.iter().all(|d| d["ok"] == true), "groups": done }),
        moved,
    ))
}

/// The edit groups of the last 30 days, newest first.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    ensure(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, title, why, state, created_at, applied_at, reverted_at, report, entries, checkpoint
         FROM edit_groups WHERE created_at >= ?1 ORDER BY created_at DESC LIMIT 100",
    )?;
    stmt.query_map([now_secs() - 30 * 86_400], |r| {
        let report: Value = serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or(Value::Null);
        let entries = entries_of(&r.get::<_, String>(8)?);
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "title": r.get::<_, String>(1)?,
            "why": r.get::<_, String>(2)?,
            "state": r.get::<_, String>(3)?,
            "created_at": r.get::<_, i64>(4)?,
            "applied_at": r.get::<_, Option<i64>>(5)?,
            "reverted_at": r.get::<_, Option<i64>>(6)?,
            "additive": report["additive"],
            "violations": report["violations"],
            "safe": report["safe"],
            "safe_violations": report["safe_violations"],
            "followers": report["followers"],
            "names": entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
            "checkpoint": r.get::<_, Option<String>>(9)?,
        }))
    })?
    .collect()
}

// ---------- checkpoints ----------

/// The checkpoints kept, newest first, with the bytes they keep alive.
pub fn list_checkpoints(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    ensure(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, at, root, note, grp FROM construct_checkpoints ORDER BY at DESC LIMIT 100",
    )?;
    stmt.query_map([], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "at": r.get::<_, i64>(1)?,
            "root": r.get::<_, String>(2)?,
            "note": r.get::<_, String>(3)?,
            "group": r.get::<_, Option<String>>(4)?,
        }))
    })?
    .collect()
}

/// Every pod name the element `root` reaches, with the element it reached
/// (the first seen, breadth first); `@core/` names aren't followed.
fn closure(conn: &Connection, root: &str) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    out.insert(crate::HOME_APP.to_owned(), root.to_owned());
    let mut queue = VecDeque::from([root.to_owned()]);
    let mut seen = BTreeSet::new();
    while let Some(h) = queue.pop_front() {
        if !seen.insert(h.clone()) {
            continue;
        }
        let Some((el, _)) = cstore::object(conn, &h)? else {
            continue;
        };
        for (r, _) in construct_elements::refs::all(&el["node"]) {
            if r.name.starts_with(CORE_PREFIX) {
                continue;
            }
            if let Some(e) = r.el {
                out.entry(r.name.clone()).or_insert_with(|| e.clone());
                queue.push_back(e);
            }
        }
    }
    Ok(out)
}

/// `restore_checkpoint {id, dry_run?}`: put the screens home reached then
/// back. A dry run answers what it would move, the owner's own edits it
/// would undo and the layouts home would no longer reach.
pub fn restore_checkpoint(
    pod: &Pod,
    conn: &Connection,
    args: &Value,
    grant: Option<&Grant>,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    ensure(conn).map_err(store_err)?;
    let id = args["id"]
        .as_str()
        .ok_or_else(|| refuse("invalid_arguments", "\"id\" is required"))?;
    let (at, root, names): (i64, String, String) = conn
        .query_row(
            "SELECT at, root, names FROM construct_checkpoints WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(store_err)?
        .ok_or_else(|| refuse("checkpoint_not_found", format!("no checkpoint '{id}'")))?;
    // Every pod name as it was then (a name made since stays as it is).
    let mut then: BTreeMap<String, String> = serde_json::from_str::<Map<String, Value>>(&names)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(n, h)| Some((n, h.as_str()?.to_owned())))
        .collect();
    for (n, h) in closure(conn, &root).map_err(store_err)? {
        then.entry(n).or_insert(h);
    }
    let now = closure(conn, &home_hash(conn)?).map_err(store_err)?;
    let mut moves: Vec<(String, String)> = Vec::new();
    let mut yours: Vec<Value> = Vec::new();
    for (name, el) in &then {
        let current = cstore::name(conn, name).map_err(store_err)?;
        if current.as_deref() == Some(el.as_str()) {
            continue;
        }
        moves.push((name.clone(), el.clone()));
        // The owner's own edits since: history entries after the one that
        // put this element there (by seq: times are seconds).
        let history = cstore::history(conn, name).map_err(store_err)?;
        let from = history
            .iter()
            .filter(|h| h.hash == *el)
            .map(|h| h.seq)
            .max()
            .unwrap_or(0);
        for h in history {
            let note = h.note.clone().unwrap_or_default();
            if h.seq > from
                && (from > 0 || h.at > at)
                && h.author != AUTHOR_POD
                && !note.starts_with(NOTE_PREFIX)
                && !note.starts_with(REVERT_PREFIX)
            {
                yours.push(json!({ "name": name, "version": h.seq, "at": h.at, "note": note }));
            }
        }
    }
    let then_reached = closure(conn, &root).map_err(store_err)?;
    let unlinked: Vec<&String> = now
        .keys()
        .filter(|n| !then_reached.contains_key(*n))
        .collect();
    let summary = json!({
        "checkpoint": id,
        "moves": moves.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "undoes_your_edits": yours,
        "unlinked": unlinked,
        "note": "Screens come back as they were, over today's core components; rows aren't touched.",
    });
    if args["dry_run"] == true {
        return Ok((
            json!({ "ok": true, "dry_run": true, "would": summary }),
            Vec::new(),
        ));
    }
    // Under a grant, a restore that would move a protected name is refused.
    if let Some(gr) = grant {
        let protected = protected_closure(conn, gr).map_err(store_err)?;
        if let Some((n, _)) = moves.iter().find(|(n, _)| protected.contains(n)) {
            return Err(refuse("protected", format!("{n} is protected")));
        }
    }
    // Restoring can itself be undone: a checkpoint of now first.
    let before = checkpoint(conn, &format!("Before restoring {id}"), None)?;
    let home_now = home_hash(conn)?;
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for (name, el) in &moves {
        conn.execute_batch("SAVEPOINT restore_one")
            .map_err(store_err)?;
        let mut target = el.clone();
        // Home keeps the feeds it binds now as well as those it bound then,
        // so a layout that binds one now still validates.
        if name == crate::HOME_APP {
            let mut old = (*element(conn, el)?).clone();
            let cur = element(conn, &home_now)?;
            let mut feeds: Vec<Value> =
                old["bind"]["feeds"].as_array().cloned().unwrap_or_default();
            for f in cur["bind"]["feeds"].as_array().cloned().unwrap_or_default() {
                if !feeds.contains(&f) {
                    feeds.push(f);
                }
            }
            old["bind"]["feeds"] = Value::Array(feeds);
            old["node"] = write::normalize(conn, pod.catalog(), &old["node"])?;
            let r = write::put(
                conn,
                pod.catalog(),
                name,
                old,
                true,
                AUTHOR_CLAUDE,
                &format!("Restored checkpoint {id}"),
            );
            match r {
                Ok(m) => {
                    moved.extend(m);
                    conn.execute_batch("RELEASE restore_one")
                        .map_err(store_err)?;
                }
                Err(e) => {
                    conn.execute_batch("ROLLBACK TO restore_one; RELEASE restore_one")
                        .map_err(store_err)?;
                    failed.push(json!({ "name": name, "error": e }));
                }
            }
            continue;
        }
        match put_back(
            pod,
            conn,
            name,
            &std::mem::take(&mut target),
            &format!("Restored checkpoint {id}"),
        ) {
            Ok(m) => {
                moved.extend(m);
                conn.execute_batch("RELEASE restore_one")
                    .map_err(store_err)?;
            }
            Err(e) => {
                conn.execute_batch("ROLLBACK TO restore_one; RELEASE restore_one")
                    .map_err(store_err)?;
                failed.push(json!({ "name": name, "error": e }));
            }
        }
    }
    // The groups applied since are undone by this.
    conn.execute(
        "UPDATE edit_groups SET state = 'reverted', reverted_at = ?2 WHERE state = 'applied' AND applied_at >= ?1",
        params![at, now_secs()],
    )
    .map_err(store_err)?;
    Ok((
        json!({ "ok": failed.is_empty(), "restored": summary, "failed": failed, "checkpoint_before": before }),
        moved,
    ))
}

// ---------- the app's routes ----------

/// The paths the app binds and calls (Settings' manifest declares them, so
/// any of the owner's layouts may: `source-shell`).
pub const SCREEN_PATH: &str = "/v1/edit-groups/screen";
pub const APPLY_PATH: &str = "/v1/edit-groups/apply";
pub const REVERT_PATH: &str = "/v1/edit-groups/revert";
pub const RESTORE_PATH: &str = "/v1/checkpoints/restore";
/// The queries [`SCREEN_PATH`] answers.
pub const GROUPS_QUERY: &str = "edit_groups";
pub const CHECKPOINTS_QUERY: &str = "checkpoints";

pub(crate) fn router() -> axum::Router<Pod> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(SCREEN_PATH, get(get_screen))
        .route(APPLY_PATH, post(post_apply))
        .route(REVERT_PATH, post(post_revert))
        .route(RESTORE_PATH, post(post_restore))
}

fn ago(secs: i64) -> String {
    match secs {
        s if s < 3_600 => "just now".into(),
        s if s < 86_400 => format!("{} h ago", s / 3_600),
        s if s < 2 * 86_400 => "yesterday".into(),
        s => format!("{} days ago", s / 86_400),
    }
}

/// The edit groups and checkpoints as entities (`edit_groups://<id>`,
/// `checkpoints://<id>`) and their queries, newest first.
pub fn screen(conn: &Connection) -> rusqlite::Result<Value> {
    let now = now_secs();
    let mut entities = Map::new();
    let mut groups = Vec::new();
    for g in list(conn)? {
        let id = g["id"].as_str().unwrap_or_default().to_owned();
        let state = g["state"].as_str().unwrap_or_default().to_owned();
        let when = g["applied_at"]
            .as_i64()
            .or(g["created_at"].as_i64())
            .unwrap_or(now);
        let state_text = match state.as_str() {
            "applied" => format!("Changed {}", ago(now - when)),
            "proposed" => "Waiting for you".to_owned(),
            "trial" => format!("Trying it since {}", ago(now - when)),
            "reverted" => "Undone".to_owned(),
            "conflict" => "You've changed this screen since: undo it from its history".to_owned(),
            other => other.to_owned(),
        };
        let uri = format!("edit_groups://{id}");
        entities.insert(uri.clone(), json!({
            "id": id, "title": g["title"], "why": g["why"], "state": state,
            "state_text": state_text,
            "can_undo": state == "applied" || state == "trial",
            "can_keep": state == "trial",
            "can_redo": state == "reverted",
            "can_apply": state == "proposed" && g["safe"] == true,
            "names_text": g["names"].as_array().map(|n| n.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
        }));
        groups.push(json!(uri));
    }
    let mut ckpts = Vec::new();
    for c in list_checkpoints(conn)? {
        let id = c["id"].as_str().unwrap_or_default().to_owned();
        let uri = format!("checkpoints://{id}");
        let at = c["at"].as_i64().unwrap_or(now);
        entities.insert(
            uri.clone(),
            json!({
                "id": id, "note": c["note"], "when_text": ago(now - at),
                "restore_text": format!("Go back to your screens from {}", ago(now - at)),
            }),
        );
        ckpts.push(json!(uri));
    }
    Ok(json!({
        "entities": entities,
        "queries": { GROUPS_QUERY: groups, CHECKPOINTS_QUERY: ckpts },
    }))
}

async fn get_screen(
    axum::extract::State(pod): axum::extract::State<Pod>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Some(r) = crate::refuse_unless_signed(&pod, &headers).await {
        return r;
    }
    match crate::blocking(&pod, |s| s.with_conn(screen)).await {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn answer(r: Result<Value, Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match r {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => {
            let status = match e["error"].as_str() {
                Some("group_not_found" | "checkpoint_not_found") => {
                    axum::http::StatusCode::NOT_FOUND
                }
                Some("store_error" | "internal") => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                _ => axum::http::StatusCode::CONFLICT,
            };
            crate::json_error(status, e)
        }
    }
}

/// Only the owner's own sessions (a device of the pod) undo, apply or
/// restore from the app; an MCP credential uses the tools, under its grant.
async fn owner_body(
    pod: &Pod,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<Value, Box<axum::response::Response>> {
    if let Some(r) = crate::refuse_unless_signed(pod, headers).await {
        return Err(Box::new(r));
    }
    if pod.internal_caller(headers).is_some() {
        return Err(Box::new(crate::json_error(
            axum::http::StatusCode::FORBIDDEN,
            json!({ "error": "owner_only", "detail": "only the owner's own devices change this" }),
        )));
    }
    serde_json::from_slice::<Value>(body).map_err(|_| {
        Box::new(crate::json_error(
            axum::http::StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_json", "detail": "the body is a JSON object" }),
        ))
    })
}

async fn post_apply(
    axum::extract::State(pod): axum::extract::State<Pod>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let args = match owner_body(&pod, &headers, &body).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    answer(write::run_write(&pod, &args, |p, c, a| apply(p, c, a, None)).await).await
}

async fn post_revert(
    axum::extract::State(pod): axum::extract::State<Pod>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let args = match owner_body(&pod, &headers, &body).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    answer(
        write::run_write(&pod, &args, |p, c, a| {
            if a["since"].is_i64() {
                revert_since(p, c, a, None)
            } else {
                revert(p, c, a, None)
            }
        })
        .await,
    )
    .await
}

async fn post_restore(
    axum::extract::State(pod): axum::extract::State<Pod>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let args = match owner_body(&pod, &headers, &body).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    answer(write::run_write(&pod, &args, |p, c, a| restore_checkpoint(p, c, a, None)).await).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(label: &str, endpoint: &str) -> Value {
        json!({ "type": "core://Button", "label": label,
                "on_click": { "intent": "action.mutation.optimistic", "endpoint": endpoint } })
    }

    #[test]
    fn adding_a_button_that_calls_a_known_endpoint_is_additive() {
        let before = json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": "Recipe" },
            button("Cooked", "/v1/notes/meals/update"),
        ]});
        let after = json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": "Recipe" },
            button("Add to Groceries", "/v1/notes/meals/update"),
            button("Cooked", "/v1/notes/meals/update"),
        ]});
        let d = additive(&before, &after, &Failing::default());
        assert!(d.violations.is_empty(), "{:?}", d.violations);
        assert_eq!(d.added, vec!["/children/1"]);
    }

    #[test]
    fn new_endpoints_links_deletes_and_removals_are_not() {
        let before = json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": "Recipe" },
        ]});
        let cases = [
            json!({ "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Recipe" },
                button("Clear", "/v1/notes/groceries/delete") ]}),
            json!({ "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Recipe" },
                { "type": "core://Tappable", "on_click": { "intent": "action.open_link", "url": "https://phish.example/x" } } ]}),
            json!({ "type": "core://Column", "children": [] }),
            json!({ "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Recipe!" } ]}),
            json!({ "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Recipe" },
                { "type": "core://Text", "value_expr": "${notes://anticipate_needs/x#title}" } ]}),
        ];
        for after in cases {
            let d = additive(&before, &after, &Failing::default());
            assert!(!d.violations.is_empty(), "{after}");
        }
    }

    #[test]
    fn a_binding_may_change_inside_the_failing_node_only() {
        let before = json!({ "type": "core://Column", "children": [
            { "type": "core://List", "query_key": "collection:meals" },
            { "type": "core://List", "query_key": "collection:meals" },
        ]});
        let after = json!({ "type": "core://Column", "children": [
            { "type": "core://List", "query_key": "collection:meals:view:mine" },
            { "type": "core://List", "query_key": "collection:meals" },
        ]});
        let inside = Failing {
            pointer: Some("/children/0".into()),
            ..Failing::default()
        };
        assert!(additive(&before, &after, &inside).violations.is_empty());
        let elsewhere = Failing {
            pointer: Some("/children/1".into()),
            ..Failing::default()
        };
        assert!(!additive(&before, &after, &elsewhere).violations.is_empty());
        assert!(
            !additive(&before, &after, &Failing::default())
                .violations
                .is_empty()
        );
    }

    #[test]
    fn a_restyle_is_safe_and_losing_data_or_a_button_is_not() {
        let before = json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Title", "value_expr": "$item.name" },
            button("Cooked", "/v1/notes/meals/update"),
        ]});
        // Restyled and regrouped: a card, a new style, the button moved first.
        let restyled = json!({ "type": "core://Box", "background": "${local://theme#card}", "corner_dp": 16,
            "content": { "type": "core://Row", "children": [
                button("Cooked", "/v1/notes/meals/update"),
                { "type": "core://Text", "style": "Display", "value_expr": "$item.name" } ]}});
        assert!(safe(&before, &restyled, &Failing::default()).is_empty());
        assert!(
            !additive(&before, &restyled, &Failing::default())
                .violations
                .is_empty()
        );
        // The name is gone; the button is gone; a new call.
        let no_name = json!({ "type": "core://Column", "children": [button("Cooked", "/v1/notes/meals/update")]});
        assert!(!safe(&before, &no_name, &Failing::default()).is_empty());
        let no_button =
            json!({ "type": "core://Text", "style": "Title", "value_expr": "$item.name" });
        assert!(!safe(&before, &no_button, &Failing::default()).is_empty());
        let new_call = json!({ "type": "core://Column", "children": [
            { "type": "core://Text", "style": "Title", "value_expr": "$item.name" },
            button("Cooked", "/v1/notes/meals/update"),
            button("Clear", "/v1/notes/meals/actions/purge") ]});
        assert!(!safe(&before, &new_call, &Failing::default()).is_empty());
    }

    #[test]
    fn a_resealed_reference_is_the_same_reference() {
        let before = json!({ "type": "core://Layout", "ref": { "name": "c-x", "el": "e1-a", "pin": false } });
        let after = json!({ "type": "core://Layout", "ref": { "name": "c-x", "el": "e1-b", "pin": false } });
        assert!(
            additive(&before, &after, &Failing::default())
                .violations
                .is_empty()
        );
    }
}
