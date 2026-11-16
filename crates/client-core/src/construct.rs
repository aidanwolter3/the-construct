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
use construct_elements::{Objects, materialize};
use protocol::{DataBundle, WorkspaceSpec};
use serde_json::{Value, json};

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
    /// Each key's own edges.
    pub edges: BTreeMap<String, Vec<EdgeRef>>,
    /// Elements materialized, by id (immutable, so kept while reachable),
    /// and their edges.
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
        let mut edges = BTreeMap::new();
        let mut queue = VecDeque::from([(crate::DEFAULT_WORKSPACE_ID.to_owned(), root)]);
        while let Some((key, id)) = queue.pop_front() {
            if keys.contains_key(&key) {
                continue;
            }
            keys.insert(key.clone(), id.clone());
            let own = match (self.nodes.contains_key(&id), self.edges_by_id.get(&id)) {
                (true, Some(e)) => e.clone(),
                _ => match materialize(&Have(&self.objects), &id) {
                    Ok(m) => {
                        missing.extend(m.missing.iter().cloned());
                        if m.missing.is_empty() {
                            self.nodes.insert(id.clone(), m.node);
                            self.edges_by_id.insert(id.clone(), m.edges.clone());
                        }
                        m.edges
                    }
                    Err(_) => Vec::new(),
                },
            };
            for e in &own {
                if let Some(el) = &e.el
                    && !keys.contains_key(&e.key)
                {
                    queue.push_back((e.key.clone(), el.clone()));
                }
            }
            edges.insert(key, own);
        }
        self.keys = keys;
        self.edges = edges;
        missing
    }

    /// The objects the root and the previous root reach (client GC).
    fn collect_garbage(&mut self) {
        let mut keep = BTreeSet::new();
        let mut queue: VecDeque<String> = self
            .root
            .iter()
            .chain(self.previous.iter())
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
        self.nodes.retain(|id, _| keep.contains(id));
        self.edges_by_id.retain(|id, _| keep.contains(id));
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

    /// Fetch what `root` reaches that the device lacks, and key it. Answers
    /// the keys whose element changed (or appeared) and those gone.
    /// Blocking.
    pub(crate) fn construct_sync(
        &self,
        root: &str,
        token: Option<&str>,
    ) -> Result<(Vec<String>, Vec<String>), FetchError> {
        let before = lock(&self.construct).keys.clone();
        let mut want = vec![root.to_owned()];
        let mut fetched = 0;
        for _ in 0..16 {
            let have: Vec<String> = lock(&self.construct).objects.keys().cloned().collect();
            let url = format!("{}/v1/objects", self.base_url);
            let body = json!({ "have": have, "want": want });
            let resp = client(Duration::from_secs(20))
                .and_then(|c| self.construct_request(c.post(&url).json(&body), token))?;
            let v: Value = resp.json().map_err(|e| FetchError::Other(e.to_string()))?;
            let mut c = lock(&self.construct);
            if let Some(objs) = v["objects"].as_object() {
                for (id, o) in objs {
                    // An object is its hash: one that doesn't match is not taken.
                    if construct_elements::object_id(o) == *id {
                        c.objects.insert(id.clone(), Arc::new(o.clone()));
                        fetched += 1;
                    }
                }
            }
            if c.root.as_deref() != Some(root) {
                c.previous = c.root.take();
                c.root = Some(root.to_owned());
            }
            let missing = c.rebuild();
            if missing.is_empty() && v["more"] != json!(true) {
                c.fetched.push(fetched);
                c.collect_garbage();
                break;
            }
            want = if missing.is_empty() {
                vec![root.to_owned()]
            } else {
                missing.into_iter().collect()
            };
        }
        let json = lock(&self.construct).snapshot_json();
        lock(&self.state).construct_json = json;
        let c = lock(&self.construct);
        let changed: Vec<String> = c
            .keys
            .iter()
            .filter(|(k, id)| before.get(*k) != Some(*id))
            .map(|(k, _)| k.clone())
            .collect();
        let gone: Vec<String> = before
            .keys()
            .filter(|k| !c.keys.contains_key(*k))
            .cloned()
            .collect();
        Ok((changed, gone))
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
                    let gone = loaded
                        .iter()
                        .filter(|k| !keys.contains_key(*k))
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
        let found = {
            let c = lock(&self.construct);
            c.keys
                .get(id)
                .and_then(|h| c.nodes.get(h).map(|n| (h.clone(), n.clone())))
        };
        let Some((hash, node)) = found else {
            return Some(Err(FetchError::Status(404)));
        };
        let workspace = match serde_json::from_value(node) {
            Ok(w) => w,
            Err(e) => return Some(Err(FetchError::Other(format!("bad element: {e}")))),
        };
        let url = format!("{}/v1/construct/data/{hash}", self.base_url);
        let data = client(crate::DATA_LOAD_TIMEOUT)
            .and_then(|c| self.construct_request(c.get(&url), token))
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
