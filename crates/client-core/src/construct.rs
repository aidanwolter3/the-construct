//! The content-addressed construct on the device (decision
//! `content-addressed-construct`).
//!
//! The engine asks the pod which system it is on (`GET /v1/construct`): a
//! pod that isn't switched answers `409 not_switched` (an older pod `404`),
//! and the engine uses `/v1/workspaces/<id>` exactly as before. A switched
//! pod answers home's hash. The engine then fetches the objects that hash
//! reaches and it doesn't have (`POST /v1/objects {have, want}`), expands
//! every element itself (`construct_elements::materialize`) and keys each
//! reachable layout by its edge, so the rest of the engine and the renderer
//! see layouts by id as before; a layout's data comes from
//! `/v1/construct/data/<hash>`. Whether the device is current is one
//! comparison: its root against the pod's (`event: construct`).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use construct_elements::refs::{EdgeRef, Site};
use construct_elements::{Objects, instance_id, materialize_with};
use protocol::{DataBundle, WorkspaceSpec};
use serde_json::{Map, Value, json};

use crate::store::StoreOps;
use crate::{FetchError, Inner, lock};

/// Which system the pod is on, as last asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Not asked yet (or a stream reconnected: ask again).
    #[default]
    Unknown,
    /// The layout system: `/v1/workspaces/<id>`.
    Old,
    /// The construct.
    On,
}

/// The device's construct.
#[derive(Default)]
pub struct Construct {
    pub mode: Mode,
    /// Home's element, as the pod last said, and the one before it.
    pub root: Option<String>,
    pub previous: Option<String>,
    /// Objects by id.
    pub objects: HashMap<String, Arc<Value>>,
    /// Each layout reachable from the root: its edge key → element.
    pub keys: BTreeMap<String, String>,
    /// The inputs each key's use site binds (`bind` on its edge).
    pub inputs: BTreeMap<String, Map<String, Value>>,
    /// Layouts the root doesn't reach, opened by name (a notification's):
    /// key → element.
    pub extra: BTreeMap<String, String>,
    /// Whether the objects kept in the engine's store were read.
    pub loaded: bool,
    /// Each key's own edges.
    pub edges: BTreeMap<String, Vec<EdgeRef>>,
    /// Elements materialized, by instance id (`instance_id`: the element
    /// and its bound inputs; immutable, so kept while reachable), and their
    /// edges.
    pub nodes: HashMap<String, Value>,
    pub edges_by_id: HashMap<String, Vec<EdgeRef>>,
    /// How many objects each sync fetched (newest last; tests).
    pub fetched: Vec<usize>,
}

struct Have<'a>(&'a HashMap<String, Arc<Value>>);

impl Objects for Have<'_> {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        self.0.get(id).cloned()
    }
}

impl Construct {
    /// What each key reaches (its edges and its embeds' edges), for the
    /// renderer's stack (`StateSnapshot::construct_json`).
    pub fn snapshot_json(&self) -> String {
        if self.mode != Mode::On {
            return String::new();
        }
        let reach = construct_elements::reach(&self.edges);
        json!({ "root": self.root, "reach": reach }).to_string()
    }

    /// Key every element reachable from the root; `missing` objects listed.
    fn rebuild(&mut self) -> BTreeSet<String> {
        let mut missing = BTreeSet::new();
        let Some(root) = self.root.clone() else {
            return missing;
        };
        let mut keys = BTreeMap::new();
        let mut inputs = BTreeMap::new();
        let mut edges = BTreeMap::new();
        let mut queue =
            VecDeque::from([(crate::DEFAULT_WORKSPACE_ID.to_owned(), root, Map::new())]);
        while let Some((key, id, bound)) = queue.pop_front() {
            if keys.contains_key(&key) {
                continue;
            }
            keys.insert(key.clone(), id.clone());
            let (own, lacks) = self.materialized(&id, &bound);
            missing.extend(lacks);
            for e in &own {
                if let Some(el) = &e.el
                    && !keys.contains_key(&e.key)
                {
                    queue.push_back((e.key.clone(), el.clone(), e.bind.clone()));
                }
            }
            if !bound.is_empty() {
                inputs.insert(key.clone(), bound);
            }
            edges.insert(key, own);
        }
        for (key, id) in self.extra.clone() {
            if !keys.contains_key(&key) {
                missing.extend(self.materialized(&id, &Map::new()).1);
            }
        }
        self.keys = keys;
        self.inputs = inputs;
        self.edges = edges;
        missing
    }

