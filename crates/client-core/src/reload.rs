//! Reloading what the pod says changed (`event: data`, D50; decision
//! `scoped-data-events`).
//!
//! An event that says what changed (`uri_prefixes`, `query_prefixes`)
//! reloads only the cached data whose last answer it matches: the data on
//! screen ([`ConstructEngine::set_visible`]) at once, top first; the rest
//! is marked stale and reloaded once it is on screen again (opening a
//! screen loads its data anyway). An event that doesn't say reloads
//! everything cached. Either way at most [`PARALLEL_LOADS`] requests run at
//! once, and a path the pod answers 404 for is dropped from the cache, so
//! it isn't reloaded again.
//!
//! The engine knows no app: the prefixes come from the pod, and are only
//! compared with entity URIs and query keys.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use serde_json::Value;

use super::{ConstructEngine, Inner, LAYOUT_LOAD_PREFIX, lock};

/// Most data loads a reload runs at once.
pub const PARALLEL_LOADS: usize = 4;

/// What a scoped `event: data` says changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scope {
    /// Entity URIs that start with one of these changed.
    pub uri_prefixes: Vec<String>,
    /// Queries whose key is one of these, or starts with one and `:`.
    pub query_prefixes: Vec<String>,
}

impl Scope {
    /// The scope of an `event: data`'s data, or `None` when it doesn't say
    /// what changed (an older pod, or a source that doesn't know).
    pub fn parse(data: &Value) -> Option<Scope> {
        let list = |k: &str| -> Option<Vec<String>> {
            data.get(k)?
                .as_array()?
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        let uri_prefixes = list("uri_prefixes");
        let query_prefixes = list("query_prefixes");
        if uri_prefixes.is_none() && query_prefixes.is_none() {
            return None;
        }
        Some(Scope {
            uri_prefixes: uri_prefixes.unwrap_or_default(),
            query_prefixes: query_prefixes.unwrap_or_default(),
        })
    }

    /// Whether data holding these entities and queries (`key -> URIs`) may
    /// have changed.
    pub fn matches<'a>(
        &self,
        mut uris: impl Iterator<Item = &'a String>,
        queries: &std::collections::BTreeMap<String, Vec<String>>,
    ) -> bool {
        let uri = |u: &String| self.uri_prefixes.iter().any(|p| u.starts_with(p.as_str()));
        let key = |k: &String| {
            self.query_prefixes.iter().any(|p| {
                k.strip_prefix(p.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
            })
        };
        uris.any(uri) || queries.iter().any(|(k, v)| key(k) || v.iter().any(uri))
    }
}

impl Inner {
    /// The reload key of the engine's own workspace (`layout:<id>`, like
    /// the other layouts it shows).
    fn workspace_key(&self) -> String {
        format!("{LAYOUT_LOAD_PREFIX}{}", self.workspace_id)
    }

    /// An `event: data` that doesn't say what changed: refetch the
    /// workspace and every data path loaded so far.
    pub(crate) fn reload_all(&self) {
        let mut keys = vec![self.workspace_key()];
        keys.extend(lock(&self.state).data.keys().cloned());
        println!(
            "[ConstructEngine] events: data changed; reloading everything ({} cached)",
            keys.len()
        );
        self.reload(keys);
    }

    /// The events stream reconnected after a gap: `event: data` sent while
    /// it was down never arrived, so treat the gap as one unscoped event
    /// that respects what's on screen. Reload the data on screen (and the
    /// workspace) now, and mark the rest of the cached data stale, to be
    /// reloaded once it is shown again. Blocks until the reloads are done.
    pub(crate) fn catch_up_data(&self) {
        let (now, stale, cached) = {
            let mut st = lock(&self.state);
            let mut all = vec![self.workspace_key()];
            all.extend(st.data.keys().cloned());
            let cached = all.len();
            let now: Vec<String> = match &st.visible {
                None => all.clone(),
                Some(visible) => visible
                    .iter()
                    .filter(|k| all.contains(k))
                    .cloned()
                    .collect(),
            };
            let stale: Vec<String> = all.into_iter().filter(|k| !now.contains(k)).collect();
            st.stale.extend(stale.iter().cloned());
            (now, stale.len(), cached)
        };
        println!(
            "[ConstructEngine] events: reconnected; catching up on data changes missed meanwhile \
             (reloading {} of {cached} cached, {stale} marked stale)",
            now.len()
        );
        self.reload(now);
    }

