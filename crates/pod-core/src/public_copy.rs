//! Public links (decision `public-web-links`): a layout and the rows it
//! shows, copied to the host for anyone with the link to look at in a
//! browser, read-only, without a pod. Only when the host allows it
//! (`CONSTRUCT_PUBLIC_WEB=1`).
//!
//! Privacy is storage (decision `sharing`): a visitor never reaches the pod.
//! Sharing copies the layout as the owner's device draws it (its plumbing,
//! components expanded) with its data, then takes out everything that could
//! reach further: what writes, what loads more, links to other layouts and
//! embeds of them, screens. Only Collections data goes, and of it only the
//! rows the copy lists and the fields it reads. The owner reviews the copy
//! (every text and value a visitor would read) before it goes out, and the
//! pod copies it again when its Collections data changes, so the page stays
//! current. Revoking takes the copy off the host.

use axum::http::HeaderMap;
use protocol::{DataBundle, EntityRecord};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

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

/// The largest copy (layout and data, as JSON).
pub const MAX_COPY_BYTES: usize = 512 * 1024;
/// Most public links a pod keeps live.
pub const MAX_LIVE: usize = 20;
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
    /// The rows it shows (entity URIs); empty: every row the layout shows.
    pub rows: Vec<String>,
    pub title: String,
    /// `{title, node, data, item?}`: what a visitor's page draws.
    pub copy: Value,
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

/// The intents a public page keeps: nothing that writes, loads, signs in or
/// opens another layout.
const KEPT_INTENTS: [&str; 3] = [
    "action.navigation.back",
    "action.local.set",
    "action.link.open",
];

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

/// Whether an intent (a node's `on_click`, …) may stay.
fn kept(intent: &Value) -> bool {
    match intent["intent"].as_str() {
        Some(i) if KEPT_INTENTS.contains(&i) => true,
        Some("action.sequence") => intent["steps"]
            .as_array()
            .is_some_and(|s| s.iter().all(kept)),
        _ => false,
    }
}