    /// Element `id` with `bound` inputs materialized (kept by instance id):
    /// its edges, and the objects it lacks.
    fn materialized(
        &mut self,
        id: &str,
        bound: &Map<String, Value>,
    ) -> (Vec<EdgeRef>, BTreeSet<String>) {
        let inst = instance_id(id, bound);
        if self.nodes.contains_key(&inst)
            && let Some(e) = self.edges_by_id.get(&inst)
        {
            return (e.clone(), BTreeSet::new());
        }
        match materialize_with(&Have(&self.objects), id, bound) {
            Ok(m) => {
                if m.missing.is_empty() {
                    self.nodes.insert(inst.clone(), m.node);
                    self.edges_by_id.insert(inst, m.edges.clone());
                }
                (m.edges, m.missing)
            }
            Err(_) => (Vec::new(), BTreeSet::new()),
        }
    }

    /// Layout `key`'s element, its inputs and its materialized tree, whether
    /// the root reaches it or it was opened by name.
    pub fn layout(&self, key: &str) -> Option<(String, Map<String, Value>, Value)> {
        let el = self.keys.get(key).or_else(|| self.extra.get(key))?;
        let bound = if self.keys.contains_key(key) {
            self.inputs.get(key).cloned().unwrap_or_default()
        } else {
            Map::new()
        };
        let node = self.nodes.get(&instance_id(el, &bound))?.clone();
        Some((el.clone(), bound, node))
    }

    /// Each key and what it draws (its element and inputs), for comparing
    /// roots.
    fn instances(&self) -> BTreeMap<String, String> {
        self.keys
            .iter()
            .map(|(k, el)| {
                let bound = self.inputs.get(k).cloned().unwrap_or_default();
                (k.clone(), instance_id(el, &bound))
            })
            .collect()
    }

    /// The objects the root and the previous root reach, and the layouts
    /// opened by name (client GC). Answers what is kept.
    fn collect_garbage(&mut self) -> BTreeSet<String> {
        let mut keep = BTreeSet::new();
        let mut queue: VecDeque<String> = self
            .root
            .iter()
            .chain(self.previous.iter())
            .chain(self.extra.values())
            .cloned()
            .collect();
        while let Some(id) = queue.pop_front() {
            if !keep.insert(id.clone()) {
                continue;
            }
            if let Some(v) = self.objects.get(&id) {
                construct_elements::refs::visit(&v["node"], &mut |r, _| {
                    if let Some(e) = &r.el {
                        queue.push_back(e.clone());
                    }
                });
            }
        }
        self.objects.retain(|id, _| keep.contains(id));
        let element = |inst: &str| inst.split('?').next().unwrap_or("").to_owned();
        self.nodes.retain(|inst, _| keep.contains(&element(inst)));
        self.edges_by_id
            .retain(|inst, _| keep.contains(&element(inst)));
        keep
    }
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, FetchError> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .build()
        .map_err(|e| FetchError::Other(format!("client build: {e}")))
}

impl Inner {
    fn construct_request(
        &self,
        req: reqwest::blocking::RequestBuilder,
        token: Option<&str>,
    ) -> Result<reqwest::blocking::Response, FetchError> {
        let mut req = req
            .header(protocol::FEATURES_HEADER, protocol::features_header())
            .header(protocol::DEVICE_HEADER, &self.device_id);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().map_err(|e| FetchError::Other(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Status(resp.status().as_u16()));
        }
        Ok(resp)
    }

