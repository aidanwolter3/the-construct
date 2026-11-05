//! The Collections MCP tools (decision `collections`): Claude declares
//! collections, changes them together with the apps that show them, and
//! edits rows (confirmed, undoable). This is the one place an MCP
//! credential touches app data, and only Collections' own tables.

use std::collections::HashMap;

use axum::http::StatusCode;
use pod_core::custom::{self, Writer};
use pod_core::{BoxFuture, Pod};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::changes::{self, Planned};
use crate::db::{self, Row};
use crate::model::{self, Definition, Write};
use crate::{CollectionsAdapter, KEEP_SECS, NAME, fail, refusable};

pub const INSTRUCTIONS: &str = "Data a custom app keeps itself (a todo list, a reading log) lives in \
Collections: define_collection declares typed fields, and any custom app can then list the collection \
as a feed ([\"collections\", \"<name>\"]) and add, change and delete rows with the usual mutation \
intents (describe_collection gives ready-made layout snippets). Change a collection with apply_change, \
sending the app layout changes that go with it in the same call: nothing applies if any app would \
break. add_row adds one row the user just asked for (\"log that I had the oil changed\") at once. \
edit_rows changes rows directly (imports, backfills): it first answers a preview, which you \
must show the user (the count and the sample) before calling again with its confirm token; every edit \
can be undone with undo_edit. Any field may be computed from the row's other fields \
whenever they change: compute: {kind: \"expr\", expr} evaluates a small expression at once (entity.<field>, \
literals, + joins text or adds numbers, -, !, c ? a : b, parentheses); compute: {kind: \"lookup\", from, \
collection, key, value, match: exact|contains|regex, learn?} takes the value of the best matching row of \
another collection, kept as editable rules (the longest key wins); and compute: {kind: \"model\", from: \
[fields], prompt} has a model answer the prompt about those fields in the background (a choice answers one \
of its options; each input is asked once, and a value set by hand is kept and remembered for that input). \
compute may be a list tried in order, e.g. [lookup with learn: true, model]: rules first, the model only \
when none matches, and its answers (and hand edits) are added as rules. After editing rules, apply_change \
{op: recompute_field} computes the field again for every row. \
order_by may list several fields ([\"section\", \"done\"]; a choice sorts by the order of its options). \
A collection can remind the user: apply_change {op: add_reminder, field: <a date field>, title: \"Due today: \
{title}\", unless: {field: \"done\", equals: true}, at?: \"09:00\"} sends a notification from the app showing \
the collection once a row's date is reached (at that time of day in the user's time zone), unless the row \
then matches unless. Set the zone first with set_time_zone (ask the user if you don't know it). \
A collection can keep itself current from the web with fillers (define_collection fillers, or apply_change \
{op: set_filler}; several per collection, e.g. one per website, each writing only its own rows): {kind: \
\"fetch\", url, extract: events|json|embedded-json|jsonld|rss|ics|selectors|llm, items?, types?, map: \
{field: path}} is a recipe the pod runs on a schedule; prefer it: try the page with collections_preview \
first (without map it shows sample items to write paths from) and pick the cheapest extractor that gives \
the fields: events (an ics feed or a page's schema.org events, normalized), an API's json, jsonld, rss or \
ics, embedded-json, CSS selectors, and only then llm (a model reads each changed page; fields and hint \
instead of map). {kind: \"agent\", prompt} has a model with web search answer the rows (each fill costs \
model calls; budgeted by fills_per_day): use it only when no page holds the data. Combine them for \
shopping: an agent finds the items once, a fetch filler with url_field keeps each row's price current. \
every_minutes schedules fills; on_open_minutes fills when the app opens and the data is older; upcoming \
(a date field) drops past items; zone places times without an offset; details reads each new row's own \
page. A datetime field (2026-11-06T19:30-07:00) is also served as <name>_date, <name>_time and \
<name>_text. Alerts (apply_change {op: add_alert, any?, all?, none?, filler?, days?, within_days?}) notify \
once when a row matches, e.g. a show listing a team. collections_status shows each filler's health: say so \
when one is failing or finds nothing. An API key goes in with collections_set_secret (with the host it is for) and is sent as a \
header value \"{secret:<name>}\", only to that host; ask the user for it and never repeat it. The Events app (app events) \
shows the collection events: to watch a website for events, add a filler there (extract events, map \
{source_id: id, title: title, start: start, end: end, venue: venue, url: url, tickets_url: tickets_url}, \
key source_id, upcoming start, every_minutes 360) and alerts for what the user wants to hear about.";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

