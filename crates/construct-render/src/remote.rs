//! A live session over a pod (feature `remote`): home and every layout it
//! opens fetched as a client fetches them (`GET /v1/workspaces/<id>`), the
//! device's list of layouts (`local:layouts`) from `GET /v1/apps`, screen
//! data from data paths, and images downloaded on background threads.
//!
//! Writes, device commands (notification switches, hiding and deleting
//! layouts, older notifications) and pinned versions go to the pod as the
//! shared UI sends them (`crate::interact`). There is no cache, outbox or
//! event stream.

use protocol::{DataBundle, EntityRecord, UiNode, WorkspaceSpec};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    titles: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Versions picked on this device (`pin_version`), by layout: the
    /// layout is drawn at that version until `follow_latest`.
    pins: Arc<Mutex<Pins>>,
    /// Called with the pins after each change, to keep them.
    on_pins: Option<KeepPins>,
    /// The servers the platform offers (`local:servers`): id, title, URL.
    servers: Vec<(String, String, String)>,
    /// This device's id on the pod (`/v1/devices/<id>`), for its
    /// notification channels.
    device_id: String,
}

/// Pinned versions by layout.
pub type Pins = std::collections::BTreeMap<String, i64>;

/// What keeps the pins when they change.
type KeepPins = Arc<dyn Fn(&Pins) + Send + Sync>;

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
            on_pins: None,
            servers: Vec::new(),
            device_id: String::new(),
        })
    }

    /// The versions pinned on this device, and what keeps them when they
    /// change.
    pub fn with_pins(mut self, pins: Pins, keep: impl Fn(&Pins) + Send + Sync + 'static) -> Self {
        self.pins = Arc::new(Mutex::new(pins));
        self.on_pins = Some(Arc::new(keep));
        self
    }

    /// The servers the platform offers, for `local:servers` (id, title,
    /// URL).
    pub fn with_servers(mut self, servers: Vec<(String, String, String)>) -> Self {
        self.servers = servers;
        self
    }

    /// This device's id (`[A-Za-z0-9_-]{8,64}`), registered on the pod
    /// when a notification channel is first switched.
    pub fn with_device_id(mut self, id: &str) -> Self {
        self.device_id = id.to_owned();
        self
    }

    fn set_pin(&self, app: &str, version: Option<i64>) {
        let Ok(mut p) = self.pins.lock() else {
            return;
        };
        match version {
            Some(n) => p.insert(app.to_owned(), n),
            None => p.remove(app),
        };
        if let Some(keep) = &self.on_pins {
            keep(&p);
        }
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
        self.call(reqwest::Method::POST, path, Some(body))
    }

    /// A request to a pod path with a JSON body; the answer's JSON (`null`
    /// when empty).
    pub fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        let mut req = self
            .http
            .request(method, &url)
            .header(protocol::FEATURES_HEADER, protocol::features_header());
        if let Some(b) = body {
            req = req.json(b);
        }
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
        let v = self.get("/v1/apps?all=1")?;
        let mut out = DataBundle::default();
        let (mut listed, mut all) = (Vec::new(), Vec::new());
        for a in v["apps"].as_array().into_iter().flatten() {
            let Some(id) = a["id"].as_str() else {
                continue;
            };
            if let Ok(mut t) = self.titles.lock() {
                t.insert(id.to_owned(), a["title"].as_str().unwrap_or(id).to_owned());
            }
            if id == "home" {
                continue;
            }
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
            let latest = a["latest_version"].as_i64();
            e.set(
                "detail",
                latest.map(|v| format!("v{v}")).unwrap_or_default(),
            );
            e.set(
                "version",
                latest.map(|v| format!("v{v} · latest")).unwrap_or_default(),
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

    /// The rest of the device data the shared UI gives layouts
    /// (`DeviceData`): connection, session, device, inbox and server; and,
    /// when `needs` (a layout's JSON) binds them, the notifications, their
    /// channels and the sign-ins.
    pub fn device(&self, needs: &str) -> DataBundle {
        let mut out = DataBundle::default();
        let mut set = |uri: &str, v: Value| {
            if let Ok(e) = serde_json::from_value(v) {
                out.entities.insert(uri.to_owned(), e);
            }
        };
        set(
            "local://connection",
            json!({ "online": true, "pending": 0, "label": "Online", "attention": false, "color": "#2E7D32" }),
        );
        set(
            "local://session",
            json!({ "open": true, "server": self.base, "error": "", "message": "", "status": "" }),
        );
        set(
            "local://device",
            json!({ "platform": if cfg!(target_os = "android") { "android" } else { "desktop" },
                    "notification_permission": "" }),
        );
        let mut servers = Vec::new();
        for (id, title, url) in &self.servers {
            let uri = format!("local://servers/{id}");
            set(
                &uri,
                json!({ "id": id, "title": title, "url": url,
                        "selected": url.trim_end_matches('/') == self.base }),
            );
            servers.push(uri);
        }
        if servers.is_empty() {
            set(
                "local://servers/current",
                json!({ "id": "current", "title": self.base, "url": self.base, "selected": true }),
            );
            servers.push("local://servers/current".into());
        }
        out.queries.insert("local:servers".into(), servers);
        let mut unread_total = 0;
        let mut next_before = None;
        if (needs.contains("local:notifications") || needs.contains("local://inbox"))
            && let Ok(page) = self.notifications(None)
        {
            unread_total = page.1;
            next_before = page.2;
            out.entities.extend(page.0.entities);
            out.queries.extend(page.0.queries);
        }
        if let Ok(e) = serde_json::from_value(json!({
            "unread": unread_total,
            "label": if unread_total == 0 { "All read".to_owned() } else { format!("{unread_total} unread") },
            "has_older": next_before.is_some(), "next_before": next_before.unwrap_or_default(),
            "loading": false }))
        {
            out.entities.insert("local://inbox".into(), e);
        }
        if needs.contains("local:channels")
            && let Ok(v) = self.get("/v1/notifications/channels")
        {
            let mut list = Vec::new();
            for c in v["channels"].as_array().into_iter().flatten() {
                let (app, id) = (
                    c["app_id"].as_str().unwrap_or_default(),
                    c["id"].as_str().unwrap_or_default(),
                );
                let uri = format!("local://channels/{app}/{id}");
                let mut e = c.clone();
                e["app_title"] = json!(self.app_title(app));
                if let Ok(rec) = serde_json::from_value(e) {
                    out.entities.insert(uri.clone(), rec);
                }
                list.push(uri);
            }
            out.queries.insert("local:channels".into(), list);
        }
        if (needs.contains("local:sign_ins") || needs.contains("local:providers"))
            && let Ok(v) = self.get("/v1/auth/sign-ins")
        {
            let mut ins = Vec::new();
            let mut signed: Vec<String> = Vec::new();
            for si in v["sign_ins"].as_array().into_iter().flatten() {
                let id = si["id"].as_str().unwrap_or_default();
                let host = si["server"].as_str().map(|s| {
                    s.split("://")
                        .last()
                        .unwrap_or(s)
                        .trim_end_matches('/')
                        .to_owned()
                });
                let detail: Vec<String> = si["username"]
                    .as_str()
                    .map(|u| format!("@{u}"))
                    .into_iter()
                    .chain(host)
                    .collect();
                let uri = format!("local://sign_ins/{id}");
                if let Ok(rec) = serde_json::from_value(json!({ "id": id,
                    "title": si["title"], "detail": detail.join(" · ") }))
                {
                    out.entities.insert(uri.clone(), rec);
                }
                ins.push(uri);
                signed.push(id.to_owned());
            }
            let mut providers = Vec::new();
            for p in v["providers"].as_array().into_iter().flatten() {
                let id = p["id"].as_str().unwrap_or_default();
                if p["signed_in"].as_bool().unwrap_or(false)
                    || p["intent"].is_null()
                    || signed.iter().any(|s| s == id)
                {
                    continue;
                }
                let uri = format!("local://providers/{id}");
                if let Ok(rec) = serde_json::from_value(json!({ "id": id, "title": p["title"],
                    "custom_server": !p["server_placeholder"].is_null(),
                    "server_placeholder": p["server_placeholder"].as_str().unwrap_or("") }))
                {
                    out.entities.insert(uri.clone(), rec);
                }
                providers.push(uri);
            }
            out.queries.insert("local:sign_ins".into(), ins);
            out.queries.insert("local:providers".into(), providers);
        }
        out
    }

    /// A page of notifications, newest first (before `before`, a cursor
    /// the previous page gave): `local:notifications` and an entity for
    /// each, the unread total and where the next page starts.
    pub fn notifications(
        &self,
        before: Option<&str>,
    ) -> Result<(DataBundle, i64, Option<String>), String> {
        let mut path = "/v1/notifications?limit=50".to_owned();
        if let Some(b) = before {
            path.push_str(&format!("&before={}", url_segment(b)));
        }
        let v = self.get(&path)?;
        let now = now_secs();
        let mut out = DataBundle::default();
        let mut list = Vec::new();
        for n in v["notifications"].as_array().into_iter().flatten() {
            let id = n["id"].as_str().unwrap_or_default();
            let at = n["created_at"].as_i64().unwrap_or(now);
            let uri = format!("local://notifications/{id}");
            let app = n["app_id"].as_str().unwrap_or_default();
            let mut e = n.clone();
            e["app_title"] = json!(self.app_title(app));
            e["when"] = json!(time_label(at, now));
            e["day"] = json!(day_label(at, now));
            e["unread"] = json!(!n["read"].as_bool().unwrap_or(true));
            if let Ok(rec) = serde_json::from_value(e) {
                out.entities.insert(uri.clone(), rec);
            }
            list.push(uri);
        }
        out.queries.insert("local:notifications".into(), list);
        let next = match &v["next_before"] {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        Ok((out, v["unread_total"].as_i64().unwrap_or(0), next))
    }

    /// Switches one notification channel on or off for this device,
    /// registering the device first when the pod doesn't know it.
    fn set_channel(&self, app: &str, id: &str, enabled: bool) -> Result<(), String> {
        if self.device_id.is_empty() {
            return Err("this device has no id to keep notification choices under".into());
        }
        let path = format!("/v1/devices/{}/channels", self.device_id);
        let body = json!({ protocol::channel_key(app, id): enabled });
        match self.call(reqwest::Method::PUT, &path, Some(&body)) {
            Err(e) if e.contains("device_not_found") || e.contains("404") => {
                let platform = if cfg!(target_os = "android") {
                    "android"
                } else {
                    "desktop"
                };
                self.call(
                    reqwest::Method::PUT,
                    &format!("/v1/devices/{}", self.device_id),
                    Some(&json!({ "platform": platform, "name": "The Construct (Rust)" })),
                )?;
                self.call(reqwest::Method::PUT, &path, Some(&body))
                    .map(|_| ())
            }
            r => r.map(|_| ()),
        }
    }

    /// A layout as this device draws it: its pinned version's layout in
    /// place of the latest, when one is pinned.
    fn pinned(&self, id: &str, latest: UiNode) -> UiNode {
        let Some(n) = self.pins.lock().ok().and_then(|p| p.get(id).copied()) else {
            return latest;
        };
        let v = self.get(&format!("/v1/apps/{}/versions/{n}", url_segment(id)));
        match v.and_then(|v| serde_json::from_value(v["layout"].clone()).map_err(|e| e.to_string()))
        {
            Ok(root) => root,
            Err(e) => {
                eprintln!("construct-render: v{n} of {id}: {e}; drawing the latest");
                latest
            }
        }
    }

    /// `local://versions` and `local:versions` for a layout, as the shared UI
    /// builds them (`DeviceData`): its history, newest first.
    pub fn versions(&self, app: &str) -> Result<DataBundle, String> {
        let v = self.get(&format!("/v1/apps/{app}/versions?limit=100"))?;
        let pinned = self.pins.lock().ok().and_then(|p| p.get(app).copied());
        let list = v["versions"].as_array().cloned().unwrap_or_default();
        let newest = list
            .iter()
            .find(|x| x["compatible"].as_bool().unwrap_or(true))
            .and_then(|x| x["version"].as_i64());
        let now = now_secs();
        let mut out = DataBundle::default();
        let mut uris = Vec::new();
        for x in &list {
            let n = x["version"].as_i64().unwrap_or(0);
            let detail = match (x["incompatible"].as_str(), x["note"].as_str()) {
                (Some(i), note) => {
                    let why = match i {
                        "update_required" => "Needs a newer version of the app",
                        "retired" => "Made for an older version of the app",
                        _ => "This version of the app can't show it",
                    };
                    note.map_or(why.to_owned(), |n| format!("{why} · {n}"))
                }
                (None, Some(note)) => note.to_owned(),
                (None, None) => match x["source"].as_str().unwrap_or("") {
                    "file" => "Published",
                    "seed" => "Built-in",
                    "agent" => "AI edit",
                    "rollback" => "Rollback",
                    "restore" => "Restore",
                    "manual" => "Manual edit",
                    "switch" => "Layout switch",
                    _ => "Edit",
                }
                .to_owned(),
            };
            let uri = format!("local://versions/{app}/{n}");
            if let Ok(e) = serde_json::from_value(json!({
                "app_id": app, "version": n, "label": format!("v{n}"), "detail": detail,
                "when": version_time_label(
                    x["created_at"].as_i64().or_else(|| x["created_at"].as_str().and_then(parse_rfc3339)).unwrap_or(0),
                    now,
                ),
                "compatible": x["compatible"].as_bool().unwrap_or(true),
                "showing": match pinned { Some(p) => p == n, None => Some(n) == newest },
            })) {
                out.entities.insert(uri.clone(), e);
            }
            uris.push(uri);
        }
        out.queries.insert("local:versions".into(), uris);
        if let Ok(e) = serde_json::from_value(json!({
            "app_id": app, "app_title": self.app_title(app),
            "pinned": pinned.map(|p| p.to_string()).unwrap_or_default(), "loading": false,
        })) {
            out.entities.insert("local://versions".into(), e);
        }
        Ok(out)
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
        let root = self.pinned(layout, spec.workspace);
        let root_json = serde_json::to_string(&root).unwrap_or_default();
        let device = self.device(&root_json);
        data.entities.extend(device.entities);
        data.queries.extend(device.queries);
        let mut s = Session::new(layout, root, data, viewport);
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
                let n = id.trim().parse::<i64>().ok()?;
                self.set_pin(app, Some(n));
                Some(self.versions(app))
            }
            "follow_latest" => {
                self.set_pin(app, None);
                Some(self.versions(app))
            }
            _ => None,
        }
    }

    fn device_command(&self, command: &str, args: &Value) -> Option<Result<DataBundle, String>> {
        let text = |k: &str| args[k].as_str().unwrap_or("").trim().to_owned();
        let app = text("app");
        Some(match command {
            "toggle_channel" => self
                .set_channel(&app, &text("id"), args["enabled"].as_bool().unwrap_or(true))
                .map(|()| DataBundle::default()),
            "load_older_notifications" => {
                self.notifications(Some(&text("before")))
                    .map(|(mut page, _, next)| {
                        if let Ok(e) = serde_json::from_value(json!({
                            "has_older": next.is_some(), "next_before": next.unwrap_or_default() }))
                        {
                            page.entities.insert("local://inbox".into(), e);
                        }
                        page
                    })
            }
            "hide_layout" | "unhide_layout" => self
                .call(
                    reqwest::Method::PUT,
                    &format!("/v1/apps/{}/hidden", url_segment(&app)),
                    Some(&json!({ "hidden": command == "hide_layout" })),
                )
                .and_then(|_| self.layouts()),
            "delete_layout" => {
                let deleted = self.call(
                    reqwest::Method::DELETE,
                    &format!("/v1/custom-apps/{}", url_segment(&app)),
                    None,
                );
                deleted.and_then(|_| {
                    self.set_pin(&app, None);
                    self.layouts()
                })
            }
            _ => return None,
        })
    }

    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let spec = self.workspace(id)?;
        let mut data = DataBundle {
            entities: spec.entities,
            queries: spec.queries,
        };
        let root = self.pinned(id, spec.workspace);
        // Fresh device data for what this layout binds (the inbox, Settings).
        let device = self.device(&serde_json::to_string(&root).unwrap_or_default());
        data.entities.extend(device.entities);
        data.queries.extend(device.queries);
        Ok((root, data))
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

/// A path segment with what a URL can't carry as is escaped.
fn url_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// (year, month 1-12, day) of days since 1970-01-01 (civil, proleptic).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = [
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
];

