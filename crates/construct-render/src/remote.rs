//! A live session over a pod (feature `remote`): home and every layout it
//! opens fetched as a client fetches them, from the construct (decision
//! `one-layout-system`: home's root, the objects it reaches, each element
//! materialized here and its data from `/v1/construct/data/<element>`),
//! screen data from data paths, and images downloaded on background threads.
//!
//! Read-only: what a tap would change on the pod is reported, not sent
//! (`crate::interact`). There is no cache, outbox or event stream.

use construct_elements::{Objects, keyed, materialize_with};
use protocol::{DataBundle, UiNode, WorkspaceSpec};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::device_data;
pub use crate::device_data::{
    base_device, channels_bundle, day_label, inbox_entity, needs_channels, needs_notifications,
    needs_sign_ins, notifications_bundle, now_secs, sign_ins_bundle, time_label,
};
use crate::images::ImageStore;
use crate::interact::{Done, Loaded, Loader, Session, Write, Written};
use crate::layout::Viewport;

/// The key home has in the construct.
const HOME: &str = "home";
/// Rounds of `POST /v1/objects` one layout may take.
const MAX_OBJECT_ROUNDS: usize = 16;

/// A pod and a session on it.
#[derive(Clone)]
pub struct PodClient {
    base: String,
    token: String,
    http: reqwest::blocking::Client,
    /// Layout titles by key (from their elements), for device data rows.
    titles: Arc<Mutex<HashMap<String, String>>>,
    /// The construct's objects fetched so far, by id (immutable).
    objects: Arc<Mutex<HashMap<String, Arc<Value>>>>,
}

/// The objects a client has, for materializing.
struct Have(HashMap<String, Arc<Value>>);

