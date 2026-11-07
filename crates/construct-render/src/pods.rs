//! A device's own pod, as an app keeps it (feature `remote`): the pod it
//! opened last and its session, kept in a file (`pod.json`); the pod's
//! sign-in screen when there is none, with the last pod code filled in;
//! signing in, leaving the pod and loading it again. The Android app and the
//! desktop viewer both start here, so they behave the same.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::interact::Session;
use crate::layout::Viewport;
use crate::window::Reopen;

/// The staging pod: the default server, as the main app's.
pub const DEFAULT_SERVER: &str = "https://the-construct-pod.fly.dev";

/// What the file keeps: the server, the session token (none after leaving
/// the pod), the last pod code used, the versions pinned on this device,
/// this device's id, and what it keeps for the other servers it has used.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PodState {
    pub url: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub last_code: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: crate::remote::Pins,
    #[serde(default)]
    pub device_id: Option<String>,
    /// The other servers' sessions, codes and pins, by URL (`set_server`
    /// goes back to them).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub servers: BTreeMap<String, ServerState>,
}

/// What a server not in use keeps.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerState {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub last_code: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: crate::remote::Pins,
}

/// A server the device offers (`local:servers`): id, title, URL.
pub type ServerOption = (String, String, String);

/// A device's pod: where its state is kept, the server to sign in to when
/// it has none, the servers it offers, and where to log.
#[derive(Clone)]
pub struct PodHome {
    pub file: Option<PathBuf>,
    pub server: String,
    pub servers: Vec<ServerOption>,
    pub log: fn(&str),
}

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

/// A new device id: 32 hex digits from the standard library's random
/// hasher keys.
fn new_device_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    (0..2)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
            );
            format!("{:016x}", h.finish())
        })
        .collect()
}

impl PodHome {
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

    /// The session to start with: the pod opened last (or its sign-in
    /// screen after leaving it), else `found` (a session the platform has
    /// from elsewhere, such as the main app's sign-in), else the sign-in
    /// screen of the default server.
    pub fn start(&self, found: impl FnOnce() -> Option<(String, String)>) -> Session {
        if let Some(state) = self.read() {
            return match state.token.as_deref().filter(|t| !t.is_empty()) {
                Some(token) => self.live(&state.url, token),
                None => self.sign_in(&state.url, state.last_code.as_deref()),
            };
        }
        match found() {
            Some((url, token)) => {
                self.write(&PodState {
                    url: url.clone(),
                    token: Some(token.clone()),
                    last_code: Some(token.clone()),
                    ..Default::default()
                });
                self.live(&url, &token)
            }
            None => self.sign_in(&self.server, None),
        }
    }

    /// The session to show after signing in, leaving the pod, or to load
    /// the pod again.
    pub fn reopen(&self, how: Reopen) -> Session {
        let mut state = self.read().unwrap_or_else(|| PodState {
            url: self.server.clone(),
            ..Default::default()
        });
        match how {
            Reopen::SignedIn { token, code } => {
                state.token = Some(token.clone());
                state.last_code = Some(code);
                self.write(&state);
                self.live(&state.url, &token)
            }
            Reopen::Left => {
                state.token = None;
                state.pins.clear();
                self.write(&state);
                self.sign_in(&state.url, state.last_code.as_deref())
            }
            Reopen::Server(url) => {
                let url = url.trim_end_matches('/').to_owned();
                // Keep this server's session for coming back to it.
                let here = ServerState {
                    token: state.token.take(),
                    last_code: state.last_code.take(),
                    pins: std::mem::take(&mut state.pins),
                };
                let old = std::mem::replace(&mut state.url, url.clone());
                state.servers.insert(old, here);
                let there = state.servers.remove(&url).unwrap_or_default();
                state.token = there.token;
                state.last_code = there.last_code;
                state.pins = there.pins;
                self.write(&state);
                (self.log)(&format!("switched to {url}"));
                match state.token.clone().filter(|t| !t.is_empty()) {
                    Some(token) => self.live(&url, &token),
                    None => self.sign_in(&url, state.last_code.as_deref()),
                }
            }
            Reopen::Reload => match state.token.clone().filter(|t| !t.is_empty()) {
                Some(token) => self.live(&state.url, &token),
                None => self.sign_in(&state.url, state.last_code.as_deref()),
            },
        }
    }

