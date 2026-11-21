//! Sharing elements between the pods of one server (decision `sharing`).
//!
//! A pod's objects never leave its own database except through
//! [`publish`]: run as the owner, it copies a name's closure (everything it
//! embeds, links to and uses) to the host's [`Shelf`], with each literal
//! reference to the owner's data turned into an input (the sources do that,
//! [`crate::Source::share_inputs`]) and every reference inside pinned. Other
//! pods read the shelf only through a publication's share link, and
//! [`adopt`] copies a publication into the adopter's own store, bound to the
//! adopter's data in the adopter's own reference, so the shared objects keep
//! their hashes and an owner's later revoke or delete can't break the copy.
//!
//! Everything here needs a host with a shelf; the MCP tools answer
//! `no_shelf` otherwise.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use construct_elements::refs::{Ref, Site};
use construct_elements::{Objects, object_id};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::store as cstore;
use super::write::ConnObjects;
use crate::Pod;

/// Between an input's name and a field's in a field input
/// (`$in.<input>__<field>`).
pub const FIELD_SEP: &str = "__";
/// The one visibility there is for now: unlisted, anyone on the server with
/// the share link.
pub const VISIBILITY_LINK: &str = "link";
/// Listed: waits for moderation and limits (refused for now).
pub const VISIBILITY_PUBLIC: &str = "public";
/// The prefix of a share link.
pub const LINK_PREFIX: &str = "share-";
/// History author of a name moved by an adoption.
pub const AUTHOR_ADOPT: &str = "adopt";
/// Handles nobody may take: names of the construct, and every first segment
/// of a path the host or a pod routes, since a public link's short path is
/// `/<handle>/<slug>` (`pod_host::web::short_path`).
const RESERVED_HANDLES: &[&str] = &[
    "core",
    "me",
    "admin",
    "construct",
    "server",
    "system",
    "shared",
    "public",
    "v1",
    "v2",
    "web",
    "oauth",
    "healthz",
    "ical",
    "api",
    "static",
    "assets",
];
/// The most elements one publication may carry.
const MAX_ELEMENTS: usize = 500;
/// The most bytes of elements one publication may carry: the shelf is the
/// host's, outside every pod's storage quota.
const MAX_PUBLICATION_BYTES: usize = 1 << 20;
/// The most live publications a pod may have on the shelf.
pub const MAX_PUBLICATIONS: usize = 20;

/// The field input of field `field` of data input `input`.
pub fn field_input(input: &str, field: &str) -> String {
    format!("{input}{FIELD_SEP}{field}")
}

/// One field a shared element reads or writes of a data input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FieldUse {
    pub name: String,
    /// The owner's type for it (`text`, `bool`, ...; `unknown`).
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub written: bool,
}

/// A data input of a shared element: a data set of one source the adopter
/// binds (decision `sharing`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataInput {
    /// The input's name (`$in.<name>`).
    pub name: String,
    /// The source whose data it is (`collections`).
    pub source: String,
    /// What kind of data set (`collection`).
    pub kind: String,
    pub title: String,
    /// The source's feed of the bound data set, with `$in.<name>` standing
    /// for it (what the adopter's home binds).
    pub feed: String,
    /// The fields it reads or writes; each is the field input
    /// `<name>__<field>`.
    pub fields: Vec<FieldUse>,
    /// An MCP tool call that makes an empty data set like the owner's (its
    /// definition without data; fillers and alerts left out):
    /// `{"tool", "args"}`, `args.name` set by the adopter.
    #[serde(default)]
    pub schema: Value,
}

impl DataInput {
    fn merge(&mut self, o: DataInput) {
        for f in o.fields {
            match self.fields.iter_mut().find(|x| x.name == f.name) {
                Some(x) => {
                    x.read |= f.read;
                    x.written |= f.written;
                }
                None => self.fields.push(f),
            }
        }
        self.fields.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

/// One data set an adopter has ([`crate::Source::data_sets`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataSet {
    pub source: String,
    pub kind: String,
    pub name: String,
    pub title: String,
    /// `(name, type)` of each field.
    pub fields: Vec<(String, String)>,
}

/// A publication on the shelf.
#[derive(Debug, Clone, PartialEq)]
pub struct Publication {
    /// The share link (`share-…`): unguessable, it is what grants reading.
    pub link: String,
    /// `@<handle>/<slug>`.
    pub name: String,
    pub owner_pod: String,
    /// The owner's name it publishes.
    pub source: String,
    pub visibility: String,
    /// 1 for the first version.
    pub version: i64,
    /// The copy's root element.
    pub root: String,
    /// What an adopter binds and needs ([`describe`]).
    pub interface: Value,
    /// What the owner approved: `{"flags": [...], "leave_out": [...]}`.
    pub approved: Value,
    /// New versions go out without asking while they bring no new flags.
    pub keep_shared: bool,
    /// A new version waiting for the owner's review.
    pub pending: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    pub revoked_at: Option<i64>,
}

/// A version to put on the shelf.
#[derive(Debug, Clone)]
pub struct NewVersion {
    pub owner_pod: String,
    pub name: String,
    pub source: String,
    pub visibility: String,
    pub root: String,
    /// `(id, object, features)` of every object the root reaches.
    pub objects: Vec<(String, Value, String)>,
    pub interface: Value,
    pub approved: Value,
    pub keep_shared: bool,
}

/// What the shelf refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShelfError {
    /// Another pod has the handle.
    HandleTaken,
    /// The pod's publications carry its handle.
    HandleInUse,
    /// Another live public link of the pod has the slug.
    SlugTaken,
    /// The pod already keeps the most live public links it may.
    TooManyLinks,
    Store(String),
}

impl ShelfError {
    fn answer(&self) -> Value {
        match self {
            ShelfError::HandleTaken => refuse(
                "handle_taken",
                "another pod on this server has that handle: pick another",
            ),
            ShelfError::HandleInUse => refuse(
                "handle_in_use",
                "this pod's publications carry its handle: revoke them before changing it",
            ),
            ShelfError::SlugTaken => refuse(
                "slug_taken",
                "another live public link of this pod has that slug: pick another",
            ),
            ShelfError::TooManyLinks => refuse(
                "too_many_links",
                format!(
                    "a pod keeps at most {} public links: unshare one first",
                    crate::public_copy::MAX_LIVE
                ),
            ),
            ShelfError::Store(e) => refuse("shelf_error", e.clone()),
        }
    }
}

/// The host's public store (decision `sharing`): owner handles,
/// publications and the objects of their closures. Only [`publish`] writes
/// objects to it; other pods read a publication's objects by its link.
pub trait Shelf: Send + Sync {
    /// The pod's handle, if it chose one.
    fn handle(&self, pod: &str) -> Result<Option<String>, ShelfError>;
    /// Give the pod a handle (unique on the server).
    fn set_handle(&self, pod: &str, handle: &str) -> Result<(), ShelfError>;
    /// Put a version: the next of the pod's live publication of that name,
    /// else a new publication with a new link.
    fn publish(&self, v: NewVersion) -> Result<Publication, ShelfError>;
    /// Record (or clear) a version waiting for the owner's review.
    fn set_pending(&self, link: &str, pending: Option<&Value>) -> Result<(), ShelfError>;
    /// A publication by its link, revoked ones too.
    fn by_link(&self, link: &str) -> Result<Option<Publication>, ShelfError>;
    /// A pod's own publications, revoked ones too, newest first.
    fn of_pod(&self, pod: &str) -> Result<Vec<Publication>, ShelfError>;
    /// The objects of a live publication's current version (none once
    /// revoked): `(id, object, features)`.
    fn objects(&self, link: &str) -> Result<Vec<(String, Value, String)>, ShelfError>;
    /// Revoke the pod's live publication `name` (its objects leave the
    /// shelf); the revoked publication, if there was one.
    fn revoke(&self, pod: &str, name: &str) -> Result<Option<Publication>, ShelfError>;

    /// Whether the host serves public links (`CONSTRUCT_PUBLIC_WEB=1`,
    /// decision `public-web-links`).
    fn public_links_enabled(&self) -> bool {
        false
    }
    /// Put a public copy: a new link, or the next copy of a live one (by
    /// `link`); answers it as stored. A new link is refused with
    /// [`ShelfError::TooManyLinks`] once the pod keeps
    /// [`MAX_LIVE`](crate::public_copy::MAX_LIVE) live ones, counted in the
    /// same transaction as the insert.
    fn put_public(&self, _c: PublicCopy) -> Result<PublicCopy, ShelfError> {
        Err(ShelfError::Store("public links are off".into()))
    }
    /// A pod's public copies, revoked ones too, newest first.
    fn public_of_pod(&self, _pod: &str) -> Result<Vec<PublicCopy>, ShelfError> {
        Ok(Vec::new())
    }
    /// A live public copy by its link.
    fn public_by_link(&self, _link: &str) -> Result<Option<PublicCopy>, ShelfError> {
        Ok(None)
    }
    /// A live public copy by its short path, `/<handle>/<slug>`.
    fn public_by_slug(&self, _handle: &str, _slug: &str) -> Result<Option<PublicCopy>, ShelfError> {
        Ok(None)
    }
    /// Revoke the pod's live public link: its copy leaves the host, and the
    /// pod keeps only its newest [`MAX_REVOKED`](crate::public_copy::MAX_REVOKED)
    /// revoked rows. False when there was none.
    fn revoke_public(&self, _pod: &str, _link: &str) -> Result<bool, ShelfError> {
        Ok(false)
    }