pub fn tools() -> Vec<Value> {
    let collection = json!({ "type": "string", "description": "A collection's name" });
    let note = json!({ "type": "string", "description": "What changed, in the user's words (shown in the history)" });
    let kind = json!({ "type": "object",
        "properties": { "kind": { "type": "string", "enum": ["expr", "lookup", "model"] }, "expr": { "type": "string" },
            "from": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] },
            "prompt": { "type": "string" }, "collection": { "type": "string" }, "key": { "type": "string" },
            "value": { "type": "string" }, "match": { "type": "string", "enum": model::Match::ALL },
            "learn": { "type": "boolean" } }, "required": ["kind"] });
    let compute = json!({
        "description": "The pod computes the value from the row's other fields (not from other computed fields), again whenever they change; a value written by hand stays until they change. One kind, or a list of kinds tried in order (the first with an answer wins; a model is last). kind \"lookup\": {kind, from: field, collection, key, value, match?: exact (the default; ignores case and spacing) | contains (the key's whole words, ignoring case) | regex (case-insensitive, at most 200 characters; an invalid one is skipped and listed by describe_collection), learn?}: the value field of the row of another collection whose key (a text field) matches this row's from field, the longest matching key winning; evaluated in the write; not recomputed when the rules change (apply_change recompute_field does). learn: true adds a rule (key: the input, lowercase with spaces collapsed; for match regex, that text escaped and anchored) when a later kind (the model) answers, and a hand edit of the field adds or changes that rule. Example: [{kind: \"lookup\", from: \"name\", collection: \"section_rules\", key: \"pattern\", value: \"section\", match: \"contains\", learn: true}, {kind: \"model\", from: \"name\", prompt: \"...\"}]. kind \"expr\": {kind, expr}, e.g. \"entity.quantity ? entity.name + \\\" (\\\" + entity.quantity + \\\")\\\" : entity.name\" (entity.<field>, JSON literals, + joins text or adds numbers, -, !, c ? a : b, parentheses; an empty value is \"\" or 0 and false in a condition), evaluated in the write. kind \"model\": {kind, from: field or [fields], prompt}, e.g. {from: \"name\", prompt: \"Which grocery store section is this item in?\"}: a model answers in the background (a choice: one of its options; text: trimmed), each input once; a value set by hand is remembered as the answer for that input. Needs optional or a default (what the row reads until computed). null: stop computing.",
        "anyOf": [kind, { "type": "array", "items": kind }, { "type": "null" }] });
    let field = json!({ "type": "object", "description": "{name, type: text|number|bool|date|datetime|choice, options? (choice), optional?, default?, compute?}",
        "properties": { "name": { "type": "string" }, "type": { "type": "string", "enum": model::Kind::ALL },
            "options": { "type": "array", "items": { "type": "string" } }, "optional": { "type": "boolean" }, "default": {},
            "compute": compute },
        "required": ["name", "type"] });
    let order = json!({ "description": "A field, or a list of fields (first key first), rows are listed by (else oldest first). A choice sorts by the order of its options; empty values last.",
        "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] });
    let filler = json!({ "type": "object",
        "description": "A filler: what keeps rows current from the web (decision collection-fillers); each writes only its own rows. kind \"fetch\" (a recipe, no model call per fill): {kind, title?, url (one page) | url_field (a text field holding each row's own page; that row is filled from it), extract: events (an ics feed or a page's schema.org events: items {id, title, start, end, venue, description, url, tickets_url}) | json | embedded-json (a page's <script id=script, default __NEXT_DATA__>) | jsonld (schema.org objects of types, e.g. [\"Product\"]) | rss (items {title, link, description, published, updated, id, author, category}) | ics (VEVENT properties lowercase: summary, dtstart, location, ...) | selectors (CSS) | llm (a model reads each changed page's text: fields? [names] and hint? instead of map), items? (json/embedded-json: a path to the list; selectors: a CSS selector per item), types?, script?, zone? (IANA, for times without an offset), map: {field: path} (paths: a.b, a[0], a[*]; a list key is looked into, so offers.price works; selectors: a CSS selector inside the item, sel@attr for an attribute, @attr for the item's own), headers? {name: value, value may be \"Bearer {secret:<name>}\"}, details? {link: a mapped field with the row's page, extract, types?, map: {field: path} (fields not in map), text?: a field for the page's text when nothing is found}: read each new row's own page}. kind \"agent\" (a model with web search and fetch answers the rows; costs model calls): {kind, title?, prompt, fields? (default every field that isn't computed), max_searches? (default 5, at most 20), max_fetches? (default 10, at most 30), fills_per_day? (default 4)}. Both: mode? replace (default: its rows are the latest answer; rows it no longer lists are deleted; they can't be added or deleted by hand) | upsert (adds and updates rows by key, keeps others; people may add rows), key? (a mapped field identifying a row; with replace, rows keep their ids and unfilled fields), upcoming? (a mapped date or datetime field: past items are left out), every_minutes? (fetch: 15..10080; agent: 60..10080), on_open_minutes? (1..1440: fill when an app showing it opens and the last fill is older). Values are coerced to the fields' types (prices like \"$1,299\" and feed dates are read); a value that doesn't fit is skipped. Filled fields can't be set by hand." });
    let alert = json!({ "type": "object",
        "description": "An alert: notify once when a row matches. {name?, any?: [phrases] (one must appear), all?: [phrases], none?: [phrases], fields?: [text fields searched; default all text fields], filler?: a filler id (only its rows), days?: [mon..sun], within_days?, date_field? (default the first date or datetime field; past rows stop matching), title?, body? (templates: {field}, {start_text}, {alert}, {filler}), label?, app?}" });
    let apps = json!({ "type": "array", "description": "Custom app layouts that change with it: [{app, layout | json_patch, base_version?, note?}]",
        "items": { "type": "object", "properties": { "app": { "type": "string" }, "layout": { "type": "object" },
            "json_patch": { "type": "array", "items": { "type": "object" } }, "base_version": { "type": "integer" }, "note": { "type": "string" } },
            "required": ["app"] } });
    vec![
        tool(
            "list_collections",
            "The pod's collections: fields, row counts, versions and the custom apps that use each.",
            json!({}),
            &[],
        ),
        tool(
            "define_collection",
            "Declare a new collection (name [a-z][a-z0-9_]*, permanent) with typed fields. Custom apps then use it as a feed [\"collections\", name].",
            json!({ "name": collection, "title": { "type": "string" },
                    "fields": { "type": "array", "items": field },
                    "order_by": order,
                    "descending": { "type": "boolean" },
                    "fillers": { "type": "array", "items": filler.clone() },
                    "alerts": { "type": "array", "items": alert },
                    "note": note }),
            &["name", "fields"],
        ),
        tool(
            "describe_collection",
            "A collection's definition (live and removed fields, aliases), version history, row count, values kept aside, recent row edits, the apps using it, and layout snippets for listing, adding, ticking and deleting rows.",
            json!({ "collection": collection, "version": { "type": "integer", "description": "An older definition version" } }),
            &["collection"],
        ),
        tool(
            "apply_change",
            "Change a collection's definition and the custom apps that show it in one step. changes: [{op: add_field, name, type, options?, optional?, default?, compute?} | {op: rename_field, field, to} | {op: remove_field, field} | {op: restore_field, field} | {op: update_field, field, type?, options?, optional?, default?, compute?} | {op: recompute_field, field} | {op: set_order, field?: name or [names], descending?} | {op: set_title, title} | {op: add_reminder, field, title, body?, at?, unless?, label?, app?} | {op: update_reminder, reminder: id, ...} | {op: remove_reminder, reminder: id} | {op: set_filler, filler} | {op: remove_filler, filler: id} | {op: add_alert, ...} | {op: update_alert, alert: id, ...} | {op: remove_alert, alert: id}]. set_filler adds a filler (as define_collection's fillers), or changes the one filler.id names, and starts its fill; remove_filler stops one (its rows stay, no longer locked; its alerts go). add_alert notifies once per row when a row matches (as define_collection's alerts). describe_collection lists fillers and alerts with their ids. A reminder notifies the user once per row when its date field is reached: on that date at at (HH:MM, default 09:00) in the pod's time zone (set_time_zone), unless the row then has unless {field, equals} (e.g. {field: \"done\", equals: true}); dates before the day it was added never notify, and a reminder more than 7 days late is dropped. title and body are the notification's text with {field} replaced by the row's value; label names it in the user's notification settings (default \"<Title>: <field>\"); app (a custom app id) is the app it comes from and opens, default the oldest custom app with the collection as a feed (with none, nothing is sent). describe_collection lists reminders with their ids. compute (one kind or a list tried in order: {kind: \"expr\", expr}, {kind: \"lookup\", from, collection, key, value, match?, learn?} or {kind: \"model\", from, prompt}, as in define_collection) is computed for existing rows too (a model only fills empty ones); null stops computing and keeps the values. A lookup is not recomputed when its rules change: after editing them, recompute_field computes the field again for every row (a row only the model could answer keeps its value). Every custom app using Collections is checked against the new definition; if any would break, nothing applies and the error names the app and the binding. Renamed fields keep their old name as an alias; removed fields are hidden and restorable for 30 days; type changes convert row by row and keep values that don't convert aside.",
            json!({ "collection": collection, "base_version": { "type": "integer", "description": "The definition version you read" },
                    "changes": { "type": "array", "items": { "type": "object" } }, "apps": apps, "note": note }),
            &["collection", "changes"],
        ),
        tool(
            "restore_collection_version",
            "Make an older definition current again (appends a copy): removed fields come back with their data, fields added since are soft-removed, types convert back (values kept aside return). Pass the app layouts that change with it, as for apply_change.",
            json!({ "collection": collection, "version": { "type": "integer" }, "apps": apps, "note": note }),
            &["collection", "version"],
        ),
        tool(
            "edit_rows",
            "Change rows directly, for imports, backfills and conversions: edits [{op: create, values: {field: value}} | {op: update, id, values} | {op: delete, id} | {op: restore, id}]. The first call answers a preview (count and sample) and a confirm token and changes nothing: show the preview to the user, and only after they agree call again with the same edits and confirm. Undo with undo_edit.",
            json!({ "collection": collection, "edits": { "type": "array", "items": { "type": "object" } },
                    "note": note, "confirm": { "type": "string", "description": "The token of the preview the user agreed to" } }),
            &["collection", "edits"],
        ),
        tool(
            "add_row",
            "Add one row the user just asked for in this conversation (\"log that I had the oil changed\", \"add milk\"): written at once, no preview, values as for a create in edit_rows (fields left out get their default; dates are YYYY-MM-DD). Answers the row id and an edit id for undo_edit. For several rows, imports or changes to existing rows, use edit_rows.",
            json!({ "collection": collection, "values": { "type": "object", "description": "{field: value}" }, "note": note }),
            &["collection", "values"],
        ),
        tool(
            "undo_edit",
            "Undo one edit_rows or add_row edit as one step (rows changed since are left alone and reported).",
            json!({ "edit": { "type": "string", "description": "The edit id edit_rows answered" } }),
            &["edit"],
        ),
        tool(
            "collections_preview",
            "Try a fetch recipe now and store nothing: fetch the page, extract and map, and answer the rows (or, without map, sample items to write the paths from). Pass the recipe's {url, extract, items?, types?, script?, zone?, map?, headers?}, or collection (and filler, an id; default its first) to try that filler (a url_field filler tries its first row's page). Use it before set_filler, and when collections_status says a filler finds nothing.",
            json!({ "collection": collection, "filler": { "type": "string" }, "url": { "type": "string" },
                    "extract": { "type": "string", "enum": web_extract::items::Kind::ALL }, "zone": { "type": "string" },
                    "items": { "type": "string" }, "types": { "type": "array", "items": { "type": "string" } },
                    "script": { "type": "string" }, "map": { "type": "object", "description": "{name: path}" },
                    "headers": { "type": "object" } }),
            &[],
        ),
        tool(
            "collections_status",
            "Each filler's health (of one collection, or all): state (pending, ok, failing, empty, off, unavailable), detail, last and next fill, items found, model fills today, and its alerts. Say so when one is failing or finds no items.",
            json!({ "collection": collection }),
            &[],
        ),
        tool(
            "collections_refresh",
            "Fill a collection's fillers now (or one: filler, an id), refetching even an unchanged page, and answer what changed. A model filler (agent or llm) runs in the background and counts toward its budget.",
            json!({ "collection": collection, "filler": { "type": "string" } }),
            &["collection"],
        ),
        tool(
            "collections_set_secret",
            "Store a value a fetch recipe sends in a header (an API key), sealed on the pod: headers name it as \"{secret:<name>}\". It is never shown back, and is sent only to requests for host (the API's host name, e.g. api.example.com; ask the user which site the key is for). value null forgets it. Answers the stored names.",
            json!({ "name": { "type": "string", "description": "[a-z][a-z0-9_]*" },
                    "value": { "type": ["string", "null"] },
                    "host": { "type": "string", "description": "The host name the secret may be sent to (required with a value)" } }),
            &["name", "value"],
        ),
        tool(
            "export_collection",
            "Everything in a collection, deleted rows, removed fields' values and values kept aside included: the pod is the only copy of this data.",
            json!({ "collection": collection }),
            &["collection"],
        ),
    ]
}

pub fn call<'a>(
    adapter: &'a CollectionsAdapter,
    pod: &'a Pod,
    tool: &'a str,
    args: &'a Value,
) -> Option<BoxFuture<'a, Result<Value, Value>>> {
    let caps = adapter.caps;
    Some(match tool {
        "list_collections" => Box::pin(list(pod)),
        "define_collection" => Box::pin(define(pod, adapter, args)),
        "describe_collection" => Box::pin(describe(pod, args)),
        "apply_change" => Box::pin(apply_change(pod, adapter, args)),
        "collections_preview" => Box::pin(preview(pod, adapter, args)),
        "collections_status" => Box::pin(status(pod, args)),
        "collections_refresh" => Box::pin(refresh(pod, args)),
        "collections_set_secret" => Box::pin(set_secret(pod, args)),
        "restore_collection_version" => Box::pin(restore_version(pod, caps, args)),
        "edit_rows" => Box::pin(edit_rows(pod, caps, args)),
        "add_row" => Box::pin(add_row(pod, caps, args)),
        "undo_edit" => Box::pin(undo_edit(pod, args)),
        "export_collection" => Box::pin(export(pod, args)),
        _ => return None,
    })
}