impl Objects for Have {
    fn object(&self, id: &str) -> Option<Arc<Value>> {
        self.0.get(id).cloned()
    }
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
            objects: Arc::default(),
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
        // No session: the pod answers what it shows a caller without one. A
        // layout may name an absolute URL: the token goes only to the pod's
        // own origin.
        if !self.token.is_empty() && same_origin(&self.base, &url) {
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

    /// The objects fetched so far.
    fn have(&self) -> Have {
        Have(
            self.objects
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        )
    }

    /// Fetches what `want` reaches that this client lacks (`POST
    /// /v1/objects {have, want}`), until nothing is missing for `check`
    /// (elements to materialize).
    fn fetch_objects(&self, mut want: Vec<String>, check: &[String]) -> Result<(), String> {
        for _ in 0..MAX_OBJECT_ROUNDS {
            let have: Vec<String> = self
                .objects
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .keys()
                .cloned()
                .collect();
            let v = self.post("/v1/objects", &json!({ "have": have, "want": want }))?;
            {
                let mut objects = self.objects.lock().unwrap_or_else(|p| p.into_inner());
                for (id, o) in v["objects"].as_object().into_iter().flatten() {
                    // An object is its hash: one that doesn't match is not taken.
                    if construct_elements::object_id(o) == *id {
                        objects.insert(id.clone(), Arc::new(o.clone()));
                    }
                }
            }
            let have = self.have();
            let mut missing: Vec<String> = Vec::new();
            for el in check {
                if let Ok(m) = materialize_with(&have, el, &Map::new()) {
                    missing.extend(m.missing);
                }
            }
            if missing.is_empty() && v["more"] != json!(true) {
                return Ok(());
            }
            if !missing.is_empty() {
                want = missing;
            }
        }
        Err("the pod's objects didn't all come".into())
    }

    /// Home's root (`GET /v1/construct`), with what it reaches fetched.
    fn root(&self) -> Result<String, String> {
        let v = self.get("/v1/construct")?;
        let root = v["root"]
            .as_str()
            .filter(|r| construct_elements::is_object_id(r))
            .ok_or("/v1/construct: no root")?
            .to_owned();
        self.fetch_objects(vec![root.clone()], std::slice::from_ref(&root))?;
        Ok(root)
    }

    /// Layout `id`'s element and the inputs its use site binds: by key
    /// from home's root, else by name (`/v1/construct/names/<id>`).
    fn element(&self, id: &str) -> Result<(String, Map<String, Value>), String> {
        let root = self.root()?;
        let keys = keyed(&self.have(), HOME, &root);
        {
            let have = self.have();
            let mut titles = self.titles.lock().unwrap_or_else(|p| p.into_inner());
            for (k, kd) in &keys {
                if let Some(t) = have
                    .object(&kd.el)
                    .and_then(|v| v["title"].as_str().map(str::to_owned))
                {
                    titles.insert(k.clone(), t);
                }
            }
        }
        if let Some(k) = keys.get(id) {
            return Ok((k.el.clone(), k.inputs.clone()));
        }
        let v = self.get(&format!("/v1/construct/names/{id}"))?;
        let el = v["el"]
            .as_str()
            .filter(|e| construct_elements::is_object_id(e))
            .ok_or_else(|| format!("layout {id}: the pod has no such layout"))?
            .to_owned();
        self.fetch_objects(vec![el.clone()], std::slice::from_ref(&el))?;
        if let Some(t) = self
            .have()
            .object(&el)
            .and_then(|v| v["title"].as_str().map(str::to_owned))
        {
            self.titles
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(id.to_owned(), t);
        }
        Ok((el, Map::new()))
    }

    /// A layout's bundle: its element materialized here, with its data.
    pub fn workspace(&self, id: &str) -> Result<WorkspaceSpec, String> {
        let (el, inputs) = self.element(id)?;
        let m = materialize_with(&self.have(), &el, &inputs)
            .map_err(|e| format!("layout {id}: {e}"))?;
        let workspace: UiNode =
            serde_json::from_value(m.node).map_err(|e| format!("layout {id}: {e}"))?;
        let mut path = format!("/v1/construct/data/{el}");
        if !inputs.is_empty() {
            let mut url = reqwest::Url::parse("http://x/").map_err(|e| e.to_string())?;
            url.query_pairs_mut()
                .append_pair("bind", &Value::Object(inputs).to_string());
            path.push('?');
            path.push_str(url.query().unwrap_or(""));
        }
        let data: DataBundle =
            serde_json::from_value(self.get(&path)?).map_err(|e| format!("layout {id}: {e}"))?;
        Ok(WorkspaceSpec {
            version: "1.0.0".into(),
            workspace,
            entities: data.entities,
            queries: data.queries,
        })
    }

    /// The rest of the device data the shared UI gives layouts
    /// (`DeviceData`): connection, session, device, inbox and server; and,
    /// when `needs` (a layout's JSON) binds them, the notifications, their
    /// channels and the sign-ins.
    pub fn device(&self, needs: &str) -> DataBundle {
        let titles = self.titles.lock().map(|t| t.clone()).unwrap_or_default();
        device_data::device(&self.base, needs, &titles, |p| self.get(p))
    }

    /// A live session on `layout` (usually `home`): its bundle, a loader
    /// for what taps open, and images.
    pub fn session(&self, layout: &str, viewport: Viewport) -> Result<Session, String> {
        let spec = self.workspace(layout)?;
        let mut data = DataBundle {
            entities: spec.entities,
            queries: spec.queries,
        };
        let root_json = serde_json::to_string(&spec.workspace).unwrap_or_default();
        let device = self.device(&root_json);
        data.entities.extend(device.entities);
        data.queries.extend(device.queries);
        let mut s = Session::new(layout, spec.workspace, data, viewport);
        s.loader = Some(Box::new(self.clone()));
        fetch_images(&s.images, None);
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
/// paths); one that fails keeps its placeholder. A `file://` image is read
/// only from `staged`, the engine's staged attachments.
pub fn fetch_images(store: &ImageStore, staged: Option<&std::path::Path>) {
    // Newest first: what is on screen now goes before what scrolled past.
    let queue: Arc<(Mutex<Vec<String>>, std::sync::Condvar)> = Arc::default();
    let staged = staged.map(std::path::Path::to_path_buf);
    for _ in 0..IMAGE_WORKERS {
        let queue = queue.clone();
        let store = store.clone();
        let staged = staged.clone();
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
                    Ok(u) if u.scheme() == "file" => read_staged(&u, staged.as_deref()),
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

/// The bytes of a `file://` image, when the file is inside `staged` (links
/// and `..` resolved). A layout or a pod's data can name any `file://` URL,
/// and must not draw the device's other files.
fn read_staged(url: &reqwest::Url, staged: Option<&std::path::Path>) -> Option<Vec<u8>> {
    let dir = std::fs::canonicalize(staged?).ok()?;
    let path = std::fs::canonicalize(url.to_file_path().ok()?).ok()?;
    if !path.starts_with(&dir) {
        eprintln!("construct-render: not drawing a local file outside the staged photos");
        return None;
    }
    std::fs::read(path).ok()
}

/// Whether `url` is on the pod at `base` (scheme, host and port), so it may
/// carry the pod's token.
fn same_origin(base: &str, url: &str) -> bool {
    match (reqwest::Url::parse(base), reqwest::Url::parse(url)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin() && a.origin().is_tuple(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_goes_only_to_the_pod() {
        let base = "https://pod.example";
        assert!(same_origin(base, "https://pod.example/v1/construct"));
        assert!(same_origin(
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/v1/x"
        ));
        for url in [
            "https://elsewhere.example/v1/construct",
            "http://pod.example/v1/construct",
            "https://pod.example:8443/v1/construct",
            "https://pod.example.elsewhere.example/v1/construct",
            "https://pod.example@elsewhere.example/v1/construct",
            "not a url",
        ] {
            assert!(!same_origin(base, url), "{url}");
        }
    }

    /// A data path that is an absolute URL off the pod is fetched without
    /// the pod's token.
    #[test]
    fn absolute_urls_off_the_pod_get_no_token() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let other = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0; 8192];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            let body = "{}";
            write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            req
        });
        let pod = PodClient {
            base: "http://pod.example".into(),
            token: "session-secret".into(),
            http: client(),
            titles: Arc::default(),
            objects: Arc::default(),
        };
        pod.get(&format!("{other}/x")).unwrap();
        let req = seen.join().unwrap();
        assert!(!req.contains("authorization"), "{req}");
        assert!(!req.contains("session-secret"), "{req}");
    }

    #[test]
    fn only_staged_files_are_read() {
        let root = std::env::temp_dir().join(format!("construct-staged-{}", std::process::id()));
        let staged = root.join("attachments");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("a.jpg"), b"photo").unwrap();
        std::fs::write(root.join("private.db"), b"secret").unwrap();
        let url = |p: &std::path::Path| reqwest::Url::from_file_path(p).unwrap();
        assert_eq!(
            read_staged(&url(&staged.join("a.jpg")), Some(&staged)).as_deref(),
            Some(&b"photo"[..])
        );
        assert_eq!(
            read_staged(&url(&root.join("private.db")), Some(&staged)),
            None
        );
        let dotdot = reqwest::Url::parse(&format!("{}/../private.db", url(&staged))).unwrap();
        assert_eq!(read_staged(&dotdot, Some(&staged)), None);
        // A plain URL for the file outside the staged folder, written with `..`
        // the parser doesn't fold.
        let raw = reqwest::Url::parse(&format!(
            "file://{}/attachments/%2E%2E/private.db",
            root.display()
        ))
        .unwrap();
        assert_eq!(read_staged(&raw, Some(&staged)), None);
        // No staged folder (a preview without an engine): no local files.
        assert_eq!(read_staged(&url(&staged.join("a.jpg")), None), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
