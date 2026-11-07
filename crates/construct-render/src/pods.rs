//! A device's own pod, as an app keeps it (feature `remote`): the pod it
//! opened last and the last pod code used, kept in a file (`pod.json`); its
//! session in the platform's secure store (the Keychain on macOS, the
//! Android keystore, else a private file), where the engine keeps it. The
//! engine (`crate::engine`) shows the pod's home, or its sign-in screen
//! without a session (with the last pod code filled in); signing in,
//! leaving the pod and loading it again go through it, as does signing in
//! to an upstream (`crate::signin`). The Android app and
//! the desktop viewer both start here, so they behave the same.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use client_core::SecureStore;

use crate::engine::PodEngine;
use crate::interact::Session;
use crate::layout::Viewport;
use crate::signin::SignIn;
use crate::window::Reopen;

/// The staging pod: the default server, as the main app's.
pub const DEFAULT_SERVER: &str = "https://the-construct-pod.fly.dev";

/// What the file keeps: the server, the last pod code used, and the last
/// code of each other server used (`set_server` goes back to them; their
/// sessions are in the secure store). `token` is read once from files
/// written before sessions moved to the secure store, moved there, and
/// dropped.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct PodState {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default)]
    pub last_code: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub other_codes: BTreeMap<String, String>,
}

pub use crate::engine::ServerOption;

/// The servers a device offers, as the main app's picker: staging and
/// `local` (on Android, the emulator's host and USB), and `server` when
/// it is none of those.
pub fn server_options(local: &[(&str, &str, &str)], server: &str) -> Vec<ServerOption> {
    let mut out: Vec<ServerOption> =
        vec![("staging".into(), "Staging".into(), DEFAULT_SERVER.into())];
    out.extend(
        local
            .iter()
            .map(|(id, title, url)| ((*id).into(), (*title).into(), (*url).into())),
    );
    let server = server.trim_end_matches('/');
    if !server.is_empty() && !out.iter().any(|(_, _, u)| u == server) {
        out.push(("custom".into(), "Custom".into(), server.into()));
    }
    out
}

/// A device's pod: where its state and the engine's cache are kept, the
/// server to sign in to when it has none, its secure store, and where to
/// log.
pub struct PodHome {
    pub file: Option<PathBuf>,
    /// The engines' caches (a folder per server); in memory when `None`.
    pub storage: Option<PathBuf>,
    pub server: String,
    pub secure: Arc<dyn SecureStore>,
    pub log: fn(&str),
    /// How this platform signs in to upstreams (Pixelfed); none: it
    /// doesn't.
    sign_in: Option<SignIn>,
    /// The servers it offers (`local:servers`).
    pub servers: Vec<ServerOption>,
    /// The pod's engine, kept from one session to the next.
    engine: Mutex<Option<PodEngine>>,
}

impl PodHome {
    pub fn new(
        file: Option<PathBuf>,
        storage: Option<PathBuf>,
        server: String,
        secure: Arc<dyn SecureStore>,
        log: fn(&str),
    ) -> PodHome {
        PodHome {
            file,
            storage,
            server,
            secure,
            log,
            sign_in: None,
            servers: Vec::new(),
            engine: Mutex::new(None),
        }
    }

    /// Signs in to upstreams the platform's way (its browser and redirect).
    pub fn with_sign_in(mut self, sign_in: SignIn) -> PodHome {
        self.sign_in = Some(sign_in);
        self
    }

    /// The servers this device offers (`local:servers`, `set_server`).
    pub fn with_servers(mut self, servers: Vec<ServerOption>) -> PodHome {
        self.servers = servers;
        self
    }

