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
break. edit_rows changes rows directly (imports, backfills): it first answers a preview, which you \
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
order_by may list several fields ([\"section\", \"done\"]; a choice sorts by the order of its options).";

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
        "description": "The pod computes the value from the row's other fields (not from other computed fields), again whenever they change; a value written by hand stays until they change. One kind, or a list of kinds tried in order (the first with an answer wins; a model is last). kind \"lookup\": {kind, from: field, collection, key, value, match?: exact (the default; ignores case and spacing) | contains (the key's whole words, ignoring case) | regex (case-insensitive, at most 200 characters; an invalid one is skipped and listed by describe_collection), learn?}: the value field of the row of another collection whose key (a text field) matches this row's from field, the longest matching key winning; evaluated in the write; not recomputed when the rules change (apply_change recompute_field does). learn: true adds a rule (the input as an exact key, or an anchored regex) when a later kind (the model) answers, and a hand edit of the field adds or changes that rule. Example: [{kind: \"lookup\", from: \"name\", collection: \"section_rules\", key: \"pattern\", value: \"section\", match: \"contains\", learn: true}, {kind: \"model\", from: \"name\", prompt: \"...\"}]. kind \"expr\": {kind, expr}, e.g. \"entity.quantity ? entity.name + \\\" (\\\" + entity.quantity + \\\")\\\" : entity.name\" (entity.<field>, JSON literals, + joins text or adds numbers, -, !, c ? a : b, parentheses; an empty value is \"\" or 0 and false in a condition), evaluated in the write. kind \"model\": {kind, from: field or [fields], prompt}, e.g. {from: \"name\", prompt: \"Which grocery store section is this item in?\"}: a model answers in the background (a choice: one of its options; text: trimmed), each input once; a value set by hand is remembered as the answer for that input. Needs optional or a default (what the row reads until computed). null: stop computing.",
        "anyOf": [kind, { "type": "array", "items": kind }, { "type": "null" }] });
    let field = json!({ "type": "object", "description": "{name, type: text|number|bool|date|choice, options? (choice), optional?, default?, compute?}",
        "properties": { "name": { "type": "string" }, "type": { "type": "string", "enum": model::Kind::ALL },
            "options": { "type": "array", "items": { "type": "string" } }, "optional": { "type": "boolean" }, "default": {},
            "compute": compute },
        "required": ["name", "type"] });
    let order = json!({ "description": "A field, or a list of fields (first key first), rows are listed by (else oldest first). A choice sorts by the order of its options; empty values last.",
        "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] });
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
                    "descending": { "type": "boolean" }, "note": note }),
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
            "Change a collection's definition and the custom apps that show it in one step. changes: [{op: add_field, name, type, options?, optional?, default?, compute?} | {op: rename_field, field, to} | {op: remove_field, field} | {op: restore_field, field} | {op: update_field, field, type?, options?, optional?, default?, compute?} | {op: recompute_field, field} | {op: set_order, field?: name or [names], descending?} | {op: set_title, title}]. compute (one kind or a list tried in order: {kind: \"expr\", expr}, {kind: \"lookup\", from, collection, key, value, match?, learn?} or {kind: \"model\", from, prompt}, as in define_collection) is computed for existing rows too (a model only fills empty ones); null stops computing and keeps the values. A lookup is not recomputed when its rules change: after editing them, recompute_field computes the field again for every row (a row only the model could answer keeps its value). Every custom app using Collections is checked against the new definition; if any would break, nothing applies and the error names the app and the binding. Renamed fields keep their old name as an alias; removed fields are hidden and restorable for 30 days; type changes convert row by row and keep values that don't convert aside.",
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
            "undo_edit",
            "Undo one edit_rows edit as one step (rows changed since are left alone and reported).",
            json!({ "edit": { "type": "string", "description": "The edit id edit_rows answered" } }),
            &["edit"],
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
        "define_collection" => Box::pin(define(pod, caps, args)),
        "describe_collection" => Box::pin(describe(pod, args)),
        "apply_change" => Box::pin(apply_change(pod, caps, args)),
        "restore_collection_version" => Box::pin(restore_version(pod, caps, args)),
        "edit_rows" => Box::pin(edit_rows(pod, caps, args)),
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

async fn define(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
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
    model::check_definition(&def, caps.fields)
        .map_err(|e| json!({ "error": "invalid_definition", "detail": e, "status": 400 }))?;
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
    Ok(json!({
        "ok": true, "collection": name, "version": version,
        "definition": def.describe(pod_core::util::now_secs(), KEEP_SECS, &with_new),
        "next": format!("Make an app with create_app and feeds [[\"collections\", \"{name}\"]]; describe_collection has layout snippets."),
    }))
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
        "delete": { "type": "core://IconButton", "icon": "close", "content_description": "Delete",
            "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                "field": "deleted", "value_expr": "true", "rollback_value": false,
                "endpoint": format!("/v1/collections/{n}/delete"), "payload": { "id": "$item.id" } } },
        "notes": "Give every create's entity \"deleted\": false and an \"id\" of \"pending-$nonce\" (so the List's where shows it and a tick before the pod answers still reaches it). Fields left out of a create get their default. Dates are YYYY-MM-DD text.",
    });
    if let Some(b) = first_bool {
        let box_for = |equals: &str, glyph: &str| {
            json!({ "type": "core://Conditional", "value": format!("$item.{}", b.name), "equals": equals,
                    "content": { "type": "core://Text", "style": "Body", "value_expr": glyph } })
        };
        out["toggle"] = json!({ "type": "core://Tappable", "content_description": b.name,
            "content": { "type": "core://Container", "orientation": "Vertical", "padding_dp": 0,
                "children": [box_for("true", "☑"), box_for("false", "☐")] },
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
    out["notes"] = json!(format!(
        "{} Use \"row\" as the list's item_template. Inside a core://Row, use core://Container (not core://Column, which takes the whole width) and a core://Spacer to push the delete to the end.",
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
                Ok(Some((
                    latest,
                    shown,
                    rows,
                    (aside, computed, lookups, others),
                    db::def_history(c, &lookup, 20)?,
                    db::recent_edits(c, &lookup)?,
                    users(&apps, &lookup),
                )))
            })
        })
        .await
        .map_err(db_err)?;
    let Some((latest, shown, rows, (aside, computed, lookups, others), history, edits, used_by)) =
        found
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
        "layout_hints": hints(&def),
    }))
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

async fn apply_change(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
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
    let planned = changes::plan(&def, ops, &others, pod_core::util::now_secs(), caps.text_chars, caps.fields)
        .map_err(|(i, e)| json!({ "error": "invalid_change", "at": format!("/changes/{i}"), "detail": e, "status": 400 }))?;
    let note = note_arg(args, "Changed");
    commit(pod, caps, &name, version, planned, args, note).await
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
    if edits.is_empty() || edits.len() > caps.edit_rows {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "too_many_edits",
            format!("an edit changes 1..={} rows", caps.edit_rows),
        ));
    }
    let confirm = args["confirm"].as_str().map(str::to_owned);
    let note = note_arg(args, "Edited rows");
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
            if op == "create" {
                let mut row = Row {
                    id: db::new_id("r"),
                    client_id: None,
                    values: Map::new(),
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
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
        if confirm.as_deref() != Some(token.as_str()) {
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
