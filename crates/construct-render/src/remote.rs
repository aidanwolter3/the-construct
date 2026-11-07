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
    /// Layout titles by id (from `/v1/apps`), for device data rows.
    titles: Arc<Mutex<std::collections::HashMap<String, String>>>,
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
        })
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
            if let Ok(mut t) = self.titles.lock() {
                t.insert(id.to_owned(), a["title"].as_str().unwrap_or(id).to_owned());
            }
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
        set(
            "local://servers/current",
            json!({ "id": "current", "title": self.base, "url": self.base, "selected": true }),
        );
        out.queries.insert(
            "local:servers".into(),
            vec!["local://servers/current".into()],
        );
        let now = now_secs();
        let mut unread_total = 0;
        if (needs.contains("local:notifications") || needs.contains("local://inbox"))
            && let Ok(v) = self.get("/v1/notifications?limit=50")
        {
            unread_total = v["unread_total"].as_i64().unwrap_or(0);
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
        }
        if let Ok(e) = serde_json::from_value(json!({
            "unread": unread_total,
            "label": if unread_total == 0 { "All read".to_owned() } else { format!("{unread_total} unread") },
            "has_older": false, "loading": false }))
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
    }
}
