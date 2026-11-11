//! A live session over a pod (feature `remote`): home and every layout it
//! opens fetched as a client fetches them (`GET /v1/workspaces/<id>`), the
//! device's list of layouts (`local:layouts`) from `GET /v1/apps`, screen
//! data from data paths, and images downloaded on background threads.
//!
//! Read-only: what a tap would change on the pod is reported, not sent
//! (`crate::interact`). There is no cache, outbox or event stream.

use protocol::{DataBundle, UiNode, WorkspaceSpec};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::device_data;
pub use crate::device_data::{
    base_device, channels_bundle, day_label, inbox_entity, layouts_bundle, needs_channels,
    needs_notifications, needs_sign_ins, notifications_bundle, now_secs, parse_rfc3339,
    sign_ins_bundle, time_label, version_time_label, versions_bundle,
};
use crate::images::ImageStore;
use crate::interact::{Done, Loaded, Loader, Session, Write, Written};
use crate::layout::Viewport;

/// A pod and a session on it.
#[derive(Clone)]
pub struct PodClient {
    base: String,
    token: String,
    http: reqwest::blocking::Client,
    /// Layout titles by id (from `/v1/apps`), for device data rows.
    titles: Arc<Mutex<HashMap<String, String>>>,
    /// Versions picked on this device (`pin_version`), by layout. Drawn in
    /// the version list; the layout itself still shows its latest version.
    pins: Arc<Mutex<HashMap<String, i64>>>,
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("construct-render/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("an HTTP client")
}

impl PodClient {
    /// `token`: a session token, or a pod code (`cpod_…`), which is checked
    /// with `POST /v1/pods/open` and the session token it answers is used.
    pub fn connect(base: &str, token: &str) -> Result<PodClient, String> {
        let base = base.trim_end_matches('/').to_owned();
        let http = client();
        let token = if token.starts_with("cpod_") {
            let v: Value = http
                .post(format!("{base}/v1/pods/open"))
                .json(&json!({ "token": token }))
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json())
                .map_err(|e| format!("opening the pod: {e}"))?;
            v["session_token"]
                .as_str()
                .ok_or("opening the pod: no session_token")?
                .to_owned()
        } else {
            token.to_owned()
        };
        Ok(PodClient {
            base,
            token,
            http,
            titles: Arc::default(),
            pins: Arc::default(),
        })
    }

    /// The pod's base URL.
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn session_token(&self) -> &str {
        &self.token
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{}/{}", self.base, path.trim_start_matches('/'))
        };
        let mut req = self
            .http
            .get(&url)
            .header(protocol::FEATURES_HEADER, protocol::features_header());
        // No session: the pod answers its sign-in screen.
        if !self.token.is_empty() {
            req = req.bearer_auth(&self.token);
        }
        let resp = req.send().map_err(|e| format!("{path}: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 401 {
            return Err(format!("{path}: signed out (401): sign in again"));
        }
        if !status.is_success() {
            return Err(format!("{path}: HTTP {status}"));
        }
        resp.json().map_err(|e| format!("{path}: {e}"))
    }

    /// POSTs `body` to a pod path; the answer's JSON (`null` when empty).
    pub fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        let mut req = self
            .http
            .post(&url)
            .header(protocol::FEATURES_HEADER, protocol::features_header())
            .json(body);
        if !self.token.is_empty() {
            req = req.bearer_auth(&self.token);
        }
        let resp = req.send().map_err(|e| format!("{path}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(format!(
                "{path}: HTTP {status} {}",
                text.chars().take(200).collect::<String>()
            ));
        }
        let text = resp.text().map_err(|e| format!("{path}: {e}"))?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A layout's bundle.
    pub fn workspace(&self, id: &str) -> Result<WorkspaceSpec, String> {
        serde_json::from_value(self.get(&format!("/v1/workspaces/{id}"))?)
            .map_err(|e| format!("layout {id}: {e}"))
    }

    /// The device data of the pod's layouts, as the shared UI builds it:
    /// `local:layouts` (hidden ones left out), `local:all_layouts`,
    /// `local:apps`, and an entity for each.
    pub fn layouts(&self) -> Result<DataBundle, String> {
        let v = self.get(device_data::LAYOUTS_PATH)?;
        let mut titles = self.titles.lock().unwrap_or_else(|p| p.into_inner());
        Ok(layouts_bundle(&v, &mut titles))
    }

    /// The rest of the device data the shared UI gives layouts
    /// (`DeviceData`): connection, session, device, inbox and server; and,
    /// when `needs` (a layout's JSON) binds them, the notifications, their
    /// channels and the sign-ins.
    pub fn device(&self, needs: &str) -> DataBundle {
        let titles = self.titles.lock().map(|t| t.clone()).unwrap_or_default();
        device_data::device(&self.base, needs, &titles, |p| self.get(p))
    }

    /// `local://versions` and `local:versions` for a layout, as the shared UI
    /// builds them (`DeviceData`): its history, newest first.
    pub fn versions(&self, app: &str) -> Result<DataBundle, String> {
        let v = self.get(&device_data::versions_path(app))?;
        let pinned = self.pins.lock().ok().and_then(|p| p.get(app).copied());
        Ok(versions_bundle(
            app,
            &v,
            pinned,
            &self.app_title(app),
            now_secs(),
        ))
    }

    /// An app's title for device data rows (its id when unknown).
    fn app_title(&self, id: &str) -> String {
        self.titles
            .lock()
            .ok()
            .and_then(|t| t.get(id).cloned())
            .unwrap_or_else(|| id.to_owned())
    }

    /// A live session on `layout` (usually `home`): its bundle, the
    /// device's list of layouts, a loader for what taps open, and images.
    pub fn session(&self, layout: &str, viewport: Viewport) -> Result<Session, String> {
        let spec = self.workspace(layout)?;
        let mut data = DataBundle {
            entities: spec.entities,
            queries: spec.queries,
        };
        match self.layouts() {
            Ok(l) => {
                data.entities.extend(l.entities);
                data.queries.extend(l.queries);
            }
            // Home still draws, with an empty list.
            Err(e) => eprintln!("construct-render: layouts: {e}"),
        }
        let root_json = serde_json::to_string(&spec.workspace).unwrap_or_default();
        let device = self.device(&root_json);
        data.entities.extend(device.entities);
        data.queries.extend(device.queries);
        let mut s = Session::new(layout, spec.workspace, data, viewport);
        s.loader = Some(Box::new(self.clone()));
        fetch_images(&s.images);
        Ok(s)
    }
}