    /// Put a calendar feed's text (decision `ical-feeds`): a new token, or
    /// the next text of a live one; a revoked token stays revoked.
    fn put_calendar(&self, _c: CalendarCopy) -> Result<(), ShelfError> {
        Err(ShelfError::Store("public links are off".into()))
    }
    /// A live calendar feed by its token.
    fn calendar_by_token(&self, _token: &str) -> Result<Option<CalendarCopy>, ShelfError> {
        Ok(None)
    }
    /// Revoke the pod's live feed: its text leaves the host. False when
    /// there was none.
    fn revoke_calendar(&self, _pod: &str, _token: &str) -> Result<bool, ShelfError> {
        Ok(false)
    }
}

pub use crate::ical::CalendarCopy;
pub use crate::public_copy::PublicCopy;

fn refuse(error: &str, detail: impl Into<String>) -> Value {
    json!({ "error": error, "detail": detail.into() })
}

fn store_err(e: rusqlite::Error) -> Value {
    refuse("store_error", e.to_string())
}

/// Whether `h` is a handle a pod may choose.
pub fn valid_handle(h: &str) -> bool {
    let b = h.as_bytes();
    (2..=30).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !h.ends_with('-')
        && !RESERVED_HANDLES.contains(&h)
}

/// Whether `s` is a publication's slug.
pub fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=48).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(48).collect();
    out.trim_end_matches('-').to_owned()
}

/// A new share link.
pub fn new_link() -> String {
    let mut buf = [0u8; 20];
    getrandom::fill(&mut buf).expect("OS random number generator");
    format!(
        "{LINK_PREFIX}{}",
        construct_elements::canonical::base32(&buf)
    )
}

/// The link in what an adopter pasted (the link, or text ending with it).
fn link_arg(args: &Value) -> Result<String, Value> {
    let raw = args["link"].as_str().unwrap_or("").trim();
    let link = raw
        .rsplit(['/', ' ', ':'])
        .find(|p| !p.is_empty())
        .unwrap_or("")
        .to_owned();
    let link = if link.starts_with(LINK_PREFIX) {
        link
    } else {
        format!("{LINK_PREFIX}{link}")
    };
    if link.len() <= LINK_PREFIX.len()
        || !link.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(refuse(
            "invalid_arguments",
            "\"link\" is a share link (share-…)",
        ));
    }
    Ok(link)
}

// ---------------------------------------------------------------------------
// The copy: a name's closure as it is published.

/// One flag of a review: literal text or a URL the owner looks at before it
/// is shared.
fn flag(kind: &str, element: &str, at: &str, value: &str) -> Value {
    json!({ "kind": kind, "element": element, "at": at, "value": value })
}

/// The keys whose literal strings are text a person reads.
const TEXT_KEYS: &[&str] = &[
    "value",
    "value_expr",
    "label",
    "title",
    "text",
    "placeholder",
    "content_description",
    "subtitle",
    "message",
    "body",
    "doc",
    "note",
];

/// What `s` says once expressions (`$…`, `${…}`) are taken out.
fn literal_text(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = if let Some(inner) = after.strip_prefix('{') {
            inner.find('}').map_or(after.len(), |e| e + 2)
        } else {
            after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(after.len())
        };
        rest = &after[end..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

fn escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

/// The expressions that read data when drawn: a bound input, a list's item,
/// an argument (which may carry either), an entity field, what was typed,
/// the route.
const DATA_EXPRS: &[&str] = &[
    "$in.", "$item.", "$arg.", "$form.", "$route.", "${", "entity.",
];

/// Whether `s` reads data when drawn (decision `adopt-shows-urls`).
fn reads_data(s: &str) -> bool {
    DATA_EXPRS.iter().any(|e| s.contains(e))
}

/// The keys whose strings are URLs a device fetches.
fn url_key(k: &str) -> bool {
    matches!(k, "url" | "src" | "href" | "image") || k.ends_with("_url")
}

/// Whether `s` is one expression and nothing else (`$item.photo`): a URL
/// that is wholly the user's own value goes nowhere the element chose.
fn bare_expr(s: &str) -> bool {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix("${") {
        return inner.find('}') == Some(inner.len() - 1);
    }
    t.strip_prefix('$').is_some_and(|r| {
        !r.is_empty()
            && r.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    })
}

/// A URL that sends what it reads to whoever serves it: a fixed host
/// with an expression that reads data, or a URL put together from one.
fn data_url(key: Option<&str>, s: &str) -> bool {
    reads_data(s)
        && (s.contains("http://")
            || s.contains("https://")
            || (key.is_some_and(url_key) && !bare_expr(s)))
}

fn flags_in(v: &Value, element: &str, at: &str, key: Option<&str>, out: &mut Vec<Value>) {
    match v {
        Value::String(s) => {
            if data_url(key, s) {
                let mut f = flag("url", element, at, s);
                f["reads_data"] = json!(true);
                out.push(f);
            } else if s.contains("http://") || s.contains("https://") {
                out.push(flag("url", element, at, s));
            } else if key.is_some_and(|k| TEXT_KEYS.contains(&k)) {
                let t = literal_text(s);
                if t.chars().any(char::is_alphanumeric) {
                    out.push(flag("text", element, at, s));
                }
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                flags_in(c, element, &format!("{at}/{i}"), key, out);
            }
        }
        Value::Object(o) => {
            for (k, c) in o {
                if k == "type" || k == "ref" || k == "icon" || k == "intent" {
                    continue;
                }
                flags_in(c, element, &format!("{at}/{}", escape(k)), Some(k), out);
            }
        }
        _ => {}
    }
}

/// A literal binding to one entity of a source (`${books://b/1#title}`):
/// its pointer, if `v` has one.
fn entity_literal(v: &Value, at: &str) -> Option<(String, String)> {
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(i) = rest.find("${") {
                let after = &rest[i + 2..];
                if let Some(e) = after.find("://") {
                    let scheme = &after[..e];
                    if !scheme.is_empty()
                        && scheme
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        && !matches!(scheme, "local" | "core")
                    {
                        return Some((at.to_owned(), s.clone()));
                    }
                }
                rest = after;
            }
            None
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, c)| entity_literal(c, &format!("{at}/{i}"))),
        Value::Object(o) => o
            .iter()
            .find_map(|(k, c)| entity_literal(c, &format!("{at}/{}", escape(k)))),
        _ => None,
    }
}

/// A closure being copied for publishing.
struct Copier<'a> {
    pod: &'a Pod,
    objects: ConnObjects<'a>,
    leave_out: BTreeSet<String>,
    /// Copies by id: the object and its features.
    out: BTreeMap<String, (Value, String)>,
    inputs: BTreeMap<String, DataInput>,
    flags: Vec<Value>,
    elements: Vec<Value>,
    left_out: BTreeSet<String>,
    memo: BTreeMap<(String, String), String>,
}

/// An empty node, where a left-out embed was.
fn nothing() -> Value {
    json!({ "type": "core://Column", "children": [] })
}

impl Copier<'_> {
    /// The copy of element `id` (named `name` on the owner's pod) with
    /// `inputs` bound.
    fn copy(
        &mut self,
        id: &str,
        name: &str,
        inputs: &Map<String, Value>,
        component: bool,
    ) -> Result<String, Value> {
        let key = (
            id.to_owned(),
            construct_elements::canonical(&Value::Object(inputs.clone())),
        );
        if let Some(c) = self.memo.get(&key) {
            return Ok(c.clone());
        }
        if self.memo.len() >= MAX_ELEMENTS {
            return Err(refuse(
                "too_large",
                format!("a publication carries at most {MAX_ELEMENTS} elements"),
            ));
        }
        let Some(v) = self.objects.object(id) else {
            return Err(refuse(
                "missing_object",
                format!("'{name}' uses {id}, which isn't stored"),
            ));
        };
        let mut work = construct_elements::template::Work::default();
        let node = construct_elements::template::bind_inputs(&v["node"], inputs, &mut work)
            .map_err(|e| refuse("invalid_layout", e.to_string()))?;
        let mut node = self.refs(&node, name)?.unwrap_or_else(nothing);
        // The sources turn literal references to their data into inputs.
        let mut made: Vec<DataInput> = Vec::new();
        for a in self.pod.catalog().adapters() {
            let more = a.share_inputs(self.objects.conn, &mut node).map_err(|e| {
                json!({ "error": "literal_data_reference", "element": name,
                        "at": e.pointer, "detail": e.message })
            })?;
            made.extend(more);
        }
        if component && let Some(d) = made.first() {
            return Err(json!({ "error": "literal_data_reference", "element": name,
                "detail": format!("component '{name}' names the {} '{}' itself: pass it in an \
                    argument from the layout that uses it, so the copy can bind it", d.kind, d.name) }));
        }
        if let Some((at, s)) = entity_literal(&node, "/node") {
            return Err(
                json!({ "error": "literal_data_reference", "element": name, "at": at,
                "detail": format!("{s:?} binds one entity of your data: a shared copy can't \
                    carry it (bind a list's item instead, or leave this element out)") }),
            );
        }
        let mut el = (*v).clone();
        if let Some(o) = el.as_object_mut() {
            o.remove("bind");
            o.remove("needs");
        }
        el["node"] = node;
        let mut flags = Vec::new();
        flags_in(&el, name, "", None, &mut flags);
        self.flags.extend(flags);
        for d in made {
            match self.inputs.get_mut(&d.name) {
                Some(x) => x.merge(d),
                None => {
                    self.inputs.insert(d.name.clone(), d);
                }
            }
        }
        // Its needs, as the adopter's pod derives them (no feeds of its own:
        // its data is its inputs).
        let probe = object_id(&el);
        self.objects
            .extra
            .borrow_mut()
            .insert(probe.clone(), Arc::new(el.clone()));
        let (needs, features) =
            super::core::derive(self.pod.catalog(), &[], &self.objects, &probe, !component);
        el["needs"] = needs;
        let cid = object_id(&el);
        self.objects
            .extra
            .borrow_mut()
            .insert(cid.clone(), Arc::new(el.clone()));
        self.elements
            .push(json!({ "name": name, "el": cid, "from": id,
            "title": el["title"].clone() }));
        self.out.insert(cid.clone(), (el, features));
        self.memo.insert(key, cid.clone());
        Ok(cid)
    }

    /// `v` with every reference copied and pinned; `None`: `v` goes (a
    /// left-out embed, or a node whose intent opened a left-out element).
    fn refs(&mut self, v: &Value, owner: &str) -> Result<Option<Value>, Value> {
        match v {
            Value::Array(a) => {
                let mut out = Vec::new();
                for c in a {
                    if let Some(c) = self.refs(c, owner)? {
                        out.push(c);
                    }
                }
                Ok(Some(Value::Array(out)))
            }
            Value::Object(o) => {
                let site = construct_elements::refs::site_of(o);
                let r = o.get("ref").and_then(Ref::parse);
                let mut out = Map::new();
                if let (Some(site), Some(r)) = (site, r) {
                    if site != Site::Use && self.leave_out.contains(&r.name) {
                        self.left_out.insert(r.name.clone());
                        return Ok(None);
                    }
                    let Some(el) = r.el.clone() else {
                        return Err(refuse(
                            "unsealed_reference",
                            format!(
                                "'{owner}' has a reference to '{}' that isn't sealed",
                                r.name
                            ),
                        ));
                    };
                    let bind: Map<String, Value> = o
                        .get(construct_elements::refs::BIND_FIELD)
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let child = self.copy(&el, &r.name, &bind, site == Site::Use)?;
                    let reads = self
                        .out
                        .get(&child)
                        .map(|(c, _)| construct_elements::template::inputs_of(c))
                        .unwrap_or_default();
                    let pinned = Ref {
                        name: r.name.clone(),
                        el: Some(child),
                        pin: true,
                        as_: r.as_.clone(),
                    };
                    out.insert("ref".into(), pinned.to_value());
                    if site != Site::Use && !reads.is_empty() {
                        let b: Map<String, Value> = reads
                            .into_iter()
                            .map(|i| (i.clone(), json!(format!("$in.{i}"))))
                            .collect();
                        out.insert(
                            construct_elements::refs::BIND_FIELD.into(),
                            Value::Object(b),
                        );
                    }
                }
                for (k, c) in o {
                    if out.contains_key(k)
                        || (k == construct_elements::refs::BIND_FIELD && site.is_some())
                    {
                        continue;
                    }
                    match self.refs(c, owner)? {
                        Some(c) => {
                            out.insert(k.clone(), c);
                        }
                        // The intent opened a left-out element: its node goes.
                        None if c.get("intent").is_some() => return Ok(None),
                        None => {
                            out.insert(k.clone(), nothing());
                        }
                    }
                }
                Ok(Some(Value::Object(out)))
            }
            other => Ok(Some(other.clone())),
        }
    }
}

