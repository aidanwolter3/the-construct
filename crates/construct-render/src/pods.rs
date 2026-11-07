//! A device's own pod, as an app keeps it (feature `remote`): the server it
//! opened last, kept in a file (`pod.json`); its session and the last pod
//! code used with each server in the platform's secure store (the Keychain
//! on macOS, the Android keystore, else a private file), where the engine
//! keeps sessions. A pod code is the login itself, so it is never in
//! `pod.json` or a log. The engine (`crate::engine`) shows the pod's home,
//! or its sign-in screen without a session (with the last pod code filled
//! in); signing in, leaving the pod and loading it again go through it, as
//! does signing in to an upstream (`crate::signin`). A server reached over
//! plain `http://` is refused unless it is on this machine or a private
//! network ([`refuses_cleartext`]). The Android app and the desktop viewer
//! both start here, so they behave the same.

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

/// What the file keeps: the server. `token`, `last_code` and `other_codes`
/// are read once from files written before sessions and pod codes moved to
/// the secure store, moved there, and dropped: they are never written.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct PodState {
    pub url: String,
    #[serde(default, skip_serializing)]
    pub token: Option<String>,
    #[serde(default, skip_serializing)]
    pub last_code: Option<String>,
    #[serde(default, skip_serializing)]
    pub other_codes: BTreeMap<String, String>,
}

/// Where the secure store keeps the last pod code used with `url`.
pub fn code_key(url: &str) -> String {
    format!("pod_code:{}", url.trim_end_matches('/'))
}

