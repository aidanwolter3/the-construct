//! Switching a pod to the construct, its dry run, and the revert (decision
//! `content-addressed-construct`): MCP `switch_construct` and `server pods
//! switch <id> [--dry-run | --revert]`.
//!
//! - **Dry run**: convert in memory ([`super::convert`]), render every pod
//!   layout both ways with the pod's data, and report each difference.
//!   Changes nothing.
//! - **Switch**: write the conversion (creating the tables), set the flag.
//!   The old tables stay as they are.
//! - **Revert**: clear the flag; the pod answers from the old tables again.
//!   The construct's tables stay, with their history.

use std::collections::BTreeMap;

use protocol::{ClientCaps, DataBundle, UiNode};
use serde_json::{Value, json};

use super::convert::{self, Plan};
use super::store as cstore;
use super::{CONSTRUCT_EVENT_ID, SWITCHED_SETTING};
use crate::Pod;
use crate::store::Store;

/// The `LayoutEvent::version` that tells home's streams the pod changed
/// systems: they end, and clients ask again which system it is on.
pub const MODE_CHANGED: i64 = -2;

/// What a client that draws everything this build knows sends.
pub fn full_client() -> ClientCaps {
    ClientCaps::from_header(&protocol::features_header())
}

/// Apply `plan`: tables, objects, names, follows, the flag. Answers the
/// names moved with their new `seq`. Blocking.
pub fn apply(store: &Store, plan: &Plan) -> rusqlite::Result<Vec<(String, i64)>> {
    store.transaction(|tx| {
        cstore::ensure_tables(tx)?;
        for (id, v) in &plan.objects {
            let f = plan.features.get(id).map_or("", String::as_str);
            cstore::put_object(tx, id, v, f)?;
        }
        let mut moved = Vec::new();
        for (name, moves) in &plan.names {
            for m in moves {
                if let Some(seq) = cstore::move_name(tx, name, &m.hash, &m.author, Some(&m.note))? {
                    moved.push((name.clone(), seq));
                }
            }
            if let Some(m) = moves.last()
                && let Some(v) = plan.objects.get(&m.hash)
            {
                cstore::set_follows(tx, name, &super::write::followed(v))?;
            }
        }
        cstore::bump_generation(tx)?;
        tx.execute(
            "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, '1')",
            [SWITCHED_SETTING],
        )?;
        Ok(moved)
    })
}

/// Move a switched pod's `@core/` names to this build's blueprints and
/// server components, re-sealing what follows them. Blocking.
pub fn refresh_core(pod: &Pod) -> Result<Vec<(String, i64)>, String> {
    let plan = convert::convert(pod.store(), pod.catalog()).map_err(|e| e.to_string())?;
    let st = super::state(pod);
    let _g = st.write_lock.lock().unwrap_or_else(|e| e.into_inner());
    pod.store()
        .transaction(|tx| {
            let mut changed = Vec::new();
            for (name, moves) in &plan.names {
                if !convert::is_core(name) {
                    continue;
                }
                let Some(m) = moves.last() else { continue };
                if cstore::name(tx, name)?.as_deref() == Some(m.hash.as_str()) {
                    continue;
                }
                changed.push((name.clone(), m.clone()));
            }
            if changed.is_empty() {
                return Ok(Vec::new());
            }
            for (id, v) in &plan.objects {
                let f = plan.features.get(id).map_or("", String::as_str);
                cstore::put_object(tx, id, v, f)?;
            }
            let mut moved = Vec::new();
            for (name, m) in &changed {
                if let Some(seq) = cstore::move_name(tx, name, &m.hash, &m.author, Some(&m.note))? {
                    moved.push((name.clone(), seq));
                }
                if let Some(v) = plan.objects.get(&m.hash) {
                    cstore::set_follows(tx, name, &super::write::followed(v))?;
                }
            }
            for (name, _) in &changed {
                moved.extend(super::write::reseal_followers(tx, pod.catalog(), name)?);
            }
            cstore::bump_generation(tx)?;
            cstore::collect_garbage(tx)?;
            Ok(moved)
        })
        .map_err(|e| e.to_string())
}

/// Home's bindings in `plan`: the old system's home feeds, which on the
/// construct are home's `bind` (only home binds the pod's data; every other
/// layout reads them through it or through `bind` on its edges).
fn home_bindings(plan: &Plan) -> Value {
    plan.current(crate::HOME_APP)
        .and_then(|id| plan.objects.get(id))
        .map_or(json!({}), |v| v["bind"].clone())
}

/// Tell every stream the pod changed systems.
fn announce_mode(pod: &Pod) {
    super::reset(pod);
    pod.publish_layout(CONSTRUCT_EVENT_ID, MODE_CHANGED);
}

