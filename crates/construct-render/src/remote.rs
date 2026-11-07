//! A live session over a pod (feature `remote`): home and every layout it
//! opens fetched as a client fetches them (`GET /v1/workspaces/<id>`), the
//! device's list of layouts (`local:layouts`) from `GET /v1/apps`, screen
//! data from data paths, and images downloaded on background threads.
//!
//! Read-only: what a tap would change on the pod is reported, not sent
//! (`crate::interact`). There is no cache, outbox or event stream.

use protocol::{DataBundle, EntityRecord, UiNode, WorkspaceSpec};
use serde_json::{Value, json};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::images::ImageStore;
use crate::interact::{Loader, Session};
use crate::layout::Viewport;

/// A pod and a session on it.
#[derive(Clone)]
pub struct PodClient {
    base: String,
    token: String,
    http: reqwest::blocking::Client,
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
        Ok(PodClient { base, token, http })
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
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .header(protocol::FEATURES_HEADER, protocol::features_header())
            .send()
            .map_err(|e| format!("{path}: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 401 {
            return Err(format!("{path}: signed out (401): sign in again"));
        }
        if !status.is_success() {
            return Err(format!("{path}: HTTP {status}"));
        }
        resp.json().map_err(|e| format!("{path}: {e}"))
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
        let v = self.get("/v1/apps?all=1")?;
        let mut out = DataBundle::default();
        let (mut listed, mut all) = (Vec::new(), Vec::new());
        for a in v["apps"].as_array().into_iter().flatten() {
            let Some(id) = a["id"].as_str().filter(|id| *id != "home") else {
                continue;
            };
            let hidden = a["hidden"].as_bool().unwrap_or(false);
            let mut e = EntityRecord::new();
            e.set("id", id);
            e.set("title", a["title"].as_str().unwrap_or(id));
            e.set("icon", a["icon"].as_str().unwrap_or(""));
            e.set(
                "kind",
                if a["kind"] == "custom" {
                    "custom"
                } else {
                    "blueprint"
                },
            );
            e.set(
                "detail",
                a["latest_version"]
                    .as_i64()
                    .map(|v| format!("v{v}"))
                    .unwrap_or_default(),
            );
            e.set("unread", 0);
            e.set("hidden", hidden);
            for prefix in ["local://layouts/", "local://apps/"] {
                out.entities.insert(format!("{prefix}{id}"), e.clone());
            }
            all.push(format!("local://layouts/{id}"));
            if !hidden {
                listed.push(id.to_owned());
            }
        }
        let mut count = EntityRecord::new();
        count.set("count", listed.len());
        out.entities.insert("local://layouts".into(), count.clone());
        out.entities.insert("local://apps".into(), count);
        out.queries.insert(
            "local:layouts".into(),
            listed
                .iter()
                .map(|i| format!("local://layouts/{i}"))
                .collect(),
        );
        out.queries.insert(
            "local:apps".into(),
            listed.iter().map(|i| format!("local://apps/{i}")).collect(),
        );
        out.queries.insert("local:all_layouts".into(), all);
        Ok(out)
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
        let mut s = Session::new(layout, spec.workspace, data, viewport);
        s.loader = Some(Box::new(self.clone()));
        fetch_images(&s.images);
        Ok(s)
    }
}

impl Loader for PodClient {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let spec = self.workspace(id)?;
        Ok((
            spec.workspace,
            DataBundle {
                entities: spec.entities,
                queries: spec.queries,
            },
        ))
    }

    fn data(&self, path: &str) -> Result<DataBundle, String> {
        serde_json::from_value(self.get(path)?).map_err(|e| format!("{path}: {e}"))
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
    let (tx, rx) = mpsc::channel::<String>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..IMAGE_WORKERS {
        let rx = rx.clone();
        let store = store.clone();
        std::thread::spawn(move || {
            let http = client();
            loop {
                let url = match rx.lock().map(|r| r.recv()) {
                    Ok(Ok(u)) => u,
                    _ => return,
                };
                let img = http
                    .get(&url)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .and_then(|r| r.bytes())
                    .ok()
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
    let tx = Mutex::new(tx);
    store.set_fetcher(move |url| {
        if let Ok(t) = tx.lock() {
            let _ = t.send(url);
        }
    });
}
