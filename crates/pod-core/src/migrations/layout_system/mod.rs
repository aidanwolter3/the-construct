//! The one-time move of a pod from the old layout system to the construct
//! (decision `content-addressed-construct`, issue #300).
//!
//! The old system kept each layout's versions (`custom_apps`,
//! `app_versions`, `layout_blobs`), the components (`components`) and a
//! few settings. A pod that a build from before the construct made, and
//! that was never switched, is converted the first time it opens
//! ([`open`]): the conversion ([`convert`]) is drawn both ways first
//! ([`dry_run`]) and every difference is logged, then written in one
//! transaction ([`apply`]). Afterwards (on every pod, switched before or
//! now) the old tables and settings are dropped by [`STEPS`] (scope
//! [`SCOPE`]).
//!
//! Nothing outside this module reads the old tables.

pub mod convert;
pub mod expand;
pub mod tables;

use std::collections::BTreeMap;

use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};

use self::convert::Plan;
use self::tables::Old;
use super::Step;
use crate::adapter::Catalog;
use crate::construct::store as cstore;
use crate::store::Store;

/// The scope of [`STEPS`] in `schema_migrations`.
pub const SCOPE: &str = "construct";

/// Run after a pod is on the construct ([`open`] has converted it, or it
/// was switched before): the old layout system's tables and settings go.
pub const STEPS: &[Step] = &[Step::sql(
    "drop_old_layout_system",
    "DROP TABLE IF EXISTS custom_apps;
     DROP TABLE IF EXISTS app_versions;
     DROP TABLE IF EXISTS layout_blobs;
     DROP TABLE IF EXISTS components;
     DROP TABLE IF EXISTS layouts;
     DROP TABLE IF EXISTS layout_versions;
     DROP TABLE IF EXISTS workspace_revisions;
     DELETE FROM pod_settings
       WHERE key IN ('apps.hidden', 'updates.dismissed', 'blueprints.convert', 'layouts.convert');",
)];

/// One layout of the dry run, compared.
#[derive(Debug, Clone, PartialEq)]
pub struct Compared {
    pub id: String,
    /// The element it converts to.
    pub element: Option<String>,
    /// What draws differently (empty: nothing).
    pub differences: Vec<String>,
}

/// What a conversion found and did.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Every pod layout, drawn the old way and the new.
    pub layouts: Vec<Compared>,
    /// What the conversion changed in meaning, as `{at, kind, detail}`.
    pub notes: Vec<Value>,
    /// Home's element.
    pub construct: Option<String>,
}

impl Report {
    /// The layouts that draw differently.
    pub fn different(&self) -> impl Iterator<Item = &Compared> {
        self.layouts.iter().filter(|c| !c.differences.is_empty())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "construct": self.construct,
            "layouts_compared": self.layouts.len(),
            "layouts_different": self.different().count(),
            "layouts": self.layouts.iter().map(|c| json!({
                "id": c.id, "element": c.element, "same": c.differences.is_empty(),
                "differences": c.differences,
            })).collect::<Vec<_>>(),
            "notes": self.notes,
        })
    }
}

/// Write `plan`: objects, names, follows, the flag. Answers the names moved
/// with their new `seq`. One transaction: a failure writes nothing.
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
                cstore::set_follows(tx, name, &crate::construct::write::followed(v))?;
            }
        }
        cstore::bump_generation(tx)?;
        tx.execute(
            "INSERT OR REPLACE INTO pod_settings (key, value) VALUES (?1, '1')",
            [crate::construct::SWITCHED_SETTING],
        )?;
        Ok(moved)
    })
}

/// The old system's tree of layout `id` for a client that draws everything,
/// with what it embeds.
fn old_tree(old: &Old, catalog: &Catalog, id: &str) -> Option<(UiNode, BTreeMap<String, UiNode>)> {
    let served = |id: &str| -> Option<UiNode> {
        let (_, v) = old.version(id, None)?;
        serde_json::from_value(expand::served(old, catalog, &v.json)).ok()
    };
    let root = served(id)?;
    let mut embeds = BTreeMap::new();
    for e in crate::preview::embedded_layouts(&root) {
        if let Some(n) = served(&e) {
            embeds.insert(e, n);
        }
    }
    Some((root, embeds))
}