/// A name's copy for publishing and what the owner reviews.
pub struct Prepared {
    pub root: String,
    pub objects: Vec<(String, Value, String)>,
    pub interface: Value,
    pub flags: Vec<Value>,
    pub elements: Vec<Value>,
    pub left_out: Vec<String>,
}

/// The inputs home binds for `name` at the first use site that reaches it.
fn owner_binding(conn: &Connection, objects: &dyn Objects, name: &str) -> Map<String, Value> {
    let Ok(Some(home)) = cstore::name(conn, crate::HOME_APP) else {
        return Map::new();
    };
    let keyed = construct_elements::keyed(objects, crate::HOME_APP, &home);
    if let Some(k) = keyed.get(name) {
        return k.inputs.clone();
    }
    let Ok(Some(el)) = cstore::name(conn, name) else {
        return Map::new();
    };
    keyed
        .into_values()
        .find(|k| k.el == el)
        .map(|k| k.inputs)
        .unwrap_or_default()
}

/// Copy `name`'s closure for publishing (see the module docs).
pub fn prepare(
    pod: &Pod,
    conn: &Connection,
    name: &str,
    leave_out: &[String],
    notes: Option<&str>,
) -> Result<Prepared, Value> {
    let Some(el) = cstore::name(conn, name).map_err(store_err)? else {
        return Err(refuse(
            "unknown_name",
            format!("nothing is named '{name}' (list_layouts and list_components list the names)"),
        ));
    };
    if name.starts_with(super::CORE_PREFIX) {
        return Err(refuse(
            "not_yours",
            "what ships with the server is on every pod already: publish your own layout or component",
        ));
    }
    let component = name.starts_with(super::ME_PREFIX);
    let mut c = Copier {
        pod,
        objects: ConnObjects::new(conn),
        leave_out: leave_out.iter().cloned().collect(),
        out: BTreeMap::new(),
        inputs: BTreeMap::new(),
        flags: Vec::new(),
        elements: Vec::new(),
        left_out: BTreeSet::new(),
        memo: BTreeMap::new(),
    };
    // The root's own bindings become its interface: its data is bound
    // literally, then the sources turn it into inputs.
    let bound = owner_binding(conn, &c.objects, name);
    let root = c.copy(&el, name, &bound, component)?;
    let Some((root_el, _)) = c.out.get(&root).cloned() else {
        return Err(refuse("internal", "the copy has no root"));
    };
    let reads = construct_elements::template::inputs_of(&root_el);
    let mut inputs: Vec<Value> = Vec::new();
    let mut described: BTreeSet<String> = BTreeSet::new();
    for d in c.inputs.values() {
        if reads.contains(&d.name) {
            described.insert(d.name.clone());
            for f in &d.fields {
                described.insert(field_input(&d.name, &f.name));
            }
            inputs.push(serde_json::to_value(d).unwrap_or(Value::Null));
        }
    }
    for r in &reads {
        if !described.contains(r) {
            inputs.push(json!({ "name": r, "kind": "value",
                "title": "A value the use site passes in" }));
        }
    }
    let interface = json!({
        "kind": if component { "component" } else { "layout" },
        "title": root_el["title"].clone(),
        "icon": root_el["icon"].clone(),
        "inputs": inputs,
        "params": root_el["params"].clone(),
        "needs": root_el["needs"].clone(),
        "elements": c.out.len(),
        "left_out": c.left_out.iter().collect::<Vec<_>>(),
        "notes": notes.unwrap_or(""),
    });
    let mut objects: Vec<(String, Value, String)> = c
        .out
        .iter()
        .map(|(id, (v, f))| (id.clone(), v.clone(), f.clone()))
        .collect();
    // The root first.
    objects.sort_by_key(|(id, _, _)| id != &root);
    let bytes: usize = objects
        .iter()
        .map(|(_, v, _)| construct_elements::canonical(v).len())
        .sum();
    if bytes > MAX_PUBLICATION_BYTES {
        return Err(refuse(
            "too_large",
            format!(
                "a publication carries at most {} KiB of elements (this one is {} KiB): \
                 leave some out",
                MAX_PUBLICATION_BYTES >> 10,
                bytes.div_ceil(1024)
            ),
        ));
    }
    let mut flags = c.flags;
    flags.dedup();
    Ok(Prepared {
        root,
        objects,
        interface,
        flags,
        elements: c.elements,
        left_out: c.left_out.into_iter().collect(),
    })
}

/// The review id of a prepared copy: what `confirm` repeats.
fn review_id(name: &str, p: &Prepared, visibility: &str) -> String {
    construct_elements::canonical::id_of_text(&format!("{name}\n{visibility}\n{}", p.root))
        .replacen("e1-", "r-", 1)
}