/// A notification's time, as the shared UI's `notificationTimeLabel`
/// ("now", "5 min", "3 h", "Fri", "Nov 5"). Days are UTC days here.
pub fn time_label(at: i64, now: i64) -> String {
    let d = (now - at).max(0);
    match d {
        0..60 => "now".into(),
        60..3_600 => format!("{} min", d / 60),
        3_600..86_400 => format!("{} h", d / 3_600),
        86_400..604_800 => WEEKDAYS[at.div_euclid(86_400).rem_euclid(7) as usize][..3].to_owned(),
        _ => {
            let (_, m, day) = civil(at.div_euclid(86_400));
            format!("{} {day}", MONTHS[m as usize - 1])
        }
    }
}

/// Seconds since 1970 of an RFC 3339 UTC time (`2026-11-06T10:54:30Z`, or
/// with an offset).
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from civil (the inverse of `civil`).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut t = days * 86_400 + h * 3_600 + mi * 60 + sec;
    // An offset after the seconds (and any fraction): +hh:mm or -hh:mm.
    let rest = s.get(19..).unwrap_or("");
    if let Some(i) = rest.find(['+', '-']) {
        let sign = if rest.as_bytes()[i] == b'-' { -1 } else { 1 };
        let off = &rest[i + 1..];
        let oh = off
            .get(0..2)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let om = off
            .get(3..5)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        t -= sign * (oh * 3_600 + om * 60);
    }
    Some(t)
}

