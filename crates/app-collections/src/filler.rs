//! Fillers (decision `collection-fillers`): what keeps a collection's rows
//! current without anyone writing them. A collection may have several (the
//! Events app has one per website); each is part of the definition (data,
//! not code: Claude declares it, the pod runs it, [`crate::fill`]):
//!
//! - `fetch`: a declarative recipe. A URL (or each row's URL in a field),
//!   an extractor ([`web_extract::items`]: events, json, embedded-json,
//!   jsonld, rss, ics, selectors; or `llm`, a model reading the page's
//!   text) and a map from fields to paths in each item. `details` fetches
//!   each new row's own page for more (an event's full description).
//! - `agent`: a model with web search and fetch, a prompt and a budget,
//!   answering rows that fit the collection's fields.
//!
//! Each row remembers the filler that wrote it, and a filler only ever
//! touches its own rows. Rows are written by `mode`: `replace` (its rows are
//! the latest answer; with a `key`, rows keep their ids and unfilled
//! fields) or `upsert` by `key` (new keys add rows, known keys update
//! theirs, rows not in the answer stay). `upcoming` drops items whose date
//! has passed. The fields a filler writes are *filled*: apps and
//! `edit_rows` can't set them, and rows a `replace` filler owns can't be
//! added or deleted by hand, so a fill never overwrites a person's edit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::model::{Definition, Kind};

/// Most fillers per collection.
pub const MAX_FILLERS: usize = 50;
/// Most map entries of a recipe.
pub const MAX_MAP: usize = 40;
/// Most request headers of a recipe.
pub const MAX_HEADERS: usize = 10;
/// Longest header value.
pub const MAX_HEADER_CHARS: usize = 1_000;
/// Longest agent prompt or `llm` hint.
pub const MAX_PROMPT_CHARS: usize = 2_000;
/// Longest filler title.
pub const MAX_TITLE_CHARS: usize = 100;
/// Interval bounds, minutes.
pub const MIN_EVERY: u32 = 15;
pub const MAX_EVERY: u32 = 7 * 24 * 60;
/// An agent's shortest interval, minutes.
pub const MIN_AGENT_EVERY: u32 = 60;
/// `on_open_minutes` bounds.
pub const MIN_ON_OPEN: u32 = 1;
pub const MAX_ON_OPEN: u32 = 24 * 60;
/// Agent budget bounds and defaults.
pub const MAX_SEARCHES: u32 = 20;
pub const MAX_FETCHES: u32 = 30;
pub const DEFAULT_SEARCHES: u32 = 5;
pub const DEFAULT_FETCHES: u32 = 10;
pub const DEFAULT_FILLS_PER_DAY: u32 = 4;
/// The extractor name of a model reading the page's text.
pub const LLM: &str = "llm";

/// Each new row's own page, read for more of its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Details {
    /// The filled field (id) holding the row's page.
    pub link: String,
    pub extract: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub items: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    /// Field id -> path in the page's first item.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub map: BTreeMap<String, String>,
    /// The field (id) that gets the page's readable text when the
    /// extractor finds nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// With extract [`LLM`]: notes for the model reading each page.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

impl Details {
    /// Whether a model reads each page (extract [`LLM`]); its fields are
    /// `map`'s keys.
    pub fn is_llm(&self) -> bool {
        self.extract == LLM
    }
}

/// What fills the rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
// A definition holds a few fillers: boxing the recipe would only add noise.
#[allow(clippy::large_enum_variant)]
pub enum Source {
    /// A declarative recipe: fetch, extract, map.
    Fetch {
        /// The page to read (`None`: each row's URL in `url_field`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        /// The field (id) holding each row's own URL: the row is filled
        /// from it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_field: Option<String>,
        /// The field (id) holding each row's search query (decision
        /// `web-and-agent-modules`): the row's item is searched with the
        /// pod's `web_search` jack and a model fills the row's fields from
        /// the results (extract [`LLM`] only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query_field: Option<String>,
        /// Request headers; values may name a secret, `{secret:<name>}`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        /// The extractor ([`web_extract::items::Kind`], or [`LLM`]).
        extract: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        items: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        types: Vec<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        script: String,
        /// The zone a time without an offset is in (IANA or `±HH:MM`).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        zone: String,
        /// `llm`: notes for the model about the page.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        hint: String,
        /// Field id -> path (or CSS selector) in each item (`llm`: the
        /// fields it answers, paths empty).
        map: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Box<Details>>,
    },
    /// A model with web tools answers the rows.
    Agent {
        prompt: String,
        /// The field ids it fills.
        fields: Vec<String>,
        max_searches: u32,
        max_fetches: u32,
        fills_per_day: u32,
    },
}

/// How a fill writes rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Replace,
    Upsert,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Replace => "replace",
            Mode::Upsert => "upsert",
        }
    }
}

/// When a per-row filler (`url_field`, `query_field`) fills a row again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Refresh {
    /// Once, when the row's URL or query first appears or changes.
    Once,
    /// On every fill (only when the page changed, for `llm`).
    Every,
}

impl Refresh {
    pub fn as_str(self) -> &'static str {
        match self {
            Refresh::Once => "once",
            Refresh::Every => "every",
        }
    }
}

/// What a per-row fill does when the row's page or search yields nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnEmpty {
    /// Keep the row's last values (a blocked or empty page never erases
    /// them).
    #[default]
    Keep,
    /// Clear the filled fields.
    Clear,
}

impl OnEmpty {
    pub fn as_str(self) -> &'static str {
        match self {
            OnEmpty::Keep => "keep",
            OnEmpty::Clear => "clear",
        }
    }
}