/// The construct's tree of layout `id` in `plan`, with what it embeds.
fn new_tree(plan: &Plan, id: &str) -> Result<(UiNode, BTreeMap<String, UiNode>), String> {
    let parse =
        |v: Value| serde_json::from_value::<UiNode>(v).map_err(|e| format!("not a node: {e}"));
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

type Tree = (UiNode, BTreeMap<String, UiNode>);

/// Draw both trees (no data, a fixed clock) and list what differs.
fn compare(
    id: &str,
    old: Option<Tree>,
    new: Result<Tree, String>,
    zone: Option<jiff::tz::TimeZone>,
) -> Vec<String> {
    let clock = || construct_render::dates::Clock {
        fixed: Some(1_800_000_000),
        zone: zone.clone(),
    };
    let (old, new) = match (old, new) {
        (Some(o), Ok(n)) => (o, n),
        (None, _) => return vec!["the old system serves nothing for it".into()],
        (_, Err(e)) => return vec![format!("it doesn't convert: {e}")],
    };
    let a = crate::preview::draw(id, old.0, old.1, DataBundle::default(), clock());
    let b = crate::preview::draw(id, new.0, new.1, DataBundle::default(), clock());
    let (a, b) = match (a, b) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            return vec![format!(
                "drawing failed: old {:?}, new {:?}",
                a.err(),
                b.err()
            )];
        }
    };
    let mut diffs = Vec::new();
    for i in 0..a.lines.len().max(b.lines.len()) {
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
    if diffs.is_empty() && a.pixels != b.pixels {
        diffs.push("the nodes are the same, but some pixels differ".into());
    }
    diffs
}

/// Draw every converted layout the old way and the new, with no data
/// (the old way reads the old tables), and report each difference.
/// Changes nothing.
pub fn dry_run(store: &Store, catalog: &Catalog, old: &Old, plan: &Plan) -> Report {
    let zone = crate::tz::time_zone(store).ok();
    let layouts = plan
        .layouts
        .iter()
        .map(|id| {
            let element = plan.current(id).map(str::to_owned);
            let differences = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compare(
                    id,
                    old_tree(old, catalog, id),
                    new_tree(plan, id),
                    zone.clone(),
                )
            }))
            .unwrap_or_else(|_| vec!["drawing it panicked".into()]);
            Compared {
                id: id.clone(),
                element,
                differences,
            }
        })
        .collect();
    Report {
        layouts,
        notes: plan.notes.iter().map(convert::Note::to_json).collect(),
        construct: plan.current(crate::HOME_APP).map(str::to_owned),
    }
}

/// Whether the pod is on the construct already.
fn switched(store: &Store) -> bool {
    crate::construct::read_switched(store)
}

/// Convert a pod the old system still holds (see the module docs): `None`
/// when there was nothing to convert (switched before, or a new pod).
/// `Err` when the conversion couldn't be written: nothing was, and the pod
/// doesn't open (its old tables stay as they are).
pub fn open(store: &Store, catalog: &Catalog) -> Result<Option<Report>, String> {
    if switched(store) {
        return Ok(None);
    }
    let has = store
        .with_conn(tables::has_layouts)
        .map_err(|e| format!("reading the old layout tables: {e}"))?;
    if !has {
        return Ok(None);
    }
    let old = store
        .with_conn(Old::read)
        .map_err(|e| format!("reading the old layout tables: {e}"))?;
    let plan = convert::convert(&old, catalog);
    let report = dry_run(store, catalog, &old, &plan);
    log(&report);
    apply(store, &plan).map_err(|e| format!("writing the conversion to the construct: {e}"))?;
    tracing::info!(
        names = plan.names.len(),
        objects = plan.objects.len(),
        layouts = plan.layouts.len(),
        "pod moved from the old layout system to the construct"
    );
    Ok(Some(report))
}

/// Log a dry run: each layout that draws differently (a warning), else
/// that every layout draws the same.
pub fn log(report: &Report) {
    let different: Vec<&Compared> = report.different().collect();
    if different.is_empty() {
        tracing::info!(
            layouts = report.layouts.len(),
            "construct conversion dry run: every layout draws the same"
        );
    }
    for c in different {
        tracing::warn!(
            layout = %c.id,
            element = ?c.element,
            differences = ?c.differences,
            "construct conversion dry run: this layout draws differently"
        );
    }
    for n in &report.notes {
        tracing::info!(note = %n, "construct conversion");
    }
}