/// A version's time, as the shared UI's `versionTimeLabel`: "just now",
/// "5 min ago", "3 h ago", "yesterday", "4 days ago", else the date.
pub fn version_time_label(at: i64, now: i64) -> String {
    if at <= 0 {
        return String::new();
    }
    let d = (now - at).max(0);
    match d {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", d / 60),
        3_600..86_400 => format!("{} h ago", d / 3_600),
        86_400..172_800 => "yesterday".into(),
        172_800..2_592_000 => format!("{} days ago", d / 86_400),
        _ => {
            let (y, m, day) = civil(at.div_euclid(86_400));
            format!("{} {day}, {y}", MONTHS[m as usize - 1])
        }
    }
}

/// The inbox's day header ("Today", "Yesterday", "Thursday, Nov 5").
pub fn day_label(at: i64, now: i64) -> String {
    let (a, n) = (at.div_euclid(86_400), now.div_euclid(86_400));
    match n - a {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let (_, m, d) = civil(a);
            format!(
                "{}, {} {d}",
                WEEKDAYS[a.rem_euclid(7) as usize],
                MONTHS[m as usize - 1]
            )
        }
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
    store.set_fetcher(move |url| {
        let (lock, ready) = &*queue;
        lock.lock().unwrap_or_else(|p| p.into_inner()).push(url);
        ready.notify_one();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_the_shared_ui() {
        // 2026-11-05 (a Thursday) 12:00 UTC.
        let thu = 1_793_880_000;
        assert_eq!(civil(thu / 86_400), (2026, 11, 5));
        assert_eq!(time_label(thu, thu + 30), "now");
        assert_eq!(time_label(thu, thu + 300), "5 min");
        assert_eq!(time_label(thu, thu + 7_200), "2 h");
        assert_eq!(time_label(thu, thu + 2 * 86_400), "Thu");
        assert_eq!(time_label(thu, thu + 30 * 86_400), "Nov 5");
        assert_eq!(day_label(thu, thu + 60), "Today");
        assert_eq!(day_label(thu, thu + 86_400), "Yesterday");
        assert_eq!(day_label(thu, thu + 3 * 86_400), "Thursday, Nov 5");
        assert_eq!(parse_rfc3339("2026-11-05T12:00:00Z"), Some(thu));
        assert_eq!(parse_rfc3339("2026-11-05T05:00:00-07:00"), Some(thu));
        assert_eq!(version_time_label(thu, thu + 7_200), "2 h ago");
    }
}