/// Switch the pod: convert and write. Blocking.
pub fn switch(pod: &Pod) -> Result<Value, String> {
    let plan = convert::convert(pod.store(), pod.catalog()).map_err(|e| e.to_string())?;
    let moved = apply(pod.store(), &plan).map_err(|e| e.to_string())?;
    announce_mode(pod);
    let root = plan.current(crate::HOME_APP).map(str::to_owned);
    tracing::info!(
        names = plan.names.len(),
        objects = plan.objects.len(),
        "pod switched to the construct"
    );
    Ok(json!({
        "ok": true,
        "switched": true,
        "construct": root,
        "home_bindings": home_bindings(&plan),
        "layouts": plan.layouts,
        "names": plan.names.len(),
        "objects": plan.objects.len(),
        "moved": moved.len(),
        "notes": plan.notes.iter().map(convert::Note::to_json).collect::<Vec<_>>(),
        "revert": "switch_construct {\"revert\": true} (or `server pods switch <id> --revert`) puts the pod back on the old system",
    }))
}

/// Put the pod back on the old system. Blocking.
pub fn revert(pod: &Pod) -> Result<Value, String> {
    let was = super::read_switched(pod.store());
    pod.store()
        .with_conn(|c| {
            c.execute(
                "DELETE FROM pod_settings WHERE key = ?1",
                [SWITCHED_SETTING],
            )
        })
        .map_err(|e| e.to_string())?;
    announce_mode(pod);
    // Installed clients refetch every layout from the old tables.
    if let Ok(apps) = pod.store().with_conn(crate::custom::custom_apps_on) {
        for a in apps {
            if let Ok(Some(v)) = pod.store().app_latest_meta(&a.id) {
                pod.publish_layout(&a.id, v.version);
            }
        }
    }
    tracing::info!(was_switched = was, "pod reverted to the old layout system");
    Ok(json!({
        "ok": true,
        "switched": false,
        "was_switched": was,
        "detail": "The pod answers from the old layout tables again, as they were at the switch. \
                   The construct's names keep their history; switching again converts the old \
                   tables again.",
    }))
}

/// One layout of the dry run: its old tree, its new one, and both drawn.
struct Pair {
    id: String,
    old: Option<(UiNode, BTreeMap<String, UiNode>)>,
    new: Result<(UiNode, BTreeMap<String, UiNode>), String>,
    element: Option<String>,
}

fn parse(v: Value) -> Result<UiNode, String> {
    serde_json::from_value(v).map_err(|e| format!("not a node: {e}"))
}

/// The old system's tree of layout `id` for a full client, with what it
/// embeds. Blocking.
fn old_tree(pod: &Pod, id: &str) -> Option<(UiNode, BTreeMap<String, UiNode>)> {
    let client = full_client();
    let root = crate::layouts::active_for(pod.store(), pod.catalog(), id, &client)
        .ok()
        .flatten()?
        .layout;
    let mut embeds = BTreeMap::new();
    for e in crate::preview::embedded_layouts(&root) {
        if let Ok(Some(s)) = crate::layouts::active_for(pod.store(), pod.catalog(), &e, &client) {
            embeds.insert(e, s.layout);
        }
    }
    Some((root, embeds))
}

/// The construct's tree of layout `id` in `plan`, with what it embeds.
fn new_tree(plan: &Plan, id: &str) -> Result<(UiNode, BTreeMap<String, UiNode>), String> {
    let el = plan.current(id).ok_or("not converted")?;
    let m = construct_elements::materialize(plan, el).map_err(|e| e.to_string())?;
    let root = parse(m.node)?;
    let mut embeds = BTreeMap::new();
    for e in m.edges {
        if e.site != construct_elements::refs::Site::Embed || embeds.contains_key(&e.key) {
            continue;
        }
        let Some(el) = e.el.as_deref() else { continue };
        if let Ok(m) = construct_elements::materialize(plan, el)
            && let Ok(n) = parse(m.node)
        {
            embeds.insert(e.key, n);
        }
    }
    Ok((root, embeds))
}

/// The dry run's report (see the module docs).
pub async fn dry_run(pod: &Pod) -> Result<Value, String> {
    let p = pod.clone();
    let (plan, pairs) = tokio::task::spawn_blocking(move || {
        let plan = convert::convert(p.store(), p.catalog()).map_err(|e| e.to_string())?;
        let pairs: Vec<Pair> = plan
            .layouts
            .iter()
            .map(|id| Pair {
                id: id.clone(),
                old: old_tree(&p, id),
                new: new_tree(&plan, id),
                element: plan.current(id).map(str::to_owned),
            })
            .collect();
        Ok::<_, String>((plan, pairs))
    })
    .await
    .map_err(|e| e.to_string())??;
    let zone = pod
        .blocking(|s| Ok(crate::tz::time_zone(s)))
        .await
        .ok()
        .and_then(Result::ok);
    let mut layouts = Vec::new();
    let mut differences = 0;
    for pair in pairs {
        let data = layout_data(pod, &pair).await;
        let id = pair.id.clone();
        let zone = zone.clone();
        let row = tokio::task::spawn_blocking(move || compare(pair, data, zone))
            .await
            .unwrap_or_else(|e| json!({ "id": id, "same": false, "differences": [e.to_string()] }));
        if row["same"] != json!(true) {
            differences += 1;
        }
        layouts.push(row);
    }
    Ok(json!({
        "dry_run": true,
        "changed": false,
        "layouts_compared": layouts.len(),
        "layouts_different": differences,
        "layouts": layouts,
        "notes": plan.notes.iter().map(convert::Note::to_json).collect::<Vec<_>>(),
        "construct": plan.current(crate::HOME_APP),
        "home_bindings": home_bindings(&plan),
        "names": plan.names.len(),
        "objects": plan.objects.len(),
        "switch": "switch_construct {} (or `server pods switch <id>`) writes this; the old tables stay as they are",
    }))
}

