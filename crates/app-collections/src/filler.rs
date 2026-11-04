//! Fillers (decision `collection-fillers`): what keeps a collection's rows
//! current without anyone writing them. A filler is part of the definition
//! (data, not code: Claude declares it, the pod runs it, [`crate::fill`]):
//!
//! - `fetch`: a declarative recipe. A URL (or each row's URL in a field),
//!   an extractor ([`web_extract::items`]: json, embedded-json, jsonld,
//!   rss, ics, selectors) and a map from fields to paths in each item.
//! - `agent`: a model with web search and fetch, a prompt and a budget,
//!   answering rows that fit the collection's fields.
//!
//! Rows are written by `mode`: `replace` (the rows are the latest answer;
//! with a `key`, rows keep their ids and unfilled fields) or `upsert` by
//! `key` (new keys add rows, known keys update theirs, rows not in the
//! answer stay). The fields a filler writes are *filled*: apps and
//! `edit_rows` can't set them, and in `replace` mode they can't add or
//! delete rows either, so a fill never overwrites a person's edit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::model::{Definition, Kind};

/// Most map entries of a recipe.
pub const MAX_MAP: usize = 40;
/// Most request headers of a recipe.
pub const MAX_HEADERS: usize = 10;
/// Longest header value.
pub const MAX_HEADER_CHARS: usize = 1_000;
/// Longest agent prompt.
pub const MAX_PROMPT_CHARS: usize = 2_000;
/// Interval bounds, minutes (as Watcher targets).
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

/// What fills the rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
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
        /// Request headers; values may name a secret, `{secret:<name>}`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        /// The extractor ([`web_extract::items::Kind`]).
        extract: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        items: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        types: Vec<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        script: String,
        /// Field id -> path (or CSS selector) in each item.
        map: BTreeMap<String, String>,
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

