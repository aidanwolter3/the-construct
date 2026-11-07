//! Fixture mode: a fake pod serving bundles from `data/` (no server, no
//! network), so shots are the same on every run.
//!
//! - Layouts: `blueprint_<id>_page.json` (the shell's Settings and inbox, as
//!   a pod serves them), else the layout's default bundle ([`default_bundle`]).
//! - Data paths (a screen's `data`, `action.data.load`): from
//!   `pixelfed_screen_data.json` and `events_screen_data.json`.
//! - Writes are accepted and change nothing on the pod: the device keeps its
//!   optimistic change. Offline, loads and writes fail as an unreachable pod
//!   does (the Rust clients have no outbox), so a write is undone.
//! - Notifications and channels: `notifications.json` (created `age_seconds`
//!   before the run starts); `notify` adds one.
//! - Images: a URL ending in `/storage/<file>` or `/v1/pixelfed/media/<file>`
//!   is the fake Pixelfed's `fake-pixelfed/fixtures/media/<file>`; any other
//!   URL fails at once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use construct_render::images::ImageStore;
use construct_render::interact::{Done, Loaded, Loader, Write, Written};
use construct_render::remote::{day_label, time_label};
use protocol::{DataBundle, UiNode, WorkspaceSpec};
use serde_json::{Value, json};

/// The data directory, relative to the repository root.
pub const DATA_DIR: &str = "crates/construct-sim/data";
/// The fake Pixelfed's images, relative to the repository root.
pub const MEDIA_DIR: &str = "fake-pixelfed/fixtures/media";
/// What an unreachable pod answers.
pub const OFFLINE: &str = "Can't reach your pod. Check your connection and try again.";
/// The custom "Kitchen" layout over the sample posts.
pub const KITCHEN: &str = "c-kitchen2dm";

/// A workspace's own bundle in `data/`, if it has one.
pub fn default_bundle(workspace: &str) -> Option<&'static str> {
    Some(match workspace {
        "pixelfed_sample_no_login" => "pixelfed_sample_bundle.json",
        "pixelfed" => "pixelfed_feed_bundle.json",
        "events" => "events_bundle.json",
        KITCHEN => "custom_kitchen_bundle.json",
        _ => return None,
    })
}

/// The repository root at or above `from` (where `data/` is found).
pub fn repo_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|d| d.join(DATA_DIR).is_dir())
        .map(Path::to_path_buf)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A bundle file (`{workspace, entities, queries}`).
pub fn read_bundle(path: &Path) -> Result<(UiNode, DataBundle), String> {
    let spec: WorkspaceSpec = serde_json::from_value(read_json(path)?)
        .map_err(|e| format!("{}: not a workspace bundle: {e}", path.display()))?;
    Ok((
        spec.workspace,
        DataBundle {
            entities: spec.entities,
            queries: spec.queries,
        },
    ))
}

struct State {
    data: PathBuf,
    media: PathBuf,
    online: bool,
    screen_data: BTreeMap<String, DataBundle>,
    /// Wire shape, `created_at` in seconds.
    notifications: Vec<Value>,
    channels: Vec<Value>,
    next_id: u32,
}

/// The fake pod. Cheap to clone (shared state).
#[derive(Clone)]
pub struct FixturePod {
    state: Arc<Mutex<State>>,
}