fn bad(detail: impl Into<String>) -> Value {
    fail(StatusCode::BAD_REQUEST, "invalid_arguments", detail)
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Result<&'a str, Value> {
    args[k]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("\"{k}\" is required")))
}

fn note_arg(args: &Value, default: &str) -> String {
    args["note"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(default)
        .chars()
        .take(custom::MAX_NOTE_CHARS)
        .collect()
}

fn db_err(e: (StatusCode, axum::Json<Value>)) -> Value {
    let (status, axum::Json(mut body)) = e;
    body["status"] = json!(status.as_u16());
    body
}

fn missing(name: &str) -> Value {
    json!({ "error": "collection_not_found", "collection": name, "status": 404,
            "detail": "list_collections lists them; define_collection makes one" })
}

/// The custom apps whose feeds include Collections, by collection: `(app
/// id, title)` of each app that lists the collection as a feed or binds it.
fn users(apps: &[(custom::CustomApp, i64, Value)], name: &str) -> Vec<Value> {
    let key = db::query_key(name);
    let scheme = format!("collections://{name}/");
    let route = format!("/v1/collections/{name}/");
    apps.iter()
        .filter(|(a, _, layout)| {
            a.feeds.iter().any(|(s, f)| s == NAME && f == name) || {
                let text = layout.to_string();
                text.contains(&format!("\"{key}\""))
                    || text.contains(&scheme)
                    || text.contains(&route)
            }
        })
        .map(|(a, v, _)| json!({ "app": a.id, "title": a.title, "version": v }))
        .collect()
}

async fn list(pod: &Pod) -> Result<Value, Value> {
    let p = pod.clone();
    let out = pod
        .blocking(move |s| {
            let apps = custom::apps_using(s, p.catalog(), NAME)?;
            s.with_conn(|c| {
                let others = db::defs_by_name(c)?;
                db::all_defs(c)?
                    .into_iter()
                    .map(|(version, def)| {
                        let rows = db::all_rows(c, &def.name, false)?.len();
                        Ok(json!({
                            "name": def.name, "title": def.title, "version": version, "rows": rows,
                            "fields": def.live_fields().map(|f| def.describe_field(f, &others)).collect::<Vec<_>>(),
                            "used_by": users(&apps, &def.name),
                        }))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        })
        .await
        .map_err(db_err)?;
    Ok(json!({ "collections": out }))
}

async fn define(pod: &Pod, adapter: &CollectionsAdapter, args: &Value) -> Result<Value, Value> {
    let caps = adapter.caps;
    let name = str_arg(args, "name")?.to_owned();
    let Some(specs) = args["fields"].as_array() else {
        return Err(bad("\"fields\" is a list of {name, type, ...}"));
    };
    let mut fields = Vec::new();
    let invalid = |i: usize, e: String| json!({ "error": "invalid_field", "at": format!("/fields/{i}"), "detail": e, "status": 400 });
    for (i, spec) in specs.iter().enumerate() {
        let f = model::parse_field(spec, format!("f{}", i + 1), caps.text_chars)
            .map_err(|e| invalid(i, e))?;
        fields.push(f);
    }
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map_or_else(|| title_case(&name), str::to_owned);
    let mut def = Definition {
        name: name.clone(),
        title,
        next_field: fields.len() as u32 + 1,
        fields,
        reminders: Vec::new(),
        fillers: Vec::new(),
        alerts: Vec::new(),
        system: None,
        order_by: Vec::new(),
        descending: args["descending"].as_bool().unwrap_or(false),
    };
    // After every field exists: a compute may read a later one.
    let others = pod
        .blocking(|s| s.with_conn(db::defs_by_name))
        .await
        .map_err(db_err)?;
    for (i, spec) in specs.iter().enumerate() {
        model::set_compute(
            &mut def,
            &format!("f{}", i + 1),
            spec.get("compute"),
            &others,
        )
        .map_err(|e| invalid(i, e))?;
    }
    let order = model::order_names(args.get("order_by")).map_err(bad)?;
    def.order_by = model::resolve_order(&def, &order).map_err(bad)?;
    // One filler (`filler`) or several (`fillers`); alerts after them.
    let specs: Vec<(String, Value)> = match (args.get("fillers"), args.get("filler")) {
        (Some(Value::Array(a)), _) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("/fillers/{i}"), v.clone()))
            .collect(),
        (_, Some(v)) if v.is_object() => vec![("/filler".to_owned(), v.clone())],
        _ => Vec::new(),
    };
    for (at, spec) in specs {
        let f = crate::filler::parse(&def, &spec, db::new_id("s"), adapter.allow_private_hosts())
            .map_err(
            |e| json!({ "error": "invalid_filler", "at": at, "detail": e, "status": 400 }),
        )?;
        def.fillers.push(f);
    }
    if let Some(Value::Array(a)) = args.get("alerts") {
        for (i, spec) in a.iter().enumerate() {
            crate::alerts::add(&mut def, spec, pod_core::util::now_secs()).map_err(
                |e| json!({ "error": "invalid_alert", "at": format!("/alerts/{i}"), "detail": e, "status": 400 }),
            )?;
        }
    }
    model::check_definition(&def, caps.fields)
        .map_err(|e| json!({ "error": "invalid_definition", "detail": e, "status": 400 }))?;
    crate::filler::check(&def, caps.agent_fills_per_day)
        .map_err(|e| json!({ "error": "invalid_filler", "detail": e, "status": 400 }))?;
    let mut with_new = others.clone();
    with_new.insert(def.name.clone(), def.clone());
    model::check_lookups(&def, &with_new)
        .map_err(|e| json!({ "error": "invalid_definition", "detail": e, "status": 400 }))?;
    let note = note_arg(args, "Created");
    let stored = def.clone();
    let version = refusable(pod, move |tx| {
        if db::latest_def(tx, &stored.name)?.is_some() {
            return Ok(Err(
                json!({ "error": "collection_exists", "collection": stored.name, "status": 409,
                "detail": "change it with apply_change (describe_collection shows it)" }),
            ));
        }
        if db::all_defs(tx)?.len() >= caps.collections {
            return Ok(Err(fail(
                StatusCode::CONFLICT,
                "too_many_collections",
                format!("a pod has at most {} collections", caps.collections),
            )));
        }
        Ok(Ok(db::append_def(
            tx,
            &stored,
            &note,
            custom::AUTHOR_CLAUDE,
            pod_core::util::now_secs(),
        )?))
    })
    .await?;
    let mut out = json!({
        "ok": true, "collection": name, "version": version,
        "definition": def.describe(pod_core::util::now_secs(), KEEP_SECS, &with_new),
        "next": format!("Make an app with create_app and feeds [[\"collections\", \"{name}\"]]; describe_collection has layout snippets."),
    });
    if !def.fillers.is_empty() {
        crate::fill::spawn(pod, &name, None, true);
        out["filling"] = json!("a first fill started; collections_status shows how it went");
    }
    Ok(out)
}

fn title_case(name: &str) -> String {
    let t = name.replace('_', " ");
    let mut c = t.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// Layout snippets for `def`'s collection.
fn hints(def: &Definition) -> Value {
    let n = &def.name;
    let first_text = def
        .live_fields()
        .find(|f| f.kind == model::Kind::Text)
        .map_or("title", |f| f.name.as_str());
    let first_bool = def.live_fields().find(|f| f.kind == model::Kind::Bool);
    let mut entity = json!({ "id": "pending-$nonce", "deleted": false });
    entity[first_text] = json!(format!("$form.{first_text}"));
    let mut out = json!({
        "feed": ["collections", n],
        "list": { "type": "core://List", "query_key": db::query_key(n),
            "where": { "field": "deleted", "equals": "false" },
            "empty_text": format!("No {} yet", def.title.to_lowercase()),
            "item_template": { "type": "core://Text", "style": "Body", "value_expr": format!("$item.{first_text}") } },
        "add": { "type": "core://TextField", "field": first_text, "placeholder": "Add…",
            "on_submit": { "intent": "action.mutation.create", "query_key": db::query_key(n),
                "entity_uri": format!("collections://{n}/pending-$nonce"), "entity": entity,
                "endpoint": format!("/v1/collections/{n}/create"),
                "payload": { first_text: format!("$form.{first_text}"), "client_id": "$nonce" },
                "refresh_workspace": true } },
        "delete": { "type": "core://IconButton", "icon": "delete", "content_description": "Delete",
            "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                "field": "deleted", "value_expr": "true", "rollback_value": false,
                "endpoint": format!("/v1/collections/{n}/delete"), "payload": { "id": "$item.id" } } },
        "notes": "Give every create's entity \"deleted\": false and an \"id\" of \"pending-$nonce\" (so the List's where shows it and a tick before the pod answers still reaches it). Fields left out of a create get their default. Dates are YYYY-MM-DD text.",
    });
    if let Some(b) = first_bool {
        out["toggle"] = json!({ "type": "core://IconButton", "icon": "check_box_outline_blank",
            "active": format!("$item.{}", b.name), "active_icon": "check_box",
            "content_description": b.name,
            "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                "field": b.name, "value_expr": format!("!entity.{}", b.name),
                "rollback_value": false,
                "endpoint": format!("/v1/collections/{n}/update"),
                "payload": { "id": "$item.id", b.name.as_str(): "$value" } } });
    }
    // A whole row: the tick, the text, then the delete at the end. Inside a
    // Row, a core://Column takes the full width (pushing the delete off
    // screen); a core://Container takes what it needs.
    let mut children = Vec::new();
    if out.get("toggle").is_some() {
        children.push(out["toggle"].clone());
    }
    children.push(
        json!({ "type": "core://Container", "orientation": "Vertical", "padding_dp": 0,
        "children": [out["list"]["item_template"].clone()] }),
    );
    children.push(json!({ "type": "core://Spacer" }));
    children.push(out["delete"].clone());
    out["row"] = json!({ "type": "core://Row", "spacing_dp": 12, "padding_h_dp": 16, "padding_v_dp": 6,
        "align": "Center", "children": children });
    // Display formatting is the client's (decision `text-dates-and-counts`).
    if let Some(d) = def.live_fields().find(|f| f.kind == model::Kind::Date) {
        let mut due = json!({ "type": "core://Text", "style": "Secondary",
            "value_expr": format!("Due $item.{}", d.name),
            "format": { "date": "Relative", "past": "Overdue" } });
        if d.optional {
            due = json!({ "type": "core://Conditional", "value": format!("$item.{}", d.name), "content": due });
        }
        out["date"] = due;
    }
    let mut open = vec![json!({ "field": "deleted", "equals": "false" })];
    if let Some(b) = first_bool {
        open.push(json!({ "field": b.name, "equals": "false" }));
    }
    out["count"] = json!({ "type": "core://Text", "style": "Secondary",
        "value_expr": if first_bool.is_some() { "$count left" } else { "$count" },
        "count": { "query_key": db::query_key(n), "where": open } });
    out["notes"] = json!(format!(
        "{} Dates and counts are drawn by the device: \"date\" shows a date field as \"Due tomorrow\" or \"Overdue\" (format), \"count\" counts rows (count; add \"zero_text\" for an empty list). Use \"row\" as the list's item_template. Inside a core://Row, use core://Container (not core://Column, which takes the whole width) and a core://Spacer to push the delete to the end.",
        out["notes"].as_str().unwrap_or("")
    ));
    out
}

async fn describe(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let version = args["version"].as_i64();
    let lookup = name.clone();
    let p = pod.clone();
    let found = pod
        .blocking(move |s| {
            let apps = custom::apps_using(s, p.catalog(), NAME)?;
            s.with_conn(|c| {
                let Some((latest, def)) = db::latest_def(c, &lookup)? else {
                    return Ok(None);
                };
                let shown = match version {
                    Some(v) => db::def_version(c, &lookup, v)?.map(|d| (v, d)),
                    None => Some((latest, def.clone())),
                };
                let rows = db::all_rows(c, &lookup, false)?.len();
                let aside: Vec<Value> = db::aside_counts(c, &lookup)?
                    .into_iter()
                    .map(|(f, n)| json!({ "field": def.by_id(&f).map_or(f.clone(), |x| x.name.clone()), "values": n }))
                    .collect();
                let computed: Vec<Value> = db::computed_counts(c, &lookup)?
                    .into_iter()
                    .filter_map(|(f, n)| def.by_id(&f).filter(|x| x.computed()).map(|x| json!({ "field": x.name, "answers": n })))
                    .collect();
                let lookups = crate::lookup::report(c, &def)?;
                let others = db::defs_by_name(c)?;
                let fill_state = crate::fill::states(c, &def)?;
                Ok(Some((
                    latest,
                    shown,
                    rows,
                    (aside, computed, lookups, others, (def.clone(), fill_state)),
                    db::def_history(c, &lookup, 20)?,
                    db::recent_edits(c, &lookup)?,
                    users(&apps, &lookup),
                )))
            })
        })
        .await
        .map_err(db_err)?;
    let Some((
        latest,
        shown,
        rows,
        (aside, computed, lookups, others, (live, fill_state)),
        history,
        edits,
        used_by,
    )) = found
    else {
        return Err(missing(&name));
    };
    let Some((v, def)) = shown else {
        return Err(json!({ "error": "version_not_found", "collection": name, "status": 404 }));
    };
    Ok(json!({
        "collection": name,
        "version": v,
        "latest_version": latest,
        "definition": def.describe(pod_core::util::now_secs(), KEEP_SECS, &others),
        "rows": rows,
        "kept_aside": aside,
        "computed_answers": computed,
        "lookups": lookups,
        "history": history,
        "recent_edits": edits,
        "used_by": used_by,
        "fill": fill_health(pod, &live, &fill_state).await,
        "layout_hints": hints(&def),
    }))
}

/// Every filler's health of `def` ([`crate::fill::health`]).
async fn fill_health(
    pod: &Pod,
    def: &Definition,
    states: &HashMap<String, crate::fill::State>,
) -> Value {
    if def.fillers.is_empty() {
        return Value::Null;
    }
    let off = !crate::fill::allowed(pod).await;
    let agent = pod
        .adapter::<CollectionsAdapter>()
        .is_some_and(|a| a.agent().is_some());
    let now = pod_core::util::now_secs();
    Value::Array(
        def.fillers
            .iter()
            .map(|f| {
                let st = states.get(&f.id).cloned().unwrap_or_default();
                crate::fill::health(def, f, &st, now, off, agent)
            })
            .collect(),
    )
}

async fn status(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let only = args["collection"].as_str().map(str::to_owned);
    let rows = pod
        .blocking(|s| {
            s.with_conn(|c| {
                let mut out = Vec::new();
                for (_, def) in db::all_defs(c)? {
                    if !def.fillers.is_empty() {
                        let st = crate::fill::states(c, &def)?;
                        out.push((def, st));
                    }
                }
                Ok((out, crate::fill::secret_names(c)?))
            })
        })
        .await
        .map_err(db_err)?;
    let (rows, secrets) = rows;
    let mut out = Vec::new();
    for (def, st) in rows {
        if only.as_ref().is_some_and(|o| *o != def.name) {
            continue;
        }
        if let Value::Array(h) = fill_health(pod, &def, &st).await {
            out.extend(h);
        }
    }
    if let Some(o) = &only
        && out.is_empty()
    {
        return Err(
            json!({ "error": "no_filler", "collection": o, "status": 404,
            "detail": "that collection has no filler (or doesn't exist)" }),
        );
    }
    Ok(json!({ "fillers": out, "secrets": secrets }))
}

async fn refresh(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let (_, def) = current(pod, &name).await?;
    if def.fillers.is_empty() {
        return Err(
            json!({ "error": "no_filler", "collection": name, "status": 409,
            "detail": "set one with apply_change {op: set_filler}" }),
        );
    }
    let ids: Vec<String> = match args["filler"].as_str() {
        Some(id) => match crate::filler::find(&def, id) {
            Some(f) => vec![f.id.clone()],
            None => return Err(bad(format!("no filler '{id}' in '{name}'"))),
        },
        None => def.fillers.iter().map(|f| f.id.clone()).collect(),
    };
    let mut out = Vec::new();
    for id in ids {
        let f = crate::filler::find(&def, &id).cloned();
        if f.as_ref().is_some_and(crate::filler::Filler::uses_model) {
            crate::fill::spawn(pod, &name, Some(&id), true);
            out.push(json!({ "filler": id, "ok": true, "started": true,
                "detail": "a model is filling it in the background (it can take minutes); collections_status shows how it went" }));
            continue;
        }
        let mut v = crate::fill::fill(pod, &name, &id, true).await.to_json();
        v["filler"] = json!(id);
        v["title"] = json!(f.map(|f| f.title).unwrap_or_default());
        out.push(v);
    }
    Ok(json!({ "collection": name, "fills": out }))
}

async fn set_secret(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "name")?.to_owned();
    if !crate::filler::valid_secret_name(&name) {
        return Err(bad(
            "a secret's name is [a-z][a-z0-9_]*, at most 40 characters",
        ));
    }
    let value = match &args["value"] {
        Value::Null => None,
        Value::String(v) if !v.trim().is_empty() && v.len() <= 4_000 => Some(v.trim().to_owned()),
        _ => {
            return Err(bad(
                "\"value\" is the secret (1..=4000 characters) or null to forget it",
            ));
        }
    };
    let host = match (&value, args["host"].as_str()) {
        (None, _) => None,
        (Some(_), Some(h)) => Some(crate::fill::secret_host(h).ok_or_else(|| {
            bad("\"host\" is the host name the secret is sent to, e.g. api.example.com")
        })?),
        (Some(_), None) => {
            return Err(bad(
                "\"host\" is required: the host name the secret may be sent to (e.g. api.example.com)",
            ));
        }
    };
    let now = pod_core::util::now_secs();
    let n = name.clone();
    let shown = host.clone();
    let names = pod
        .blocking(move |s| {
            let v = value.as_deref().zip(host.as_deref());
            crate::fill::set_secret(s, &n, v, now)?;
            s.with_conn(crate::fill::secret_names)
        })
        .await
        .map_err(db_err)?;
    Ok(
        json!({ "ok": true, "secret": name, "secrets": names, "host": shown,
        "detail": "stored sealed; a filler's header sends it as {secret:<name>}, only to this host" }),
    )
}

/// Most rows or sample items a preview shows.
const PREVIEW_ROWS: usize = 10;

async fn preview(pod: &Pod, adapter: &CollectionsAdapter, args: &Value) -> Result<Value, Value> {
    if !crate::fill::allowed(pod).await {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "fill_off",
            crate::fill::FILL_OFF,
        ));
    }
    // The recipe: a collection's filler, or the arguments.
    let (url, spec, map, headers, def) = if let Some(name) = args["collection"].as_str() {
        let (_, def) = current(pod, name).await?;
        let f = match args["filler"].as_str() {
            Some(id) => crate::filler::find(&def, id).cloned(),
            None => def.fillers.first().cloned(),
        };
        let Some(f) = f else {
            return Err(bad(format!(
                "'{name}' has no such filler; pass filler (an id from describe_collection) or a recipe"
            )));
        };
        if f.is_llm() {
            return Err(bad(
                "an llm filler can't be previewed (it asks a model; collections_refresh runs it)",
            ));
        }
        let crate::filler::Source::Fetch {
            url,
            url_field,
            headers,
            map,
            ..
        } = &f.source
        else {
            return Err(bad(
                "an agent filler can't be previewed (collections_refresh runs it)",
            ));
        };
        let url = match (url, url_field) {
            (Some(u), _) => u.clone(),
            (None, Some(field)) => {
                let (n, field) = (name.to_owned(), field.clone());
                pod.blocking(move |s| s.with_conn(|c| db::all_rows(c, &n, false)))
                    .await
                    .map_err(db_err)?
                    .into_iter()
                    .find_map(|r| {
                        r.values
                            .get(&field)
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .ok_or_else(|| bad("no row has a URL yet"))?
            }
            _ => return Err(bad("the filler has no URL")),
        };
        let map: Vec<(String, String)> = map
            .iter()
            .map(|(id, p)| (def.name_of(id), p.clone()))
            .collect();
        (url, f.extract_spec(), map, headers.clone(), Some(def))
    } else {
        let url = str_arg(args, "url")?.to_owned();
        let kind = web_extract::items::Kind::parse(args["extract"].as_str().unwrap_or(""))
            .ok_or_else(|| {
                bad(format!(
                    "\"extract\" is one of {}",
                    web_extract::items::Kind::ALL.join(", ")
                ))
            })?;
        let types = match &args["types"] {
            Value::String(t) => vec![t.clone()],
            Value::Array(a) => a
                .iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        let spec = web_extract::items::Spec {
            kind,
            items: args["items"].as_str().unwrap_or("").to_owned(),
            types,
            script: args["script"].as_str().unwrap_or("").to_owned(),
            zone: args["zone"].as_str().unwrap_or("").to_owned(),
        };
        let map: Vec<(String, String)> = args["map"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let headers = args["headers"]
            .as_object()
            .map(|h| {
                h.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        (url, Some(spec), map, headers, None)
    };
    let Some(spec) = spec else {
        return Err(bad("unknown extractor"));
    };
    let url = web_extract::fetch::check_url(&url, adapter.allow_private_hosts()).map_err(bad)?;
    if spec.kind == web_extract::items::Kind::Selectors && map.is_empty() {
        return Err(bad(
            "the selectors extractor needs map: {name: CSS selector}",
        ));
    }
    web_extract::items::check(&spec, &map).map_err(bad)?;
    let mut sent = std::collections::BTreeMap::new();
    for (k, v) in headers {
        sent.insert(k, v);
    }
    let resolved = crate::fill::resolve_headers_for(pod, &sent, &url).map_err(bad)?;
    let client = if resolved.has_secret {
        adapter.secret_fetch_client()
    } else {
        adapter.fetch_client()
    };
    let fetched = web_extract::fetch::fetch(client, &url, None, None, &resolved.headers)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, "fetch_failed", e.message))?;
    let web_extract::fetch::Fetched::Body { bytes, .. } = fetched else {
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "fetch_failed",
            "the site answered 304",
        ));
    };
    let body = String::from_utf8_lossy(&bytes);
    let mut out = json!({ "url": url, "extract": spec.kind.name(), "bytes": bytes.len() });
    if map.is_empty() {
        // Sample items to write the paths from.
        let items = crate::fill::sample_items(&spec, &body).map_err(bad)?;
        out["items"] = json!(items.len());
        out["sample_items"] = json!(items.into_iter().take(3).collect::<Vec<_>>());
        out["next"] = json!("write map: {field: path into an item} and preview again");
        return Ok(out);
    }
    let rows = web_extract::items::extract(&spec, &body, &url, &map).map_err(bad)?;
    out["items"] = json!(rows.len());
    if let Some(def) = &def {
        // As the collection would store them.
        let by_id: Vec<Map<String, Value>> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .filter_map(|(n, v)| def.field(n).map(|f| (f.id.clone(), v.clone())))
                    .collect()
            })
            .collect();
        let (vals, skipped) =
            crate::fill::coerce_preview(def, by_id, adapter.caps.text_chars, spec.zone().as_ref());
        out["rows"] = json!(
            vals.iter()
                .take(PREVIEW_ROWS)
                .map(|v| model::by_name(def, v))
                .collect::<Vec<_>>()
        );
        out["values_skipped"] = json!(skipped);
    } else {
        out["rows"] = json!(rows.into_iter().take(PREVIEW_ROWS).collect::<Vec<_>>());
    }
    if out["items"] == 0 {
        out["detail"] =
            json!("no items: check items/types (preview without map shows what the page has)");
    }
    Ok(out)
}

