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
        let token = self.token.clone();
        let pod = ImagePod {
            base: self.base.clone(),
            bearer: Arc::new(move || (!token.is_empty()).then(|| token.clone())),
        };
        fetch_images(&s.images, None, Some(pod));
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
/// Largest image downloaded (the pod's image proxy passes on no more).
pub const MAX_IMAGE_BYTES: usize = 10 << 20;
/// Largest side an image may claim before it is decoded.
const MAX_DECODE_SIDE: u32 = 12_000;
/// Most memory one decode may take (a decompression bomb fails cleanly).
const MAX_DECODE_ALLOC: u64 = 128 << 20;
/// The pod's image proxy (`pod_core::image_proxy`).
const IMAGE_PROXY_PATH: &str = "/v1/image";

/// The pod images come from: its own are fetched from it directly, every
/// other site's through its image proxy with the session's bearer.
#[derive(Clone)]
pub struct ImagePod {
    pub base: String,
    /// The session's bearer now (it changes with sign-ins).
    pub bearer: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

/// How an image url is loaded.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// A staged photo, read from the engine's attachments.
    Staged(reqwest::Url),
    /// The pod's own (its media paths are public): fetched without a token.
    Pod(String),
    /// Another site's, through the pod's image proxy.
    Proxy(String),
    /// Not loaded: no pod to ask, or not `http(s)`.
    Refused,
}

/// A layout can name any image url (rows filled from scraped pages, adopted
/// layouts): the device fetches only its pod, never another host, so it
/// never reaches its own network (LAN, loopback) for one, nor shows a site
/// its address (issue #443).
fn route(url: &str, pod: Option<&ImagePod>) -> Route {
    let Ok(u) = reqwest::Url::parse(url) else {
        return Route::Refused;
    };
    match (u.scheme(), pod) {
        ("file", _) => Route::Staged(u),
        ("http" | "https", Some(p)) if same_origin(&p.base, url) => Route::Pod(url.to_owned()),
        ("http" | "https", Some(p)) => {
            let mut proxy = match reqwest::Url::parse(&format!(
                "{}{IMAGE_PROXY_PATH}",
                p.base.trim_end_matches('/')
            )) {
                Ok(proxy) => proxy,
                Err(_) => return Route::Refused,
            };
            proxy.query_pairs_mut().append_pair("url", u.as_str());
            Route::Proxy(proxy.into())
        }
        _ => Route::Refused,
    }
}

/// The HTTP client for images: redirects only within the pod's origin (the
/// proxy already followed the other site's), at most a few.
fn image_client(base: Option<&str>) -> reqwest::blocking::Client {
    let base = base.map(str::to_owned);
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("construct-render/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let ok = attempt.previous().len() < 3
                && base
                    .as_deref()
                    .is_some_and(|b| same_origin(b, attempt.url().as_str()));
            if ok { attempt.follow() } else { attempt.stop() }
        }))
        .build()
        .expect("an HTTP client")
}

/// GET `url`, at most [`MAX_IMAGE_BYTES`] (a longer answer fails).
fn download(http: &reqwest::blocking::Client, url: &str, bearer: Option<&str>) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut req = http.get(url);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().and_then(|r| r.error_for_status()).ok()?;
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_IMAGE_BYTES as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    resp.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_IMAGE_BYTES).then_some(bytes)
}

/// Decodes an image within limits (side, allocation), scaled down to
/// [`MAX_IMAGE_SIDE`]. `None` for what isn't an image or is too large.
pub fn decode_image(bytes: &[u8]) -> Option<image::RgbaImage> {
    decode(bytes).ok()
}

fn decode(bytes: &[u8]) -> image::ImageResult<image::RgbaImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let i = reader.decode()?;
    Ok(if i.width().max(i.height()) > MAX_IMAGE_SIDE {
        i.thumbnail(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE)
    } else {
        i
    }
    .to_rgba8())
}

