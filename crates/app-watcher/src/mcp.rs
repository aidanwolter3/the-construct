//! The watcher's MCP tools: Claude adds websites and rules for the user
//! (no server push needed), tries an extractor on a page before saving a
//! target, and reads each target's health.

use axum::http::StatusCode;
use pod_core::{BoxFuture, Pod};
use serde_json::{Value, json};

use crate::check::{self, Fetched};
use crate::db;
use crate::edit::{self, Refusal};
use crate::extract::Kind;
use crate::html;
use crate::when::{Zone, parse_time_zone, parse_utc_offset};
use crate::{NAME, WatcherAdapter};

pub const INSTRUCTIONS: &str = "The Watcher app checks websites for events and notifies the user when \
one matches a rule (\"tell me when Improv Boulder lists a Budget Cuts show\"). A website is a target: \
its URL, how often to check it, and an extractor: ics (an iCalendar feed), jsonld (schema.org Event \
markup, which most event sites embed), llm (a model reads the page text; any page, but each changed \
page costs a model call) or auto (ics, else jsonld, else llm). Before saving a target, try the page with \
watcher_preview and pick the cheapest extractor that finds the events; set details: true when the \
listing lacks what the rules need (lineups) and each event's own page has it. Each site has its own \
rules: add one with watcher_set_rule and the site's id as target (any: phrases of which one must \
appear; none: phrases that exclude); to watch for the same thing on several sites, add a rule to each. \
watcher_status shows each target's health: say so when one is failing or finds no events.";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

pub fn tools() -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let list =
        |d: &str| json!({ "type": "array", "items": { "type": "string" }, "description": d });
    let extractor = json!({ "type": "string", "enum": ["auto", "ics", "jsonld", "llm"],
        "description": "How the page is read (default auto)" });
    vec![
        tool(
            "watcher_status",
            "The watched websites with their health (last check, errors, events found), the rules, the upcoming matches, and whether the llm extractor is available.",
            json!({}),
            &[],
        ),
        tool(
            "watcher_preview",
            "Fetch a page now and run an extractor on it, without saving anything: the events it finds (first 15) and the extractor used. Use it to choose a target's extractor.",
            json!({ "url": s("The page"), "extractor": extractor, "hint": s("Notes for the llm extractor"),
                    "time_zone": s("e.g. America/Denver: the site's zone, for times without an offset (follows daylight saving)"),
                    "utc_offset": s("e.g. -07:00: a fixed offset, only when the zone has no name (time_zone wins)") }),
            &["url"],
        ),
        tool(
            "watcher_set_target",
            "Watch a website (no id) or change one (id). Archive with archived: true. A new or changed target is checked at once.",
            json!({ "id": s("A target id, to change it"), "url": s("The page listing the events"),
                    "title": s("Shown in the app (default: the host)"), "extractor": extractor,
                    "interval_minutes": { "type": "integer", "description": "How often to check (15-10080, default 360)" },
                    "hint": s("Notes for the llm extractor about this page"),
                    "details": { "type": "boolean", "description": "Fetch each new event's own page once for its full description" },
                    "time_zone": s("e.g. America/Denver: the site's zone, for times without an offset (follows daylight saving)"),
                    "utc_offset": s("e.g. -07:00: a fixed offset, only when the zone has no name (time_zone wins)"),
                    "archived": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "watcher_set_rule",
            "Add a rule to a site (no id; target: the site's id) or change one (id). An event of that site matches when every set condition holds; matching ignores case and reads the title, venue and description. New matches are notified at once.",
            json!({ "id": s("A rule id, to change it"), "target": s("The site (target id) the rule belongs to; required for a new rule when the pod watches several sites"),
                    "title": s("Shown in the app and in notifications"),
                    "any": list("At least one of these phrases"), "all": list("Every one of these phrases"),
                    "none": list("None of these phrases (e.g. class, workshop)"),
                    "venue": s("The venue contains this"), "days": list("mon … sun"),
                    "within_days": { "type": "integer", "description": "Only events starting within this many days" },
                    "archived": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "watcher_refresh",
            "Check one website (id) or every one now, in the background; read the result with watcher_status a little later.",
            json!({ "id": s("A target id (default: all)") }),
            &[],
        ),
    ]
}

pub fn call<'a>(
    adapter: &'a WatcherAdapter,
    pod: &'a Pod,
    tool: &'a str,
    args: &'a Value,
) -> Option<BoxFuture<'a, Result<Value, Value>>> {
    Some(match tool {
        "watcher_status" => Box::pin(status(adapter, pod)),
        "watcher_preview" => Box::pin(preview(adapter, pod, args)),
        "watcher_set_target" => {
            Box::pin(async move { edit::save_target(pod, args.clone()).await.map_err(err) })
        }
        "watcher_set_rule" => {
            Box::pin(async move { edit::save_rule(pod, args.clone()).await.map_err(err) })
        }
        "watcher_refresh" => Box::pin(refresh(pod, args)),
        _ => return None,
    })
}

fn err((status, mut body): Refusal) -> Value {
    body["status"] = json!(status.as_u16());
    body
}

fn store_error() -> Value {
    json!({ "error": "store_error", "status": 500 })
}

async fn status(adapter: &WatcherAdapter, pod: &Pod) -> Result<Value, Value> {
    let now = pod_core::util::now_secs();
    let (targets, rules, matches) = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                let mut targets = Vec::new();
                for t in db::targets(c)? {
                    let mut e = db::target_entity(c, &t, now)?.0;
                    e.insert("last_error".into(), json!(t.last_error));
                    e.insert("failures".into(), json!(t.failures));
                    e.insert("last_extractor".into(), json!(t.last_extractor));
                    targets.push(Value::Object(e));
                }
                let mut rules: Vec<Value> = Vec::new();
                for r in db::rules(c)? {
                    let mut e = db::rule_entity(&r, &db::rule_site(c, &r)?).0;
                    e.insert("spec".into(), json!(r.spec));
                    rules.push(Value::Object(e));
                }
                let b = db::matches_bundle(c, now)?;
                let matches: Vec<Value> = b.queries[db::MATCHES_QUERY]
                    .iter()
                    .take(20)
                    .filter_map(|u| b.entities.get(u))
                    .map(|e| {
                        json!({ "id": e.get("id"), "title": e.get("title"), "meta": e.get("meta"),
                                "rules": e.get("rules_text"), "url": e.get("url") })
                    })
                    .collect();
                Ok((targets, rules, matches))
            })
        })
        .await
        .map_err(|_| store_error())?;
    let fetching = pod
        .blocking(|s| s.pod_setting(check::FETCH_SETTING))
        .await
        .ok()
        .flatten()
        .is_none_or(|v| v != "0");
    Ok(json!({
        "targets": targets,
        "rules": rules,
        "upcoming_matches": matches,
        "llm_extractor": adapter.config().llm.is_some(),
        "fetching": fetching,
    }))
}

