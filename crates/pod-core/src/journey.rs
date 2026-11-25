//! Journey walks (decision `journeys-and-dry-runs`): `walk_journey` replays
//! a script of the simulator's commands (docs/simulator.md, the parser in
//! `construct_render::script`) on the owner's real screens, as a preview
//! does (decision `previews-read-like-a-device`), and answers pass or fail
//! per step, deterministically: no model judges it.
//!
//! - **Writes are dry runs.** A tap's write goes through the pod's own
//!   router as a [`crate::dryrun::DryRun`]: a source that can
//!   ([`crate::Source::dry_runs`]) runs its real write path in a rolled-back
//!   transaction and says what rows it would leave. Those rows join the
//!   walk (its **overlay**): later steps see them, and later dry runs put
//!   them back first, so "add a recipe, then put its ingredients on
//!   Groceries" walks end to end. `expect-write <collection> {fields}`
//!   checks them. A write to a source that can't dry-run isn't sent: the
//!   step is `unverified`.
//! - **Staged screens.** With `group`, the walk reads a staged edit group's
//!   elements for its names (`crate::construct::edit_groups::staged`).
//! - **Classes.** A step is `ok`, `broken` (a target isn't on screen, a
//!   write doesn't match), `data` (an `assert-text` over the live rows
//!   failed: the screens may be fine), `unverified` or `error` (the script).
//! - **Stored.** Every walk is kept (`journey_walks`, 30 days) with the
//!   construct hash it walked and, when it failed, the layout it failed on
//!   and the collection its `expect-write` named: `stage_edit_group
//!   {walk_id}` reads them, so the server, not the caller, says where a fix
//!   may change a binding and what it may call.
//! - **Clock.** Dates read in the pod's zone, at the walk's start.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request};
use construct_render::interact::{Done, Effect, Loader, Session, Target, Write, Written};
use construct_render::script::{self, Step};
use protocol::{DataBundle, UiNode};
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

use crate::Pod;
use crate::dryrun::{DRY_RUN, DryRun, Row};
use crate::preview::{self, Reader};

/// The most steps a walk runs.
const MAX_STEPS: usize = 60;
/// How long a walk's interactions may settle, each.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(5);
/// Walks kept.
const KEEP_SECS: i64 = 30 * 86_400;

pub(crate) fn tools() -> Vec<Value> {
    vec![json!({
        "name": "walk_journey",
        "description": "Replay a journey through the owner's real screens, as their phone would, and say step by step whether it works (decision journeys-and-dry-runs). script: the simulator's commands separated by ; or newlines (docs/simulator.md): open-layout <id> [push|sheet|panel], click <target>, type <field> <text>, back, scroll-to <target>, assert-text <text>, assert-no-text <text>, expect-write <collection> {\"field\": value}. A target is text, tag:<node id>, or <target> in tag:<parent id> (text inside a node, for lists that change). It starts on home (or start). Taps that write are dry runs: the pod runs the write and rolls it back, and the rows it would write join the walk (later steps see them; expect-write checks them); a write no source can dry-run is unverified. group: walk a staged edit group's screens instead of the live ones. Each step's class: ok, broken (the screens fail), data (live data didn't match: the screens may be fine), unverified. The walk is stored: pass its walk_id to stage_edit_group. Nothing is written.",
        "inputSchema": { "type": "object", "properties": {
            "script": { "type": "string" },
            "start": { "type": "string", "description": "The layout to start on (default home)" },
            "group": { "type": "string", "description": "A staged edit group's id: walk its screens" },
            "journey": { "type": "string", "description": "The journey's name, kept with the walk" },
            "shots": { "type": "boolean", "description": "Answer a screenshot of the last screen (and of the failing one)" },
            "viewport": { "type": "object" }
        }, "required": ["script"] }
    })]
}

