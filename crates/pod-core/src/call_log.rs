//! The call log (decision `mcp-call-log`): every MCP tool call a credential
//! makes, recorded by the pod where the caller can't write, and the owner's
//! one-tap undo of what the pod's background agents did.
//!
//! A call is logged when the pod's host says the caller is an MCP credential
//! (`crate::CREDENTIAL_HEADER` = `mcp`): its name ([`LABEL_HEADER`], set by
//! the host from the credential's label, which it strips from every incoming
//! request), whether it has a grant, the tool, one short target (the first of
//! the arguments' `id`, `collection`, `component`, `group`, `edit`, `name`,
//! `layout` that is text), and whether it was allowed, refused (by the
//! credential's access or grant, [`REFUSALS`]) or failed. Never the rest of
//! the arguments or the answer: what a call wrote is in its source's history.
//! Two things of an answer are kept, because undoing needs them: a row edit's
//! id (`edit`, for the source's `undo_edit`) and the checkpoint an applied
//! edit group took first (`checkpoint`). Kept 30 days.
//!
//! **Undo this morning** ([`undo`]): for the granted credentials (or one
//! named), since a time (default the start of today in the pod's zone):
//! restore the checkpoint taken before the first edit group they applied, then
//! undo every row edit they made, newest first. Only the owner undoes: a
//! device of the pod (`POST /v1/background/undo`, Today's summary card) or an
//! MCP connection without a grant. A second undo starts after the first, so a
//! second tap doesn't restore again.

use std::collections::BTreeSet;

use axum::http::HeaderMap;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::Pod;
use crate::util::now_secs;

/// Request header a pod host sets beside [`crate::CREDENTIAL_HEADER`]` =
/// mcp`: the credential's label, base64url (no padding) of its UTF-8. The
/// host removes any value a client sent.
pub const LABEL_HEADER: &str = "x-construct-mcp-label";

/// The error codes that are a refusal by the caller's access or grant, not a
/// failure of the call.
pub const REFUSALS: &[&str] = &[
    "grant_refuses",
    "access_too_low",
    "protected",
    "new_host",
    "not_safe",
    "owner_keeps",
    "owner_only",
];

/// How long calls are kept.
pub const KEEP_SECS: i64 = 30 * 86_400;

/// Refusals in [`WARN_WINDOW_SECS`] that make a warning
/// ([`refused_recently`]).
pub const WARN_AT: i64 = 3;
pub const WARN_WINDOW_SECS: i64 = 3_600;

/// The tool a source answers a row edit's undo with.
const UNDO_TOOL: &str = "undo_edit";

/// The argument keys a call's target is read from, in order.
const TARGET_KEYS: &[&str] = &[
    "id",
    "collection",
    "component",
    "group",
    "edit",
    "name",
    "layout",
];

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mcp_calls (
             id          INTEGER PRIMARY KEY,
             at          INTEGER NOT NULL,
             credential  TEXT NOT NULL,
             granted     INTEGER NOT NULL,
             tool        TEXT NOT NULL,
             target      TEXT NOT NULL,
             outcome     TEXT NOT NULL,
             error       TEXT NOT NULL,
             edit        TEXT,
             checkpoint  TEXT
         );
         CREATE INDEX IF NOT EXISTS mcp_calls_at ON mcp_calls (at);
         CREATE INDEX IF NOT EXISTS mcp_calls_credential ON mcp_calls (credential, at);
         CREATE TABLE IF NOT EXISTS mcp_call_undos (
             at          INTEGER NOT NULL,
             credential  TEXT NOT NULL
         );",
    )
}

/// One logged call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub at: i64,
    pub credential: String,
    pub granted: bool,
    pub tool: String,
    pub target: String,
    /// `allowed`, `refused` or `failed`.
    pub outcome: String,
    /// The answer's error code, when it has one.
    pub error: String,
    pub edit: Option<String>,
    pub checkpoint: Option<String>,
}

/// The caller's name as its host says ([`LABEL_HEADER`]), when the caller is
/// an MCP credential; `None` for anyone else (a device, the pod token).
pub fn caller(pod: &Pod, headers: &HeaderMap) -> Option<String> {
    if pod.host_credential(headers).as_deref() != Some("mcp") {
        return None;
    }
    let label = headers
        .get(LABEL_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(v)
                .ok()
        })
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| clip(s.trim(), 80))
        .filter(|s| !s.is_empty());
    Some(label.unwrap_or_else(|| "MCP connection".to_owned()))
}