impl Loader for PodClient {
    fn device(&self, command: &str, app: &str, id: &str) -> Option<Result<DataBundle, String>> {
        match command {
            "show_versions" => Some(self.versions(app)),
            "pin_version" => {
                let n = id.parse::<i64>().ok()?;
                if let Ok(mut p) = self.pins.lock() {
                    p.insert(app.to_owned(), n);
                }
                Some(self.versions(app))
            }
            "follow_latest" => {
                if let Ok(mut p) = self.pins.lock() {
                    p.remove(app);
                }
                Some(self.versions(app))
            }
            _ => None,
        }
    }

    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let spec = self.workspace(id)?;
        let mut data = DataBundle {
            entities: spec.entities,
            queries: spec.queries,
        };
        // Fresh device data for what this layout binds (the inbox, Settings).
        let device = self.device(&serde_json::to_string(&spec.workspace).unwrap_or_default());
        data.entities.extend(device.entities);
        data.queries.extend(device.queries);
        Ok((spec.workspace, data))
    }

    fn data(&self, path: &str) -> Result<DataBundle, String> {
        serde_json::from_value(self.get(path)?).map_err(|e| format!("{path}: {e}"))
    }

    fn open_session(&self, path: &str, payload: &Value) -> Option<Result<String, String>> {
        let answer = PodClient {
            token: String::new(),
            ..self.clone()
        }
        .post(path, payload)
        .map_err(|e| {
            if e.contains("404") || e.contains("401") {
                "That pod code didn't open a pod.".to_owned()
            } else {
                e
            }
        });
        Some(answer.and_then(|v| {
            v["session_token"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "the pod answered no session".to_owned())
        }))
    }

    fn fetch(&self, path: &str, done: Loaded) -> bool {
        let pod = self.clone();
        let path = path.to_owned();
        std::thread::spawn(move || done(Loader::data(&pod, &path)));
        true
    }

    fn send(&self, write: Write, done: Done) -> bool {
        let pod = self.clone();
        std::thread::spawn(move || {
            let result = pod.post(&write.path, &write.body);
            let mut refreshed = Vec::new();
            if result.is_ok() {
                for id in &write.refresh {
                    match Loader::layout(&pod, id) {
                        Ok((_, d)) => refreshed.push(d),
                        Err(e) => eprintln!("construct-render: reloading {id}: {e}"),
                    }
                }
            }
            done(Written { result, refreshed });
        });
        true
    }
}

/// Largest side images are kept at once decoded.
const MAX_IMAGE_SIDE: u32 = 1600;
/// Downloads at once.
const IMAGE_WORKERS: usize = 4;

/// Makes `store` download what it is asked for, on background threads.
/// Images are fetched without the session (the pod serves media from public
/// paths); one that fails keeps its placeholder.
pub fn fetch_images(store: &ImageStore) {
    // Newest first: what is on screen now goes before what scrolled past.
    let queue: Arc<(Mutex<Vec<String>>, std::sync::Condvar)> = Arc::default();
    for _ in 0..IMAGE_WORKERS {
        let queue = queue.clone();
        let store = store.clone();
        std::thread::spawn(move || {
            let http = client();
            loop {
                let url = {
                    let (lock, ready) = &*queue;
                    let mut q = lock.lock().unwrap_or_else(|p| p.into_inner());
                    loop {
                        if let Some(u) = q.pop() {
                            break u;
                        }
                        q = ready.wait(q).unwrap_or_else(|p| p.into_inner());
                    }
                };
                // A staged photo (`construct://attachments/<id>`) is a local file.
                let bytes = match reqwest::Url::parse(&url) {
                    Ok(u) if u.scheme() == "file" => {
                        u.to_file_path().ok().and_then(|p| std::fs::read(p).ok())
                    }
                    _ => http
                        .get(&url)
                        .send()
                        .and_then(|r| r.error_for_status())
                        .and_then(|r| r.bytes())
                        .ok()
                        .map(|b| b.to_vec()),
                };
                let img = bytes
                    .and_then(|b| image::load_from_memory(&b).ok())
                    .map(|i| {
                        if i.width().max(i.height()) > MAX_IMAGE_SIDE {
                            i.thumbnail(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE)
                        } else {
                            i
                        }
                        .to_rgba8()
                    });
                store.insert(&url, img);
            }
        });
    }
    store.set_fetcher(move |url| {
        let (lock, ready) = &*queue;
        lock.lock().unwrap_or_else(|p| p.into_inner()).push(url);
        ready.notify_one();
    });
}