/// Create the table if it isn't there.
fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS journey_walks (
             id              TEXT PRIMARY KEY,
             at              INTEGER NOT NULL,
             journey         TEXT,
             construct_hash  TEXT,
             grp             TEXT,
             ok              INTEGER NOT NULL,
             class           TEXT NOT NULL,
             failing_layout  TEXT,
             failing_pointer TEXT,
             expect_endpoint TEXT,
             steps           TEXT NOT NULL
         );",
    )
}

/// One write the walk made (dry-run or not sent).
#[derive(Debug, Clone)]
struct WriteLog {
    path: String,
    status: &'static str,
    detail: String,
    rows: Vec<Row>,
}

/// The walk's state the loader shares with the steps.
#[derive(Default)]
struct WalkState {
    overlay: Vec<Row>,
    writes: Vec<WriteLog>,
}

/// Reads the pod as a preview does, serves a staged group's names, and
/// dry-runs writes.
struct WalkReader {
    inner: Reader,
    pod: Pod,
    headers: HeaderMap,
    rt: tokio::runtime::Handle,
    staged: BTreeMap<String, String>,
    objects: BTreeMap<String, Value>,
    state: Arc<Mutex<WalkState>>,
}

impl WalkReader {
    /// A staged layout: its element (with the staged objects over the
    /// stored ones) materialized, and the data of the live element of the
    /// same name.
    fn staged_layout(&self, id: &str, hash: &str) -> Result<(UiNode, DataBundle), String> {
        let objects = self.objects.clone();
        let node = self
            .pod
            .store()
            .with_conn(|c| {
                let o = crate::construct::write::ConnObjects::new(c);
                for (h, v) in &objects {
                    o.extra.borrow_mut().insert(h.clone(), Arc::new(v.clone()));
                }
                Ok::<_, rusqlite::Error>(construct_elements::materialize(&o, hash).map(|m| m.node))
            })
            .map_err(|e: rusqlite::Error| e.to_string())?
            .map_err(|e| format!("staged {id}: {e}"))?;
        let layout: UiNode =
            serde_json::from_value(node).map_err(|e| format!("staged {id}: {e}"))?;
        crate::validate::check_node_values(&layout)?;
        let live = self.inner.layout(id).map(|(_, d)| d).unwrap_or_default();
        Ok((layout, live))
    }

    /// Dry-run `write` through the pod's router.
    fn dry_run(&self, write: &Write) -> WriteLog {
        let path = write.path.clone();
        let can = self
            .pod
            .catalog()
            .adapters()
            .iter()
            .any(|a| a.dry_runs(&path));
        if !can {
            return WriteLog {
                path,
                status: "unverified",
                detail: "no source dry-runs this write, so it wasn't sent".into(),
                rows: Vec::new(),
            };
        }
        let overlay = self
            .state
            .lock()
            .map(|s| s.overlay.clone())
            .unwrap_or_default();
        let dry = DryRun::new(overlay);
        let pod = self.pod.clone();
        let headers = self.headers.clone();
        let body = write.body.to_string();
        let d = dry.clone();
        let p = path.clone();
        let answer = self.rt.block_on(DRY_RUN.scope(d, async move {
            let mut req = Request::builder()
                .method(Method::POST)
                .uri(&p)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .map_err(|e| e.to_string())?;
            for (k, v) in headers.iter() {
                if k != axum::http::header::CONTENT_TYPE && k != axum::http::header::CONTENT_LENGTH
                {
                    req.headers_mut().insert(k.clone(), v.clone());
                }
            }
            let owner = crate::blocking(&pod, |s| s.owner_session())
                .await
                .ok()
                .flatten();
            if let Some(hash) = owner
                && let Ok(v) =
                    HeaderValue::from_str(&pod.internal_as(&hash, crate::session::Scope::Mcp))
            {
                req.headers_mut().insert(crate::session::INTERNAL_HEADER, v);
            }
            let resp = crate::app(pod.clone())
                .oneshot(req)
                .await
                .unwrap_or_else(|e| match e {});
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                .await
                .unwrap_or_default();
            Ok::<_, String>((
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            ))
        }));
        match answer {
            Ok((_, body)) if body["error"] == "dry_run" => WriteLog {
                path,
                status: "dry_run",
                detail: String::new(),
                rows: dry.written(),
            },
            Ok((status, body)) => WriteLog {
                path,
                status: "refused",
                detail: format!(
                    "{} {}",
                    status.as_u16(),
                    body["detail"]
                        .as_str()
                        .or(body["error"].as_str())
                        .unwrap_or("")
                ),
                rows: Vec::new(),
            },
            Err(e) => WriteLog {
                path,
                status: "refused",
                detail: e,
                rows: Vec::new(),
            },
        }
    }
}