    /// The pod's home, live with this session; the sign-in screen when the
    /// pod no longer knows it; what went wrong when it can't be reached.
    pub fn live(&self, url: &str, token: &str) -> Session {
        (self.log)(&format!("opening {url}"));
        let started = std::time::Instant::now();
        match crate::remote::PodClient::connect(url, token)
            .map(|c| self.equip(c))
            .and_then(|c| c.session("home", Viewport::PHONE))
        {
            Ok(s) => {
                (self.log)(&format!(
                    "home loaded in {:.0} ms",
                    started.elapsed().as_secs_f64() * 1000.0
                ));
                s
            }
            Err(e) if e.contains("401") => {
                (self.log)(&format!("the session is no longer valid: {e}"));
                let mut state = self.read().unwrap_or_default();
                state.url = url.to_owned();
                state.token = None;
                self.write(&state);
                self.sign_in(url, state.last_code.as_deref())
            }
            Err(e) => {
                (self.log)(&format!("couldn't open the pod: {e}"));
                error_page(&e)
            }
        }
    }

    /// A client with what this device keeps: its pins (saved again when
    /// they change), its servers and its device id.
    fn equip(&self, client: crate::remote::PodClient) -> crate::remote::PodClient {
        let mut state = self.read().unwrap_or_default();
        let id = match state.device_id.clone() {
            Some(id) => id,
            None => {
                let id = new_device_id();
                state.device_id = Some(id.clone());
                self.write(&state);
                id
            }
        };
        let home = self.clone();
        client
            .with_servers(self.servers.clone())
            .with_device_id(&id)
            .with_pins(state.pins, move |pins| {
                let mut s = home.read().unwrap_or_default();
                s.pins = pins.clone();
                home.write(&s);
            })
    }

    /// The pod's own sign-in screen (what it answers without a session: a
    /// pod code box, and a temporary pod), with the last code filled in.
    pub fn sign_in(&self, url: &str, last_code: Option<&str>) -> Session {
        (self.log)(&format!("signing in to {url}"));
        match crate::remote::PodClient::connect(url, "")
            .map(|c| self.equip(c))
            .and_then(|c| c.session("home", Viewport::PHONE))
        {
            Ok(mut s) => {
                if let Some(code) = last_code {
                    s.form.insert("pod_token".into(), code.to_owned());
                }
                s
            }
            Err(e) => {
                (self.log)(&format!("couldn't reach the pod: {e}"));
                error_page(&e)
            }
        }
    }
}

/// A page saying what went wrong, in place of the pod's home, with Try
/// again (`refresh`: the pod is loaded again) and Sign in with another code.
pub fn error_page(message: &str) -> Session {
    let root: protocol::UiNode = serde_json::from_value(serde_json::json!({
        "type": "core://Page", "id": "error", "root": { "type": "core://Column",
        "padding_h_dp": 16, "padding_v_dp": 16, "spacing_dp": 12, "children": [
            { "type": "core://Text", "value_expr": "Couldn't open the pod", "style": "Title" },
            { "type": "core://Text", "value_expr": message, "style": "Body" },
            { "type": "core://Button", "label": "Try again",
              "on_click": { "intent": "action.device", "command": "refresh" } },
            { "type": "core://Button", "label": "Sign in with another code",
              "on_click": { "intent": "action.device", "command": "leave_pod" } }
        ] } }))
    .expect("a valid page");
    Session::new(
        "error",
        root,
        protocol::DataBundle::default(),
        Viewport::PHONE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_servers_keeps_each_ones_session() {
        let dir = std::env::temp_dir().join(format!("construct-pods-{}", std::process::id()));
        let home = PodHome {
            file: Some(dir.join("pod.json")),
            // Nothing listens on port 1: each open fails at once.
            server: "http://127.0.0.1:1".into(),
            servers: server_options(
                &[("local", "Local", "http://127.0.0.1:2")],
                "http://127.0.0.1:1",
            ),
            log: |_| {},
        };
        assert_eq!(
            home.servers
                .iter()
                .map(|s| s.0.as_str())
                .collect::<Vec<_>>(),
            ["staging", "local", "custom"]
        );
        home.write(&PodState {
            url: "http://127.0.0.1:1".into(),
            token: Some("session-one".into()),
            last_code: Some("code-one".into()),
            pins: BTreeMap::from([("notes".to_owned(), 2)]),
            ..Default::default()
        });
        home.reopen(Reopen::Server("http://127.0.0.1:2/".into()));
        let s = home.read().unwrap();
        assert_eq!(s.url, "http://127.0.0.1:2");
        assert_eq!((s.token, s.last_code), (None, None));
        assert!(s.pins.is_empty());
        home.reopen(Reopen::Server("http://127.0.0.1:1".into()));
        let s = home.read().unwrap();
        assert_eq!(s.token.as_deref(), Some("session-one"));
        assert_eq!(s.last_code.as_deref(), Some("code-one"));
        assert_eq!(s.pins.get("notes"), Some(&2));
        assert!(s.servers.contains_key("http://127.0.0.1:2"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