/// The data a layout and what it embeds draw, as the owner's device gets
/// it (the pod's own router: the preview path).
async fn layout_data(pod: &Pod, pair: &Pair) -> DataBundle {
    let mut data = DataBundle::default();
    let mut ids = vec![pair.id.clone()];
    if let Some((_, e)) = &pair.old {
        ids.extend(e.keys().cloned());
    }
    for id in ids {
        if let Ok(spec) = crate::preview::fetch(pod, &axum::http::HeaderMap::new(), &id).await {
            for (k, v) in spec.entities {
                data.entities.entry(k).or_insert(v);
            }
            for (k, v) in spec.queries {
                data.queries.entry(k).or_insert(v);
            }
        }
    }
    data
}

fn compare(pair: Pair, data: DataBundle, zone: Option<jiff::tz::TimeZone>) -> Value {
    let clock = || construct_render::dates::Clock {
        fixed: Some(1_800_000_000),
        zone: zone.clone(),
    };
    let mut row = json!({ "id": pair.id, "element": pair.element });
    let (old, new) = match (pair.old, pair.new) {
        (Some(o), Ok(n)) => (o, n),
        (None, _) => {
            row["same"] = json!(false);
            row["differences"] = json!(["the old system serves nothing for it"]);
            return row;
        }
        (_, Err(e)) => {
            row["same"] = json!(false);
            row["differences"] = json!([format!("it doesn't convert: {e}")]);
            return row;
        }
    };
    let tree_same = serde_json::to_value(&old.0).ok() == serde_json::to_value(&new.0).ok();
    let a = crate::preview::draw(&pair.id, old.0, old.1, data.clone(), clock());
    let b = crate::preview::draw(&pair.id, new.0, new.1, data, clock());
    let (a, b) = match (a, b) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            row["same"] = json!(false);
            row["differences"] = json!([format!(
                "drawing failed: old {:?}, new {:?}",
                a.err(),
                b.err()
            )]);
            return row;
        }
    };
    let mut diffs = Vec::new();
    let n = a.lines.len().max(b.lines.len());
    for i in 0..n {
        let (x, y) = (a.lines.get(i), b.lines.get(i));
        if x != y {
            diffs.push(format!(
                "node {i}: was {} now {}",
                x.map_or("(none)".into(), |s| s.clone()),
                y.map_or("(none)".into(), |s| s.clone())
            ));
            if diffs.len() >= 10 {
                break;
            }
        }
    }
    let pixels_same = a.pixels == b.pixels;
    if diffs.is_empty() && !pixels_same {
        diffs.push("the nodes are the same, but some pixels differ".into());
    }
    row["same"] = json!(diffs.is_empty());
    row["tree_same"] = json!(tree_same);
    row["nodes"] = json!(a.lines.len());
    if !diffs.is_empty() {
        row["differences"] = json!(diffs);
    }
    row
}

/// MCP `switch_construct {dry_run?, revert?}`.
pub async fn tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let fail = |e: String| json!({ "error": "switch_failed", "detail": e });
    if args["revert"].as_bool() == Some(true) {
        let p = pod.clone();
        return tokio::task::spawn_blocking(move || revert(&p))
            .await
            .map_err(|e| fail(e.to_string()))?
            .map_err(fail);
    }
    if args["dry_run"].as_bool() == Some(true) {
        return dry_run(pod).await.map_err(fail);
    }
    let p = pod.clone();
    tokio::task::spawn_blocking(move || {
        let st = super::state(&p);
        let _g = st.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        switch(&p)
    })
    .await
    .map_err(|e| fail(e.to_string()))?
    .map_err(fail)
}

/// The tool's description.
pub fn tool_spec() -> Value {
    json!({
        "name": "switch_construct",
        "title": "Switch to the content-addressed construct",
        "description": "Move this pod from the old layout system (layouts with versions) to the \
            construct (immutable elements by hash, names that move; one hash says whether a device \
            is current). dry_run: true converts in memory, renders every layout both ways with the \
            user's data and reports each difference, changing nothing. Without it the pod is \
            switched (the old tables stay as they are, and installed clients keep working); \
            revert: true puts it back on the old system. Only switch when the user asks.",
        "inputSchema": { "type": "object", "properties": {
            "dry_run": { "type": "boolean" }, "revert": { "type": "boolean" } } },
    })
}

/// Whether a pod's old tables hold anything the switch would convert.
pub fn has_layouts(store: &Store) -> bool {
    store
        .with_conn(crate::custom::custom_apps_on)
        .is_ok_and(|a| !a.is_empty())
}