impl FixturePod {
    /// The fake pod over the repository at `root`.
    pub fn new(root: &Path) -> Result<FixturePod, String> {
        let data = root.join(DATA_DIR);
        let mut screen_data = BTreeMap::new();
        for name in ["pixelfed_screen_data.json", "events_screen_data.json"] {
            let v = read_json(&data.join(name))?;
            let paths: BTreeMap<String, DataBundle> =
                serde_json::from_value(v).map_err(|e| format!("{name}: {e}"))?;
            screen_data.extend(paths);
        }
        let n = read_json(&data.join("notifications.json"))?;
        let now = now_secs();
        let notifications = n["notifications"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| {
                let mut x = x.clone();
                let age = x["age_seconds"].as_i64().unwrap_or(0);
                if let Some(o) = x.as_object_mut() {
                    o.remove("age_seconds");
                }
                x["created_at"] = json!(now - age);
                x
            })
            .collect();
        let channels = n["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let mut c = c.clone();
                if c.get("enabled").is_none() {
                    c["enabled"] = c["default_enabled"].clone();
                }
                c
            })
            .collect();
        Ok(FixturePod {
            state: Arc::new(Mutex::new(State {
                data,
                media: root.join(MEDIA_DIR),
                online: true,
                screen_data,
                notifications,
                channels,
                next_id: 1,
            })),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn online(&self) -> bool {
        self.lock().online
    }

    pub fn set_online(&self, online: bool) {
        self.lock().online = online;
    }

    /// A live notification arrives (`notify`).
    pub fn notify(&self, app: &str, channel: &str, title: &str) {
        let mut s = self.lock();
        let id = format!("sim-{}", s.next_id);
        s.next_id += 1;
        s.notifications.insert(
            0,
            json!({ "id": id, "app_id": app, "channel": channel, "title": title, "body": "",
                    "created_at": now_secs(), "read": false }),
        );
    }

    /// The layout `id`'s plumbing root and data, with no device data.
    fn bundle(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let dir = self.lock().data.clone();
        let blueprint = dir.join(format!("blueprint_{id}_page.json"));
        if blueprint.is_file() {
            let root: UiNode = serde_json::from_value(read_json(&blueprint)?)
                .map_err(|e| format!("{}: {e}", blueprint.display()))?;
            return Ok((root, DataBundle::default()));
        }
        match default_bundle(id) {
            Some(name) => read_bundle(&dir.join(name)),
            None => Err(format!("the fixture pod has no layout '{id}'")),
        }
    }

    /// The device data the shared UI gives layouts (`DeviceData`), as
    /// `PodClient::device` builds it from a pod.
    pub fn device(&self) -> DataBundle {
        let s = self.lock();
        let mut out = DataBundle::default();
        let set = |out: &mut DataBundle, uri: &str, v: Value| {
            if let Ok(e) = serde_json::from_value(v) {
                out.entities.insert(uri.to_owned(), e);
            }
        };
        let connection = if s.online {
            json!({ "online": true, "pending": 0, "label": "Online", "attention": false, "color": "#2E7D32" })
        } else {
            json!({ "online": false, "pending": 0, "label": "Offline", "attention": true, "color": "#9E9E9E" })
        };
        set(&mut out, "local://connection", connection);
        set(
            &mut out,
            "local://session",
            json!({ "open": true, "server": "fixture", "error": "", "message": "", "status": "" }),
        );
        set(
            &mut out,
            "local://device",
            json!({ "platform": "desktop", "notification_permission": "" }),
        );
        set(
            &mut out,
            "local://servers/current",
            json!({ "id": "current", "title": "Fixture pod", "url": "fixture", "selected": true }),
        );
        out.queries.insert(
            "local:servers".into(),
            vec!["local://servers/current".into()],
        );
        let now = now_secs();
        let mut list = Vec::new();
        let mut unread = 0;
        for n in &s.notifications {
            let id = n["id"].as_str().unwrap_or_default();
            let at = n["created_at"].as_i64().unwrap_or(now);
            let read = n["read"].as_bool().unwrap_or(true);
            unread += i64::from(!read);
            let mut e = n.clone();
            e["app_title"] = json!(app_title(n["app_id"].as_str().unwrap_or_default()));
            e["when"] = json!(time_label(at, now));
            e["day"] = json!(day_label(at, now));
            e["unread"] = json!(!read);
            let uri = format!("local://notifications/{id}");
            set(&mut out, &uri, e);
            list.push(uri);
        }
        out.queries.insert("local:notifications".into(), list);
        set(
            &mut out,
            "local://inbox",
            json!({ "unread": unread,
                    "label": if unread == 0 { "All read".to_owned() } else { format!("{unread} unread") },
                    "has_older": false, "loading": false }),
        );
        let mut list = Vec::new();
        for c in &s.channels {
            let (app, id) = (
                c["app_id"].as_str().unwrap_or_default(),
                c["id"].as_str().unwrap_or_default(),
            );
            let mut e = c.clone();
            e["app_title"] = json!(app_title(app));
            let uri = format!("local://channels/{app}/{id}");
            set(&mut out, &uri, e);
            list.push(uri);
        }
        out.queries.insert("local:channels".into(), list);
        out.queries.insert("local:sign_ins".into(), Vec::new());
        out.queries.insert("local:providers".into(), Vec::new());
        out
    }

    /// Makes `store` load images from the fake Pixelfed's media folder.
    pub fn serve_images(&self, store: &ImageStore) {
        let media = self.lock().media.clone();
        let into = store.clone();
        store.set_fetcher(move |url| {
            let img = media_file(&url)
                .map(|f| media.join(f))
                .and_then(|p| image::open(p).ok())
                .map(|i| i.to_rgba8());
            into.insert(&url, img);
        });
    }
}

/// The media file an image URL names, if the fake Pixelfed serves it.
pub fn media_file(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let file = ["/storage/", "/v1/pixelfed/media/"]
        .iter()
        .find_map(|p| path.rfind(p).map(|i| &path[i + p.len()..]))?;
    (!file.is_empty() && !file.contains('/') && !file.contains("..")).then_some(file)
}

fn app_title(id: &str) -> String {
    match id {
        "pixelfed" => "Pixelfed".into(),
        "pixelfed_sample_no_login" => "Pixelfed sample".into(),
        "events" => "Events".into(),
        KITCHEN => "Kitchen".into(),
        other => other.into(),
    }
}

impl Loader for FixturePod {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        if !self.online() {
            return Err(OFFLINE.into());
        }
        let (root, mut data) = self.bundle(id)?;
        let device = self.device();
        data.entities.extend(device.entities);
        data.queries.extend(device.queries);
        Ok((root, data))
    }

    fn data(&self, path: &str) -> Result<DataBundle, String> {
        let s = self.lock();
        if !s.online {
            return Err(OFFLINE.into());
        }
        s.screen_data
            .get(path)
            .cloned()
            .ok_or_else(|| format!("the fixture pod has no data at {path}"))
    }

    fn device(&self, command: &str, app: &str, id: &str) -> Option<Result<DataBundle, String>> {
        match command {
            "toggle_channel" => {
                {
                    let mut s = self.lock();
                    if !s.online {
                        return Some(Err(OFFLINE.into()));
                    }
                    let c = s
                        .channels
                        .iter_mut()
                        .find(|c| c["app_id"] == app && c["id"] == id);
                    match c {
                        Some(c) => c["enabled"] = json!(!c["enabled"].as_bool().unwrap_or(false)),
                        None => return Some(Err(format!("no channel {app}.{id}"))),
                    }
                }
                Some(Ok(self.device()))
            }
            _ => None,
        }
    }

    fn send(&self, write: Write, done: Done) -> bool {
        let result = if self.online() {
            Ok(json!({}))
        } else {
            Err(OFFLINE.to_owned())
        };
        let _ = write;
        done(Written {
            result,
            refreshed: Vec::new(),
        });
        true
    }

    fn fetch(&self, path: &str, done: Loaded) -> bool {
        done(Loader::data(self, path));
        true
    }
}