// ---------- change sets ----------

/// The app edits of a change set, `[{app, layout | json_patch,
/// base_version?, note?}]`, as `update_layout` bodies.
async fn app_bodies(
    pod: &Pod,
    args: &Value,
    note: &str,
) -> Result<Vec<(custom::CustomApp, Value)>, Value> {
    let Some(items) = args.get("apps").filter(|a| !a.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(items) = items.as_array() else {
        return Err(bad(
            "\"apps\" is a list of {app, layout | json_patch, base_version?}",
        ));
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let id = item["app"].as_str().unwrap_or("").to_owned();
        let lookup = id.clone();
        let found = pod
            .blocking(move |s| {
                let app = s.with_conn(|c| custom::custom_app_on(c, &lookup))?;
                let latest = s.app_latest(&lookup)?;
                Ok((app, latest))
            })
            .await
            .map_err(db_err)?;
        let (Some(app), Some((latest, json_text))) = found else {
            return Err(
                json!({ "error": "app_not_found", "at": format!("/apps/{i}"), "app": id, "status": 404,
                "detail": "only custom apps change in a change set" }),
            );
        };
        if out
            .iter()
            .any(|(a, _): &(custom::CustomApp, Value)| a.id == app.id)
        {
            return Err(bad(format!("app '{id}' is listed twice")));
        }
        let mut body = item.clone();
        if let Some(patch) = item.get("json_patch").filter(|p| !p.is_null()) {
            let mut doc: Value = serde_json::from_str(&json_text).unwrap_or(Value::Null);
            pod_core::mcp::apply_patch(&mut doc, patch).map_err(|e| {
                json!({ "error": "invalid_patch", "app": id, "detail": e, "latest_version": latest.version, "status": 400 })
            })?;
            body["layout"] = doc;
            if body.get("base_version").is_none_or(Value::is_null) {
                body["base_version"] = json!(latest.version);
            }
        } else if !item["layout"].is_object() {
            return Err(bad(format!("apps[{i}]: pass \"layout\" or \"json_patch\"")));
        }
        if body.get("note").is_none_or(Value::is_null) {
            body["note"] = json!(note);
        }
        out.push((app, body));
    }
    Ok(out)
}

/// Apply `planned` (a new definition of `name`, from `base`) with the app
/// edits of `args["apps"]`, atomically: every custom app using Collections
/// must fit the new definition, else nothing applies.
async fn commit(
    pod: &Pod,
    caps: crate::Caps,
    name: &str,
    base: i64,
    planned: Planned,
    args: &Value,
    note: String,
) -> Result<Value, Value> {
    let bodies = app_bodies(pod, args, &note).await?;
    // Stage the app edits: the core's checks now, ours below.
    let mut staged = Vec::new();
    let mut edited: HashMap<String, Value> = HashMap::new();
    for (app, body) in &bodies {
        let (s, expanded) = custom::stage_layout(pod, app, body).map_err(|(status, mut b)| {
            b["app"] = json!(app.id);
            b["status"] = json!(status.as_u16());
            b
        })?;
        edited.insert(app.id.clone(), expanded);
        staged.push(s);
    }
    // Every app using Collections, against the new definitions.
    let catalog_apps = {
        let pod2 = pod.clone();
        pod.blocking(move |s| {
            let apps = custom::apps_using(s, pod2.catalog(), NAME)?;
            let defs = s.with_conn(db::all_defs)?;
            Ok((apps, defs))
        })
        .await
        .map_err(db_err)?
    };
    let (apps, defs) = catalog_apps;
    let mut new_defs: HashMap<String, Definition> =
        defs.into_iter().map(|(_, d)| (d.name.clone(), d)).collect();
    new_defs.insert(name.to_owned(), planned.def.clone());
    // Every lookup (into this collection too) against the new definitions.
    for d in new_defs.values() {
        model::check_lookups(d, &new_defs)
            .map_err(|e| json!({ "error": "invalid_change", "detail": e, "status": 400 }))?;
    }
    for (app, _, latest) in &apps {
        let layout = edited.get(&app.id).unwrap_or(latest);
        let feeds: Vec<String> = app
            .feeds
            .iter()
            .filter(|(s, _)| s == NAME)
            .map(|(_, f)| f.clone())
            .collect();
        if let Err(e) = crate::bindings::check(layout, &feeds, &new_defs) {
            return Err(json!({
                "error": "breaks_app", "status": 422, "app": app.id, "title": app.title,
                "in_change_set": edited.contains_key(&app.id),
                "at": e.pointer, "allowed": e.allowed,
                "detail": format!("app '{}' ({}) would break: {}{}", app.title, app.id,
                    if e.pointer.is_empty() { String::new() } else { format!("{}: ", e.pointer) }, e.message),
                "fix": "change that app's layout in the same apply_change (apps: [{app, layout | json_patch}]), or change the collection differently",
            }));
        }
    }
    // Apps in the change set that don't use Collections are fine too.
    if !staged.is_empty() {
        custom::allow_write(pod).map_err(|(status, mut b)| {
            b["status"] = json!(status.as_u16());
            b
        })?;
    }
    let pod2 = pod.clone();
    let def = planned.def.clone();
    let convert = planned.convert.clone();
    let to_compute = planned.compute.clone();
    let to_recompute = planned.recompute.clone();
    let coll = name.to_owned();
    let others = new_defs.clone();
    let (version, conversions, versions, jobs) = refusable(pod, move |tx| {
        let current = db::latest_def(tx, &coll)?.map(|(v, _)| v);
        if current != Some(base) {
            return Ok(Err(json!({ "error": "stale_base", "status": 409, "latest_version": current,
                "detail": "the collection changed since that version; describe_collection and reapply" })));
        }
        let now = pod_core::util::now_secs();
        let version = db::append_def(tx, &def, &note, custom::AUTHOR_CLAUDE, now)?;
        let mut conversions = Vec::new();
        for id in &convert {
            if let Some(f) = def.by_id(id) {
                conversions.push(db::convert_field(tx, &coll, f, version, now, caps.text_chars)?);
            }
        }
        // The lookups were checked against the other collections as read
        // above; one changed meanwhile: refuse.
        if db::defs_by_name(tx)?
            .into_iter()
            .any(|(n, d)| n != coll && others.get(&n) != Some(&d))
        {
            return Ok(Err(json!({ "error": "stale_base", "status": 409,
                "detail": "another collection changed meanwhile; apply the change again" })));
        }
        let mut jobs = Vec::new();
        for id in &to_compute {
            jobs.extend(crate::compute::backfill(tx, &def, id, false, caps.rows, now, caps)?);
        }
        for id in &to_recompute {
            jobs.extend(crate::compute::backfill(tx, &def, id, true, caps.rows, now, caps)?);
        }
        let mut versions = Vec::new();
        for s in &staged {
            match custom::commit_staged(tx, &pod2, s, Writer::Claude)? {
                Ok(v) => versions.push((s.id.clone(), v)),
                Err((status, mut b)) => {
                    b["status"] = json!(status.as_u16());
                    return Ok(Err(b));
                }
            }
        }
        Ok(Ok((version, conversions, versions, jobs)))
    })
    .await?;
    for (app, v) in &versions {
        pod.publish_layout(app, *v);
    }
    pod.publish_data_all(NAME);
    let computing = jobs.len();
    crate::compute::spawn(pod, name, jobs);
    Ok(json!({
        "ok": true, "collection": name, "version": version,
        "conversions": conversions.iter().map(|c| c.to_json(&planned.def.title)).collect::<Vec<_>>(),
        "apps": versions.iter().map(|(a, v)| json!({ "app": a, "version": v })).collect::<Vec<_>>(),
        "definition": planned.def.describe(pod_core::util::now_secs(), KEEP_SECS, &new_defs),
        "computing_rows": computing,
    }))
}

async fn current(pod: &Pod, name: &str) -> Result<(i64, Definition), Value> {
    let lookup = name.to_owned();
    pod.blocking(move |s| s.with_conn(|c| db::latest_def(c, &lookup)))
        .await
        .map_err(db_err)?
        .ok_or_else(|| missing(name))
}

async fn apply_change(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    args: &Value,
) -> Result<Value, Value> {
    let caps = adapter.caps;
    let name = str_arg(args, "collection")?.to_owned();
    let Some(ops) = args["changes"].as_array() else {
        return Err(bad("\"changes\" is a list of ops"));
    };
    let (version, def) = current(pod, &name).await?;
    let others = pod
        .blocking(|s| s.with_conn(db::defs_by_name))
        .await
        .map_err(db_err)?;
    if let Some(b) = args["base_version"].as_i64()
        && b != version
    {
        return Err(
            json!({ "error": "stale_base", "status": 409, "latest_version": version,
            "detail": "the collection changed since that version; describe_collection and reapply" }),
        );
    }
    let planned = changes::plan(&def, ops, &others, pod_core::util::now_secs(), caps.text_chars, caps.fields, adapter.allow_private_hosts())
        .map_err(|(i, e)| json!({ "error": "invalid_change", "at": format!("/changes/{i}"), "detail": e, "status": 400 }))?;
    crate::filler::check(&planned.def, caps.agent_fills_per_day)
        .map_err(|e| json!({ "error": "invalid_change", "detail": e, "status": 400 }))?;
    let changed_fillers = planned.fillers_changed(&def);
    let keep: Vec<String> = planned.def.fillers.iter().map(|f| f.id.clone()).collect();
    for r in &planned.def.reminders {
        if let Some(app) = r.app.clone() {
            let exists = pod
                .blocking(move |s| s.with_conn(|c| custom::custom_app_on(c, &app)))
                .await
                .map_err(db_err)?
                .is_some();
            if !exists {
                return Err(json!({ "error": "invalid_change", "status": 400,
                    "detail": format!("reminder '{}': no custom app '{}' (list_apps lists them)", r.id, r.app.as_deref().unwrap_or("")) }));
            }
        }
    }
    let note = note_arg(args, "Changed");
    let mut out = commit(pod, caps, &name, version, planned, args, note).await?;
    let n = name.clone();
    let _ = pod
        .blocking(move |s| s.transaction(|tx| crate::fill::forget_removed(tx, &n, &keep)))
        .await;
    for f in &changed_fillers {
        crate::fill::spawn(pod, &name, Some(f), true);
    }
    if !changed_fillers.is_empty() {
        out["filling"] = json!({ "fillers": changed_fillers,
            "detail": "a fill started for each new or changed filler; collections_status shows how it went" });
    }
    Ok(out)
}

async fn restore_version(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let Some(n) = args["version"].as_i64() else {
        return Err(bad("\"version\" is required"));
    };
    let (version, def) = current(pod, &name).await?;
    let lookup = name.clone();
    let old = pod
        .blocking(move |s| s.with_conn(|c| db::def_version(c, &lookup, n)))
        .await
        .map_err(db_err)?
        .ok_or_else(|| json!({ "error": "version_not_found", "collection": name, "version": n, "status": 404 }))?;
    let planned = changes::restore(&def, &old, pod_core::util::now_secs(), caps.fields)
        .map_err(|e| json!({ "error": "invalid_change", "detail": e, "status": 400 }))?;
    let note = note_arg(args, &format!("Restored version {n}"));
    commit(pod, caps, &name, version, planned, args, note).await
}

// ---------- row edits ----------

/// One planned row change: `(op, row id, before, after)`.
type Change = (&'static str, String, Option<Row>, Row);

async fn edit_rows(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let Some(edits) = args["edits"].as_array().cloned() else {
        return Err(bad("\"edits\" is a list of {op, id?, values?}"));
    };
    let confirm = Confirm::Token(args["confirm"].as_str().map(str::to_owned));
    edit(
        pod,
        caps,
        name,
        edits,
        note_arg(args, "Edited rows"),
        confirm,
    )
    .await
}

/// One row the user just asked for in the conversation ("log that I had
/// the oil changed"): written at once, without edit_rows' preview, since
/// the request itself is the confirmation. Still recorded as an edit, so
/// undo_edit reverts it.
async fn add_row(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let Some(values) = args["values"].as_object().cloned() else {
        return Err(bad("\"values\" is an object {field: value}"));
    };
    let edits = vec![json!({ "op": "create", "values": values })];
    let mut out = edit(
        pod,
        caps,
        name,
        edits,
        note_arg(args, "Added a row"),
        Confirm::Skip,
    )
    .await
    .map_err(|mut e| {
        if e["at"] == "/edits/0" {
            e["at"] = json!("/values");
        }
        e
    })?;
    if let Some(id) = out["created"][0].as_str() {
        out["id"] = json!(id);
    }
    Ok(out)
}

/// Whether an edit waits for the user's agreement.
enum Confirm {
    /// edit_rows: applies only with the token of the preview the user saw.
    Token(Option<String>),
    /// add_row: the user's own request is the agreement.
    Skip,
}

async fn edit(
    pod: &Pod,
    caps: crate::Caps,
    name: String,
    edits: Vec<Value>,
    note: String,
    confirm: Confirm,
) -> Result<Value, Value> {
    if edits.is_empty() || edits.len() > caps.edit_rows {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "too_many_edits",
            format!("an edit changes 1..={} rows", caps.edit_rows),
        ));
    }
    let collection = name.clone();
    let out = refusable(pod, move |tx| {
        let Some((version, def)) = db::latest_def(tx, &name)? else {
            return Ok(Err(missing(&name)));
        };
        let now = pod_core::util::now_secs();
        let mut planned: Vec<Change> = Vec::new();
        let mut writes: Vec<Map<String, Value>> = Vec::new();
        let mut hasher = Sha256::new();
        hasher.update(format!("{name}\n{version}\n"));
        for (i, e) in edits.iter().enumerate() {
            let at = format!("/edits/{i}");
            let op = e["op"].as_str().unwrap_or("");
            let values = e["values"].as_object().cloned().unwrap_or_default();
            hasher.update(e.to_string());
            let invalid = |detail: String| Ok(Err(json!({ "error": "invalid_edit", "at": at, "detail": detail, "status": 422 })));
            if op == "create"
                && let Err(d) = crate::filler::check_write(&def, &values, op, None)
            {
                return invalid(d);
            }
            if op == "create" {
                let mut row = Row {
                    id: db::new_id("r"),
                    client_id: None,
                    values: Map::new(),
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
                    filler: None,
                };
                if let Err(d) = model::apply_write(&def, &mut row.values, &values, Write::Create, caps.text_chars) {
                    return invalid(d);
                }
                planned.push(("create", row.id.clone(), None, row));
                writes.push(values);
                continue;
            }
            let id = e["id"].as_str().unwrap_or("");
            let Some(before) = db::row(tx, &name, id)? else {
                return invalid(format!("no row '{id}'"));
            };
            if let Err(d) = crate::filler::check_write(&def, &values, op, before.filler.as_deref()) {
                return invalid(d);
            }
            if planned.iter().any(|(_, r, _, _)| r == id) {
                return invalid(format!("row '{id}' is edited twice"));
            }
            hasher.update(before.state().to_string());
            let mut after = before.clone();
            after.updated_at = now;
            match op {
                "update" => {
                    if before.deleted_at.is_some() {
                        return invalid(format!("row '{id}' is deleted (restore it first)"));
                    }
                    if let Err(d) = model::apply_write(&def, &mut after.values, &values, Write::Update, caps.text_chars) {
                        return invalid(d);
                    }
                }
                "delete" => after.deleted_at = Some(now),
                "restore" => after.deleted_at = None,
                other => return invalid(format!("unknown op '{other}' (create, update, delete, restore)")),
            }
            let op = match op {
                "update" => "update",
                "delete" => "delete",
                _ => "restore",
            };
            planned.push((op, id.to_owned(), Some(before), after));
            writes.push(if op == "update" { values } else { Map::new() });
        }
        let token = hasher
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let count = |op: &str| planned.iter().filter(|(o, ..)| *o == op).count();
        let confirm = match &confirm {
            Confirm::Skip => None,
            Confirm::Token(t) => Some(t.as_deref()),
        };
        if let Some(confirm) = confirm
            && confirm != Some(token.as_str())
        {
            let sample: Vec<Value> = planned
                .iter()
                .take(5)
                .map(|(op, id, before, after)| {
                    let mut v = json!({ "op": op, "after": model::by_name(&def, &after.values) });
                    if let Some(b) = before {
                        v["id"] = json!(id);
                        v["before"] = json!(model::by_name(&def, &b.values));
                    }
                    v
                })
                .collect();
            let mut preview = json!({
                "needs_confirmation": true, "confirm": token, "count": planned.len(),
                "creates": count("create"), "updates": count("update"),
                "deletes": count("delete"), "restores": count("restore"), "sample": sample,
                "detail": "Nothing changed yet. Show the user this preview (the count and the sample); if they agree, call edit_rows again with the same edits and this confirm token.",
            });
            if confirm.is_some() {
                preview["error"] = json!("stale_preview");
                preview["status"] = json!(409);
                preview["detail"] = json!("the rows or the collection changed since that preview (or the edits differ); show the user this new preview");
                return Ok(Err(preview));
            }
            return Ok(Ok((Vec::new(), preview)));
        }
        let added_rows = count("create") as i64 + planned.iter().filter(|(o, _, b, _)| *o == "restore" && b.as_ref().is_some_and(|b| b.deleted_at.is_some())).count() as i64 - count("delete") as i64;
        let bytes: i64 = planned
            .iter()
            .map(|(_, _, b, a)| {
                let live = |r: &Row| if r.deleted_at.is_none() { crate::bytes_of(&r.values) } else { 0 };
                live(a) - b.as_ref().map_or(0, live)
            })
            .sum();
        if let Err(e) = crate::check_usage(tx, caps, added_rows, bytes)? {
            return Ok(Err(e));
        }
        let mut records = Vec::new();
        let mut jobs = Vec::new();
        for ((op, id, before, after), write) in planned.iter_mut().zip(&writes) {
            if *op == "create" || *op == "update" {
                jobs.extend(crate::compute::after_write(
                    tx,
                    &def,
                    before.as_ref().map(|b| &b.values),
                    after,
                    write,
                    now,
                    caps,
                )?);
            }
            let after = &*after;
            if *op == "create" {
                db::insert_row(tx, &name, after)?;
            } else {
                db::save_row(tx, &name, after)?;
            }
            records.push((id.clone(), before.as_ref().map(Row::state), after.state()));
        }
        let edit = db::record_edit(tx, &name, &note, &records, now)?;
        let count = |op: &str| planned.iter().filter(|(o, ..)| *o == op).count();
        Ok(Ok((jobs, json!({
            "ok": true, "edit": edit, "count": planned.len(),
            "creates": count("create"), "updates": count("update"),
            "deletes": count("delete"), "restores": count("restore"),
            "created": planned.iter().filter(|(o, ..)| *o == "create").map(|(_, id, ..)| id).collect::<Vec<_>>(),
            "undo": "undo_edit with this edit id (for 30 days)",
        }))))
    })
    .await?;
    let (jobs, out) = out;
    if out["ok"] == true {
        pod.publish_data_all(NAME);
        crate::compute::spawn(pod, &collection, jobs);
    }
    Ok(out)
}