/// Whether a node carries an intent that may not stay.
fn has_dropped_intent(o: &Map<String, Value>) -> bool {
    o.iter().any(|(k, v)| {
        k != "item_template" && v.get("intent").is_some_and(|_| !kept(v))
            || v.as_array()
                .is_some_and(|a| a.iter().any(|x| x.get("intent").is_some_and(|_| !kept(x))))
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
/// go elsewhere taken out (a tappable keeps its content), embeds of other
/// layouts and screens dropped. `None`: the node goes.
pub fn strip(v: &Value, removed: &mut Removed) -> Option<Value> {
    match v {
        Value::Array(a) => Some(Value::Array(
            a.iter().filter_map(|x| strip(x, removed)).collect(),
        )),
        Value::Object(o) => {
            let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
            if ty == "core://Layout" {
                removed.embeds += 1;
                return None;
            }
            if has_dropped_intent(o) {
                if WRAPPERS.contains(&ty) {
                    removed.controls += 1;
                    return o.get("content").and_then(|c| strip(c, removed));
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
                if x.get("intent").is_some() && !kept(x) {
                    removed.controls += 1;
                    continue;
                }
                match strip(x, removed) {
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

/// The field names a node reads: `$item.<f>`, `#<f>}` and quoted names
/// (a list's `where`, `group_by`, `filter`).
fn fields_read(node: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let word = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    };
    for (i, _) in node.match_indices("$item.") {
        out.insert(word(&node[i + 6..]));
    }
    for (i, _) in node.match_indices('#') {
        out.insert(word(&node[i + 1..]));
    }
    for (i, _) in node.match_indices('"') {
        let w = word(&node[i + 1..]);
        if node[i + 1 + w.len()..].starts_with('"') {
            out.insert(w);
        }
    }
    out.remove("");
    out
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

/// The data a visitor gets: the rows the copy's lists show (only `rows`
/// when given), each with only the fields the node reads.
pub fn minimize(data: &DataBundle, node: &Value, rows: &[String]) -> DataBundle {
    let text = node.to_string();
    let read = fields_read(&text);
    let mut out = DataBundle::default();
    for (key, uris) in &data.queries {
        let kept: Vec<String> = uris
            .iter()
            .filter(|u| rows.is_empty() || rows.contains(u))
            .cloned()
            .collect();
        out.queries.insert(key.clone(), kept);
    }
    let listed: BTreeSet<&String> = out.queries.values().flatten().collect();
    if let Some(t) = data.entities.get(THEME) {
        out.entities.insert(THEME.to_owned(), t.clone());
    }
    for (uri, e) in &data.entities {
        let named = text.contains(uri.as_str()) || rows.contains(uri);
        if !(listed.contains(uri) || named) || uri.starts_with("core://") {
            continue;
        }
        let mut m = Map::new();
        if let Ok(Value::Object(all)) = serde_json::to_value(e) {
            for (f, v) in all {
                if f == "id" || read.contains(&f) {
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

/// The review id of a copy: what `confirm` must repeat.
fn review_id(copy: &Value) -> String {
    let h = Sha256::digest(copy.to_string().as_bytes());
    format!("review-{}", crate::util::hex(&h[..8]))
}

/// A layout copied for the public, with what was taken out.
pub struct Made {
    pub copy: Value,
    pub removed: Removed,
}

fn refuse(error: &str, detail: impl Into<String>) -> Value {
    json!({ "error": error, "detail": detail.into() })
}

/// Copies `layout` (as the owner's device draws it) for the public.
pub async fn make(
    pod: &Pod,
    layout: &str,
    rows: &[String],
    item: Option<&str>,
) -> Result<Made, Value> {
    let spec = crate::preview::fetch(pod, &HeaderMap::new(), layout).await?;
    let data = DataBundle {
        entities: spec.entities,
        queries: spec.queries,
    };
    let (allowed, names) = PublicData::of(pod);
    if let Err(other) = shareable(&data, &allowed) {
        return Err(refuse(
            "data_not_shareable",
            format!(
                "a public link carries only data its source lets out ({}); this layout draws {}",
                names.join(", "),
                other.join(", ")
            ),
        ));
    }
    let node =
        serde_json::to_value(&spec.workspace).map_err(|e| refuse("copy_failed", e.to_string()))?;
    let title = node["title"].as_str().unwrap_or(layout).to_owned();
    let mut removed = Removed::default();
    let node = strip(&node, &mut removed).unwrap_or(Value::Null);
    let mut rows: Vec<String> = rows.to_vec();
    if let Some(i) = item {
        rows.push(i.to_owned());
    }
    for r in &rows {
        if !data.entities.contains_key(r) {
            return Err(refuse(
                "unknown_row",
                format!(
                    "{r} is not a row this layout shows (rows are the entity URIs the layout's lists hold)"
                ),
            ));
        }
    }
    let data = minimize(&data, &node, &rows);
    let mut copy = json!({ "title": title, "node": node, "data": data });
    if let Some(i) = item {
        copy["item"] = json!(i);
    }
    if copy.to_string().len() > MAX_COPY_BYTES {
        return Err(refuse(
            "too_large",
            format!(
                "the copy is over {} KiB: share fewer rows",
                MAX_COPY_BYTES / 1024
            ),
        ));
    }
    Ok(Made { copy, removed })
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

fn answer(c: &PublicCopy, base: Option<&str>) -> Value {
    let path = format!("/s/{}", c.link);
    json!({
        "link": c.link,
        "url": base.map_or(path.clone(), |b| format!("{}{path}", b.trim_end_matches('/'))),
        "layout": c.layout,
        "rows": c.rows,
        "title": c.title,
        "updated_at": c.updated_at,
        "revoked": c.revoked_at.is_some(),
    })
}

/// The MCP tools (only listed while the host serves public links).
pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({ "name": "share_publicly",
            "description": "Make a page anyone can open in a browser, without a pod: a copy of one of the pod's layouts and the rows it shows, read-only (decision public-web-links). Without confirm nothing is shared: the answer is a review of everything a visitor would read (every text and value, every URL) and what was taken out (buttons that write, links to other layouts, embeds, screens); show it to the user and ask before confirming. With confirm (the review_id) it goes out and answers the page's url. The copy is refreshed when the data changes, so new rows the layout shows appear too; say so. rows limits it to some rows (one recipe: its entity URI); item draws the layout with that row as $item. Sharing the same layout and rows again updates the same link.",
            "inputSchema": { "type": "object", "properties": {
                "layout": { "type": "string", "description": "A layout id: home or a c-... id from list_layouts" },
                "rows": { "type": "array", "items": { "type": "string" }, "description": "Only these rows (entity URIs the layout's lists hold)" },
                "item": { "type": "string", "description": "A row (entity URI) the layout draws as $item" },
                "confirm": { "type": "string", "description": "The review_id of the review the user approved" } },
                "required": ["layout"] } }),
        json!({ "name": "list_public_links",
            "description": "The pod's public links (share_publicly): each link's url, layout, rows and when it was last copied; revoked ones too.",
            "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "unshare_publicly",
            "description": "Revoke a public link: the page stops working at once and its copy leaves the server.",
            "inputSchema": { "type": "object", "properties": {
                "link": { "type": "string", "description": "The link (pub-...) from list_public_links" } },
                "required": ["link"] } }),
    ]
}

/// The base URL visitors use, from the MCP request (or none).
fn base_url(headers: &HeaderMap) -> Option<String> {
    if let Ok(u) = std::env::var("CONSTRUCT_PUBLIC_URL")
        && !u.trim().is_empty()
    {
        return Some(u.trim().to_owned());
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
    Some(match name {
        "share_publicly" => share(pod, &pod_id, shelf.as_ref(), args, base.as_deref()).await,
        "list_public_links" => shelf
            .public_of_pod(&pod_id)
            .map(|v| json!({ "links": v.iter().map(|c| answer(c, base.as_deref())).collect::<Vec<_>>() }))
            .map_err(|e| refuse("shelf_error", format!("{e:?}"))),
        "unshare_publicly" => {
            let link = args["link"].as_str().unwrap_or("");
            match shelf.revoke_public(&pod_id, link) {
                Ok(true) => Ok(json!({ "ok": true, "link": link,
                    "detail": "the page no longer opens and its copy left the server" })),
                Ok(false) => Err(refuse("unknown_link", "no live public link of this pod has that link")),
                Err(e) => Err(refuse("shelf_error", format!("{e:?}"))),
            }
        }
        _ => return None,
    })
}

async fn share(
    pod: &Pod,
    pod_id: &str,
    shelf: &dyn Shelf,
    args: &Value,
    base: Option<&str>,
) -> Result<Value, Value> {
    let layout = args["layout"].as_str().unwrap_or("").to_owned();
    let mut rows: Vec<String> = args["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_owned))
        .collect();
    rows.sort();
    rows.dedup();
    let item = args["item"].as_str();
    let made = make(pod, &layout, &rows, item).await?;
    let id = review_id(&made.copy);
    if args["confirm"].as_str() != Some(id.as_str()) {
        let (texts, urls) = review_texts(&made.copy);
        let data = &made.copy["data"];
        let rows: Vec<&String> = data["entities"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(u, _)| !u.starts_with("core://"))
            .map(|(u, _)| u)
            .collect();
        let fields: BTreeSet<String> = data["entities"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(u, _)| !u.starts_with("core://"))
            .flat_map(|(_, e)| e.as_object().into_iter().flatten().map(|(k, _)| k.clone()))
            .collect();
        return Ok(json!({
            "review_id": id,
            "title": made.copy["title"],
            "shared": "nothing yet: confirm with this review_id once the user approves",
            "anyone_with_the_link_reads": {
                "rows": rows.len(),
                "fields": fields,
                "texts": texts,
                "urls": urls,
            },
            "taken_out": {
                "controls_that_write_or_go_elsewhere": made.removed.controls,
                "embedded_layouts": made.removed.embeds,
                "screens": made.removed.screens,
            },
            "updates": "the copy is made again whenever the data changes: new rows this layout shows will appear on the page too",
        }));
    }
    let item_key = item.map(str::to_owned);
    let mut key_rows = rows.clone();
    if let Some(i) = &item_key {
        key_rows.push(format!("item:{i}"));
    }
    let live = shelf
        .public_of_pod(pod_id)
        .map_err(|e| refuse("shelf_error", format!("{e:?}")))?;
    let existing = live
        .iter()
        .find(|c| c.revoked_at.is_none() && c.layout == layout && c.rows == key_rows);
    if existing.is_none() && live.iter().filter(|c| c.revoked_at.is_none()).count() >= MAX_LIVE {
        return Err(refuse(
            "too_many_links",
            format!("a pod keeps at most {MAX_LIVE} public links: unshare one first"),
        ));
    }
    let title = made.copy["title"].as_str().unwrap_or(&layout).to_owned();
    let c = shelf
        .put_public(PublicCopy {
            link: existing.map_or_else(new_link, |c| c.link.clone()),
            owner_pod: pod_id.to_owned(),
            layout,
            rows: key_rows,
            title,
            copy: made.copy,
            created_at: 0,
            updated_at: 0,
            revoked_at: None,
        })
        .map_err(|e| refuse("shelf_error", format!("{e:?}")))?;
    tracing::info!("a public link was shared");
    let mut out = answer(&c, base);
    out["detail"] =
        json!("anyone with this url can read the page, without a pod; unshare_publicly stops it");
    Ok(out)
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
        let mut rows = Vec::new();
        let mut item = None;
        for r in &c.rows {
            match r.strip_prefix("item:") {
                Some(i) => item = Some(i.to_owned()),
                None => rows.push(r.clone()),
            }
        }
        match make(pod, &c.layout, &rows, item.as_deref()).await {
            Ok(made) if made.copy != c.copy => {
                let _ = shelf.put_public(PublicCopy {
                    copy: made.copy,
                    ..c
                });
            }
            Ok(_) => {}
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
        let out = strip(&node, &mut removed).unwrap();
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
        let all = rows_of(&minimize(&data, &node, &[]));
        assert_eq!(all.len(), 2, "the diary isn't listed: it doesn't go");
        assert_eq!(
            all["notes://recipes/1"],
            json!({ "id": "1", "title": "Lentil soup", "minutes": 40 })
        );
        let one = minimize(&data, &node, &["notes://recipes/2".into()]);
        assert_eq!(
            one.queries["note:recipes"],
            vec!["notes://recipes/2".to_owned()]
        );
        assert_eq!(rows_of(&one).len(), 1);
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
        assert_eq!(review_id(&copy), review_id(&copy.clone()));
    }
}