    /// Ask the pod which system it is on (when not known), and follow a
    /// switched pod's root. Blocking.
    pub(crate) fn construct_mode(&self, token: Option<&str>) -> Mode {
        let mode = lock(&self.construct).mode;
        if mode != Mode::Unknown {
            return mode;
        }
        let url = format!("{}/v1/construct", self.base_url);
        let answer = client(Duration::from_secs(5))
            .and_then(|c| self.construct_request(c.get(&url), token))
            .and_then(|r| {
                r.json::<Value>()
                    .map_err(|e| FetchError::Other(e.to_string()))
            });
        match answer {
            Ok(v) => match v["root"].as_str() {
                Some(root) => {
                    println!("[ConstructEngine] the pod is on the construct (root {root})");
                    lock(&self.construct).mode = Mode::On;
                    if let Err(e) = self.construct_sync(root, token) {
                        eprintln!("[ConstructEngine] construct sync failed: {e:?}");
                    }
                    Mode::On
                }
                None => Mode::Unknown,
            },
            Err(FetchError::Status(404 | 409)) => {
                let mut c = lock(&self.construct);
                if c.mode == Mode::On {
                    println!("[ConstructEngine] the pod is back on the layout system");
                }
                *c = Construct {
                    mode: Mode::Old,
                    ..Construct::default()
                };
                Mode::Old
            }
            Err(_) => Mode::Unknown,
        }
    }