/// One filler of a collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filler {
    /// `s` and 10 base32 characters: rows and fill state name it.
    pub id: String,
    /// What people see ("Improv Millbrook").
    pub title: String,
    #[serde(flatten)]
    pub source: Source,
    #[serde(default)]
    pub mode: Mode,
    /// The field (id) that identifies a row across fills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Fill every this many minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_minutes: Option<u32>,
    /// Fill when an app showing the collection opens and the last fill is
    /// older than this many minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_open_minutes: Option<u32>,
    /// A date or datetime field (id): items whose value has passed are left
    /// out (so past events drop off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upcoming: Option<String>,
    /// Per-row fillers: when a row is filled again (default: `every` for
    /// `url_field`, `once` for `query_field`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<Refresh>,
    /// Per-row fillers: what an empty answer does (default `keep`).
    #[serde(default, skip_serializing_if = "is_keep")]
    pub on_empty: OnEmpty,
}

fn is_keep(o: &OnEmpty) -> bool {
    *o == OnEmpty::Keep
}

impl Filler {
    pub fn kind(&self) -> &'static str {
        match self.source {
            Source::Fetch { .. } => "fetch",
            Source::Agent { .. } => "agent",
        }
    }

    pub fn is_agent(&self) -> bool {
        matches!(self.source, Source::Agent { .. })
    }

    /// Whether a model reads each changed page (`extract: llm`).
    pub fn is_llm(&self) -> bool {
        matches!(&self.source, Source::Fetch { extract, .. } if extract == LLM)
    }

    /// Whether running it costs model calls.
    pub fn uses_model(&self) -> bool {
        self.is_agent() || self.is_llm()
    }

    /// The field ids each item writes.
    pub fn mapped(&self) -> Vec<String> {
        match &self.source {
            Source::Fetch { map, .. } => map.keys().cloned().collect(),
            Source::Agent { fields, .. } => fields.clone(),
        }
    }

    /// Every field id it writes: the mapped ones and the details'.
    pub fn filled(&self) -> Vec<String> {
        let mut out = self.mapped();
        if let Source::Fetch {
            details: Some(d), ..
        } = &self.source
        {
            for id in d.map.keys().chain(d.text.iter()) {
                if !out.contains(id) {
                    out.push(id.clone());
                }
            }
        }
        out
    }

    /// The field id of each row's own URL (`fetch` with `url_field`).
    pub fn url_field(&self) -> Option<&str> {
        match &self.source {
            Source::Fetch { url_field, .. } => url_field.as_deref(),
            Source::Agent { .. } => None,
        }
    }

    /// The field id of each row's search query (`fetch` with `query_field`).
    pub fn query_field(&self) -> Option<&str> {
        match &self.source {
            Source::Fetch { query_field, .. } => query_field.as_deref(),
            Source::Agent { .. } => None,
        }
    }

    /// Whether it fills rows one by one (`url_field` or `query_field`)
    /// rather than answering a list.
    pub fn per_row(&self) -> bool {
        self.url_field().is_some() || self.query_field().is_some()
    }

    /// When a per-row filler fills a row again.
    pub fn refresh(&self) -> Refresh {
        self.refresh.unwrap_or(if self.query_field().is_some() {
            Refresh::Once
        } else {
            Refresh::Every
        })
    }

    /// The page it reads (`fetch` with `url`).
    pub fn url(&self) -> Option<&str> {
        match &self.source {
            Source::Fetch { url, .. } => url.as_deref(),
            Source::Agent { .. } => None,
        }
    }

    /// Whether its rows belong to it: it adds and removes them (`replace`,
    /// not filling rows from their own URLs).
    pub fn owns_rows(&self) -> bool {
        self.mode == Mode::Replace && !self.per_row()
    }

    /// A stable fingerprint: a changed recipe fills afresh (no 304 or
    /// same-body shortcut).
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let h = Sha256::digest(serde_json::to_string(self).unwrap_or_default());
        h.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    /// The secret names its headers use.
    pub fn secrets(&self) -> Vec<String> {
        match &self.source {
            Source::Fetch { headers, .. } => {
                let mut out = Vec::new();
                for v in headers.values() {
                    for name in secret_refs(v) {
                        if !out.contains(&name) {
                            out.push(name);
                        }
                    }
                }
                out
            }
            Source::Agent { .. } => Vec::new(),
        }
    }

    /// The extractor spec of a `fetch` recipe (`None` for an agent or
    /// `llm`).
    pub fn extract_spec(&self) -> Option<web_extract::items::Spec> {
        match &self.source {
            Source::Fetch {
                extract,
                items,
                types,
                script,
                zone,
                ..
            } => Some(web_extract::items::Spec {
                kind: web_extract::items::Kind::parse(extract)?,
                items: items.clone(),
                types: types.clone(),
                script: script.clone(),
                zone: zone.clone(),
            }),
            Source::Agent { .. } => None,
        }
    }

    /// The zone a time without an offset is placed in.
    pub fn zone(&self) -> Option<web_extract::when::Zone> {
        match &self.source {
            Source::Fetch { zone, .. } if !zone.is_empty() => web_extract::items::Spec {
                kind: web_extract::items::Kind::Json,
                items: String::new(),
                types: Vec::new(),
                script: String::new(),
                zone: zone.clone(),
            }
            .zone(),
            _ => None,
        }
    }

    /// As a spec with field names, for Claude.
    pub fn describe(&self, def: &Definition) -> Value {
        let name = |id: &str| def.name_of(id);
        let names = |m: &BTreeMap<String, String>| {
            Value::Object(
                m.iter()
                    .map(|(id, p)| (name(id), json!(p)))
                    .collect::<Map<String, Value>>(),
            )
        };
        let mut v = match &self.source {
            Source::Fetch {
                url,
                url_field,
                query_field,
                headers,
                extract,
                items,
                types,
                script,
                zone,
                hint,
                map,
                details,
            } => {
                let mut v = json!({ "kind": "fetch", "extract": extract });
                if let Some(u) = url {
                    v["url"] = json!(u);
                }
                if let Some(f) = url_field {
                    v["url_field"] = json!(name(f));
                }
                if let Some(f) = query_field {
                    v["query_field"] = json!(name(f));
                }
                for (k, x) in [
                    ("items", items),
                    ("script", script),
                    ("zone", zone),
                    ("hint", hint),
                ] {
                    if !x.is_empty() {
                        v[k] = json!(x);
                    }
                }
                if !headers.is_empty() {
                    v["headers"] = json!(headers);
                }
                if !types.is_empty() {
                    v["types"] = json!(types);
                }
                if extract == LLM {
                    v["fields"] = json!(map.keys().map(|id| name(id)).collect::<Vec<_>>());
                } else {
                    v["map"] = names(map);
                }
                if let Some(d) = details {
                    let mut dv = json!({ "link": name(&d.link), "extract": d.extract });
                    if d.is_llm() {
                        dv["fields"] = json!(d.map.keys().map(|id| name(id)).collect::<Vec<_>>());
                        if !d.hint.is_empty() {
                            dv["hint"] = json!(d.hint);
                        }
                    } else {
                        dv["map"] = names(&d.map);
                    }
                    if !d.items.is_empty() {
                        dv["items"] = json!(d.items);
                    }
                    if !d.types.is_empty() {
                        dv["types"] = json!(d.types);
                    }
                    if let Some(t) = &d.text {
                        dv["text"] = json!(name(t));
                    }
                    v["details"] = dv;
                }
                v
            }
            Source::Agent {
                prompt,
                fields,
                max_searches,
                max_fetches,
                fills_per_day,
            } => json!({
                "kind": "agent", "prompt": prompt,
                "fields": fields.iter().map(|f| name(f)).collect::<Vec<_>>(),
                "max_searches": max_searches, "max_fetches": max_fetches,
                "fills_per_day": fills_per_day,
            }),
        };
        v["id"] = json!(self.id);
        v["title"] = json!(self.title);
        v["mode"] = json!(self.mode.as_str());
        if let Some(k) = &self.key {
            v["key"] = json!(name(k));
        }
        if let Some(m) = self.every_minutes {
            v["every_minutes"] = json!(m);
        }
        if let Some(m) = self.on_open_minutes {
            v["on_open_minutes"] = json!(m);
        }
        if let Some(u) = &self.upcoming {
            v["upcoming"] = json!(name(u));
        }
        if self.per_row() {
            v["refresh"] = json!(self.refresh().as_str());
            v["on_empty"] = json!(self.on_empty.as_str());
        }
        v
    }
}