/// [`LABEL_HEADER`]'s value for credential label `label`.
pub fn label_header_value(label: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(label.as_bytes())
}

fn clip(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max - 1).chain(['…']).collect()
    }
}

/// The call of tool `tool` with `args` that answered `out` (a tool result:
/// `isError`, `structuredContent`), as a log entry.
pub fn entry(
    credential: &str,
    granted: bool,
    tool: &str,
    args: &Value,
    out: &Value,
    at: i64,
) -> Call {
    let target = TARGET_KEYS
        .iter()
        .find_map(|k| {
            args.get(*k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(|v| format!("{k} {}", clip(v, 80)))
        })
        .unwrap_or_default();
    let sc = out.get("structuredContent").cloned().unwrap_or(Value::Null);
    let is_error = out["isError"] == true;
    let error = if is_error {
        sc["error"]
            .as_str()
            .map(|e| clip(e, 60))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let outcome = if !is_error {
        "allowed"
    } else if REFUSALS.contains(&error.as_str()) {
        "refused"
    } else {
        "failed"
    };
    let id_of = |k: &str| {
        (!is_error)
            .then(|| sc.get(k).and_then(Value::as_str).map(|s| clip(s, 80)))
            .flatten()
    };
    Call {
        at,
        credential: credential.to_owned(),
        granted,
        tool: clip(tool, 80),
        target,
        outcome: outcome.to_owned(),
        error,
        edit: id_of("edit"),
        checkpoint: if tool == "apply_edit_group" {
            id_of("checkpoint")
        } else {
            None
        },
    }
}

/// Record `call`, and forget calls older than [`KEEP_SECS`].
pub fn record(conn: &Connection, call: &Call) -> rusqlite::Result<()> {
    ensure(conn)?;
    conn.execute(
        "INSERT INTO mcp_calls (at, credential, granted, tool, target, outcome, error, edit, checkpoint)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            call.at,
            call.credential,
            call.granted,
            call.tool,
            call.target,
            call.outcome,
            call.error,
            call.edit,
            call.checkpoint
        ],
    )?;
    conn.execute("DELETE FROM mcp_calls WHERE at < ?1", [call.at - KEEP_SECS])?;
    Ok(())
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Call> {
    Ok(Call {
        at: r.get(0)?,
        credential: r.get(1)?,
        granted: r.get(2)?,
        tool: r.get(3)?,
        target: r.get(4)?,
        outcome: r.get(5)?,
        error: r.get(6)?,
        edit: r.get(7)?,
        checkpoint: r.get(8)?,
    })
}

const COLS: &str = "at, credential, granted, tool, target, outcome, error, edit, checkpoint";

/// The calls since `since` (of `credential`, and only refused ones when
/// `refused`), newest first, at most `limit`.
pub fn calls(
    conn: &Connection,
    credential: Option<&str>,
    since: i64,
    refused: bool,
    limit: i64,
) -> rusqlite::Result<Vec<Call>> {
    ensure(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM mcp_calls
         WHERE at >= ?1 AND (?2 IS NULL OR credential = ?2) AND (?3 = 0 OR outcome = 'refused')
         ORDER BY at DESC, id DESC LIMIT ?4"
    ))?;
    stmt.query_map(params![since, credential, refused, limit], row)?
        .collect()
}

/// Each credential's calls since `since`: (credential, granted, calls,
/// refused, last call), the most recent first.
pub fn summary(conn: &Connection, since: i64) -> rusqlite::Result<Vec<Value>> {
    ensure(conn)?;
    let mut stmt = conn.prepare(
        "SELECT credential, MAX(granted), COUNT(*), SUM(outcome = 'refused'), MAX(at)
         FROM mcp_calls WHERE at >= ?1 GROUP BY credential ORDER BY MAX(at) DESC",
    )?;
    stmt.query_map([since], |r| {
        Ok(json!({
            "credential": r.get::<_, String>(0)?,
            "granted": r.get::<_, bool>(1)?,
            "calls": r.get::<_, i64>(2)?,
            "refused": r.get::<_, i64>(3)?,
            "last_at": r.get::<_, i64>(4)?,
        }))
    })?
    .collect()
}

/// The granted credentials refused at least [`WARN_AT`] times in the
/// [`WARN_WINDOW_SECS`] before `now`: (credential, refusals). What a runner
/// warning names (the Agents source sends it).
pub fn refused_recently(conn: &Connection, now: i64) -> rusqlite::Result<Vec<(String, i64)>> {
    ensure(conn)?;
    let mut stmt = conn.prepare(
        "SELECT credential, COUNT(*) FROM mcp_calls
         WHERE at >= ?1 AND granted = 1 AND outcome = 'refused'
         GROUP BY credential HAVING COUNT(*) >= ?2 ORDER BY credential",
    )?;
    stmt.query_map(params![now - WARN_WINDOW_SECS, WARN_AT], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?
    .collect()
}

fn call_json(c: &Call, tz: &jiff::tz::TimeZone) -> Value {
    let at_text = jiff::Timestamp::from_second(c.at)
        .map(|t| {
            t.to_zoned(tz.clone())
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default();
    let mut v = json!({
        "at": c.at, "at_text": at_text, "credential": c.credential, "granted": c.granted,
        "tool": c.tool, "target": c.target, "outcome": c.outcome,
    });
    if !c.error.is_empty() {
        v["error"] = json!(c.error);
    }
    if let Some(e) = &c.edit {
        v["edit"] = json!(e);
    }
    if let Some(k) = &c.checkpoint {
        v["checkpoint"] = json!(k);
    }
    v
}

/// The start of today in the pod's zone, as Unix seconds.
fn start_of_today(store: &crate::store::Store) -> rusqlite::Result<i64> {
    let tz = crate::tz::time_zone(store)?;
    Ok(jiff::Timestamp::now()
        .to_zoned(tz)
        .start_of_day()
        .map(|z| z.timestamp().as_second())
        .unwrap_or_else(|_| now_secs() - 86_400))
}

/// `call_log {credential?, since?, refused?, limit?}`: the MCP tool.
pub async fn tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let credential = args["credential"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let since = args["since"].as_i64();
    let refused = args["refused"] == true;
    let limit = args["limit"].as_i64().unwrap_or(100).clamp(1, 500);
    crate::blocking(pod, move |s| {
        let since = match since {
            Some(t) => t,
            None => start_of_today(s)?,
        };
        let tz = crate::tz::time_zone(s)?;
        s.with_conn(|c| {
            let list = calls(c, credential.as_deref(), since, refused, limit)?;
            Ok(json!({
                "since": since,
                "credentials": summary(c, since)?,
                "calls": list.iter().map(|x| call_json(x, &tz)).collect::<Vec<_>>(),
                "note": "Every MCP tool call a credential made, as the pod recorded it (the caller can't write \
                         this log). outcome: allowed, refused (by its access or grant) or failed. edit and \
                         checkpoint are what undo_background reverts.",
            }))
        })
    })
    .await
    .map_err(|_| json!({ "error": "store_error" }))
}

/// What [`undo`] would revert: the checkpoint to restore (the one taken
/// before the first group applied) and the row edits, newest first.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub since: i64,
    pub credentials: BTreeSet<String>,
    pub checkpoint: Option<String>,
    pub edits: Vec<(String, String)>,
}

/// The plan for the granted credentials (or `credential`) since `since`,
/// after the last undo of the same ones.
pub fn plan(conn: &Connection, credential: Option<&str>, since: i64) -> rusqlite::Result<Plan> {
    ensure(conn)?;
    let last_undo: Option<i64> = conn
        .query_row(
            "SELECT MAX(at) FROM mcp_call_undos WHERE credential = ?1",
            [credential.unwrap_or("")],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let from = since.max(last_undo.map_or(i64::MIN, |t| t + 1));
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM mcp_calls
         WHERE at >= ?1 AND outcome = 'allowed' AND (edit IS NOT NULL OR checkpoint IS NOT NULL)
           AND (CASE WHEN ?2 IS NULL THEN granted = 1 ELSE credential = ?2 END)
         ORDER BY at, id"
    ))?;
    let found: Vec<Call> = stmt
        .query_map(params![from, credential], row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut p = Plan {
        since: from,
        ..Plan::default()
    };
    for c in &found {
        p.credentials.insert(c.credential.clone());
        if p.checkpoint.is_none() {
            p.checkpoint.clone_from(&c.checkpoint);
        }
        if let Some(e) = &c.edit {
            p.edits.push((c.credential.clone(), e.clone()));
        }
    }
    p.edits.reverse();
    Ok(p)
}

/// `undo_background {credential?, since?, dry_run?}`: undo what the pod's
/// granted credentials (or `credential`) did since `since` (default the start
/// of today): their screens back to the checkpoint before their first group,
/// their row edits undone. Only the owner (the caller has no grant).
pub async fn undo(pod: &Pod, args: &Value) -> Result<Value, Value> {
    if crate::grant::current().is_some() {
        return Err(crate::grant::refusal(
            "undo_background, which is the owner's (from Today, or a connection without a grant)",
        ));
    }
    let credential = args["credential"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let since = args["since"].as_i64();
    let cred = credential.clone();
    let plan = crate::blocking(pod, move |s| {
        let since = match since {
            Some(t) => t,
            None => start_of_today(s)?,
        };
        s.with_conn(|c| plan(c, cred.as_deref(), since))
    })
    .await
    .map_err(|_| json!({ "error": "store_error" }))?;
    let mut out = json!({
        "ok": true,
        "since": plan.since,
        "credentials": plan.credentials,
        "checkpoint": plan.checkpoint,
        "edits": plan.edits.len(),
    });
    if args["dry_run"] == true {
        if let Some(id) = &plan.checkpoint {
            let a = json!({ "id": id, "dry_run": true });
            let would = crate::blocking(pod, {
                let p = pod.clone();
                move |s| {
                    Ok(s.with_conn(|c| {
                        crate::construct::edit_groups::restore_checkpoint(&p, c, &a, None)
                    }))
                }
            })
            .await
            .map_err(|_| json!({ "error": "store_error" }))?;
            out["screens"] = would.map(|(v, _)| v["would"].clone()).unwrap_or_else(|e| e);
        }
        out["dry_run"] = json!(true);
        return Ok(out);
    }
    // Screens first: the checkpoint before the first group, in one write.
    if let Some(id) = &plan.checkpoint {
        let a = json!({ "id": id });
        let r = crate::construct::write::run_write(pod, &a, |p, c, a| {
            crate::construct::edit_groups::restore_checkpoint(p, c, a, None)
        })
        .await;
        out["screens"] = match r {
            Ok(v) => v,
            Err(e) => {
                out["ok"] = json!(false);
                e
            }
        };
    }
    // Then each row edit, newest first, through the source that made it.
    let mut rows = Vec::new();
    for (_, edit) in &plan.edits {
        let a = json!({ "edit": edit });
        let mut done = None;
        for ad in pod.catalog().adapters() {
            if let Some(f) = ad.mcp_call(pod, UNDO_TOOL, &a) {
                done = Some(f.await);
                break;
            }
        }
        rows.push(match done {
            Some(Ok(v)) => json!({ "edit": edit, "undone": true, "skipped": v["skipped"] }),
            Some(Err(e)) if e["error"] == "already_undone" => {
                json!({ "edit": edit, "undone": false, "already": true })
            }
            Some(Err(e)) => {
                out["ok"] = json!(false);
                json!({ "edit": edit, "undone": false, "error": e["error"] })
            }
            None => {
                out["ok"] = json!(false);
                json!({ "edit": edit, "undone": false, "error": "no_source" })
            }
        });
    }
    out["rows"] = json!(rows);
    let key = credential.unwrap_or_default();
    let _ = crate::blocking(pod, move |s| {
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO mcp_call_undos (at, credential) VALUES (?1, ?2)",
                params![now_secs(), key],
            )
        })
    })
    .await;
    Ok(out)
}

/// The path Today's summary card posts to (Settings' manifest declares it,
/// so any of the owner's layouts may).
pub const UNDO_PATH: &str = "/v1/background/undo";

pub(crate) fn router() -> axum::Router<Pod> {
    axum::Router::new().route(UNDO_PATH, axum::routing::post(post_undo))
}

async fn post_undo(
    axum::extract::State(pod): axum::extract::State<Pod>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let args = match crate::construct::edit_groups::owner_body(&pod, &headers, &body).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    // A device asks without a credential or a time: today's, every one.
    let args = json!({ "dry_run": args["dry_run"] == true, "credential": args["credential"] });
    match undo(&pod, &args).await {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => crate::json_error(axum::http::StatusCode::CONFLICT, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure(&c).unwrap();
        c
    }

    fn ok(sc: Value) -> Value {
        json!({ "isError": false, "structuredContent": sc })
    }

    fn err(code: &str) -> Value {
        json!({ "isError": true, "structuredContent": { "error": code } })
    }

    #[test]
    fn an_entry_keeps_the_target_the_outcome_and_what_undo_needs() {
        let e = entry(
            "Anticipate build",
            true,
            "edit_rows",
            &json!({ "collection": "anticipations", "edits": [{"op": "create", "values": {"title": "x"}}] }),
            &ok(json!({ "ok": true, "edit": "ed_1" })),
            10,
        );
        assert_eq!(e.target, "collection anticipations");
        assert_eq!(e.outcome, "allowed");
        assert_eq!(e.edit.as_deref(), Some("ed_1"));
        assert_eq!(e.checkpoint, None);
        let r = entry(
            "Anticipate build",
            true,
            "update_layout",
            &json!({ "id": "home" }),
            &err("grant_refuses"),
            11,
        );
        assert_eq!(
            (r.outcome.as_str(), r.error.as_str()),
            ("refused", "grant_refuses")
        );
        let f = entry("x", false, "get_layout", &json!({}), &err("not_found"), 12);
        assert_eq!(f.outcome, "failed");
        let a = entry(
            "x",
            true,
            "apply_edit_group",
            &json!({ "id": "g1" }),
            &ok(json!({ "ok": true, "checkpoint": "ck_1" })),
            13,
        );
        assert_eq!(a.checkpoint.as_deref(), Some("ck_1"));
    }

    #[test]
    fn three_refusals_in_an_hour_warn_and_two_dont() {
        let c = conn();
        for (i, cred) in ["a", "a", "a", "b", "b"].iter().enumerate() {
            let e = entry(
                cred,
                true,
                "t",
                &json!({}),
                &err("grant_refuses"),
                1_000 + i as i64,
            );
            record(&c, &e).unwrap();
        }
        // An ungranted connection's refusals aren't a background agent's.
        for i in 0..3 {
            record(
                &c,
                &entry(
                    "me",
                    false,
                    "t",
                    &json!({}),
                    &err("access_too_low"),
                    1_000 + i,
                ),
            )
            .unwrap();
        }
        assert_eq!(refused_recently(&c, 1_100).unwrap(), vec![("a".into(), 3)]);
        assert!(
            refused_recently(&c, 1_000 + WARN_WINDOW_SECS + 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_plan_restores_the_first_checkpoint_and_undoes_edits_newest_first() {
        let c = conn();
        let put = |cred: &str, granted: bool, tool: &str, sc: Value, at: i64| {
            record(&c, &entry(cred, granted, tool, &json!({}), &ok(sc), at)).unwrap();
        };
        put(
            "build",
            true,
            "apply_edit_group",
            json!({"checkpoint": "ck_old"}),
            50,
        );
        put("build", true, "add_row", json!({"edit": "e1"}), 100);
        put(
            "build",
            true,
            "apply_edit_group",
            json!({"checkpoint": "ck_1"}),
            110,
        );
        put("sense", true, "add_row", json!({"edit": "e2"}), 120);
        put(
            "build",
            true,
            "apply_edit_group",
            json!({"checkpoint": "ck_2"}),
            130,
        );
        put("me", false, "add_row", json!({"edit": "mine"}), 140);
        let p = plan(&c, None, 100).unwrap();
        assert_eq!(p.checkpoint.as_deref(), Some("ck_1"));
        assert_eq!(
            p.edits,
            vec![("sense".into(), "e2".into()), ("build".into(), "e1".into())]
        );
        assert_eq!(
            p.credentials,
            ["build".to_owned(), "sense".to_owned()].into()
        );
        let only = plan(&c, Some("build"), 100).unwrap();
        assert_eq!(only.edits, vec![("build".into(), "e1".into())]);
        // After an undo, a second one starts from it.
        c.execute(
            "INSERT INTO mcp_call_undos (at, credential) VALUES (125, '')",
            [],
        )
        .unwrap();
        let again = plan(&c, None, 100).unwrap();
        assert_eq!(again.checkpoint.as_deref(), Some("ck_2"));
        assert!(again.edits.is_empty());
    }

    #[test]
    fn labels_round_trip_through_the_header() {
        let v = label_header_value("Anticipate · build");
        let back = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&v)
            .unwrap();
        assert_eq!(String::from_utf8(back).unwrap(), "Anticipate · build");
    }
}