    /// The objects kept in the engine's store, read once a session is on
    /// the construct: immutable, so what they hold is never fetched again.
    fn construct_load(&self) {
        if std::mem::replace(&mut lock(&self.construct).loaded, true) {
            return;
        }
        let rows = {
            let st = lock(&self.state);
            st.store
                .as_ref()
                .map(|s| s.construct_objects(&self.workspace_id))
        };
        let rows = match rows {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                eprintln!("[ConstructEngine] reading kept objects: {e}");
                return;
            }
            None => return,
        };
        let mut c = lock(&self.construct);
        for (id, text) in rows {
            if let Ok(v) = serde_json::from_str::<Value>(&text)
                && construct_elements::object_id(&v) == id
            {
                c.objects.entry(id).or_insert_with(|| Arc::new(v));
            }
        }
    }

    /// Fetch what `want` reaches that the device lacks (`POST /v1/objects`),
    /// keeping it in memory and in the store; the construct is keyed again
    /// after each answer. Answers how many objects came. Blocking.
    fn construct_fetch_objects(
        &self,
        mut want: Vec<String>,
        token: Option<&str>,
    ) -> Result<usize, FetchError> {
        let mut fetched = 0;
        let mut kept: Vec<(String, String)> = Vec::new();
        let mut result = Ok(());
        for _ in 0..16 {
            let have: Vec<String> = lock(&self.construct).objects.keys().cloned().collect();
            let url = format!("{}/v1/objects", self.base_url);
            let body = json!({ "have": have, "want": want });
            let v: Value = match client(Duration::from_secs(20))
                .and_then(|c| self.construct_request(c.post(&url).json(&body), token))
                .and_then(|r| r.json().map_err(|e| FetchError::Other(e.to_string())))
            {
                Ok(v) => v,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            let mut c = lock(&self.construct);
            if let Some(objs) = v["objects"].as_object() {
                for (id, o) in objs {
                    // An object is its hash: one that doesn't match is not taken.
                    if construct_elements::object_id(o) == *id {
                        c.objects.insert(id.clone(), Arc::new(o.clone()));
                        kept.push((id.clone(), construct_elements::canonical(o)));
                        fetched += 1;
                    }
                }
            }
            let missing = c.rebuild();
            if missing.is_empty() && v["more"] != json!(true) {
                break;
            }
            want = missing.into_iter().collect();
            if want.is_empty() {
                want = c.root.iter().cloned().collect();
            }
        }
        if !kept.is_empty() {
            let st = lock(&self.state);
            if let Some(Err(e)) = st
                .store
                .as_ref()
                .map(|s| s.put_construct_objects(&self.workspace_id, &kept))
            {
                eprintln!("[ConstructEngine] keeping objects: {e}");
            }
        }
        result.map(|()| fetched)
    }

    /// Fetch what `root` reaches that the device lacks, and key it. Answers
    /// the keys whose element (or bound inputs) changed, or appeared, and
    /// those gone. Nothing is fetched when the device already has every
    /// object the root reaches (kept from an earlier run, too). Blocking.
    pub(crate) fn construct_sync(
        &self,
        root: &str,
        token: Option<&str>,
    ) -> Result<(Vec<String>, Vec<String>), FetchError> {
        self.construct_load();
        let (before, missing) = {
            let mut c = lock(&self.construct);
            let before = c.instances();
            if c.root.as_deref() != Some(root) {
                c.previous = c.root.take();
                c.root = Some(root.to_owned());
            }
            (before, c.rebuild())
        };
        let fetched = if missing.is_empty() {
            0
        } else {
            let mut want = vec![root.to_owned()];
            want.extend(missing);
            self.construct_fetch_objects(want, token)?
        };
        let keep = {
            let mut c = lock(&self.construct);
            c.fetched.push(fetched);
            c.collect_garbage()
        };
        {
            let st = lock(&self.state);
            if let Some(Err(e)) = st
                .store
                .as_ref()
                .map(|s| s.retain_construct_objects(&self.workspace_id, &keep))
            {
                eprintln!("[ConstructEngine] forgetting objects: {e}");
            }
        }
        let json = lock(&self.construct).snapshot_json();
        lock(&self.state).construct_json = json;
        let c = lock(&self.construct);
        let now = c.instances();
        let changed: Vec<String> = now
            .iter()
            .filter(|(k, inst)| before.get(*k) != Some(*inst))
            .map(|(k, _)| k.clone())
            .collect();
        let gone: Vec<String> = before
            .keys()
            .filter(|k| !now.contains_key(*k) && !c.extra.contains_key(*k))
            .cloned()
            .collect();
        Ok((changed, gone))
    }

    /// Layout `key`, which the root doesn't reach, by name (`GET
    /// /v1/construct/names/<key>`): a notification's layout. Its element is
    /// fetched and kept under `key`, off the tree. Blocking.
    fn construct_open_by_name(&self, key: &str, token: Option<&str>) -> Result<(), FetchError> {
        let url = format!("{}/v1/construct/names/{key}", self.base_url);
        let v: Value = client(Duration::from_secs(10))
            .and_then(|c| self.construct_request(c.get(&url), token))
            .and_then(|r| r.json().map_err(|e| FetchError::Other(e.to_string())))?;
        let Some(el) = v["el"]
            .as_str()
            .filter(|e| construct_elements::is_object_id(e))
        else {
            return Err(FetchError::Status(404));
        };
        lock(&self.construct)
            .extra
            .insert(key.to_owned(), el.to_owned());
        println!("[ConstructEngine] opening '{key}' by name: the root doesn't reach it");
        let missing = lock(&self.construct).rebuild();
        if !missing.is_empty() {
            let mut want = vec![el.to_owned()];
            want.extend(missing);
            self.construct_fetch_objects(want, token)?;
        }
        Ok(())
    }

    /// The events stream (re)connected: ask again which system the pod is
    /// on. A pod reverted to the layout system is drawn from it again at
    /// once. Blocking (events thread).
    pub(crate) fn construct_reprobe(&self, token: Option<&str>) {
        let was = std::mem::take(&mut lock(&self.construct).mode);
        if was != Mode::On {
            return;
        }
        let before = lock(&self.construct).root.clone();
        match self.construct_mode(token) {
            Mode::Old => {
                self.refresh();
                let loaded: Vec<String> = lock(&self.state).layouts.keys().cloned().collect();
                for k in loaded {
                    let path = format!("{}{k}", crate::LAYOUT_LOAD_PREFIX);
                    if self.begin_load(&path) {
                        self.load_data(&path);
                    }
                }
            }
            Mode::On => {
                // The root moved while the stream was down: what this device
                // shows that changed is drawn again.
                let (root, keys) = {
                    let c = lock(&self.construct);
                    (c.root.clone(), c.keys.clone())
                };
                if root != before {
                    let loaded: Vec<String> = lock(&self.state).layouts.keys().cloned().collect();
                    let extra = lock(&self.construct).extra.clone();
                    let gone = loaded
                        .iter()
                        .filter(|k| !keys.contains_key(*k) && !extra.contains_key(*k))
                        .cloned()
                        .collect();
                    let mut changed = loaded;
                    changed.push(self.workspace_id.clone());
                    self.redraw(&changed, gone);
                }
            }
            Mode::Unknown => {}
        }
    }

    /// Layout `id` from the construct: its materialized tree and its data.
    /// `None`: the pod isn't on the construct (use the old path). Blocking.
    pub(crate) fn construct_fetch(
        &self,
        id: &str,
        token: Option<&str>,
    ) -> Option<Result<(WorkspaceSpec, Option<u64>), FetchError>> {
        if self.construct_mode(token) != Mode::On {
            return None;
        }
        let mut found = lock(&self.construct).layout(id);
        if found.is_none() {
            // Not reached from home (a notification's layout): by name.
            if let Err(e) = self.construct_open_by_name(id, token) {
                return Some(Err(e));
            }
            found = lock(&self.construct).layout(id);
        }
        let Some((hash, bound, node)) = found else {
            return Some(Err(FetchError::Status(404)));
        };
        let workspace = match serde_json::from_value(node) {
            Ok(w) => w,
            Err(e) => return Some(Err(FetchError::Other(format!("bad element: {e}")))),
        };
        let url = format!("{}/v1/construct/data/{hash}", self.base_url);
        let data = client(crate::DATA_LOAD_TIMEOUT)
            .and_then(|c| {
                let mut req = c.get(&url);
                // The data the use site's bindings read too.
                if !bound.is_empty() {
                    req = req.query(&[("bind", Value::Object(bound.clone()).to_string())]);
                }
                self.construct_request(req, token)
            })
            .and_then(|r| {
                r.json::<DataBundle>()
                    .map_err(|e| FetchError::Other(format!("bad data bundle: {e}")))
            });
        if let Err(FetchError::Status(409)) = data {
            // Reverted meanwhile: the layout system again.
            *lock(&self.construct) = Construct {
                mode: Mode::Old,
                ..Construct::default()
            };
            return None;
        }
        Some(data.map(|d| {
            (
                WorkspaceSpec {
                    version: "1.0.0".into(),
                    workspace,
                    entities: d.entities,
                    queries: d.queries,
                },
                None,
            )
        }))
    }

    /// `event: construct`: the pod's root moved. Fetch what changed, reload
    /// the layouts shown whose element changed, and close those no longer
    /// reachable. Blocking (events thread).
    pub(crate) fn on_construct_event(&self, data: &str) {
        let Some(root) = serde_json::from_str::<Value>(data)
            .ok()
            .and_then(|v| v["root"].as_str().map(str::to_owned))
        else {
            return;
        };
        let token = self.session_token();
        {
            let mut c = lock(&self.construct);
            if c.mode == Mode::On && c.root.as_deref() == Some(root.as_str()) {
                // Current: one comparison.
                return;
            }
            c.mode = Mode::On;
        }
        let (changed, gone) = match self.construct_sync(&root, token.as_deref()) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[ConstructEngine] construct sync failed: {e:?}");
                return;
            }
        };
        println!(
            "[ConstructEngine] construct root {root}: {} layouts changed, {} gone",
            changed.len(),
            gone.len()
        );
        self.redraw(&changed, gone);
    }

    /// Draw again the layouts this device holds whose element changed, and
    /// mark those no longer reachable gone (the renderer closes them).
    fn redraw(&self, changed: &[String], gone: Vec<String>) {
        let shown: Vec<String> = {
            let st = lock(&self.state);
            changed
                .iter()
                .filter(|k| st.layouts.contains_key(*k))
                .cloned()
                .collect()
        };
        let home_changed = changed
            .iter()
            .any(|k| k == crate::DEFAULT_WORKSPACE_ID || *k == self.workspace_id);
        {
            let snap = {
                let mut st = lock(&self.state);
                for k in &gone {
                    if let Some(l) = st.layouts.get_mut(k) {
                        l.gone = true;
                    }
                }
                st.seq += 1;
                st.snapshot()
            };
            self.notify(snap);
        }
        if home_changed {
            self.refresh();
        }
        for k in shown {
            let path = format!("{}{k}", crate::LAYOUT_LOAD_PREFIX);
            if self.begin_load(&path) {
                self.load_data(&path);
            }
        }
    }
}

/// Whether an edge is an embed (drawn with the element that has it).
pub fn is_embed(e: &EdgeRef) -> bool {
    e.site == Site::Embed
}