/// The filler of `def` with id `id`.
pub fn find<'a>(def: &'a Definition, id: &str) -> Option<&'a Filler> {
    def.fillers.iter().find(|f| f.id == id)
}

/// The secret names in a header value: `{secret:<name>}`.
pub fn secret_refs(v: &str) -> Vec<String> {
    v.split("{secret:")
        .skip(1)
        .filter_map(|r| r.split_once('}').map(|(n, _)| n.trim().to_owned()))
        .collect()
}

/// Whether `s` is a secret name: `[a-z][a-z0-9_]*`, at most 40.
pub fn valid_secret_name(s: &str) -> bool {
    let mut b = s.bytes();
    s.len() <= 40
        && b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

fn uint(o: &Map<String, Value>, k: &str) -> Result<Option<u32>, String> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| format!("filler.{k} is a whole number")),
    }
}

fn text<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}

fn types_of(o: &Map<String, Value>, at: &str) -> Result<Vec<String>, String> {
    match o.get("types") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.trim().to_owned()]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|t| t.as_str().map(|s| s.trim().to_owned()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| format!("{at}.types is a list of schema.org type names")),
        Some(_) => Err(format!("{at}.types is a list of schema.org type names")),
    }
}

/// A field name resolved against `def`.
fn field_id(def: &Definition, n: &str, role: &str) -> Result<String, String> {
    def.field(n).map(|f| f.id.clone()).ok_or_else(|| {
        format!(
            "filler {role} '{n}' is not one of the fields ({})",
            def.live_fields()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// A `{field: path}` map resolved against `def`: `(ids -> paths, names ->
/// paths)`.
type Mapped = (BTreeMap<String, String>, Vec<(String, String)>);

fn parse_map(def: &Definition, v: Option<&Value>, at: &str) -> Result<Mapped, String> {
    let Some(Value::Object(m)) = v else {
        return Err(format!(
            "{at}.map is {{field: path}} (a CSS selector for the selectors extractor)"
        ));
    };
    if m.is_empty() || m.len() > MAX_MAP {
        return Err(format!("{at}.map has 1..={MAX_MAP} fields"));
    }
    let mut map = BTreeMap::new();
    let mut pairs = Vec::new();
    for (n, p) in m {
        let Some(p) = p.as_str() else {
            return Err(format!("{at}.map.{n} is a path (text)"));
        };
        let id = field_id(def, n, "map field")?;
        pairs.push((n.clone(), p.trim().to_owned()));
        map.insert(id, p.trim().to_owned());
    }
    Ok((map, pairs))
}

/// A title for a filler without one: the page's host, else its kind.
fn default_title(url: Option<&str>, kind: &str) -> String {
    if kind == "search" {
        return "Web search".into();
    }
    url.map(web_extract::fetch::host_of)
        .filter(|h| !h.is_empty())
        .map(|h| h.trim_start_matches("www.").to_owned())
        .unwrap_or_else(|| {
            if kind == "agent" {
                "Research".into()
            } else {
                "Web page".into()
            }
        })
}

/// Parse a filler spec (field names, resolved against `def`) as filler
/// `id`. `allow_private`: URLs may name private hosts (tests, the
/// operator's choice).
pub fn parse(
    def: &Definition,
    v: &Value,
    id: String,
    allow_private: bool,
) -> Result<Filler, String> {
    let Value::Object(o) = v else {
        return Err(
            "a filler is {kind: \"fetch\", url | url_field, extract, map, ...} or {kind: \"agent\", prompt, ...}".into(),
        );
    };
    let source = match text(o, "kind") {
        "fetch" => {
            let url = Some(text(o, "url"))
                .filter(|u| !u.is_empty())
                .map(|u| web_extract::fetch::check_url(u, allow_private))
                .transpose()
                .map_err(|e| format!("filler.url: {e}"))?;
            let url_field = Some(text(o, "url_field"))
                .filter(|u| !u.is_empty())
                .map(|n| field_id(def, n, "url_field"))
                .transpose()?;
            let query_field = Some(text(o, "query_field"))
                .filter(|u| !u.is_empty())
                .map(|n| field_id(def, n, "query_field"))
                .transpose()?;
            if [url.is_some(), url_field.is_some(), query_field.is_some()]
                .iter()
                .filter(|x| **x)
                .count()
                != 1
            {
                return Err("a fetch filler has one of \"url\" (one page), \"url_field\" (each row's own page) or \"query_field\" (each row's own web search)".into());
            }
            if query_field.is_some() && text(o, "extract") != LLM {
                return Err("a query_field filler has extract \"llm\": a model picks each row's fields from its search results".into());
            }
            let mut headers = BTreeMap::new();
            match o.get("headers") {
                None | Some(Value::Null) => {}
                Some(Value::Object(h)) => {
                    for (k, v) in h {
                        let k = k.trim();
                        let Some(v) = v.as_str() else {
                            return Err(format!("filler.headers.{k} is text"));
                        };
                        if k.is_empty()
                            || !k
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                            || ["host", "content-length", "transfer-encoding", "connection"]
                                .contains(&k.to_ascii_lowercase().as_str())
                        {
                            return Err(format!("filler.headers: '{k}' can't be sent"));
                        }
                        if v.chars().count() > MAX_HEADER_CHARS || v.contains(['\r', '\n']) {
                            return Err(format!(
                                "filler.headers.{k} is one line of at most {MAX_HEADER_CHARS} characters"
                            ));
                        }
                        if let Some(bad) =
                            secret_refs(v).into_iter().find(|n| !valid_secret_name(n))
                        {
                            return Err(format!(
                                "filler.headers.{k}: '{bad}' is not a secret name ([a-z][a-z0-9_]*)"
                            ));
                        }
                        headers.insert(k.to_owned(), v.to_owned());
                    }
                    if headers.len() > MAX_HEADERS {
                        return Err(format!("a filler sends at most {MAX_HEADERS} headers"));
                    }
                }
                Some(_) => return Err("filler.headers is {name: value}".into()),
            }
            let extract = text(o, "extract");
            let hint = text(o, "hint").to_owned();
            if hint.chars().count() > MAX_PROMPT_CHARS {
                return Err(format!(
                    "filler.hint is at most {MAX_PROMPT_CHARS} characters"
                ));
            }
            let zone = text(o, "zone").to_owned();
            let (map, extract, items, types, script) = if extract == LLM {
                // A model answers the fields; no paths.
                let fields: Vec<String> = match o.get("fields") {
                    None | Some(Value::Null) => def
                        .live_fields()
                        .filter(|f| {
                            !f.computed()
                                && url_field.as_ref() != Some(&f.id)
                                && query_field.as_ref() != Some(&f.id)
                        })
                        .map(|f| f.id.clone())
                        .collect(),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|n| field_id(def, n.as_str().unwrap_or(""), "field"))
                        .collect::<Result<_, _>>()?,
                    Some(_) => return Err("filler.fields is a list of field names".into()),
                };
                if fields.is_empty() {
                    return Err("an llm filler answers at least one field".into());
                }
                let map = fields.into_iter().map(|f| (f, String::new())).collect();
                (
                    map,
                    LLM.to_owned(),
                    String::new(),
                    Vec::new(),
                    String::new(),
                )
            } else {
                let Some(kind) = web_extract::items::Kind::parse(extract) else {
                    return Err(format!(
                        "filler.extract is one of {}, {LLM}",
                        web_extract::items::Kind::ALL.join(", ")
                    ));
                };
                let (map, pairs) = parse_map(def, o.get("map"), "filler")?;
                let spec = web_extract::items::Spec {
                    kind,
                    items: text(o, "items").to_owned(),
                    types: types_of(o, "filler")?,
                    script: text(o, "script").to_owned(),
                    zone: zone.clone(),
                };
                web_extract::items::check(&spec, &pairs).map_err(|e| format!("filler: {e}"))?;
                (
                    map,
                    kind.name().to_owned(),
                    spec.items,
                    spec.types,
                    spec.script,
                )
            };
            if !zone.is_empty() {
                web_extract::items::check(
                    &web_extract::items::Spec {
                        kind: web_extract::items::Kind::Json,
                        items: String::new(),
                        types: Vec::new(),
                        script: String::new(),
                        zone: zone.clone(),
                    },
                    &[],
                )
                .map_err(|e| format!("filler: {e}"))?;
            }
            let details = match o.get("details") {
                None | Some(Value::Null) => None,
                Some(Value::Object(d)) if text(d, "extract") == LLM => {
                    // A model reads each new row's page once for `fields`.
                    let link = field_id(def, text(d, "link"), "details.link")?;
                    let fields: Vec<String> = match d.get("fields") {
                        Some(Value::Array(a)) => a
                            .iter()
                            .map(|n| field_id(def, n.as_str().unwrap_or(""), "details.fields"))
                            .collect::<Result<_, _>>()?,
                        _ => {
                            return Err(
                                "filler.details with extract llm needs fields (a list of field names)"
                                    .into(),
                            );
                        }
                    };
                    if fields.is_empty() {
                        return Err("filler.details.fields names at least one field".into());
                    }
                    if d.get("map").is_some_and(|m| !m.is_null()) || !text(d, "text").is_empty() {
                        return Err(
                            "filler.details with extract llm takes fields (and hint), not map or text"
                                .into(),
                        );
                    }
                    let dhint = text(d, "hint").to_owned();
                    if dhint.chars().count() > MAX_PROMPT_CHARS {
                        return Err(format!(
                            "filler.details.hint is at most {MAX_PROMPT_CHARS} characters"
                        ));
                    }
                    Some(Box::new(Details {
                        link,
                        extract: LLM.to_owned(),
                        items: String::new(),
                        types: Vec::new(),
                        map: fields.into_iter().map(|f| (f, String::new())).collect(),
                        text: None,
                        hint: dhint,
                    }))
                }
                Some(Value::Object(d)) => {
                    let link = field_id(def, text(d, "link"), "details.link")?;
                    let dkind =
                        web_extract::items::Kind::parse(text(d, "extract")).ok_or_else(|| {
                            format!(
                                "filler.details.extract is one of {}, {LLM}",
                                web_extract::items::Kind::ALL.join(", ")
                            )
                        })?;
                    let (dmap, pairs) = match d.get("map") {
                        None | Some(Value::Null) => (BTreeMap::new(), Vec::new()),
                        m => parse_map(def, m, "filler.details")?,
                    };
                    let dtext = Some(text(d, "text"))
                        .filter(|t| !t.is_empty())
                        .map(|t| field_id(def, t, "details.text"))
                        .transpose()?;
                    if dmap.is_empty() && dtext.is_none() {
                        return Err("filler.details needs map or text".into());
                    }
                    let spec = web_extract::items::Spec {
                        kind: dkind,
                        items: text(d, "items").to_owned(),
                        types: types_of(d, "filler.details")?,
                        script: String::new(),
                        zone: zone.clone(),
                    };
                    web_extract::items::check(&spec, &pairs)
                        .map_err(|e| format!("filler.details: {e}"))?;
                    Some(Box::new(Details {
                        link,
                        extract: dkind.name().to_owned(),
                        items: spec.items,
                        types: spec.types,
                        map: dmap,
                        text: dtext,
                        hint: String::new(),
                    }))
                }
                Some(_) => {
                    return Err(
                        "filler.details is {link, extract, map?, text?} or {link, extract: llm, fields, hint?}"
                            .into(),
                    );
                }
            };
            Source::Fetch {
                url,
                url_field,
                query_field,
                headers,
                extract,
                items,
                types,
                script,
                zone,
                hint,
                map,
                details,
            }
        }
        "agent" => {
            let prompt = text(o, "prompt");
            if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
                return Err(format!(
                    "filler.prompt (what to find, e.g. \"the five best-reviewed 27-inch monitors under $400\") is 1..={MAX_PROMPT_CHARS} characters"
                ));
            }
            let fields = match o.get("fields") {
                None | Some(Value::Null) => def
                    .live_fields()
                    .filter(|f| !f.computed())
                    .map(|f| f.id.clone())
                    .collect(),
                Some(Value::Array(a)) => {
                    let mut out: Vec<String> = Vec::new();
                    for n in a {
                        let id = field_id(def, n.as_str().unwrap_or(""), "field")?;
                        if !out.contains(&id) {
                            out.push(id);
                        }
                    }
                    out
                }
                Some(_) => return Err("filler.fields is a list of field names".into()),
            };
            if fields.is_empty() {
                return Err("an agent filler fills at least one field".into());
            }
            let bounded = |k: &str, max: u32, default: u32| -> Result<u32, String> {
                let n = uint(o, k)?.unwrap_or(default);
                if n > max {
                    return Err(format!("filler.{k} is 0..={max}"));
                }
                Ok(n)
            };
            let fills_per_day = uint(o, "fills_per_day")?.unwrap_or(DEFAULT_FILLS_PER_DAY);
            if fills_per_day == 0 {
                return Err("filler.fills_per_day is at least 1".into());
            }
            Source::Agent {
                prompt: prompt.to_owned(),
                fields,
                max_searches: bounded("max_searches", MAX_SEARCHES, DEFAULT_SEARCHES)?,
                max_fetches: bounded("max_fetches", MAX_FETCHES, DEFAULT_FETCHES)?,
                fills_per_day,
            }
        }
        other => {
            return Err(format!(
                "filler.kind '{other}' is \"fetch\" (a recipe) or \"agent\" (a model with web search)"
            ));
        }
    };
    let mode = match text(o, "mode") {
        "" | "replace" => Mode::Replace,
        "upsert" => Mode::Upsert,
        other => return Err(format!("filler.mode '{other}' is replace or upsert")),
    };
    let key = Some(text(o, "key"))
        .filter(|k| !k.is_empty())
        .map(|n| field_id(def, n, "key"))
        .transpose()?;
    let upcoming = Some(text(o, "upcoming"))
        .filter(|k| !k.is_empty())
        .map(|n| field_id(def, n, "upcoming"))
        .transpose()?;
    let kind = match source {
        Source::Agent { .. } => "agent",
        Source::Fetch {
            query_field: Some(_),
            ..
        } => "search",
        _ => "fetch",
    };
    let url = match &source {
        Source::Fetch { url, .. } => url.clone(),
        _ => None,
    };
    let title = match text(o, "title") {
        "" => default_title(url.as_deref(), kind),
        t if t.chars().count() > MAX_TITLE_CHARS => {
            return Err(format!(
                "filler.title is at most {MAX_TITLE_CHARS} characters"
            ));
        }
        t => t.to_owned(),
    };
    let per_row = matches!(
        &source,
        Source::Fetch {
            url_field: Some(_),
            ..
        } | Source::Fetch {
            query_field: Some(_),
            ..
        }
    );
    let refresh = match text(o, "refresh") {
        "" => None,
        _ if !per_row => {
            return Err("filler.refresh is for url_field and query_field fillers".into());
        }
        "once" => Some(Refresh::Once),
        "every" => Some(Refresh::Every),
        other => return Err(format!("filler.refresh '{other}' is once or every")),
    };
    let on_empty = match text(o, "on_empty") {
        "" | "keep" => OnEmpty::Keep,
        "clear" if per_row => OnEmpty::Clear,
        "clear" => {
            return Err("filler.on_empty is for url_field and query_field fillers".into());
        }
        other => return Err(format!("filler.on_empty '{other}' is keep or clear")),
    };
    Ok(Filler {
        id,
        title,
        source,
        mode,
        key,
        every_minutes: uint(o, "every_minutes")?,
        on_open_minutes: uint(o, "on_open_minutes")?,
        upcoming,
        refresh,
        on_empty,
    })
}

/// Check `def`'s fillers against its fields (also after other changes: a
/// filled field can't be removed or computed while a filler names it).
pub fn check(def: &Definition, max_fills_per_day: u32) -> Result<(), String> {
    if def.fillers.len() > MAX_FILLERS {
        return Err(format!("a collection has at most {MAX_FILLERS} fillers"));
    }
    for (i, f) in def.fillers.iter().enumerate() {
        if def.fillers[..i].iter().any(|o| o.id == f.id) {
            return Err(format!("two fillers have the id '{}'", f.id));
        }
        check_one(def, f, max_fills_per_day).map_err(|e| format!("filler '{}': {e}", f.title))?;
    }
    Ok(())
}

fn check_one(def: &Definition, f: &Filler, max_fills_per_day: u32) -> Result<(), String> {
    let live = |id: &str, role: &str| -> Result<&crate::model::Field, String> {
        match def.by_id(id) {
            Some(x) if x.live() => Ok(x),
            Some(x) => Err(format!(
                "its {role} '{}' is removed; change the filler first (apply_change set_filler or remove_filler)",
                x.name
            )),
            None => Err(format!("it names a {role} that doesn't exist")),
        }
    };
    let filled = f.filled();
    for id in &filled {
        let x = live(id, "field")?;
        if x.computed() {
            return Err(format!(
                "field '{}' is computed, so it can't be filled (fill its sources instead)",
                x.name
            ));
        }
        if x.kind == Kind::Ref {
            return Err(format!(
                "field '{}' is a ref, so it can't be filled (a page holds no row ids; fill a text field and look it up)",
                x.name
            ));
        }
    }
    if let Source::Fetch {
        details: Some(d), ..
    } = &f.source
    {
        if !f.mapped().contains(&d.link) {
            return Err("details.link must be one of the mapped fields (the row's page)".into());
        }
        if d.map
            .keys()
            .chain(d.text.iter())
            .any(|x| f.mapped().contains(x))
        {
            return Err(
                "a field filled by details can't be in map too (details would be overwritten)"
                    .into(),
            );
        }
    }
    for (u, role, what) in [
        (f.url_field(), "url_field", "URL"),
        (f.query_field(), "query_field", "search"),
    ] {
        let Some(u) = u else { continue };
        let x = live(u, role)?;
        if x.kind != Kind::Text {
            return Err(format!("its {role} '{}' must be a text field", x.name));
        }
        if filled.iter().any(|i| i == u) {
            return Err(format!(
                "'{}' holds each row's {what}, so it can't be filled too",
                x.name
            ));
        }
        if f.key.is_some() || f.mode != Mode::Replace {
            return Err(format!(
                "with {role} each row is filled from its own {what}: it takes no mode or key"
            ));
        }
    }
    if let Some(k) = &f.key {
        let x = live(k, "key")?;
        if !f.mapped().contains(k) {
            return Err(format!(
                "the key '{}' must be one of the mapped fields",
                x.name
            ));
        }
        if x.kind == Kind::Bool {
            return Err(format!("the key '{}' can't be true/false", x.name));
        }
    }
    if let Some(u) = &f.upcoming {
        let x = live(u, "upcoming")?;
        if !matches!(x.kind, Kind::Date | Kind::Datetime) || !f.mapped().contains(u) {
            return Err(format!(
                "upcoming '{}' must be a mapped date or datetime field",
                x.name
            ));
        }
    }
    if f.mode == Mode::Upsert && f.key.is_none() {
        return Err(
            "upsert needs a key: the mapped field that identifies a row (a URL, an id, a name)"
                .into(),
        );
    }
    // Rows it adds have only filled values (and defaults).
    if !f.per_row()
        && let Some(x) = def.live_fields().find(|x| {
            !filled.contains(&x.id) && !x.computed() && !x.optional && x.default.is_none()
        })
    {
        return Err(format!(
            "it adds rows, but field '{}' is required, has no default and isn't filled: make it optional, give it a default or fill it",
            x.name
        ));
    }
    match (f.uses_model(), f.every_minutes) {
        (true, Some(m)) if f.is_agent() && !(MIN_AGENT_EVERY..=MAX_EVERY).contains(&m) => {
            return Err(format!(
                "an agent fills at most hourly: every_minutes is {MIN_AGENT_EVERY}..={MAX_EVERY}"
            ));
        }
        (_, Some(m)) if !(MIN_EVERY..=MAX_EVERY).contains(&m) => {
            return Err(format!("every_minutes is {MIN_EVERY}..={MAX_EVERY}"));
        }
        _ => {}
    }
    if let Some(m) = f.on_open_minutes
        && !(MIN_ON_OPEN..=MAX_ON_OPEN).contains(&m)
    {
        return Err(format!("on_open_minutes is {MIN_ON_OPEN}..={MAX_ON_OPEN}"));
    }
    if let Source::Agent { fills_per_day, .. } = &f.source
        && *fills_per_day > max_fills_per_day
    {
        return Err(format!(
            "fills_per_day is at most {max_fills_per_day} on this pod (the operator's limit)"
        ));
    }
    Ok(())
}

/// Whether an app's or `edit_rows`' write to `def`'s rows may happen: it
/// may not set a field any filler fills; a row a `replace` filler owns
/// can't be deleted or restored by hand; and rows can't be added when every
/// filler owns its rows. `op`: create, update, delete, restore;
/// `row_filler`: the filler that wrote the row (an update, delete or
/// restore).
pub fn check_write(
    def: &Definition,
    write: &Map<String, Value>,
    op: &str,
    row_filler: Option<&str>,
) -> Result<(), String> {
    if def.fillers.is_empty() {
        return Ok(());
    }
    if op == "create" && def.fillers.iter().all(Filler::owns_rows) {
        return Err(format!(
            "the rows of '{}' come from its fillers, so they can't be added by hand (change a filler instead)",
            def.name
        ));
    }
    if matches!(op, "delete" | "restore")
        && let Some(f) = row_filler.and_then(|id| find(def, id))
        && f.owns_rows()
    {
        return Err(format!(
            "this row comes from the filler '{}', so it can't be deleted or restored by hand (change the filler instead)",
            f.title
        ));
    }
    for name in write.keys() {
        if let Some(x) = def.field(name)
            && let Some(f) = def.fillers.iter().find(|f| f.filled().contains(&x.id))
        {
            return Err(format!(
                "'{}' is filled by the filler '{}', so it can't be set by hand",
                x.name, f.title
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_field;

    fn products() -> Definition {
        let specs = [
            json!({"name": "name", "type": "text", "optional": true}),
            json!({"name": "url", "type": "text", "optional": true}),
            json!({"name": "price", "type": "number", "optional": true}),
            json!({"name": "bought", "type": "bool", "default": false}),
            json!({"name": "starts", "type": "datetime", "optional": true}),
            json!({"name": "about", "type": "text", "optional": true}),
        ];
        Definition {
            name: "products".into(),
            title: "Products".into(),
            fields: specs
                .iter()
                .enumerate()
                .map(|(i, s)| parse_field(s, format!("f{}", i + 1), 99).unwrap())
                .collect(),
            order_by: Vec::new(),
            descending: false,
            next_field: 7,
            reminders: Vec::new(),
            fillers: Vec::new(),
            alerts: Vec::new(),
            system: None,
            actions: Vec::new(),
        }
    }

    fn one(def: &Definition, spec: Value) -> Result<Filler, String> {
        parse(def, &spec, "s1".into(), false)
    }

    #[test]
    fn fetch_specs_resolve_fields_and_check_urls() {
        let mut def = products();
        let f = one(
            &def,
            json!({"kind": "fetch", "url": "https://www.shop.example/c", "extract": "jsonld",
                    "types": ["Product"], "map": {"name": "name", "price": "offers.price", "url": "url"},
                    "mode": "upsert", "key": "name", "every_minutes": 60,
                    "headers": {"Authorization": "Bearer {secret:shop_key}"},
                    "details": {"link": "url", "extract": "jsonld", "types": "Product", "map": {"about": "description"}}}),
        )
        .unwrap();
        assert_eq!(f.title, "shop.example");
        assert_eq!(
            f.mapped(),
            vec!["f1".to_owned(), "f2".to_owned(), "f3".to_owned()]
        );
        assert!(f.filled().contains(&"f6".to_owned()), "details fill too");
        assert_eq!(f.key.as_deref(), Some("f1"));
        assert_eq!(f.secrets(), vec!["shop_key".to_owned()]);
        def.fillers = vec![f.clone()];
        check(&def, 20).unwrap();
        let d = f.describe(&def);
        assert_eq!(d["map"]["price"], "offers.price");
        assert_eq!(d["details"]["map"]["about"], "description");
        assert_eq!(
            (d["key"].clone(), d["id"].clone()),
            (json!("name"), json!("s1"))
        );
        let back: Filler = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
        assert_eq!(back, f);
        let llm = one(
            &def,
            json!({"kind": "fetch", "url": "https://club.example/shows", "extract": "llm",
                   "fields": ["name", "starts"], "hint": "shows only", "zone": "America/Denver",
                   "upcoming": "starts", "title": "Club"}),
        )
        .unwrap();
        assert!(llm.is_llm() && llm.uses_model());
        assert_eq!(llm.describe(&def)["fields"], json!(["name", "starts"]));
        for bad in [
            json!({"kind": "fetch", "url": "http://127.0.0.1/x", "extract": "json", "map": {"name": "a"}}),
            json!({"kind": "fetch", "extract": "json", "map": {"name": "a"}}),
            json!({"kind": "fetch", "url": "https://a.example", "url_field": "url", "extract": "json", "map": {"name": "a"}}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "xml", "map": {"name": "a"}}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"nope": "a"}}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {}}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "jsonld", "map": {"name": "a"}}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "a..b"}}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "a"}, "headers": {"Host": "x"}}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "a"}, "mode": "merge"}),
            json!({"kind": "fetch", "url": "https://a.example", "extract": "events", "map": {"name": "a"}, "zone": "Nowhere/Land"}),
            json!({"kind": "agent"}),
            json!({"kind": "agent", "prompt": "x", "max_searches": 99}),
            json!({"kind": "script", "code": "1"}),
        ] {
            assert!(one(&def, bad.clone()).is_err(), "{bad}");
        }
        assert!(
            parse(&def, &json!({"kind": "fetch", "url": "http://127.0.0.1/x", "extract": "json", "map": {"name": "a"}}), "s2".into(), true)
                .is_ok()
        );
    }

    #[test]
    fn checks_against_the_definition() {
        let def = products();
        let with = |spec: Value| {
            let mut d = def.clone();
            d.fillers = vec![one(&d, spec)?];
            check(&d, 20)
        };
        // Upsert needs a key among the mapped fields.
        assert!(with(json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n"}, "mode": "upsert"})).is_err());
        assert!(with(json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n"}, "key": "price"})).is_err());
        // Each row from its own URL: the URL field isn't filled; no key.
        assert!(with(json!({"kind": "fetch", "url_field": "url", "extract": "jsonld", "types": ["Product"], "map": {"price": "offers.price"}})).is_ok());
        assert!(
            with(
                json!({"kind": "fetch", "url_field": "url", "extract": "json", "map": {"url": "u"}})
            )
            .is_err()
        );
        assert!(with(json!({"kind": "fetch", "url_field": "price", "extract": "json", "map": {"name": "u"}})).is_err());
        // upcoming must be a mapped date; details can't fill a mapped field.
        assert!(with(json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n"}, "upcoming": "name"})).is_err());
        assert!(with(json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n", "url": "u", "about": "a"},
            "details": {"link": "url", "extract": "jsonld", "types": "Event", "map": {"about": "description"}}})).is_err());
        // Schedules and budgets.
        assert!(with(json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n"}, "every_minutes": 5})).is_err());
        assert!(with(json!({"kind": "agent", "prompt": "monitors", "every_minutes": 30})).is_err());
        assert!(with(json!({"kind": "agent", "prompt": "monitors", "fills_per_day": 30})).is_err());
        assert!(
            with(json!({"kind": "agent", "prompt": "monitors", "on_open_minutes": 0})).is_err()
        );
        assert!(with(json!({"kind": "agent", "prompt": "monitors", "every_minutes": 1440, "key": "url", "mode": "upsert"})).is_ok());
        // A required unfilled field without a default: rows can't be added.
        let mut strict = def.clone();
        strict.fields[1].optional = false;
        strict.fillers = vec![one(&strict, json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n"}})).unwrap()];
        assert!(
            check(&strict, 20)
                .unwrap_err()
                .contains("'url' is required")
        );
    }

    #[test]
    fn filled_fields_and_owned_rows_are_locked() {
        let mut def = products();
        def.fillers = vec![one(
            &def,
            json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n", "price": "p"}}),
        )
        .unwrap()];
        let w = |v: Value| v.as_object().unwrap().clone();
        assert!(check_write(&def, &w(json!({"bought": true})), "update", Some("s1")).is_ok());
        assert!(check_write(&def, &w(json!({"price": 3})), "update", None).is_err());
        assert!(
            check_write(&def, &w(json!({})), "create", None).is_err(),
            "replace owns its rows"
        );
        assert!(check_write(&def, &w(json!({})), "delete", Some("s1")).is_err());
        assert!(
            check_write(&def, &w(json!({})), "delete", None).is_ok(),
            "a person's row"
        );
        // A second filler that fills rows from their own pages: people add.
        def.fillers.push(
            parse(
                &def,
                &json!({"kind": "fetch", "url_field": "url", "extract": "jsonld", "types": "Product", "map": {"about": "description"}}),
                "s2".into(),
                false,
            )
            .unwrap(),
        );
        assert!(
            check_write(
                &def,
                &w(json!({"url": "https://s.example/p"})),
                "create",
                None
            )
            .is_ok()
        );
        assert!(check_write(&def, &w(json!({"about": "x"})), "create", None).is_err());
    }
}