async fn preview(adapter: &WatcherAdapter, pod: &Pod, args: &Value) -> Result<Value, Value> {
    let url = args["url"].as_str().unwrap_or("");
    let url = edit::check_url(adapter, url).map_err(err)?;
    let extractor = args["extractor"]
        .as_str()
        .filter(|x| !x.is_empty())
        .unwrap_or("auto");
    if extractor != "auto" && Kind::parse(extractor).is_none() {
        return Err(err(edit::invalid(
            "\"extractor\" is auto, ics, jsonld or llm",
        )));
    }
    let offset = parse_utc_offset(args["utc_offset"].as_str().unwrap_or(""))
        .map_err(|e| err(edit::invalid(e)))?;
    let tz = parse_time_zone(args["time_zone"].as_str().unwrap_or(""))
        .map_err(|e| err(edit::invalid(e)))?;
    let zone = tz.map(Zone::Named).or(offset.map(Zone::Fixed));
    let fetching = pod
        .blocking(|s| s.pod_setting(check::FETCH_SETTING))
        .await
        .ok()
        .flatten()
        .is_none_or(|v| v != "0");
    if !fetching {
        return Err(json!({ "error": "fetch_off", "detail": check::FETCH_OFF, "status": 403 }));
    }
    let bytes = match check::fetch(adapter.fetch_client(), &url, None, None).await {
        Ok(Fetched::Body { bytes, .. }) => bytes,
        Ok(Fetched::NotModified) => Vec::new(),
        Err(e) => {
            return Err(json!({ "error": "fetch_failed", "detail": e.message,
                               "status": StatusCode::BAD_GATEWAY.as_u16() }));
        }
    };
    let body = String::from_utf8_lossy(&bytes);
    let now = pod_core::util::now_secs();
    let hint = args["hint"].as_str().unwrap_or("");
    let (events, used) = check::extract(adapter, extractor, &body, &url, hint, zone, now)
        .await
        .map_err(|e| json!({ "error": "extract_failed", "detail": e, "status": 502 }))?;
    let upcoming: Vec<_> = events
        .iter()
        .filter(|e| {
            let ts = e.start.timestamp(zone);
            db::relevant_until(&e.start, ts, e.start.offset_min.is_some() || zone.is_some()) >= now
        })
        .collect();
    let sample: Vec<Value> = upcoming
        .iter()
        .take(15)
        .map(|e| {
            json!({ "title": e.title, "start": e.start.stored(),
                    "when": format!("{} {}", e.start.shifted_to(zone).date_text(),
                                    e.start.shifted_to(zone).time_text().unwrap_or_default()).trim(),
                    "venue": e.venue, "url": e.url, "source_id": e.source_id,
                    "description": html::truncate(&e.description, 300) })
        })
        .collect();
    Ok(json!({
        "extractor": used,
        "found": events.len(),
        "upcoming": upcoming.len(),
        "events": sample,
        "page_bytes": bytes.len(),
        "page_text_sample": if events.is_empty() { html::truncate(&html::to_text(&body), 1500) } else { String::new() },
    }))
}

async fn refresh(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let id = args["id"]
        .as_str()
        .filter(|i| !i.is_empty())
        .map(str::to_owned);
    let lookup = id.clone();
    let n = pod
        .blocking(move |s| s.with_conn(|c| db::request_refresh(c, lookup.as_deref())))
        .await
        .map_err(|_| store_error())?;
    if n == 0 && id.is_some() {
        return Err(json!({ "error": "target_not_found", "id": id, "status": 404 }));
    }
    pod.publish_data_all(NAME);
    check::spawn_run(pod);
    Ok(json!({ "refreshing": n, "next": "Call watcher_status in a minute for the results." }))
}