    pub fn read(&self) -> Option<PodState> {
        let text = std::fs::read_to_string(self.file.as_ref()?).ok()?;
        match serde_json::from_str(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                (self.log)(&format!("pod.json: {e}"));
                None
            }
        }
    }

    pub fn write(&self, state: &PodState) {
        let Some(f) = &self.file else {
            return;
        };
        let _ = f.parent().map(std::fs::create_dir_all);
        if let Err(e) = serde_json::to_string(state)
            .map_err(|e| e.to_string())
            .and_then(|t| std::fs::write(f, t).map_err(|e| e.to_string()))
        {
            (self.log)(&format!("saving pod.json: {e}"));
        }
    }

    /// Puts a session token in the secure store, where the engine of `url`
    /// finds it (unless it has one already).
    fn keep_token(&self, url: &str, token: &str) {
        let key = client_core::auth::session_key(url.trim_end_matches('/'));
        if self.secure.get(&key).is_none() {
            self.secure.set(&key, token);
        }
    }

    /// The state as kept, a token from an older file moved to the secure
    /// store.
    fn state(&self) -> Option<PodState> {
        let mut state = self.read()?;
        if let Some(token) = state.token.take() {
            if !token.is_empty() {
                (self.log)("moving the saved session to the secure store");
                self.keep_token(&state.url, &token);
            }
            self.write(&state);
        }
        Some(state)
    }

    /// The session to start with: the pod opened last, else `found` (a
    /// server and session the platform has from elsewhere), else the
    /// default server; signed in or not, as its engine has it.
    pub fn start(&self, found: impl FnOnce() -> Option<(String, String)>) -> Session {
        let state = match self.state() {
            Some(s) => s,
            None => {
                let (url, last_code) = match found() {
                    Some((url, token)) => {
                        self.keep_token(&url, &token);
                        (url, None)
                    }
                    None => (self.server.clone(), None),
                };
                let state = PodState {
                    url,
                    last_code,
                    ..Default::default()
                };
                self.write(&state);
                state
            }
        };
        self.live(&state.url, state.last_code.as_deref())
    }

    /// The session to show after signing in, leaving the pod, or to load
    /// the pod again.
    pub fn reopen(&self, how: Reopen) -> Session {
        let mut state = self.state().unwrap_or_else(|| PodState {
            url: self.server.clone(),
            ..Default::default()
        });
        match how {
            Reopen::SignedIn { code, .. } => {
                state.last_code = Some(code);
                self.write(&state);
            }
            Reopen::Left => {
                (self.log)("leaving the pod");
                self.engine(&state.url).forget_session();
            }
            Reopen::Reload => self.engine(&state.url).refresh(),
            Reopen::Server(url) => {
                // Each server keeps its session (the secure store) and its
                // last code (here).
                let url = url.trim_end_matches('/').to_owned();
                let old = std::mem::replace(&mut state.url, url.clone());
                if let Some(code) = state.last_code.take() {
                    state.other_codes.insert(old, code);
                }
                state.last_code = state.other_codes.remove(&url);
                self.write(&state);
                (self.log)(&format!("switched to {url}"));
            }
        }
        self.live(&state.url, state.last_code.as_deref())
    }

    /// The engine of the pod at `url` (the same one each time).
    pub fn engine(&self, url: &str) -> PodEngine {
        let mut slot = self.engine.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = slot
            .as_ref()
            .filter(|e| e.base() == url.trim_end_matches('/'))
        {
            return e.clone();
        }
        (self.log)(&format!("opening {url}"));
        let e = PodEngine::open_signing_in(
            url,
            self.storage.as_deref(),
            self.secure.clone(),
            client_core::EngineConfig::default(),
            self.sign_in.as_ref(),
        );
        e.set_servers(self.servers.clone());
        *slot = Some(e.clone());
        e
    }

    /// The pod's home as its engine has it: the pod's own sign-in screen
    /// without a session (a pod code box, and a temporary pod), with the
    /// last code filled in.
    pub fn live(&self, url: &str, last_code: Option<&str>) -> Session {
        let engine = self.engine(url);
        let mut s = engine.session(Viewport::PHONE);
        if !engine.signed_in()
            && let Some(code) = last_code
        {
            s.form.insert("pod_token".into(), code.to_owned());
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_servers_keeps_each_ones_code() {
        let dir = std::env::temp_dir().join(format!("construct-pods-{}", std::process::id()));
        let home = PodHome::new(
            Some(dir.join("pod.json")),
            None,
            "http://127.0.0.1:1".into(),
            Arc::new(client_core::MemoryStore::new()),
            |_| {},
        )
        .with_servers(server_options(
            &[("local", "Local", "http://127.0.0.1:2")],
            "http://127.0.0.1:1",
        ));
        assert_eq!(
            home.servers
                .iter()
                .map(|s| s.0.as_str())
                .collect::<Vec<_>>(),
            ["staging", "local", "custom"]
        );
        home.write(&PodState {
            url: "http://127.0.0.1:1".into(),
            last_code: Some("code-one".into()),
            ..Default::default()
        });
        home.reopen(Reopen::Server("http://127.0.0.1:2/".into()));
        let s = home.read().unwrap();
        assert_eq!((s.url.as_str(), s.last_code), ("http://127.0.0.1:2", None));
        home.reopen(Reopen::Server("http://127.0.0.1:1".into()));
        let s = home.read().unwrap();
        assert_eq!(s.last_code.as_deref(), Some("code-one"));
        assert!(s.other_codes.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