/// Makes `store` download what it is asked for, on background threads:
/// the pod's own images from it, other sites' through its image proxy
/// ([`route`]); without a pod, only staged photos. One that fails keeps its
/// placeholder. A `file://` image is read only from `staged`, the engine's
/// staged attachments.
pub fn fetch_images(store: &ImageStore, staged: Option<&std::path::Path>, pod: Option<ImagePod>) {
    // Newest first: what is on screen now goes before what scrolled past.
    let queue: Arc<(Mutex<Vec<String>>, std::sync::Condvar)> = Arc::default();
    let staged = staged.map(std::path::Path::to_path_buf);
    for _ in 0..IMAGE_WORKERS {
        let queue = queue.clone();
        let store = store.clone();
        let staged = staged.clone();
        let pod = pod.clone();
        std::thread::spawn(move || {
            let http = image_client(pod.as_ref().map(|p| p.base.as_str()));
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
                let bytes = match route(&url, pod.as_ref()) {
                    // A staged photo (`construct://attachments/<id>`) is a local file.
                    Route::Staged(u) => read_staged(&u, staged.as_deref()),
                    Route::Pod(u) => download(&http, &u, None),
                    Route::Proxy(u) => pod
                        .as_ref()
                        .and_then(|p| (p.bearer)())
                        .and_then(|t| download(&http, &u, Some(&t))),
                    Route::Refused => None,
                };
                store.insert(&url, bytes.and_then(|b| decode_image(&b)));
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

    /// A server on 127.0.0.1 that answers each connection with `answer`
    /// and reports the requests it saw.
    fn server(answer: Vec<u8>) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for s in listener.incoming() {
                let Ok(mut s) = s else { return };
                let mut buf = vec![0; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = s.write_all(&answer);
            }
        });
        (base, rx)
    }

    fn png_answer(body: &[u8]) -> Vec<u8> {
        let mut a = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        a.extend_from_slice(body);
        a
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(w, h)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn pod_at(base: &str, token: Option<&str>) -> ImagePod {
        let token = token.map(str::to_owned);
        ImagePod {
            base: base.to_owned(),
            bearer: Arc::new(move || token.clone()),
        }
    }

    fn load(store: &ImageStore, url: &str) -> &'static str {
        store.request(url);
        for _ in 0..200 {
            match store.status(url) {
                "loading" => std::thread::sleep(Duration::from_millis(25)),
                s => return s,
            }
        }
        "loading"
    }

    #[test]
    fn images_off_the_pod_go_through_its_proxy() {
        let pod = pod_at("https://pod.example", Some("t"));
        assert_eq!(
            route("https://pod.example/v1/p/a/media/1.jpg", Some(&pod)),
            Route::Pod("https://pod.example/v1/p/a/media/1.jpg".into())
        );
        assert_eq!(
            route("http://192.168.1.1/cam.jpg?a=b&c", Some(&pod)),
            Route::Proxy(
                "https://pod.example/v1/image?url=http%3A%2F%2F192.168.1.1%2Fcam.jpg%3Fa%3Db%26c"
                    .into()
            )
        );
        for url in [
            "http://192.168.1.1/cam.jpg",
            "ftp://img.example/a.png",
            "not a url",
        ] {
            assert!(matches!(route(url, None), Route::Refused), "{url}");
        }
        assert!(matches!(
            route("ftp://img.example/a.png", Some(&pod)),
            Route::Refused
        ));
    }

    /// An image on the device's network (another port of loopback here) is
    /// never fetched: the pod's proxy is asked, with the session.
    #[test]
    fn a_private_image_url_is_not_fetched() {
        let (lan, lan_seen) = server(png_answer(&png(2, 2)));
        let (base, pod_seen) = server(png_answer(&png(2, 2)));
        let store = ImageStore::new();
        fetch_images(&store, None, Some(pod_at(&base, Some("session-secret"))));
        let url = format!("{lan}/router.png");
        assert_eq!(load(&store, &url), "ready");
        let req = pod_seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            req.starts_with("GET /v1/image?url=http%3A%2F%2F127.0.0.1"),
            "{req}"
        );
        assert!(
            req.to_ascii_lowercase()
                .contains("authorization: bearer session-secret"),
            "{req}"
        );
        assert!(lan_seen.try_recv().is_err());
        // Signed out: nothing is fetched at all.
        let store = ImageStore::new();
        fetch_images(&store, None, Some(pod_at(&base, None)));
        assert_eq!(load(&store, &format!("{lan}/other.png")), "failed");
        assert!(pod_seen.try_recv().is_err());
        assert!(lan_seen.try_recv().is_err());
    }

    #[test]
    fn an_oversized_image_fails_cleanly() {
        let mut big = png(2, 2);
        big.resize(MAX_IMAGE_BYTES + 10, 0);
        // A length that says so, and one that doesn't (read until closed).
        let (base, _) = server(png_answer(&big));
        let mut unsized_answer =
            b"HTTP/1.1 200 OK\r\ncontent-type: image/png\r\nconnection: close\r\n\r\n".to_vec();
        unsized_answer.extend_from_slice(&big);
        let (base2, _) = server(unsized_answer);
        for base in [base, base2] {
            let store = ImageStore::new();
            fetch_images(&store, None, Some(pod_at(&base, None)));
            assert_eq!(load(&store, &format!("{base}/media/big.png")), "failed");
        }
    }

    /// A PNG that claims `w` × `h` (its IHDR patched, CRC redone).
    fn claims(w: u32, h: u32) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = !0u32;
            for b in bytes {
                c ^= u32::from(*b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let mut p = png(1, 1);
        // Signature (8), length (4), "IHDR" (4), then width and height.
        p[16..20].copy_from_slice(&w.to_be_bytes());
        p[20..24].copy_from_slice(&h.to_be_bytes());
        let sum = crc(&p[12..29]);
        p[29..33].copy_from_slice(&sum.to_be_bytes());
        p
    }

    #[test]
    fn a_decompression_bomb_fails_cleanly() {
        assert!(decode_image(&png(3, 2)).is_some());
        let big = decode_image(&png(2000, 10)).unwrap();
        assert_eq!(big.width(), MAX_IMAGE_SIDE);
        // Too wide, and within the sides but past the allocation limit.
        // Refused by the limits, before any pixel is allocated.
        for (w, h) in [(50_000, 50_000), (11_000, 11_000)] {
            let e = decode(&claims(w, h)).unwrap_err();
            assert!(matches!(e, image::ImageError::Limits(_)), "{w}×{h}: {e}");
        }
        // Within them, the same file fails only on its missing pixels.
        let e = decode(&claims(100, 100)).unwrap_err();
        assert!(!matches!(e, image::ImageError::Limits(_)), "{e}");
        assert!(decode_image(b"not an image").is_none());
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