async fn undo_edit(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let id = str_arg(args, "edit")?.to_owned();
    let out = refusable(pod, move |tx| {
        let Some((collection, undone, rows)) = db::edit(tx, &id)? else {
            return Ok(Err(
                json!({ "error": "edit_not_found", "edit": id, "status": 404,
                "detail": "edits can be undone for 30 days" }),
            ));
        };
        if undone.is_some() {
            return Ok(Err(
                json!({ "error": "already_undone", "edit": id, "status": 409 }),
            ));
        }
        let now = pod_core::util::now_secs();
        let (mut restored, mut skipped) = (0, Vec::new());
        for (row_id, before, after) in rows {
            let Some(mut row) = db::row(tx, &collection, &row_id)? else {
                skipped.push(row_id);
                continue;
            };
            if row.state() != after {
                skipped.push(row_id);
                continue;
            }
            match before {
                Some(b) => {
                    row.values = b["values"].as_object().cloned().unwrap_or_default();
                    row.deleted_at = b["deleted_at"].as_i64();
                }
                // It was made by the edit.
                None => row.deleted_at = Some(now),
            }
            row.updated_at = now;
            db::save_row(tx, &collection, &row)?;
            restored += 1;
        }
        tx.execute(
            "UPDATE collections_edits SET undone_at = ?1 WHERE id = ?2",
            rusqlite::params![now, id],
        )?;
        Ok(Ok(
            json!({ "ok": true, "edit": id, "restored": restored, "skipped": skipped,
            "detail": if skipped.is_empty() { "every row is back as it was".to_owned() }
                else { format!("{} rows changed since and were left alone", skipped.len()) } }),
        ))
    })
    .await?;
    pod.publish_data_all(NAME);
    Ok(out)
}

async fn export(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let lookup = name.clone();
    pod.blocking(move |s| s.with_conn(|c| crate::export(c, &lookup, pod_core::util::now_secs())))
        .await
        .map_err(db_err)?
        .ok_or_else(|| missing(&name))
}