/// The data a dry run's rows add to the walk: each row as its entity (at
/// the URI its source gave it).
fn rows_data(rows: &[Row]) -> DataBundle {
    let mut d = DataBundle::default();
    for r in rows {
        let mut fields = r.values.clone().unwrap_or_default();
        fields.insert("id".into(), json!(r.id));
        fields.insert("uri".into(), json!(r.uri));
        fields.insert("deleted".into(), json!(r.values.is_none()));
        d.entities
            .insert(r.uri.clone(), preview::record(Value::Object(fields)));
    }
    d
}

impl Loader for WalkReader {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let (node, mut data) = match self.staged.get(id) {
            Some(h) => self.staged_layout(id, h)?,
            None => self.inner.layout(id)?,
        };
        // What the walk's writes would have left shows on every screen.
        let s = self.state.lock().map_err(|e| e.to_string())?;
        let extra = rows_data(&s.overlay);
        data.entities.extend(extra.entities);
        Ok((node, data))
    }

    fn data(&self, path: &str) -> Result<DataBundle, String> {
        self.inner.data(path)
    }

    fn send(&self, write: Write, done: Done) -> bool {
        let log = self.dry_run(&write);
        let ok = log.status != "refused";
        let detail = log.detail.clone();
        let refreshed = if log.status == "dry_run" {
            vec![rows_data(&log.rows)]
        } else {
            Vec::new()
        };
        if let Ok(mut s) = self.state.lock() {
            for r in &log.rows {
                s.overlay
                    .retain(|o| !(o.collection == r.collection && o.id == r.id));
                s.overlay.push(r.clone());
            }
            s.writes.push(log);
        }
        done(Written {
            result: if ok {
                Ok(json!({ "dry_run": true }))
            } else {
                Err(detail)
            },
            refreshed,
        });
        true
    }
}

fn center(n: &construct_render::scene::SemanticNode) -> (f32, f32) {
    (
        n.bounds[0] + n.bounds[2] / 2.0,
        n.bounds[1] + n.bounds[3] / 2.0,
    )
}

/// The node a target names, scrolling down to find it.
fn reveal(s: &mut Session, target: &str) -> Option<construct_render::scene::SemanticNode> {
    for _ in 0..15 {
        s.poll();
        let scene = s.scene().ok()?.clone();
        if let Some(n) = script::find(&scene, target) {
            let h = s.viewport.height;
            if n.bounds[1] >= 0.0 && n.bounds[1] < h {
                return Some(n);
            }
            let _ = s.scroll_by(None, n.bounds[1] - h / 3.0);
            continue;
        }
        let o = s.scroll_by(None, s.viewport.height * 0.7).ok()?;
        if o.effects.iter().all(|e| matches!(e, Effect::Nothing)) {
            return None;
        }
    }
    None
}

/// Whether a row has every field `want` names, as text.
fn row_matches(r: &Row, want: &Map<String, Value>) -> bool {
    let Some(v) = &r.values else {
        return false;
    };
    want.iter().all(|(k, w)| {
        let have = v.get(k).cloned().unwrap_or(Value::Null);
        construct_render::bind::render(&have) == construct_render::bind::render(w)
    })
}