    /// An `event: data` saying what changed: reload the matching data on
    /// screen now, and mark the rest of the matching data stale.
    pub(crate) fn reload_scoped(&self, scope: &Scope) {
        let ws = self.workspace_key();
        let (now, stale, cached) = {
            let mut st = lock(&self.state);
            let mut matched = Vec::new();
            // Unknown entities (a cached workspace not fetched this run):
            // it may hold anything.
            let ws_matches = match &st.workspace_uris {
                Some(uris) => scope.matches(uris.iter(), &st.queries),
                None => true,
            };
            if ws_matches {
                matched.push(ws.clone());
            }
            for (path, load) in &st.data {
                if scope.matches(load.entity_uris.iter(), &load.queries) {
                    matched.push(path.clone());
                }
            }
            let cached = st.data.len() + 1;
            let now: Vec<String> = match &st.visible {
                // Nothing said what's on screen: everything that matches.
                None => matched.clone(),
                Some(visible) => visible
                    .iter()
                    .filter(|k| matched.contains(k))
                    .cloned()
                    .collect(),
            };
            let stale: Vec<String> = matched.into_iter().filter(|k| !now.contains(k)).collect();
            st.stale.extend(stale.iter().cloned());
            (now, stale.len(), cached)
        };
        println!(
            "[ConstructEngine] events: data changed ({} uri / {} query prefix(es)); reloading {} \
             of {cached} cached, {stale} marked stale",
            scope.uri_prefixes.len(),
            scope.query_prefixes.len(),
            now.len()
        );
        self.reload(now);
    }

    /// Reload these keys (data paths, `layout:<id>`, or the workspace's
    /// key), [`PARALLEL_LOADS`] at a time, in order. Blocks until done.
    pub(crate) fn reload(&self, keys: Vec<String>) {
        if keys.is_empty() {
            return;
        }
        let started = Instant::now();
        let n = keys.len();
        let queue = Mutex::new(VecDeque::from(keys));
        let ws = self.workspace_key();
        let work = || {
            loop {
                if self.signal.is_shut_down() {
                    return;
                }
                let Some(key) = lock(&queue).pop_front() else {
                    return;
                };
                if key == ws {
                    self.refresh();
                } else if self.begin_load(&key) {
                    self.load_data(&key);
                }
            }
        };
        thread::scope(|s| {
            for i in 1..PARALLEL_LOADS.min(n) {
                let spawned = thread::Builder::new()
                    .name(format!("construct-reload-{i}"))
                    .spawn_scoped(s, work);
                if let Err(e) = spawned {
                    eprintln!("[ConstructEngine] cannot start a reload thread: {e}");
                }
            }
            work();
        });
        println!(
            "[ConstructEngine] reloaded {n} in {} ms",
            started.elapsed().as_millis()
        );
    }

    /// See [`ConstructEngine::set_visible`].
    fn set_visible(self: &Arc<Self>, keys: Vec<String>) {
        let due: Vec<String> = {
            let mut st = lock(&self.state);
            if st.visible.as_ref() == Some(&keys) {
                return;
            }
            let due = keys
                .iter()
                .filter(|k| st.stale.remove(*k))
                .cloned()
                .collect();
            st.visible = Some(keys);
            due
        };
        if due.is_empty() {
            return;
        }
        println!(
            "[ConstructEngine] {} stale on screen again; reloading",
            due.len()
        );
        let inner = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("construct-reload".into())
            .spawn(move || inner.reload(due));
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] cannot start a reload thread: {e}");
        }
    }
}

impl ConstructEngine {
    /// What is on screen, topmost first, as the engine's load keys: data
    /// paths (a screen's `data`) and `layout:<id>` for each layout drawn
    /// (the engine's own workspace included). A scoped `event: data`
    /// reloads only these at once and marks the rest of what it matches
    /// stale; stale data shown again (this call) is reloaded. Until the
    /// first call the engine treats everything as on screen. Never blocks.
    pub fn set_visible(&self, keys: Vec<String>) {
        self.inner.set_visible(keys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn parses_only_events_that_say_what_changed() {
        assert_eq!(Scope::parse(&json!({"workspace_id": "home"})), None);
        let s = Scope::parse(&json!({"uri_prefixes": ["a://x/"]})).unwrap();
        assert_eq!(s.uri_prefixes, vec!["a://x/"]);
        assert!(s.query_prefixes.is_empty());
        // Malformed lists say nothing.
        assert_eq!(Scope::parse(&json!({"uri_prefixes": [1]})), None);
    }

    #[test]
    fn query_keys_match_whole_names_only() {
        let s = Scope {
            uri_prefixes: vec!["a://groceries/".into()],
            query_prefixes: vec!["list:groceries".into()],
        };
        let q = |k: &str, v: &[&str]| {
            BTreeMap::from([(k.to_owned(), v.iter().map(|s| (*s).to_owned()).collect())])
        };
        let none: Vec<String> = Vec::new();
        assert!(s.matches(none.iter(), &q("list:groceries", &[])));
        assert!(s.matches(none.iter(), &q("list:groceries:row:1", &[])));
        assert!(!s.matches(none.iter(), &q("list:groceries2", &[])));
        assert!(s.matches(none.iter(), &q("other", &["a://groceries/1"])));
        let uris = ["a://meals/1".to_owned()];
        assert!(!s.matches(uris.iter(), &q("list:meals", &["a://meals/1"])));
    }
}