/// Whether every flag of `new` is among `approved` (by kind and value).
fn flags_approved(new: &[Value], approved: &Value) -> bool {
    let ok: BTreeSet<(String, String)> = approved["flags"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            (
                f["kind"].as_str().unwrap_or("").to_owned(),
                f["value"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    new.iter().all(|f| {
        ok.contains(&(
            f["kind"].as_str().unwrap_or("").to_owned(),
            f["value"].as_str().unwrap_or("").to_owned(),
        ))
    })
}

// ---------------------------------------------------------------------------
// The adopter's side: what it adopted, in its own store.

fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS adoptions (
             name TEXT PRIMARY KEY,
             link TEXT NOT NULL,
             publication TEXT NOT NULL,
             version INTEGER NOT NULL,
             mode TEXT NOT NULL,
             bind TEXT NOT NULL,
             adopted_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );",
    )?;
    // Added for decision `adopt-shows-urls`: what the user accepted.
    let has: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('adoptions') WHERE name = 'accepted'")?
        .exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE adoptions ADD COLUMN accepted TEXT NOT NULL DEFAULT '[]'")?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Adoption {
    name: String,
    link: String,
    publication: String,
    version: i64,
    mode: String,
    bind: Map<String, Value>,
    adopted_at: i64,
    /// The URLs that send what they read which the user accepted
    /// (`accept_data_urls`): a new version bringing others waits for them.
    accepted: BTreeSet<String>,
}

fn adoption(conn: &Connection, name: &str) -> rusqlite::Result<Option<Adoption>> {
    ensure_tables(conn)?;
    conn.query_row(
        "SELECT name, link, publication, version, mode, bind, adopted_at, accepted FROM adoptions \
         WHERE name = ?1",
        [name],
        |r| {
            let bind: String = r.get(5)?;
            let accepted: String = r.get(7)?;
            Ok(Adoption {
                name: r.get(0)?,
                link: r.get(1)?,
                publication: r.get(2)?,
                version: r.get(3)?,
                mode: r.get(4)?,
                bind: serde_json::from_str(&bind).unwrap_or_default(),
                adopted_at: r.get(6)?,
                accepted: serde_json::from_str(&accepted).unwrap_or_default(),
            })
        },
    )
    .optional()
}

fn adoptions(conn: &Connection) -> rusqlite::Result<Vec<Adoption>> {
    ensure_tables(conn)?;
    let names: Vec<String> = {
        let mut stmt = conn.prepare("SELECT name FROM adoptions ORDER BY name")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for n in names {
        if let Some(a) = adoption(conn, &n)? {
            out.push(a);
        }
    }
    Ok(out)
}

fn save_adoption(conn: &Connection, a: &Adoption) -> rusqlite::Result<()> {
    ensure_tables(conn)?;
    let now = crate::util::now_secs();
    conn.execute(
        "INSERT INTO adoptions (name, link, publication, version, mode, bind, adopted_at, updated_at,
             accepted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(name) DO UPDATE SET link = ?2, publication = ?3, version = ?4, mode = ?5,
             bind = ?6, updated_at = ?8, accepted = ?9",
        params![
            a.name,
            a.link,
            a.publication,
            a.version,
            a.mode,
            Value::Object(a.bind.clone()).to_string(),
            a.adopted_at,
            now,
            json!(a.accepted).to_string()
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The MCP tools.

fn spec(name: &str, title: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required },
    })
}

/// The sharing tools' descriptions.
pub fn tool_specs() -> Vec<Value> {
    let link =
        json!({ "type": "string", "description": "A share link (share-…) as the owner gave it" });
    let bindings = json!({ "type": "object", "description": "Per input of the publication's interface: \
        a data input binds {<its kind>: <one of the user's data sets of that kind>, \"fields\": {<shared field>: <the user's field>}} \
        (fields left out keep their names) or {\"new\": <name>} (an empty one made from the suggested schema); \
        a value input binds its value" });
    let accept_data_urls = json!({ "type": "array", "items": { "type": "string" },
        "description": "URLs (exactly as the sends_data refusal lists them) that the user agreed \
        may send their data to another server when drawn" });
    vec![
        spec(
            "set_handle",
            "Choose or show the pod's handle",
            "The pod's owner handle on this server (@<handle>): what its publications are named \
             under (@<handle>/<slug>). Without \"handle\" it shows the current one. A handle is \
             unique on the server (2-30 of a-z, 0-9 and -, starting with a letter); it can't change \
             while publications carry it. Only set it when the user chooses one.",
            json!({ "handle": { "type": "string" } }),
            &[],
        ),
        spec(
            "publish",
            "Share a layout or component",
            "Share one of the pod's layouts (c-… or home: the whole construct) or components (@me/…) \
             with other users on this server, unlisted: anyone with the share link can adopt it. \
             The pod copies everything it embeds, links to and uses, turns each reference to the \
             user's data into an input the adopter binds (or refuses at that pointer), and pins \
             everything inside. Without \"confirm\" it changes nothing and answers the review: \
             every element, the derived interface, and each literal text or URL that would be \
             shared (personal words, a private URL). Show the user the review; they may leave an \
             element out (leave_out: its embed or link goes). Then call again with confirm: \
             <review_id>. keep_shared: true publishes later versions without asking while they \
             bring no new text or URLs. Revoking stops new versions; adopted copies keep drawing \
             (tell the user so up front). Needs a handle (set_handle).",
            json!({
                "name": { "type": "string", "description": "A layout id (home, c-…) or @me/<component>" },
                "slug": { "type": "string", "description": "The publication's name under the handle (a-z, 0-9, -); default from the title" },
                "visibility": { "type": "string", "enum": [VISIBILITY_LINK], "description": "link (the default): unlisted, anyone with the share link" },
                "leave_out": { "type": "array", "items": { "type": "string" }, "description": "Names of elements in the closure to leave out" },
                "keep_shared": { "type": "boolean" },
                "notes": { "type": "string", "description": "A note for adopters (shown with the interface)" },
                "confirm": { "type": "string", "description": "The review_id of the review the user approved" },
            }),
            &["name"],
        ),
        spec(
            "list_publications",
            "List the pod's publications",
            "The pod's publications: each one's name, share link, version, whether it is revoked, \
             and a new version waiting for the user's review.",
            json!({}),
            &[],
        ),
        spec(
            "describe_publication",
            "Describe a shared element",
            "What a share link holds, before adopting it: its name, version, title and interface \
             (each input with the fields it reads and writes and their types, a suggested schema \
             per data input, the sources, jacks, endpoints and params it needs). Nothing is copied.",
            json!({ "link": link }),
            &["link"],
        ),
        spec(
            "adopt",
            "Adopt a shared element",
            "Copy a publication into this pod, bound to the user's own data. Without full bindings \
             (or with dry_run: true) it changes nothing and answers a proposal per input (an \
             existing data set that fits, a field mapping, or a new empty one) and what \
             is missing; confirm the bindings with the user, then call again. Adopting stores the \
             snapshot under its published name and answers a reference (\"use\", \"open\") with \
             the bindings: put it in home (update_layout) and look at it with \
             render_layout_preview. mode: follow (the default: update_adoption brings later \
             versions while they fit the bindings), pin (this version only) or fork (a layout of \
             the user's own to change, no updates). Every answer lists flags: each literal text \
             and URL the publication carries; show them to the user. A URL that puts the user's \
             data into a request (reads_data: an image URL with $item.… or $in.…) is refused \
             (sends_data) unless the user agrees and you pass it in accept_data_urls.",
            json!({
                "link": link,
                "bindings": bindings,
                "mode": { "type": "string", "enum": ["follow", "pin", "fork"] },
                "dry_run": { "type": "boolean" },
                "accept_data_urls": accept_data_urls.clone(),
            }),
            &["link"],
        ),
        spec(
            "list_adoptions",
            "List what the pod adopted",
            "Each publication this pod adopted: its name here, the version it draws, the newest \
             version, and whether the owner revoked it (a revoked one keeps drawing).",
            json!({}),
            &[],
        ),
        spec(
            "update_adoption",
            "Bring an adoption to its newest version",
            "Move a followed adoption to the publication's newest version, when that version's \
             interface still fits the bindings. A new input or field is answered as what to bind \
             (pass it in bindings, and add it to the bind of each reference that uses it). A \
             version bringing a URL that puts the user's data into a request, which they haven't \
             accepted, is refused (sends_data) until they agree and you pass it in \
             accept_data_urls. Answers the version's flags (its literal text and URLs).",
            json!({ "name": { "type": "string", "description": "The adoption's name (@handle/slug)" },
                    "bindings": bindings,
                    "accept_data_urls": accept_data_urls }),
            &["name"],
        ),
        spec(
            "revoke_publication",
            "Stop sharing",
            "Revoke one of the pod's publications: its link stops working and no new version goes \
             out. Pods that adopted it keep the copy they have.",
            json!({ "name": { "type": "string", "description": "The publication's name (@handle/slug) or its link" } }),
            &["name"],
        ),
    ]
}

/// The sharing tools; `None`: not one of them.
pub async fn tool(pod: &Pod, name: &str, args: &Value) -> Option<Result<Value, Value>> {
    if !tool_specs().iter().any(|t| t["name"] == name) {
        return None;
    }
    if pod.shelf().is_none() {
        return Some(Err(refuse(
            "no_shelf",
            "this server runs the pod without a host, so there is nothing to share with",
        )));
    }
    Some(match name {
        "set_handle" => blocking(pod, args, set_handle).await,
        "publish" => publish(pod, args).await,
        "list_publications" => blocking(pod, args, list_publications).await,
        "describe_publication" => blocking(pod, args, describe_publication).await,
        "adopt" => adopt(pod, args).await,
        "list_adoptions" => blocking(pod, args, list_adoptions).await,
        "update_adoption" => update_adoption(pod, args).await,
        "revoke_publication" => blocking(pod, args, revoke_publication).await,
        _ => return None,
    })
}

type Tool = fn(&Pod, &Value) -> Result<Value, Value>;

async fn blocking(pod: &Pod, args: &Value, f: Tool) -> Result<Value, Value> {
    let (p, a) = (pod.clone(), args.clone());
    tokio::task::spawn_blocking(move || f(&p, &a))
        .await
        .unwrap_or_else(|e| Err(refuse("internal", e.to_string())))
}

fn shelf(pod: &Pod) -> Result<(String, Arc<dyn Shelf>), Value> {
    pod.shelf()
        .map(|(id, s)| (id.to_owned(), s.clone()))
        .ok_or_else(|| refuse("no_shelf", "this pod has no host to share through"))
}

fn set_handle(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let (me, shelf) = shelf(pod)?;
    let Some(h) = args["handle"]
        .as_str()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    else {
        let h = shelf.handle(&me).map_err(|e| e.answer())?;
        return Ok(json!({ "handle": h.map(|h| format!("@{h}")) }));
    };
    let h = h.trim_start_matches('@').to_ascii_lowercase();
    if !valid_handle(&h) {
        return Err(refuse(
            "invalid_handle",
            format!(
                "a handle is 2-30 of a-z, 0-9 and -, starting with a letter, and not one of {}",
                RESERVED_HANDLES.join(", ")
            ),
        ));
    }
    shelf.set_handle(&me, &h).map_err(|e| e.answer())?;
    Ok(json!({ "ok": true, "handle": format!("@{h}") }))
}

fn publication_row(p: &Publication, owner: bool) -> Value {
    let mut v = json!({
        "name": p.name,
        "link": p.link,
        "version": p.version,
        "visibility": p.visibility,
        "revoked": p.revoked_at.is_some(),
        "updated_at": p.updated_at,
    });
    if owner {
        v["publishes"] = json!(p.source);
        v["keep_shared"] = json!(p.keep_shared);
        if let Some(pending) = &p.pending {
            v["pending_review"] = pending.clone();
        }
    }
    v
}

fn list_publications(pod: &Pod, _: &Value) -> Result<Value, Value> {
    let (me, shelf) = shelf(pod)?;
    let rows = shelf.of_pod(&me).map_err(|e| e.answer())?;
    Ok(json!({ "publications": rows.iter().map(|p| publication_row(p, true)).collect::<Vec<_>>() }))
}

fn live_by_link(shelf: &dyn Shelf, link: &str) -> Result<Publication, Value> {
    match shelf.by_link(link).map_err(|e| e.answer())? {
        Some(p) if p.revoked_at.is_some() => Err(refuse(
            "revoked",
            format!("{} is no longer shared (its owner revoked it)", p.name),
        )),
        Some(p) => Ok(p),
        None => Err(refuse("unknown_link", "no publication has that share link")),
    }
}

fn describe_publication(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let (me, shelf) = shelf(pod)?;
    let link = link_arg(args)?;
    let p = live_by_link(shelf.as_ref(), &link)?;
    let mine = p.owner_pod == me;
    let mut out = publication_row(&p, mine);
    // Another pod's interface is its publisher's text (issue #439).
    out["interface"] = if mine {
        p.interface.clone()
    } else {
        crate::mcp::untrusted(&p.name, p.interface.clone())
    };
    Ok(out)
}

fn revoke_publication(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let (me, shelf) = shelf(pod)?;
    let raw = args["name"].as_str().unwrap_or("").trim();
    let name = if raw.starts_with('@') {
        raw.to_owned()
    } else {
        let link = link_arg(&json!({ "link": raw }))?;
        match shelf.by_link(&link).map_err(|e| e.answer())? {
            Some(p) if p.owner_pod == me => p.name,
            _ => {
                return Err(refuse(
                    "unknown_publication",
                    "this pod has no such publication",
                ));
            }
        }
    };
    match shelf.revoke(&me, &name).map_err(|e| e.answer())? {
        Some(p) => Ok(json!({ "ok": true, "name": p.name, "revoked": true,
            "detail": "The link no longer works and no new version goes out; pods that adopted it keep their copy." })),
        None => Err(refuse(
            "unknown_publication",
            format!("this pod has no live publication named '{name}' (list_publications)"),
        )),
    }
}

/// `publish`: the review, or with `confirm` the publication.
async fn publish(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let (p, a) = (pod.clone(), args.clone());
    tokio::task::spawn_blocking(move || publish_blocking(&p, &a))
        .await
        .unwrap_or_else(|e| Err(refuse("internal", e.to_string())))
}

fn publish_blocking(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let (me, shelf) = shelf(pod)?;
    let Some(handle) = shelf.handle(&me).map_err(|e| e.answer())? else {
        return Err(refuse(
            "no_handle",
            "this pod has no handle yet: ask the user to choose one (set_handle)",
        ));
    };
    let visibility = args["visibility"].as_str().unwrap_or(VISIBILITY_LINK);
    match visibility {
        VISIBILITY_LINK => {}
        VISIBILITY_PUBLIC => {
            return Err(refuse(
                "visibility_not_available",
                "listed (public) sharing waits for moderation and limits; share by link \
                 (visibility: link) for now",
            ));
        }
        other => {
            return Err(refuse(
                "invalid_arguments",
                format!("visibility is \"link\", not {other:?}"),
            ));
        }
    }
    let name = args["name"]
        .as_str()
        .or_else(|| args["app"].as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if name.is_empty() {
        return Err(refuse("invalid_arguments", "\"name\" is required"));
    }
    let leave_out: Vec<String> = args["leave_out"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let notes = args["notes"]
        .as_str()
        .map(|n| n.chars().take(2000).collect::<String>());
    let prepared = pod
        .store()
        .with_conn(|c| {
            Ok::<_, rusqlite::Error>(prepare(pod, c, &name, &leave_out, notes.as_deref()))
        })
        .map_err(store_err)??;
    let title = prepared.interface["title"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let slug = match args["slug"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) if valid_slug(s) => s.to_owned(),
        Some(_) => {
            return Err(refuse(
                "invalid_slug",
                "a slug is 1-48 of a-z, 0-9 and -, starting with a letter or digit",
            ));
        }
        None => {
            let from = if title.is_empty() {
                name.trim_start_matches(super::ME_PREFIX).to_owned()
            } else {
                title.clone()
            };
            let s = slugify(&from);
            if s.is_empty() { "shared".to_owned() } else { s }
        }
    };
    let full = format!("@{handle}/{slug}");
    // A name already published from another of the pod's names is refused.
    let mine = shelf.of_pod(&me).map_err(|e| e.answer())?;
    if let Some(other) = mine
        .iter()
        .find(|p| p.name == full && p.revoked_at.is_none() && p.source != name)
    {
        return Err(refuse(
            "slug_taken",
            format!(
                "{full} already publishes '{}': pick another slug",
                other.source
            ),
        ));
    }
    let live = mine.iter().filter(|p| p.revoked_at.is_none());
    if !mine
        .iter()
        .any(|p| p.name == full && p.revoked_at.is_none())
        && live.count() >= MAX_PUBLICATIONS
    {
        return Err(refuse(
            "too_many_publications",
            format!(
                "a pod keeps at most {MAX_PUBLICATIONS} live publications: revoke one first \
                 (revoke_publication)"
            ),
        ));
    }
    let review = review_id(&full, &prepared, visibility);
    let confirmed = args["confirm"].as_str().is_some_and(|c| c == review);
    if !confirmed {
        let stale = args["confirm"].as_str().is_some();
        return Ok(json!({
            "published": false,
            "review_id": review,
            "name": full,
            "visibility": visibility,
            "detail": if stale {
                "What would be shared changed since that review: show the user this one and confirm it."
            } else {
                "Nothing is shared yet. Show the user every element, the interface and each flag \
                 (literal text and URLs that would be shared), and say that revoking later stops \
                 new versions but adopted copies keep drawing. Then call publish again with \
                 confirm: <review_id> (and leave_out for elements to keep private)."
            },
            "elements": prepared.elements,
            "left_out": prepared.left_out,
            "flags": prepared.flags,
            "interface": prepared.interface,
        }));
    }
    let keep_shared = args["keep_shared"].as_bool().unwrap_or(false);
    let p = shelf
        .publish(NewVersion {
            owner_pod: me,
            name: full.clone(),
            source: name,
            visibility: visibility.to_owned(),
            root: prepared.root.clone(),
            objects: prepared.objects,
            interface: prepared.interface,
            approved: json!({ "flags": prepared.flags, "leave_out": leave_out }),
            keep_shared,
        })
        .map_err(|e| e.answer())?;
    Ok(json!({
        "published": true,
        "name": p.name,
        "link": p.link,
        "version": p.version,
        "keep_shared": p.keep_shared,
        "detail": "Give the user the share link: anyone on this server they send it to can adopt it.",
    }))
}

/// After a write on the owner's pod: publications kept shared get the new
/// version of what they publish, unless it brings new text or URLs (then it
/// waits for review). Blocking; failures are logged.
pub fn after_write(pod: &Pod, moved: &[(String, i64)]) {
    let Some((me, shelf)) = pod.shelf().map(|(i, s)| (i.to_owned(), s.clone())) else {
        return;
    };
    if moved.is_empty() {
        return;
    }
    let Ok(mine) = shelf.of_pod(&me) else {
        return;
    };
    for p in mine.into_iter().filter(|p| {
        p.revoked_at.is_none() && p.keep_shared && moved.iter().any(|(n, _)| *n == p.source)
    }) {
        let leave_out: Vec<String> = p.approved["leave_out"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let notes = p.interface["notes"].as_str().map(str::to_owned);
        let r = pod.store().with_conn(|c| {
            Ok::<_, rusqlite::Error>(prepare(pod, c, &p.source, &leave_out, notes.as_deref()))
        });
        let outcome = match r {
            Ok(Ok(prep)) if prep.root == p.root => continue,
            Ok(Ok(prep)) if flags_approved(&prep.flags, &p.approved) => {
                let mut approved = p.approved.clone();
                approved["flags"] = json!(prep.flags);
                shelf
                    .publish(NewVersion {
                        owner_pod: me.clone(),
                        name: p.name.clone(),
                        source: p.source.clone(),
                        visibility: p.visibility.clone(),
                        root: prep.root,
                        objects: prep.objects,
                        interface: prep.interface,
                        approved,
                        keep_shared: true,
                    })
                    .map(|_| ())
            }
            Ok(Ok(prep)) => shelf.set_pending(
                &p.link,
                Some(
                    &json!({ "reason": "new text or URLs to review: call publish for it",
                    "flags": prep.flags }),
                ),
            ),
            Ok(Err(e)) => shelf.set_pending(
                &p.link,
                Some(&json!({ "reason": "the new version can't be shared as it is", "error": e })),
            ),
            Err(e) => Err(ShelfError::Store(e.to_string())),
        };
        if let Err(e) = outcome {
            tracing::warn!(publication = %p.name, error = ?e, "keeping a publication shared failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Adopting.

/// A publication fetched for adopting: it and its objects, checked.
struct Fetched {
    publication: Publication,
    objects: Vec<(String, Value, String)>,
    inputs: Vec<Value>,
    /// Every literal text and URL its elements carry, as `publish` showed
    /// the owner: what the adopter's review shows.
    flags: Vec<Value>,
}

impl Fetched {
    /// The URLs that send what they read (`reads_data`), each once.
    fn data_urls(&self) -> BTreeSet<String> {
        self.flags
            .iter()
            .filter(|f| f["reads_data"] == true)
            .filter_map(|f| f["value"].as_str().map(str::to_owned))
            .collect()
    }
}

/// The URLs the caller accepts (`accept_data_urls`).
fn accepted_arg(args: &Value) -> BTreeSet<String> {
    args["accept_data_urls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// The refusal of URLs that read the adopter's data and weren't accepted.
fn data_urls_refusal(name: &str, unaccepted: &BTreeSet<String>) -> Value {
    json!({
        "error": "sends_data",
        "detail": format!("{name} has URLs that put the user's own data (a bound input, a row's \
            fields) into a request to another server each time it draws. Nothing was adopted: show \
            the user each one and, only if they agree, call again with accept_data_urls listing \
            them exactly."),
        "data_urls": unaccepted,
    })
}

fn fetch(shelf: &dyn Shelf, me: &str, link: &str) -> Result<Fetched, Value> {
    let p = live_by_link(shelf, link)?;
    if p.owner_pod == me {
        return Err(refuse(
            "own_publication",
            format!("{} is this pod's own: use '{}' directly", p.name, p.source),
        ));
    }
    let objects = shelf.objects(link).map_err(|e| e.answer())?;
    for (id, v, _) in &objects {
        if object_id(v) != *id {
            return Err(refuse(
                "corrupt_publication",
                format!("object {id} doesn't match its hash"),
            ));
        }
    }
    if !objects.iter().any(|(id, _, _)| *id == p.root) {
        return Err(refuse(
            "corrupt_publication",
            "the publication's root is missing",
        ));
    }
    let inputs = p.interface["inputs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut flags = Vec::new();
    for (id, v, _) in &objects {
        flags_in(v, id, "", None, &mut flags);
    }
    Ok(Fetched {
        publication: p,
        objects,
        inputs,
        flags,
    })
}

/// What `bindings` make of the publication's inputs: the bind map, data
/// sets to create first, problems, and a proposal for each unbound input.
struct Plan {
    bind: Map<String, Value>,
    create: Vec<(String, Value)>,
    feeds: Vec<(String, String)>,
    problems: Vec<Value>,
    proposed: Map<String, Value>,
}

fn propose(input: &DataInput, sets: &[DataSet]) -> Value {
    // The closest shape: fields of the same name, then of the same type,
    // and the fewest fields the input doesn't use.
    let mut best: Option<(i64, &DataSet)> = None;
    for s in sets
        .iter()
        .filter(|s| s.source == input.source && s.kind == input.kind)
    {
        let named = input
            .fields
            .iter()
            .filter(|f| s.fields.iter().any(|(n, _)| *n == f.name))
            .count() as i64;
        let typed = input
            .fields
            .iter()
            .filter(|f| {
                !s.fields.iter().any(|(n, _)| *n == f.name)
                    && s.fields.iter().any(|(_, t)| *t == f.kind)
            })
            .count() as i64;
        let unused = (s.fields.len() as i64 - named - typed).max(0);
        let score = 10 * (2 * named + typed) - unused + i64::from(s.name == input.name) * 1000;
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, s));
        }
    }
    match best {
        Some((_, s)) => {
            let missing: Vec<&str> = input
                .fields
                .iter()
                .filter(|f| !s.fields.iter().any(|(n, _)| *n == f.name))
                .map(|f| f.name.as_str())
                .collect();
            let mut fields = Map::new();
            for m in &missing {
                // The same type, if there is exactly one unused field of it.
                let want = input
                    .fields
                    .iter()
                    .find(|f| f.name == *m)
                    .map(|f| f.kind.clone());
                let cands: Vec<&String> = s
                    .fields
                    .iter()
                    .filter(|(n, t)| {
                        Some(t) == want.as_ref() && !input.fields.iter().any(|f| f.name == *n)
                    })
                    .map(|(n, _)| n)
                    .collect();
                if cands.len() == 1 {
                    fields.insert((*m).to_owned(), json!(cands[0]));
                }
            }
            json!({ input.kind.clone(): s.name, "fields": fields,
                    "or": { "new": input.name } })
        }
        None => json!({ "new": input.name }),
    }
}

fn plan(pod: &Pod, conn: &Connection, inputs: &[Value], bindings: &Map<String, Value>) -> Plan {
    let mut sets: Vec<DataSet> = Vec::new();
    for a in pod.catalog().adapters() {
        sets.extend(a.data_sets(conn));
    }
    let mut out = Plan {
        bind: Map::new(),
        create: Vec::new(),
        feeds: Vec::new(),
        problems: Vec::new(),
        proposed: Map::new(),
    };
    for raw in inputs {
        let name = raw["name"].as_str().unwrap_or("").to_owned();
        let Ok(input) = serde_json::from_value::<DataInput>(raw.clone()) else {
            // A value input.
            match bindings.get(&name) {
                Some(v) => {
                    out.bind.insert(name, v.clone());
                }
                None => {
                    out.problems
                        .push(json!({ "input": name, "problem": "unbound",
                        "detail": "a value input: pass its value in bindings" }));
                }
            }
            continue;
        };
        let Some(b) = bindings.get(&name) else {
            out.proposed.insert(name.clone(), propose(&input, &sets));
            out.problems
                .push(json!({ "input": name, "problem": "unbound",
                "detail": format!("bind the {} input '{name}' (see proposed)", input.kind) }));
            continue;
        };
        let (target, mapping, new) = match b {
            Value::String(s) => (s.clone(), Map::new(), false),
            Value::Object(o) => {
                if let Some(n) = o.get("new").and_then(Value::as_str) {
                    (n.to_owned(), Map::new(), true)
                } else {
                    let t = o
                        .get(&input.kind)
                        .or_else(|| o.get("data"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let m = o
                        .get("fields")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    (t, m, false)
                }
            }
            _ => (String::new(), Map::new(), false),
        };
        if target.is_empty() {
            out.problems.push(json!({ "input": name, "problem": "invalid_binding",
                "detail": format!("bind {{\"{}\": <name>, \"fields\": {{...}}}} or {{\"new\": <name>}}", input.kind) }));
            continue;
        }
        let set = sets
            .iter()
            .find(|s| s.source == input.source && s.kind == input.kind && s.name == target);
        if new {
            if set.is_none() {
                let mut args = input.schema["args"].clone();
                if !args.is_object() || input.schema["tool"].as_str().is_none() {
                    out.problems.push(json!({ "input": name, "problem": "no_schema",
                        "detail": "the publication suggests no schema: bind an existing data set" }));
                    continue;
                }
                args["name"] = json!(target);
                out.create
                    .push((input.schema["tool"].as_str().unwrap_or("").to_owned(), args));
            }
        } else if set.is_none() {
            out.proposed.insert(name.clone(), propose(&input, &sets));
            out.problems
                .push(json!({ "input": name, "problem": "no_such_data",
                "detail": format!("there is no {} '{target}' on this pod", input.kind) }));
            continue;
        }
        out.bind.insert(name.clone(), json!(target));
        for f in &input.fields {
            let to = mapping
                .get(&f.name)
                .and_then(Value::as_str)
                .unwrap_or(&f.name)
                .to_owned();
            if !new && let Some(s) = set {
                match s.fields.iter().find(|(n, _)| *n == to) {
                    None => {
                        out.problems.push(json!({ "input": name, "field": f.name,
                            "problem": "missing_field",
                            "detail": format!("'{target}' has no field '{to}': map '{}' to one of its fields \
                                (\"fields\": {{\"{}\": <field>}}) or add it", f.name, f.name),
                            "fields": s.fields.iter().map(|(n, t)| json!({"name": n, "type": t})).collect::<Vec<_>>() }));
                    }
                    Some((_, t)) if f.kind != "unknown" && *t != f.kind => {
                        out.problems.push(json!({ "input": name, "field": f.name,
                            "problem": "type_mismatch",
                            "detail": format!("'{}' is {} in the publication, '{to}' is {t}", f.name, f.kind) }));
                    }
                    Some(_) => {}
                }
            }
            out.bind.insert(field_input(&name, &f.name), json!(to));
        }
        let feed = input.feed.replace(&format!("$in.{name}"), &target);
        out.feeds.push((input.source.clone(), feed));
    }
    out
}

/// The materialized root of a publication bound with `bind`, checked as a
/// layout over the adopter's home feeds and `feeds`: the sources' own
/// checks (fields a collection has) run too. Blocking; takes the store's
/// connection itself, so call it holding none.
fn check_bound(
    pod: &Pod,
    f: &Fetched,
    bind: &Map<String, Value>,
    feeds: &[(String, String)],
) -> Result<(), Value> {
    if f.publication.interface["kind"] == "component" {
        return Ok(());
    }
    pod.store().with_conn(|conn| {
        let objects = ConnObjects::new(conn);
        for (id, v, _) in &f.objects {
            objects
                .extra
                .borrow_mut()
                .insert(id.clone(), Arc::new(v.clone()));
        }
        let m = construct_elements::materialize_with(&objects, &f.publication.root, bind)
            .map_err(|e| refuse("invalid_layout", e.to_string()))?;
        let mut all = super::write::home_feeds(conn);
        for x in feeds {
            if !all.contains(x) {
                all.push(x.clone());
            }
        }
        let misfit = |e: crate::validate::LayoutError| {
            json!({ "error": "does_not_fit", "detail": "bound to these data sets the shared \
                layout doesn't fit them", "check": { "detail": e.to_string(), "at": e.pointer,
                "allowed": e.allowed } })
        };
        let sources = crate::layouts::feed_sources(pod, &all);
        crate::validate::custom_layout(
            m.node["id"].as_str().unwrap_or("c-adopting"),
            &m.node,
            &sources,
        )
        .map_err(misfit)?;
        for (name, _) in &sources {
            let Some((adapter, _)) = pod.catalog().source(name) else {
                continue;
            };
            let names: Vec<String> = all
                .iter()
                .filter(|(s, _)| s == name)
                .map(|(_, f)| f.clone())
                .collect();
            adapter
                .check_custom_layout(conn, &names, &m.node)
                .map_err(misfit)?;
        }
        Ok(())
    })
}

/// The edge key an adoption is drawn under.
fn key_of(name: &str) -> String {
    let s = slugify(name);
    format!("s-{s}")
}

fn snippets(name: &str, interface: &Value, bind: &Map<String, Value>) -> Value {
    if interface["kind"] == "component" {
        return json!({ "use": { "type": "core://Use", "ref": name, "args": {} } });
    }
    let r = json!({ "name": name, "as": key_of(name) });
    let mut embed = json!({ "type": "core://Layout", "ref": r });
    let mut open = json!({ "intent": "action.navigation.open", "ref": r, "presentation": "Push" });
    if !bind.is_empty() {
        embed["bind"] = Value::Object(bind.clone());
        open["bind"] = Value::Object(bind.clone());
    }
    json!({ "use": embed, "open": open })
}

async fn adopt(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let (me, shelf) = shelf(pod)?;
    let link = link_arg(args)?;
    let mode = args["mode"].as_str().unwrap_or("follow").to_owned();
    if !matches!(mode.as_str(), "follow" | "pin" | "fork") {
        return Err(refuse("invalid_arguments", "mode is follow, pin or fork"));
    }
    let bindings = args["bindings"].as_object().cloned().unwrap_or_default();
    let (p, l, b, sh) = (pod.clone(), link.clone(), bindings.clone(), shelf.clone());
    let (fetched, plan) = tokio::task::spawn_blocking(move || {
        let f = fetch(sh.as_ref(), &me, &l)?;
        let plan = p.store().with_conn(|c| plan(&p, c, &f.inputs, &b));
        Ok::<_, Value>((f, plan))
    })
    .await
    .map_err(|e| refuse("internal", e.to_string()))??;
    let report = |ready: bool| {
        json!({
            "ready": ready,
            "name": fetched.publication.name,
            "version": fetched.publication.version,
            // The publisher's text (issue #439).
            "interface": crate::mcp::untrusted(&fetched.publication.name, fetched.publication.interface.clone()),
            "bind": plan.bind,
            "create": plan.create.iter().map(|(t, a)| json!({"tool": t, "args": a})).collect::<Vec<_>>(),
            "problems": plan.problems,
            "proposed": plan.proposed,
            "flags": crate::mcp::untrusted(&fetched.publication.name, json!(fetched.flags)),
        })
    };
    let accept = accepted_arg(args);
    let unaccepted: BTreeSet<String> = fetched.data_urls().difference(&accept).cloned().collect();
    if !plan.problems.is_empty() {
        let mut r = report(false);
        r["error"] = json!("not_ready");
        r["detail"] = json!(
            "Nothing was adopted: bind each input (see problems and proposed), confirm with the user, and call adopt again."
        );
        return Err(r);
    }
    if !unaccepted.is_empty() {
        let mut r = report(false);
        for (k, v) in data_urls_refusal(&fetched.publication.name, &unaccepted)
            .as_object()
            .into_iter()
            .flatten()
        {
            r[k] = v.clone();
        }
        return Err(r);
    }
    if args["dry_run"].as_bool().unwrap_or(false) {
        let mut r = report(true);
        r["detail"] = json!(
            "Nothing was adopted yet: show the user the flags (every literal text and URL it \
             carries), then call adopt again without dry_run."
        );
        return Ok(r);
    }
    // New data sets first, through their source's own tools.
    for (tool, a) in &plan.create {
        let mut done = None;
        for ad in pod.catalog().adapters() {
            if let Some(f) = ad.mcp_call(pod, tool, a) {
                done = Some(f.await);
                break;
            }
        }
        match done {
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                return Err(json!({ "error": "create_failed", "tool": tool, "detail": e }));
            }
            None => {
                return Err(refuse(
                    "create_failed",
                    format!("no source has the tool '{tool}'"),
                ));
            }
        }
    }
    let fetched = Arc::new(fetched);
    let plan = Arc::new(plan);
    let (p, f2, pl) = (pod.clone(), fetched.clone(), plan.clone());
    tokio::task::spawn_blocking(move || check_bound(&p, &f2, &pl.bind, &pl.feeds))
        .await
        .map_err(|e| refuse("internal", e.to_string()))??;
    let args = json!({ "fetched": true });
    super::write::run_write(pod, &args, move |pod, conn, _| {
        store_adoption(pod, conn, &fetched, &plan, &mode, &link)
    })
    .await
}

/// Copy a fetched publication's objects into this pod.
fn put_objects(conn: &Connection, f: &Fetched) -> Result<(), Value> {
    for (id, v, features) in &f.objects {
        cstore::put_object(conn, id, v, features).map_err(store_err)?;
    }
    Ok(())
}

fn store_adoption(
    pod: &Pod,
    conn: &Connection,
    f: &Fetched,
    plan: &Plan,
    mode: &str,
    link: &str,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    put_objects(conn, f)?;
    let p = &f.publication;
    let name = if mode == "fork" {
        crate::layouts::new_layout_id()
    } else {
        p.name.clone()
    };
    let note = format!("Adopted {} version {}", p.name, p.version);
    let mut moved = Vec::new();
    if let Some(seq) =
        cstore::move_name(conn, &name, &p.root, AUTHOR_ADOPT, Some(&note)).map_err(store_err)?
    {
        moved.push((name.clone(), seq));
    }
    cstore::set_follows(conn, &name, &[]).map_err(store_err)?;
    moved.extend(super::write::reseal_followers(conn, pod.catalog(), &name).map_err(store_err)?);
    // Home binds the data sets the bindings name.
    if p.interface["kind"] != "component" {
        let mut feeds = super::write::home_feeds(conn);
        let before = feeds.len();
        for x in &plan.feeds {
            if !feeds.contains(x) {
                feeds.push(x.clone());
            }
        }
        if feeds.len() != before {
            moved.extend(super::write::set_home_feeds(pod, conn, &feeds)?);
        }
    }
    if mode != "fork" {
        save_adoption(
            conn,
            &Adoption {
                name: name.clone(),
                link: link.to_owned(),
                publication: p.name.clone(),
                version: p.version,
                mode: mode.to_owned(),
                bind: plan.bind.clone(),
                adopted_at: crate::util::now_secs(),
                accepted: f.data_urls(),
            },
        )
        .map_err(store_err)?;
    }
    cstore::bump_generation(conn).map_err(store_err)?;
    cstore::collect_garbage(conn).map_err(store_err)?;
    let mut out = json!({
        "ok": true,
        "name": name,
        "el": p.root,
        "version": p.version,
        "mode": mode,
        "bind": plan.bind,
        "needs": p.interface["needs"].clone(),
        "flags": f.flags,
        "detail": "Put \"use\" (embedded) or \"open\" (a link, on a button's on_click) in home with \
            update_layout, then look at it with render_layout_preview. If needs lists jacks, check \
            them with list_jacks.",
    });
    if let Some(o) = snippets(&name, &p.interface, &plan.bind).as_object() {
        for (k, v) in o {
            out[k] = v.clone();
        }
    }
    Ok((out, moved))
}

fn list_adoptions(pod: &Pod, _: &Value) -> Result<Value, Value> {
    let (_, shelf) = shelf(pod)?;
    let rows = pod.store().with_conn(adoptions).map_err(store_err)?;
    let mut out = Vec::new();
    for a in rows {
        let p = shelf.by_link(&a.link).map_err(|e| e.answer())?;
        let revoked = p.as_ref().is_none_or(|p| p.revoked_at.is_some());
        let newest = p.as_ref().filter(|_| !revoked).map(|p| p.version);
        out.push(json!({
            "name": a.name,
            "publication": a.publication,
            "mode": a.mode,
            "version": a.version,
            "newest": newest,
            "revoked": revoked,
            "up_to_date": newest.is_none_or(|n| n <= a.version),
            "adopted_at": a.adopted_at,
        }));
    }
    Ok(json!({ "adoptions": out }))
}

/// What `update_adoption` found before writing.
enum Update {
    /// Nothing to write: the answer.
    Done(Value),
    /// Move the adoption to this version, bound so.
    Go(Box<(Adoption, Fetched, Plan)>),
}

async fn update_adoption(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let (p, a) = (pod.clone(), args.clone());
    let step = tokio::task::spawn_blocking(move || {
        let step = p.store().with_conn(|c| update_check(&p, c, &a))?;
        if let Update::Go(b) = &step {
            check_bound(&p, &b.1, &b.2.bind, &b.2.feeds)?;
        }
        Ok::<_, Value>(step)
    })
    .await
    .map_err(|e| refuse("internal", e.to_string()))??;
    let found = match step {
        Update::Done(v) => return Ok(v),
        Update::Go(b) => Arc::new(*b),
    };
    super::write::run_write(pod, args, move |pod, conn, _| {
        let (a, f, plan) = &*found;
        update_store(pod, conn, a.clone(), f, plan)
    })
    .await
}

fn update_check(pod: &Pod, conn: &Connection, args: &Value) -> Result<Update, Value> {
    let (me, shelf) = shelf(pod)?;
    let name = args["name"].as_str().unwrap_or("").trim().to_owned();
    let Some(a) = adoption(conn, &name).map_err(store_err)? else {
        return Err(refuse(
            "unknown_adoption",
            format!("this pod adopted nothing named '{name}' (list_adoptions)"),
        ));
    };
    if a.mode == "pin" {
        return Err(refuse(
            "pinned",
            format!(
                "'{name}' is pinned to version {}: adopt it again to move it",
                a.version
            ),
        ));
    }
    let f = match fetch(shelf.as_ref(), &me, &a.link) {
        Ok(f) => f,
        Err(e) if e["error"] == "revoked" || e["error"] == "unknown_link" => {
            return Ok(Update::Done(
                json!({ "ok": true, "name": name, "version": a.version, "revoked": true,
                    "detail": "The owner revoked it: the copy here keeps drawing, and won't advance." }),
            ));
        }
        Err(e) => return Err(e),
    };
    if f.publication.version <= a.version {
        return Ok(Update::Done(
            json!({ "ok": true, "name": name, "version": a.version, "up_to_date": true }),
        ));
    }
    // A URL sending the user's data that they haven't accepted waits for them.
    let accept = &a.accepted | &accepted_arg(args);
    let unaccepted: BTreeSet<String> = f.data_urls().difference(&accept).cloned().collect();
    if !unaccepted.is_empty() {
        let mut r = data_urls_refusal(&f.publication.name, &unaccepted);
        r["version"] = json!(f.publication.version);
        r["flags"] = crate::mcp::untrusted(&f.publication.name, json!(f.flags));
        return Err(r);
    }
    // The saved bindings, as adopt's arguments, with any new ones on top.
    let mut bindings = Map::new();
    for raw in &f.inputs {
        let n = raw["name"].as_str().unwrap_or("");
        if let Some(v) = a.bind.get(n) {
            let fields: Map<String, Value> = raw["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|fl| {
                    let fname = fl["name"].as_str()?;
                    a.bind
                        .get(&field_input(n, fname))
                        .map(|t| (fname.to_owned(), t.clone()))
                })
                .collect();
            if raw.get("source").is_some() {
                let kind = raw["kind"].as_str().unwrap_or("data").to_owned();
                let mut b = json!({ "fields": fields });
                b[kind] = v.clone();
                bindings.insert(n.to_owned(), b);
            } else {
                bindings.insert(n.to_owned(), v.clone());
            }
        }
    }
    for (k, v) in args["bindings"].as_object().into_iter().flatten() {
        bindings.insert(k.clone(), v.clone());
    }
    let plan = plan(pod, conn, &f.inputs, &bindings);
    // A new field of a bound input with no mapping keeps its name: say so.
    let new_inputs: Vec<String> = f
        .inputs
        .iter()
        .flat_map(|raw| {
            let n = raw["name"].as_str().unwrap_or("").to_owned();
            let mut v = vec![n.clone()];
            for fl in raw["fields"].as_array().into_iter().flatten() {
                v.push(field_input(&n, fl["name"].as_str().unwrap_or("")));
            }
            v
        })
        .filter(|i| !a.bind.contains_key(i))
        .collect();
    if !plan.problems.is_empty() || !plan.create.is_empty() {
        return Err(json!({
            "error": "does_not_fit",
            "detail": format!("version {} of {} needs more than the bindings give: bind it in \
                bindings (new data sets are made by adopt)", f.publication.version, f.publication.name),
            "problems": plan.problems,
            "proposed": plan.proposed,
            "new_inputs": new_inputs,
            "flags": f.flags,
        }));
    }
    // Each reference here must bind what the new version reads.
    let root_el = f
        .objects
        .iter()
        .find(|(id, _, _)| *id == f.publication.root)
        .map(|(_, v, _)| v.clone())
        .unwrap_or(Value::Null);
    let reads = construct_elements::template::inputs_of(&root_el);
    for follower in cstore::followers(conn, &name).map_err(store_err)? {
        let Some(cur) = cstore::name(conn, &follower).map_err(store_err)? else {
            continue;
        };
        let Some((fv, _)) = cstore::object(conn, &cur).map_err(store_err)? else {
            continue;
        };
        let mut missing = BTreeSet::new();
        find_edges(&fv["node"], &name, &mut |bind| {
            for r in &reads {
                if !bind.contains_key(r) {
                    missing.insert(r.clone());
                }
            }
        });
        if !missing.is_empty() {
            let add: Map<String, Value> = missing
                .iter()
                .filter_map(|m| plan.bind.get(m).map(|v| (m.clone(), v.clone())))
                .collect();
            return Err(json!({
                "error": "unbound_input",
                "detail": format!("version {} of {name} reads {} that '{follower}' doesn't bind yet: \
                    add them to the bind of its reference to {name} (update_layout), then update again",
                    f.publication.version, missing.iter().cloned().collect::<Vec<_>>().join(", ")),
                "add_to_bind": add,
            }));
        }
    }
    Ok(Update::Go(Box::new((a, f, plan))))
}

fn update_store(
    pod: &Pod,
    conn: &Connection,
    mut a: Adoption,
    f: &Fetched,
    plan: &Plan,
) -> Result<(Value, Vec<(String, i64)>), Value> {
    let name = a.name.clone();
    put_objects(conn, f)?;
    let mut moved = Vec::new();
    let note = format!(
        "Updated to {} version {}",
        f.publication.name, f.publication.version
    );
    if let Some(seq) =
        cstore::move_name(conn, &name, &f.publication.root, AUTHOR_ADOPT, Some(&note))
            .map_err(store_err)?
    {
        moved.push((name.clone(), seq));
    }
    moved.extend(super::write::reseal_followers(conn, pod.catalog(), &name).map_err(store_err)?);
    let mut feeds = super::write::home_feeds(conn);
    let before = feeds.len();
    for x in &plan.feeds {
        if !feeds.contains(x) {
            feeds.push(x.clone());
        }
    }
    if feeds.len() != before {
        moved.extend(super::write::set_home_feeds(pod, conn, &feeds)?);
    }
    a.version = f.publication.version;
    a.bind = plan.bind.clone();
    a.accepted = f.data_urls();
    save_adoption(conn, &a).map_err(store_err)?;
    cstore::bump_generation(conn).map_err(store_err)?;
    cstore::collect_garbage(conn).map_err(store_err)?;
    Ok((
        json!({ "ok": true, "name": name, "version": a.version, "el": f.publication.root,
                "bind": plan.bind, "flags": f.flags }),
        moved,
    ))
}

/// Call `f` with the `bind` of each edge in `v` that references `name`.
fn find_edges(v: &Value, name: &str, f: &mut dyn FnMut(&Map<String, Value>)) {
    match v {
        Value::Object(o) => {
            if let Some(site) = construct_elements::refs::site_of(o)
                && site != Site::Use
                && let Some(r) = o.get("ref").and_then(Ref::parse)
                && r.name == name
                && !r.pin
            {
                let empty = Map::new();
                f(o.get(construct_elements::refs::BIND_FIELD)
                    .and_then(Value::as_object)
                    .unwrap_or(&empty));
            }
            o.values().for_each(|c| find_edges(c, name, f));
        }
        Value::Array(a) => a.iter().for_each(|c| find_edges(c, name, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_and_slugs() {
        assert!(valid_handle("aidan"));
        assert!(valid_handle("a-b2"));
        assert!(!valid_handle("a"));
        assert!(!valid_handle("Aidan"));
        assert!(!valid_handle("2x"));
        assert!(!valid_handle("core"));
        // A short path is `/<handle>/<slug>`: never a path the host routes.
        for h in ["v1", "web", "oauth", "healthz", "ical"] {
            assert!(!valid_handle(h), "{h}");
        }
        assert!(!valid_handle("x-"));
        assert!(valid_slug("dinner-party"));
        assert!(!valid_slug("-x"));
        assert_eq!(slugify("Dinner party · Sunday!"), "dinner-party-sunday");
    }

    #[test]
    fn literal_text_skips_expressions() {
        assert_eq!(literal_text("$item.title"), "");
        assert_eq!(literal_text("${local://theme#card}"), "");
        assert_eq!(literal_text("Due $item.due"), "Due");
        let mut flags = Vec::new();
        flags_in(
            &json!({"node": {"type": "core://Text", "value_expr": "Dinner party · Sunday",
                "on_click": {"intent": "action.link.open", "url": "https://runner.example/x"}},
                "title": "$in.title"}),
            "c-x",
            "",
            None,
            &mut flags,
        );
        assert_eq!(flags.len(), 2, "{flags:?}");
        let kinds: BTreeSet<&str> = flags.iter().filter_map(|f| f["kind"].as_str()).collect();
        assert_eq!(kinds, BTreeSet::from(["text", "url"]));
    }

    #[test]
    fn urls_that_send_data_are_marked() {
        // A fixed host with an expression that reads data, or a URL put
        // together from one: marked. A URL that is the user's own value,
        // or a fixed one: not.
        for (k, v, sends) in [
            ("url", "https://a.example/p.png?d=$item.notes", true),
            ("url", "https://a.example/${local://theme#x}.png", true),
            ("label", "see https://a.example/$in.who", true),
            ("url", "$arg.host$item.notes", true),
            ("src", "$in.base/p.png", true),
            ("url", "$item.photo", false),
            ("url", "${books://b/1#photo}", false),
            ("url", "https://a.example/logo.png", false),
            ("value_expr", "$item.title", false),
        ] {
            let mut flags = Vec::new();
            flags_in(&json!({ k: v }), "c-x", "", None, &mut flags);
            let marked = flags.iter().any(|f| f["reads_data"] == true);
            assert_eq!(marked, sends, "{k}: {v} -> {flags:?}");
        }
    }

    #[test]
    fn entity_literals_are_found() {
        assert!(entity_literal(&json!({"v": "${books://b/1#title}"}), "").is_some());
        assert!(entity_literal(&json!({"v": "${local://theme#card}"}), "").is_none());
        assert!(entity_literal(&json!({"v": "$item.title"}), "").is_none());
    }

    #[test]
    fn links_are_read_from_what_was_pasted() {
        assert_eq!(
            link_arg(&json!({"link": "share-abc"})).unwrap(),
            "share-abc"
        );
        assert_eq!(
            link_arg(&json!({"link": "https://x.example/s/share-abc"})).unwrap(),
            "share-abc"
        );
        assert!(link_arg(&json!({"link": ""})).is_err());
    }
}