/// One step's result.
fn step_result(i: usize, line: &script::Line, class: &str, message: String) -> Value {
    json!({ "index": i + 1, "command": line.command, "arg": line.arg, "class": class, "message": message })
}

/// `walk_journey`.
pub(crate) async fn walk(pod: &Pod, headers: &HeaderMap, args: &Value) -> Value {
    let err = |v: Value| json!({ "content": [{ "type": "text", "text": v.to_string() }], "isError": true, "structuredContent": v });
    let script_text = args["script"].as_str().unwrap_or("");
    let lines = match script::parse(script_text) {
        Ok(l) if !l.is_empty() && l.len() <= MAX_STEPS => l,
        Ok(_) => {
            return err(
                json!({ "error": "invalid_arguments", "detail": format!("a script of 1 to {MAX_STEPS} steps") }),
            );
        }
        Err(e) => return err(json!({ "error": "invalid_script", "detail": e })),
    };
    let start = args["start"].as_str().unwrap_or(crate::HOME_APP).to_owned();
    if !preview::is_layout_id(&start) {
        return err(json!({ "error": "unknown_layout", "id": start }));
    }
    let group = args["group"].as_str().map(str::to_owned);
    let (staged, objects) = match &group {
        Some(g) => {
            let g = g.clone();
            match crate::blocking(pod, move |s| {
                s.with_conn(|c| crate::construct::edit_groups::staged(c, &g))
            })
            .await
            {
                Ok(Some(x)) => x,
                _ => return err(json!({ "error": "group_not_found", "group": args["group"] })),
            }
        }
        None => Default::default(),
    };
    let construct_hash = crate::blocking(pod, |s| {
        s.with_conn(|c| crate::construct::store::name(c, crate::HOME_APP))
    })
    .await
    .ok()
    .flatten();
    let zone = crate::blocking(pod, |s| Ok(crate::tz::time_zone(s)))
        .await
        .ok()
        .and_then(Result::ok);
    let state = Arc::new(Mutex::new(WalkState::default()));
    let reader = WalkReader {
        inner: Reader::new(pod, headers, tokio::runtime::Handle::current()),
        pod: pod.clone(),
        headers: preview::forwarded(headers),
        rt: tokio::runtime::Handle::current(),
        staged,
        objects,
        state: state.clone(),
    };
    let viewport = preview::viewport(args);
    let shots = args["shots"] == true;
    let started_at = crate::util::now_secs();
    let result = preview::on_render_thread(move || {
        reader.inner.layouts();
        let (root, mut data) = match reader.layout(&start) {
            Ok(x) => x,
            Err(e) => return Err(json!({ "error": "layout_not_loaded", "detail": e })),
        };
        let needs = serde_json::to_string(&root).unwrap_or_default();
        let device = reader.inner.device(&needs);
        preview::merge(&mut data, device.entities, device.queries);
        let mut layouts = BTreeMap::new();
        for linked in preview::linked_layouts(&root).into_iter().take(8) {
            if linked != start
                && let Ok((l, d)) = reader.layout(&linked)
            {
                preview::merge(&mut data, d.entities, d.queries);
                layouts.insert(linked, l);
            }
        }
        let mut s = Session::new(&start, root, data, viewport);
        s.set_clock(construct_render::dates::Clock { fixed: Some(started_at), zone });
        s.layouts = layouts;
        s.animate = false;
        s.loader = Some(Box::new(reader));
        let mut steps = Vec::new();
        let mut failed: Option<(String, String)> = None; // (class, layout)
        // What the journey expects to write, from the whole script: a walk
        // that broke before its expect-write still names it.
        let mut expect_collection: Option<String> = lines.iter().find_map(|l| match &l.step {
            Step::ExpectWrite { collection, .. } => Some(collection.clone()),
            _ => None,
        });
        let mut last_shot = None;
        for (i, line) in lines.iter().enumerate() {
            s.poll();
            let here = s.current().to_owned();
            let (class, message): (&str, String) = match &line.step {
                Step::OpenLayout { layout, how } => {
                    match s.dispatch(&json!({ "intent": "action.navigation.open", "layout": layout, "presentation": how.wire() })) {
                        Ok(o) if o.effects.iter().any(|e| matches!(e, Effect::Opened { .. } | Effect::OpenedScreen { .. })) => ("ok", format!("opened {layout}")),
                        Ok(o) => ("broken", format!("didn't open {layout}: {}", o.effects.iter().map(preview::effect_line).collect::<Vec<_>>().join("; "))),
                        Err(e) => ("broken", e),
                    }
                }
                Step::Click(target) => match reveal(&mut s, target) {
                    Some(n) => {
                        let (x, y) = center(&n);
                        match s.tap(&Target::Point(x, y)) {
                            Ok(o) => {
                                s.settle(SETTLE);
                                let refused = state
                                    .lock()
                                    .ok()
                                    .and_then(|st| st.writes.last().filter(|w| w.status == "refused").map(|w| w.detail.clone()));
                                match refused {
                                    Some(d) if o.effects.iter().any(|e| matches!(e, Effect::Sent { .. })) => {
                                        ("broken", format!("its write was refused: {d}"))
                                    }
                                    _ => ("ok", o.effects.iter().map(preview::effect_line).collect::<Vec<_>>().join("; ")),
                                }
                            }
                            Err(e) => ("broken", e),
                        }
                    }
                    None => {
                        let scene = s.scene().map(script::texts).unwrap_or_default();
                        ("broken", format!("no '{target}' on {here}; on screen: {scene}"))
                    }
                },
                Step::Type { target, text } => {
                    let scene = s.scene().ok().cloned();
                    let field = scene.as_ref().and_then(|sc| {
                        script::find(sc, target).and_then(|n| {
                            n.id.strip_prefix("field:").map(str::to_owned).or(match &n.tap {
                                Some(construct_render::scene::Tap::Focus { field }) => Some(field.clone()),
                                _ => None,
                            })
                        })
                    }).or_else(|| scene.as_ref().and_then(|sc| sc.fields.iter().find(|f| f.field == *target).map(|f| f.field.clone())));
                    match field {
                        Some(f) => {
                            s.type_text(&f, text);
                            ("ok", format!("typed into {f}"))
                        }
                        None => ("broken", format!("no text field '{target}' on {here}")),
                    }
                }
                Step::Back => match s.dispatch(&json!({ "intent": "action.navigation.back" })) {
                    Ok(_) => ("ok", String::new()),
                    Err(e) => ("broken", e),
                },
                Step::ScrollTo(target) => match reveal(&mut s, target) {
                    Some(_) => ("ok", String::new()),
                    None => ("broken", format!("no '{target}' on {here}")),
                },
                Step::AssertText(t) => {
                    s.settle(SETTLE);
                    let found = s.scene().ok().is_some_and(|sc| script::find(sc, t).is_some()) || reveal(&mut s, t).is_some();
                    if found { ("ok", String::new()) } else {
                        let scene = s.scene().map(script::texts).unwrap_or_default();
                        ("data", format!("'{t}' isn't on {here}; on screen: {scene}"))
                    }
                }
                Step::AssertNoText(t) => {
                    s.settle(SETTLE);
                    if s.scene().ok().is_some_and(|sc| script::find(sc, t).is_none()) { ("ok", String::new()) } else { ("data", format!("'{t}' is on {here}")) }
                }
                Step::ExpectWrite { collection, fields } => {
                    expect_collection = Some(collection.clone());
                    let st = state.lock().map(|st| (st.writes.clone(), st.overlay.clone())).unwrap_or_default();
                    let unverified = st.0.iter().any(|w| w.status == "unverified");
                    if st.1.iter().any(|r| r.collection == *collection && row_matches(r, fields)) {
                        ("ok", format!("a row like that would be written to {collection}"))
                    } else if unverified {
                        ("unverified", "a write wasn't sent: its source can't dry-run".into())
                    } else {
                        let wrote: BTreeSet<String> = st.1.iter().map(|r| r.collection.clone()).collect();
                        ("broken", format!("no write would put a row like {} in {collection} (the walk's writes reach: {:?})", Value::Object(fields.clone()), wrote))
                    }
                }
                Step::Shot(_) => {
                    last_shot = s.render_cpu().ok().map(|img| construct_render::encode::webp(&img));
                    ("ok", String::new())
                }
                Step::Wait(_) | Step::Idle | Step::WaitImages(_) => {
                    s.settle(SETTLE);
                    ("ok", String::new())
                }
                other => ("error", format!("'{}' doesn't run in a journey walk", other.command())),
            };
            steps.push(step_result(i, line, class, message));
            if class != "ok" {
                failed = Some((class.to_owned(), here));
                if shots {
                    last_shot = s.render_cpu().ok().map(|img| construct_render::encode::webp(&img));
                }
                break;
            }
        }
        if shots && last_shot.is_none() {
            last_shot = s.render_cpu().ok().map(|img| construct_render::encode::webp(&img));
        }
        let writes: Vec<Value> = state
            .lock()
            .map(|st| {
                st.writes
                    .iter()
                    .map(|w| json!({ "path": w.path, "status": w.status, "detail": w.detail,
                        "rows": w.rows.iter().map(|r| json!({ "collection": r.collection, "id": r.id, "deleted": r.values.is_none(), "values": r.values })).collect::<Vec<_>>() }))
                    .collect()
            })
            .unwrap_or_default();
        Ok((steps, failed, expect_collection, writes, last_shot))
    })
    .await;
    let (steps, failed, expect_collection, writes, shot) = match result {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return err(e),
        Err(e) => return err(json!({ "error": "walk_failed", "detail": e })),
    };
    let ok = failed.is_none();
    let class = failed
        .as_ref()
        .map(|(c, _)| c.clone())
        .unwrap_or_else(|| "works".into());
    let id = {
        let mut buf = [0u8; 8];
        let _ = getrandom::fill(&mut buf);
        format!("walk_{}", crate::util::hex(&buf))
    };
    let (rid, journey, hash, grp) = (
        id.clone(),
        args["journey"].as_str().map(str::to_owned),
        construct_hash.clone(),
        group.clone(),
    );
    let failing_layout = failed.as_ref().map(|(_, l)| l.clone());
    let endpoint = expect_collection.as_ref().cloned();
    let steps_json = Value::Array(steps.clone()).to_string();
    let (c2, fl) = (class.clone(), failing_layout.clone());
    let _ = crate::blocking(pod, move |s| {
        s.with_conn(|c| {
            ensure(c)?;
            c.execute(
                "INSERT INTO journey_walks (id, at, journey, construct_hash, grp, ok, class, failing_layout,
                                            failing_pointer, expect_endpoint, steps)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![rid, crate::util::now_secs(), journey, hash, grp, ok as i64, c2, fl,
                        fl.as_ref().map(|_| ""), endpoint, steps_json],
            )?;
            c.execute("DELETE FROM journey_walks WHERE at < ?1", [crate::util::now_secs() - KEEP_SECS])?;
            Ok(())
        })
    })
    .await;
    let out = json!({
        "walk_id": id,
        "ok": ok,
        "class": class,
        "failing_layout": failing_layout,
        "construct_hash": construct_hash,
        "group": group,
        "steps": steps,
        "writes": writes,
    });
    let mut content = vec![
        json!({ "type": "text", "text": serde_json::to_string_pretty(&out).unwrap_or_default() }),
    ];
    if let Some(webp) = shot {
        use base64::Engine as _;
        content.insert(
            0,
            json!({ "type": "image", "mimeType": "image/webp",
            "data": base64::engine::general_purpose::STANDARD.encode(&webp) }),
        );
    }
    json!({ "content": content, "isError": false, "structuredContent": out })
}
