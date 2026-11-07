//! A device's own pod, as an app keeps it (feature `remote`): the pod it
//! opened last and its session, kept in a file (`pod.json`); the pod's
//! sign-in screen when there is none, with the last pod code filled in;
//! signing in, leaving the pod and loading it again. The Android app and the
//! desktop viewer both start here, so they behave the same.

use std::path::PathBuf;

use crate::interact::Session;
use crate::layout::Viewport;
use crate::window::Reopen;

/// The staging pod: the default server, as the main app's.
pub const DEFAULT_SERVER: &str = "https://the-construct-pod.fly.dev";

/// What the file keeps: the server, the session token (none after leaving
/// the pod) and the last pod code used.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct PodState {
    pub url: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub last_code: Option<String>,
}

/// A device's pod: where its state is kept, the server to sign in to when
/// it has none, and where to log.
pub struct PodHome {
    pub file: Option<PathBuf>,
    pub server: String,
    pub log: fn(&str),
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
                self.write(&state);
                self.sign_in(&state.url, state.last_code.as_deref())
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

    /// The pod's own sign-in screen (what it answers without a session: a
    /// pod code box, and a temporary pod), with the last code filled in.
    pub fn sign_in(&self, url: &str, last_code: Option<&str>) -> Session {
        (self.log)(&format!("signing in to {url}"));
        match crate::remote::PodClient::connect(url, "")
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
