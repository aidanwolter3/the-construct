//! The construct for a client, and elements as clients draw them.
//!
//! A client is sent home re-sealed for its features: each name an element
//! follows is taken at the newest entry of its history whose recorded
//! features the client draws (the latest when none fits, as the old system
//! served a blueprint's newest file), and every element on the way is
//! sealed again with those. For a client that draws everything this is the
//! stored home. The result is deterministic and cached per
//! `(generation, features)`; the objects it makes are stored, so they can be
//! fetched like any other.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use construct_elements::refs::Ref;
use construct_elements::{Materialized, Objects, object_id};
use protocol::ClientCaps;
use rusqlite::Connection;
use serde_json::Value;

use super::store as cstore;
use crate::Pod;
use crate::store::Store;

/// The objects of a pod's store, cached in its [`super::State`].
pub struct StoreObjects<'a> {
    pub store: &'a Store,
    pub state: Arc<super::State>,
}

impl Objects for StoreObjects<'_> {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        if let Some(v) = self
            .state
            .objects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
        {
            return Some(v.clone());
        }
        let found = self
            .store
            .with_conn(|c| cstore::object(c, id))
            .ok()
            .flatten()?;
        let mut cache = self.state.objects.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(id.to_owned(), found.0.clone());
        Some(found.0)
    }
}

/// A pod's objects.
pub fn objects(pod: &Pod) -> StoreObjects<'_> {
    StoreObjects {
        store: pod.store(),
        state: super::state(pod),
    }
}

/// The cache key of a client's features.
fn features_key(client: &ClientCaps) -> String {
    if *client == ClientCaps::unfiltered() {
        return "*".into();
    }
    let mut f: Vec<&str> = client.features.iter().map(String::as_str).collect();
    f.sort_unstable();
    f.join(",")
}

/// Home's hash for `client` (`None`: the pod has no home). Blocking.
pub fn root_for(pod: &Pod, client: &ClientCaps) -> rusqlite::Result<Option<String>> {
    let store = pod.store();
    let generation = store.with_conn(cstore::generation)?;
    let key = (generation, features_key(client));
    let st = super::state(pod);
    if let Some(r) = st.roots.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(Some(r.clone()));
    }
    let root =
        store.with_conn(|c| seal_name(c, crate::HOME_APP, client, &mut Sealer::default()))?;
    if let Some(r) = &root {
        let mut roots = st.roots.lock().unwrap_or_else(|e| e.into_inner());
        if roots.len() > 64 {
            roots.clear();
        }
        roots.insert(key, r.clone());
    }
    Ok(root)
}

/// Memo of one sealing.
#[derive(Default)]
pub struct Sealer {
    done: BTreeMap<String, Option<String>>,
    busy: BTreeSet<String>,
}

/// `name` sealed for `client` (see the module docs). Blocking.
pub fn seal_name(
    conn: &Connection,
    name: &str,
    client: &ClientCaps,
    s: &mut Sealer,
) -> rusqlite::Result<Option<String>> {
    if let Some(d) = s.done.get(name) {
        return Ok(d.clone());
    }
    if s.busy.contains(name) {
        return Ok(cstore::name(conn, name)?);
    }
    let Some(current) = cstore::name(conn, name)? else {
        s.done.insert(name.to_owned(), None);
        return Ok(None);
    };
    s.busy.insert(name.to_owned());
    let chosen = if *client == ClientCaps::unfiltered() {
        current.clone()
    } else {
        let mut pick = None;
        for e in cstore::history(conn, name)? {
            if e.hash.is_empty() {
                break;
            }
            if let Some((_, features)) = cstore::object(conn, &e.hash)? {
                let f: Vec<String> = features
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(str::to_owned)
                    .collect();
                if client.can_draw(&f) == protocol::Compatibility::Compatible {
                    pick = Some(e.hash);
                    break;
                }
            }
        }
        pick.unwrap_or_else(|| current.clone())
    };
    let sealed = match cstore::object(conn, &chosen)? {
        Some((el, features)) => {
            let mut v = (*el).clone();
            let mut changed = false;
            let mut wanted: Vec<(String, Option<String>)> = Vec::new();
            construct_elements::refs::visit(&v["node"], &mut |r, _| {
                if !r.pin {
                    wanted.push((r.name.clone(), r.el.clone()));
                }
            });
            let mut to: BTreeMap<String, String> = BTreeMap::new();
            for (n, el) in wanted {
                if to.contains_key(&n) {
                    continue;
                }
                if let Some(h) = seal_name(conn, &n, client, s)?
                    && Some(&h) != el.as_ref()
                {
                    to.insert(n, h);
                }
            }
            if !to.is_empty() {
                construct_elements::refs::map(&mut v["node"], &mut |r: &mut Ref, _| {
                    if !r.pin
                        && let Some(h) = to.get(&r.name)
                    {
                        r.el = Some(h.clone());
                        changed = true;
                    }
                });
            }
            if changed {
                let id = object_id(&v);
                cstore::put_object(conn, &id, &v, &features)?;
                id
            } else {
                chosen
            }
        }
        None => chosen,
    };
    s.busy.remove(name);
    s.done.insert(name.to_owned(), Some(sealed.clone()));
    Ok(Some(sealed))
}

/// The name a layout key stands for on this pod: the key itself when it is
/// a name, else the blueprint's (`settings` → `@core/settings`).
pub fn name_of_key(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    if cstore::name(conn, key)?.is_some() {
        return Ok(Some(key.to_owned()));
    }
    let bp = super::blueprint_name(key);
    Ok(cstore::name(conn, &bp)?.map(|_| bp))
}

/// Element `id` materialized as clients draw it, from the pod's objects.
pub fn materialize(pod: &Pod, id: &str) -> Option<Materialized> {
    construct_elements::materialize(&objects(pod), id).ok()
}

/// Every element reachable from `root`, the root first, then by edges
/// breadth first (the components each uses, too).
pub fn reachable(objects: &dyn Objects, root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([root.to_owned()]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        out.push(id.clone());
        let Some(el) = objects.object(&id) else {
            continue;
        };
        construct_elements::refs::visit(&el["node"], &mut |r, _| {
            if let Some(e) = &r.el
                && !seen.contains(e)
            {
                queue.push_back(e.clone());
            }
        });
        if let Some(ex) = el.get("examples") {
            construct_elements::refs::visit(ex, &mut |r, _| {
                if let Some(e) = &r.el
                    && !seen.contains(e)
                {
                    queue.push_back(e.clone());
                }
            });
        }
    }
    out
}