/// A collection's filler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filler {
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

    /// The field ids it writes.
    pub fn filled(&self) -> Vec<String> {
        match &self.source {
            Source::Fetch { map, .. } => map.keys().cloned().collect(),
            Source::Agent { fields, .. } => fields.clone(),
        }
    }

    /// The field id of each row's own URL (`fetch` with `url_field`).
    pub fn url_field(&self) -> Option<&str> {
        match &self.source {
            Source::Fetch { url_field, .. } => url_field.as_deref(),
            Source::Agent { .. } => None,
        }
    }

    /// Whether the rows belong to the filler: it adds and removes them
    /// (`replace`, not filling rows from their own URLs).
    pub fn owns_rows(&self) -> bool {
        self.mode == Mode::Replace && self.url_field().is_none()
    }

    /// Every field id it reads or writes.
    pub fn sources(&self) -> Vec<String> {
        let mut out = self.filled();
        out.extend(self.url_field().map(str::to_owned));
        out.extend(self.key.clone());
        out
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

    /// The extractor spec of a `fetch` recipe.
    pub fn extract_spec(&self) -> Option<web_extract::items::Spec> {
        match &self.source {
            Source::Fetch {
                extract,
                items,
                types,
                script,
                ..
            } => Some(web_extract::items::Spec {
                kind: web_extract::items::Kind::parse(extract)?,
                items: items.clone(),
                types: types.clone(),
                script: script.clone(),
            }),
            Source::Agent { .. } => None,
        }
    }

    /// As a spec with field names, for Claude.
    pub fn describe(&self, def: &Definition) -> Value {
        let name = |id: &str| def.name_of(id);
        let mut v = match &self.source {
            Source::Fetch {
                url,
                url_field,
                headers,
                extract,
                items,
                types,
                script,
                map,
            } => {
                let mut v = json!({ "kind": "fetch", "extract": extract });
                if let Some(u) = url {
                    v["url"] = json!(u);
                }
                if let Some(f) = url_field {
                    v["url_field"] = json!(name(f));
                }
                if !headers.is_empty() {
                    v["headers"] = json!(headers);
                }
                if !items.is_empty() {
                    v["items"] = json!(items);
                }
                if !types.is_empty() {
                    v["types"] = json!(types);
                }
                if !script.is_empty() {
                    v["script"] = json!(script);
                }
                v["map"] = Value::Object(
                    map.iter()
                        .map(|(id, p)| (name(id), json!(p)))
                        .collect::<Map<String, Value>>(),
                );
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
        v
    }
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

/// Parse a filler spec (field names, resolved against `def`); `null`: no
/// filler. `allow_private`: URLs may name private hosts (tests, the
/// operator's choice).
pub fn parse(def: &Definition, v: &Value, allow_private: bool) -> Result<Option<Filler>, String> {
    let o = match v {
        Value::Null => return Ok(None),
        Value::Object(o) => o,
        _ => return Err(
            "filler is {kind: \"fetch\", url | url_field, extract, map, ...}, {kind: \"agent\", prompt, ...} or null".into(),
        ),
    };
    let field_id = |n: &str, role: &str| -> Result<String, String> {
        def.field(n).map(|f| f.id.clone()).ok_or_else(|| {
            format!(
                "filler {role} '{n}' is not one of the fields ({})",
                def.live_fields()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
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
                .map(|n| field_id(n, "url_field"))
                .transpose()?;
            if url.is_some() == url_field.is_some() {
                return Err("a fetch filler has either \"url\" (one page) or \"url_field\" (each row's own page)".into());
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
            let Some(kind) = web_extract::items::Kind::parse(extract) else {
                return Err(format!(
                    "filler.extract is one of {}",
                    web_extract::items::Kind::ALL.join(", ")
                ));
            };
            let types = match o.get("types") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::String(s)) => vec![s.trim().to_owned()],
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|t| t.as_str().map(|s| s.trim().to_owned()))
                    .collect::<Option<Vec<_>>>()
                    .ok_or("filler.types is a list of schema.org type names")?,
                Some(_) => return Err("filler.types is a list of schema.org type names".into()),
            };
            let Some(Value::Object(m)) = o.get("map") else {
                return Err(
                    "filler.map is {field: path} (a CSS selector for the selectors extractor)"
                        .into(),
                );
            };
            if m.is_empty() || m.len() > MAX_MAP {
                return Err(format!("filler.map has 1..={MAX_MAP} fields"));
            }
            let mut map = BTreeMap::new();
            let mut pairs = Vec::new();
            for (n, p) in m {
                let Some(p) = p.as_str() else {
                    return Err(format!("filler.map.{n} is a path (text)"));
                };
                let id = field_id(n, "map field")?;
                pairs.push((n.clone(), p.trim().to_owned()));
                map.insert(id, p.trim().to_owned());
            }
            let spec = web_extract::items::Spec {
                kind,
                items: text(o, "items").to_owned(),
                types: types.clone(),
                script: text(o, "script").to_owned(),
            };
            web_extract::items::check(&spec, &pairs).map_err(|e| format!("filler: {e}"))?;
            Source::Fetch {
                url,
                url_field,
                headers,
                extract: kind.name().to_owned(),
                items: spec.items,
                types,
                script: spec.script,
                map,
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
                        let id = field_id(n.as_str().unwrap_or(""), "field")?;
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
        .map(|n| field_id(n, "key"))
        .transpose()?;
    let filler = Filler {
        source,
        mode,
        key,
        every_minutes: uint(o, "every_minutes")?,
        on_open_minutes: uint(o, "on_open_minutes")?,
    };
    Ok(Some(filler))
}

/// Check `def`'s filler against its fields (also after other changes: a
/// filled field can't be removed or computed while the filler names it).
pub fn check(def: &Definition, max_fills_per_day: u32) -> Result<(), String> {
    let Some(f) = &def.filler else { return Ok(()) };
    let live = |id: &str, role: &str| -> Result<&crate::model::Field, String> {
        match def.by_id(id) {
            Some(x) if x.live() => Ok(x),
            Some(x) => Err(format!(
                "the filler's {role} '{}' is removed; change the filler first (apply_change set_filler or remove_filler)",
                x.name
            )),
            None => Err(format!("the filler names a {role} that doesn't exist")),
        }
    };
    let filled = f.filled();
    for id in &filled {
        let x = live(id, "field")?;
        if x.computed() {
            return Err(format!(
                "field '{}' is computed, so the filler can't fill it (fill its sources instead)",
                x.name
            ));
        }
    }
    if let Some(u) = f.url_field() {
        let x = live(u, "url_field")?;
        if x.kind != Kind::Text {
            return Err(format!(
                "the filler's url_field '{}' must be a text field",
                x.name
            ));
        }
        if filled.iter().any(|i| i == u) {
            return Err(format!(
                "'{}' holds each row's URL, so the filler can't fill it too",
                x.name
            ));
        }
        if f.key.is_some() || f.mode != Mode::Replace {
            return Err(
                "a filler with url_field fills each row from its own URL: it takes no mode or key"
                    .into(),
            );
        }
    }
    if let Some(k) = &f.key {
        let x = live(k, "key")?;
        if !filled.contains(k) {
            return Err(format!(
                "the key '{}' must be one of the filled fields",
                x.name
            ));
        }
        if x.kind == Kind::Bool {
            return Err(format!("the key '{}' can't be true/false", x.name));
        }
    }
    if f.mode == Mode::Upsert && f.key.is_none() {
        return Err(
            "upsert needs a key: the filled field that identifies a row (a URL, an id, a name)"
                .into(),
        );
    }
    // Rows it adds have only filled values (and defaults).
    if f.url_field().is_none()
        && let Some(x) = def.live_fields().find(|x| {
            !filled.contains(&x.id) && !x.computed() && !x.optional && x.default.is_none()
        })
    {
        return Err(format!(
            "the filler adds rows, but field '{}' is required, has no default and isn't filled: make it optional, give it a default or fill it",
            x.name
        ));
    }
    match (f.is_agent(), f.every_minutes) {
        (true, Some(m)) if !(MIN_AGENT_EVERY..=MAX_EVERY).contains(&m) => {
            return Err(format!(
                "an agent fills at most hourly: every_minutes is {MIN_AGENT_EVERY}..={MAX_EVERY}"
            ));
        }
        (false, Some(m)) if !(MIN_EVERY..=MAX_EVERY).contains(&m) => {
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
/// may not set a filled field, and in a collection whose rows the filler
/// owns it may not add, delete or restore rows. `op`: create, update,
/// delete, restore.
pub fn check_write(def: &Definition, write: &Map<String, Value>, op: &str) -> Result<(), String> {
    let Some(f) = &def.filler else { return Ok(()) };
    if f.owns_rows() && op != "update" {
        return Err(format!(
            "the rows of '{}' come from its {} filler, so they can't be added, deleted or restored by hand (change the filler instead)",
            def.name,
            f.kind()
        ));
    }
    let filled = f.filled();
    for name in write.keys() {
        if let Some(x) = def.field(name)
            && filled.contains(&x.id)
        {
            return Err(format!(
                "'{}' is filled by the collection's {} filler, so it can't be set by hand",
                x.name,
                f.kind()
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
            next_field: 5,
            reminders: Vec::new(),
            filler: None,
        }
    }

    #[test]
    fn fetch_specs_resolve_fields_and_check_urls() {
        let mut def = products();
        let f = parse(
            &def,
            &json!({"kind": "fetch", "url": "https://shop.example/c", "extract": "jsonld",
                    "types": ["Product"], "map": {"name": "name", "price": "offers.price"},
                    "mode": "upsert", "key": "name", "every_minutes": 60,
                    "headers": {"Authorization": "Bearer {secret:shop_key}"}}),
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(f.filled(), vec!["f1".to_owned(), "f3".to_owned()]);
        assert_eq!(f.key.as_deref(), Some("f1"));
        assert_eq!(f.secrets(), vec!["shop_key".to_owned()]);
        def.filler = Some(f.clone());
        check(&def, 20).unwrap();
        assert_eq!(f.describe(&def)["map"]["price"], "offers.price");
        assert_eq!(f.describe(&def)["key"], "name");
        let back: Filler = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
        assert_eq!(back, f);
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
            json!({"kind": "agent"}),
            json!({"kind": "agent", "prompt": "x", "max_searches": 99}),
            json!({"kind": "script", "code": "1"}),
        ] {
            assert!(parse(&def, &bad, false).is_err(), "{bad}");
        }
        assert!(parse(&def, &json!({"kind": "fetch", "url": "http://127.0.0.1/x", "extract": "json", "map": {"name": "a"}}), true).is_ok());
    }

    #[test]
    fn checks_against_the_definition() {
        let def = products();
        let with = |spec: Value| {
            let mut d = def.clone();
            d.filler = parse(&d, &spec, false).unwrap();
            check(&d, 20)
        };
        // Upsert needs a key among the filled fields.
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
        strict.filler = parse(&strict, &json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n"}}), false).unwrap();
        assert!(
            check(&strict, 20)
                .unwrap_err()
                .contains("'url' is required")
        );
    }

    #[test]
    fn filled_fields_are_locked() {
        let mut def = products();
        def.filler = parse(
            &def,
            &json!({"kind": "fetch", "url": "https://a.example", "extract": "json", "map": {"name": "n", "price": "p"}}),
            false,
        )
        .unwrap();
        let w = |v: Value| v.as_object().unwrap().clone();
        assert!(check_write(&def, &w(json!({"bought": true})), "update").is_ok());
        assert!(check_write(&def, &w(json!({"price": 3})), "update").is_err());
        assert!(
            check_write(&def, &w(json!({})), "create").is_err(),
            "replace owns its rows"
        );
        assert!(check_write(&def, &w(json!({})), "delete").is_err());
        def.filler = parse(
            &def,
            &json!({"kind": "fetch", "url_field": "url", "extract": "jsonld", "types": "Product", "map": {"price": "offers.price"}}),
            false,
        )
        .unwrap();
        assert!(check_write(&def, &w(json!({"url": "https://s.example/p"})), "create").is_ok());
        assert!(check_write(&def, &w(json!({})), "delete").is_ok());
        assert!(check_write(&def, &w(json!({"price": 1})), "create").is_err());
    }
}