/// Whether a native client refuses to send a pod code or session to `url`:
/// plain `http://` to a host other than this machine (`localhost`,
/// loopback), the Android emulator's alias for it (10.0.2.2) or a private
/// network address (10/8, 172.16/12, 192.168/16, link-local, IPv6 unique
/// local), where development servers run. A host name other than
/// `localhost` is refused: what it resolves to can change. `https://` is
/// always allowed.
pub fn refuses_cleartext(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return true;
    };
    match u.scheme() {
        "https" => return false,
        "http" => {}
        _ => return true,
    }
    let Some(host) = u.host_str() else {
        return true;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            !(ip.is_loopback() || ip.is_private() || ip.is_link_local())
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            !(ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local())
        }
        Err(_) => !host.eq_ignore_ascii_case("localhost"),
    }
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
    if !server.is_empty() && !refuses_cleartext(server) && !out.iter().any(|(_, _, u)| u == server)
    {
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
        let server = if refuses_cleartext(&server) {
            log("the configured server is plain http:// to another machine: using staging");
            DEFAULT_SERVER.to_owned()
        } else {
            server
        };
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

    /// The last pod code used with `url`.
    fn code(&self, url: &str) -> Option<String> {
        self.secure.get(&code_key(url)).filter(|c| !c.is_empty())
    }

    /// The state as kept: a token or pod codes from an older file moved to
    /// the secure store (and the file written without them), and a server
    /// it refuses ([`refuses_cleartext`]) replaced by this device's.
    fn state(&self) -> Option<PodState> {
        let mut state = self.read()?;
        let token = state.token.take();
        let last_code = state.last_code.take();
        let other_codes = std::mem::take(&mut state.other_codes);
        let moved = token.is_some() || last_code.is_some() || !other_codes.is_empty();
        if let Some(token) = token.filter(|t| !t.is_empty()) {
            (self.log)("moving the saved session to the secure store");
            self.keep_token(&state.url, &token);
        }
        for (url, code) in other_codes
            .into_iter()
            .chain(last_code.map(|c| (state.url.clone(), c)))
        {
            if !code.is_empty() && self.code(&url).is_none() {
                self.secure.set(&code_key(&url), &code);
            }
        }
        if refuses_cleartext(&state.url) {
            (self.log)("the saved server is plain http:// to another machine: not opening it");
            state.url = self.server.clone();
            self.write(&state);
        } else if moved {
            (self.log)("moved the saved pod codes to the secure store");
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
                let url = match found().filter(|(url, _)| !refuses_cleartext(url)) {
                    Some((url, token)) => {
                        self.keep_token(&url, &token);
                        url
                    }
                    None => self.server.clone(),
                };
                let state = PodState {
                    url,
                    ..Default::default()
                };
                self.write(&state);
                state
            }
        };
        self.live(&state.url)
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
                self.secure.set(&code_key(&state.url), &code);
            }
            Reopen::Left => {
                (self.log)("leaving the pod");
                self.engine(&state.url).forget_session();
            }
            Reopen::Reload => self.engine(&state.url).refresh(),
            Reopen::Server(url) => {
                // Each server keeps its session and its last code (the
                // secure store).
                let url = url.trim_end_matches('/').to_owned();
                if refuses_cleartext(&url) {
                    (self.log)("not switching: plain http:// to another machine");
                } else {
                    state.url = url;
                    self.write(&state);
                    (self.log)(&format!("switched to {}", state.url));
                }
            }
        }
        self.live(&state.url)
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
    /// last code used with it filled in.
    pub fn live(&self, url: &str) -> Session {
        let engine = self.engine(url);
        let mut s = engine.session(Viewport::PHONE);
        if !engine.signed_in()
            && let Some(code) = self.code(url)
        {
            s.form.insert("pod_token".into(), code);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(dir: &std::path::Path, store: Arc<client_core::MemoryStore>) -> PodHome {
        PodHome::new(
            Some(dir.join("pod.json")),
            None,
            "http://127.0.0.1:1".into(),
            store,
            |_| {},
        )
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("construct-pods-{name}-{}", std::process::id()))
    }

    #[test]
    fn switching_servers_keeps_each_ones_code_in_the_secure_store() {
        let dir = temp("switch");
        let store = Arc::new(client_core::MemoryStore::new());
        let home = home(&dir, store.clone()).with_servers(server_options(
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
            ..Default::default()
        });
        home.reopen(Reopen::SignedIn {
            token: "session-one".into(),
            code: "code-one".into(),
        });
        assert_eq!(home.code("http://127.0.0.1:1").as_deref(), Some("code-one"));
        home.reopen(Reopen::Server("http://127.0.0.1:2/".into()));
        assert_eq!(home.read().unwrap().url, "http://127.0.0.1:2");
        assert_eq!(home.code("http://127.0.0.1:2"), None);
        home.reopen(Reopen::Server("http://127.0.0.1:1".into()));
        assert_eq!(home.code("http://127.0.0.1:1").as_deref(), Some("code-one"));
        let file = std::fs::read_to_string(dir.join("pod.json")).unwrap();
        assert!(!file.contains("code-one"), "{file}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn codes_in_an_older_file_move_to_the_secure_store() {
        let dir = temp("migrate");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pod.json"),
            r#"{"url":"http://127.0.0.1:1","token":"old-session","last_code":"code-one",
                "other_codes":{"http://127.0.0.1:2":"code-two"}}"#,
        )
        .unwrap();
        let store = Arc::new(client_core::MemoryStore::new());
        let home = home(&dir, store.clone());
        let state = home.state().unwrap();
        assert_eq!(state.url, "http://127.0.0.1:1");
        assert_eq!(home.code("http://127.0.0.1:1").as_deref(), Some("code-one"));
        assert_eq!(home.code("http://127.0.0.1:2").as_deref(), Some("code-two"));
        assert_eq!(
            store
                .get(&client_core::auth::session_key("http://127.0.0.1:1"))
                .as_deref(),
            Some("old-session")
        );
        let file = std::fs::read_to_string(dir.join("pod.json")).unwrap();
        assert_eq!(file, r#"{"url":"http://127.0.0.1:1"}"#);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plain_http_to_another_machine_is_refused() {
        for url in [
            "http://pod.example",
            "http://pod.example:8080",
            "http://203.0.113.5:8080",
            "http://[2001:db8::1]:8080",
            "http://pod.local",
            "ftp://pod.example",
            "not a url",
        ] {
            assert!(refuses_cleartext(url), "{url}");
        }
        for url in [
            "https://pod.example",
            "http://localhost:8080",
            "http://LOCALHOST",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://10.0.2.2:8080",
            "http://192.168.1.5:8080",
            "http://172.16.0.9",
            "http://169.254.1.1",
            "http://[fd00::5]:8080",
        ] {
            assert!(!refuses_cleartext(url), "{url}");
        }
    }

    #[test]
    fn a_refused_server_is_never_opened() {
        let dir = temp("refused");
        let store = Arc::new(client_core::MemoryStore::new());
        let refused = PodHome::new(
            Some(dir.join("pod.json")),
            None,
            "http://pod.example".into(),
            store.clone(),
            |_| {},
        );
        assert_eq!(refused.server, DEFAULT_SERVER);
        assert!(
            server_options(&[], "http://pod.example")
                .iter()
                .all(|s| s.2 != "http://pod.example")
        );
        let home = home(&dir, store);
        home.write(&PodState {
            url: "http://127.0.0.1:1".into(),
            ..Default::default()
        });
        home.reopen(Reopen::Server("http://pod.example".into()));
        assert_eq!(home.read().unwrap().url, "http://127.0.0.1:1");
        home.write(&PodState {
            url: "http://pod.example".into(),
            ..Default::default()
        });
        assert_eq!(home.state().unwrap().url, "http://127.0.0.1:1");
        let _ = std::fs::remove_dir_all(dir);
    }
}
