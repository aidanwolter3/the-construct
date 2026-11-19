//! The browser half of the web client (decision `web-client`): the pod's
//! HTTP API through `fetch`, a [`Loader`] over it, the events stream read
//! from a `fetch` body, images, and a `<canvas>` the CPU rasterizer draws
//! into, with pointer, wheel and keyboard input.
//!
//! The page and the API share an origin, and the session is an `HttpOnly`
//! cookie the server set when the pod code was typed: this code never sees
//! a token. Every request carries `x-construct-client: web`, which a page
//! of another site can't send without a preflight the server never answers.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use construct_render::device_data;
use construct_render::gpu::{Gpu, Renderer};
use construct_render::interact::{Changes, Done, Loaded, Loader, Session, Write, Written};
use construct_render::layout::Viewport;
use construct_render::scroll::{Fling, Flinging, VelocityTracker};
use construct_render::time::{Duration, Instant};
use construct_render::viewer::{Controller, EditKey, Event};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, HtmlInputElement, KeyboardEvent, PointerEvent,
    Request, RequestInit, Response, WheelEvent,
};

use crate::construct::{Construct, HOME, features_header, take_events};
use crate::signin;

/// The frame must be drawn again (something arrived).
static DIRTY: AtomicBool = AtomicBool::new(true);

fn wake() {
    DIRTY.store(true, Ordering::Relaxed);
}

fn window() -> web_sys::Window {
    web_sys::window().expect("a window")
}

/// The `localStorage` key of this browser's device id.
const DEVICE_KEY: &str = "construct.device";

thread_local! {
    static DEVICE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// This browser's device id (decision `device-versions`): made once and kept
/// in `localStorage`, so each browser is one device; a private window that
/// forgets it is a new one.
fn device_id() -> String {
    if let Some(id) = DEVICE.with(|d| d.borrow().clone()) {
        return id;
    }
    let storage = window().local_storage().ok().flatten();
    let stored = storage
        .as_ref()
        .and_then(|s| s.get_item(DEVICE_KEY).ok().flatten())
        .filter(|id| {
            (8..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    let id = stored.unwrap_or_else(|| {
        let id = random(24).unwrap_or_else(|| "web-device-unknown".into());
        if let Some(s) = &storage {
            let _ = s.set_item(DEVICE_KEY, &id);
        }
        id
    });
    DEVICE.with(|d| *d.borrow_mut() = Some(id.clone()));
    id
}

/// The headers that say which device this is and which build it runs.
fn identify(h: &web_sys::Headers) {
    let _ = h.set(protocol::DEVICE_HEADER, &device_id());
    let _ = h.set(protocol::BUILD_HEADER, &protocol::build_header());
    let ua = window().navigator().user_agent().unwrap_or_default();
    let _ = h.set(protocol::DEVICE_NAME_HEADER, &crate::browser_name(&ua));
}

fn log(msg: &str) {
    web_sys::console::log_1(&msg.into());
}

/// An answer: its status and JSON (`null` when empty or not JSON).
async fn request(method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
    let init = RequestInit::new();
    init.set_method(method);
    if let Some(b) = body {
        init.set_body(&JsValue::from_str(&b.to_string()));
    }
    let req = Request::new_with_str_and_init(path, &init).map_err(|e| format!("{e:?}"))?;
    let h = req.headers();
    let _ = h.set(protocol::FEATURES_HEADER, &features_header());
    let _ = h.set("x-construct-client", "web");
    identify(&h);
    if body.is_some() {
        let _ = h.set("content-type", "application/json");
    }
    let resp: Response = JsFuture::from(window().fetch_with_request(&req))
        .await
        .map_err(|_| format!("{path}: can't reach your pod"))?
        .dyn_into()
        .map_err(|_| "not a response".to_owned())?;
    let status = resp.status();
    let text = JsFuture::from(resp.text().map_err(|e| format!("{e:?}"))?)
        .await
        .ok()
        .and_then(|t| t.as_string())
        .unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

async fn get_json(path: &str) -> Result<Value, String> {
    match request("GET", path, None).await? {
        (401, _) => {
            // The session ended: back to the page that asks for the code.
            let _ = window().location().set_href("/web");
            Err("signed out".into())
        }
        (s, v) if (200..300).contains(&s) => Ok(v),
        (s, v) => Err(format!(
            "{path}: HTTP {s} {}",
            v["error"].as_str().unwrap_or("")
        )),
    }
}

async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |done, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&done, ms);
    });
    let _ = JsFuture::from(p).await;
}

/// What the loader keeps between frames.
#[derive(Default)]
struct State {
    construct: Construct,
    /// Each open layout's data, by key.
    data: HashMap<String, DataBundle>,
    /// Layouts the session opened, by key: reloaded when their data or the
    /// construct changes.
    open: BTreeSet<String>,
    /// Layouts home doesn't reach, opened by name: their element.
    named: HashMap<String, String>,
    /// Device data paths' last answers (notifications, channels, sign-ins).
    device: HashMap<String, Value>,
    changes: Option<Changes>,
    /// An upstream sign-in's progress or why it failed, shown as
    /// `local://session#status` (Settings' line under Sign-ins).
    sign_in_status: Option<String>,
}

impl State {
    fn push(&mut self, c: Changes) {
        match self.changes.as_mut() {
            Some(h) => {
                crate::merge(&mut h.data, c.data);
                h.layouts.extend(c.layouts);
            }
            None => self.changes = Some(c),
        }
        wake();
    }

    /// The device data a layout gets (`needs`: its JSON).
    fn device(&self, needs: &str) -> DataBundle {
        let base = window().location().origin().unwrap_or_default();
        let titles = self.construct.titles();
        let mut out = device_data::device(&base, needs, &titles, |p| {
            self.device
                .get(p)
                .cloned()
                .ok_or_else(|| "not loaded".to_owned())
        });
        if let (Some(s), Some(e)) = (
            &self.sign_in_status,
            out.entities.get_mut("local://session"),
        ) {
            e.set("status", s.clone());
        }
        out
    }
}

/// The session's [`Loader`]: layouts from the construct this client holds,
/// their data fetched in the background and brought by [`Loader::changes`].
#[derive(Clone, Default)]
pub struct WebLoader {
    st: Arc<Mutex<State>>,
}

impl WebLoader {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Fetches what the root, every layout it reaches and `extra` (layouts
    /// opened by name) lack.
    async fn sync_objects(&self, root: &str, extra: &[String]) -> Result<(), String> {
        let mut want = vec![root.to_owned()];
        want.extend(extra.iter().cloned());
        for _ in 0..16 {
            let have = self.lock().construct.have();
            let v = request(
                "POST",
                "/v1/objects",
                Some(&json!({ "have": have, "want": want })),
            )
            .await?
            .1;
            let missing = {
                let mut st = self.lock();
                st.construct.take(&v);
                st.construct.set_root(root);
                let mut els: Vec<String> =
                    st.construct.keys().into_values().map(|(e, _)| e).collect();
                els.push(root.to_owned());
                els.extend(extra.iter().cloned());
                st.construct.missing(&els)
            };
            if missing.is_empty() && v["more"] != json!(true) {
                return Ok(());
            }
            if !missing.is_empty() {
                want = missing;
            }
        }
        Err("the pod's objects didn't all come".into())
    }

    /// Home's root for this client, with everything it reaches.
    async fn load_root(&self) -> Result<String, String> {
        let v = get_json("/v1/construct").await?;
        let root = v["root"].as_str().ok_or("no root")?.to_owned();
        self.sync_objects(&root, &[]).await?;
        Ok(root)
    }

    /// The device data the layouts read (the inbox, channels, sign-ins).
    async fn load_device(&self) {
        for p in [
            device_data::NOTIFICATIONS_PATH,
            device_data::CHANNELS_PATH,
            device_data::SIGN_INS_PATH,
        ] {
            if let Ok(v) = get_json(p).await {
                self.lock().device.insert(p.to_owned(), v);
            }
        }
    }

    /// Shows a sign-in's progress (`None`: nothing) where Settings reads it,
    /// with the sign-ins as they are now.
    fn sign_in_status(&self, status: Option<&str>) {
        let mut st = self.lock();
        st.sign_in_status = status.map(str::to_owned);
        let data = st.device("local://session local:sign_ins");
        st.push(Changes {
            data,
            layouts: Vec::new(),
        });
    }

    /// Starts an upstream sign-in (decision `web-upstream-sign-in`):
    /// registers this page as the redirect with the pod, keeps the PKCE
    /// verifier and `state` in the tab, and goes to the authorize page.
    async fn begin_sign_in(&self, login: Value) {
        self.sign_in_status(Some(signin::STARTING));
        let fail = |status: Option<u16>, v: &Value| {
            log(&format!("sign-in: {status:?} {v}"));
            self.sign_in_status(Some(signin::error_message(status, v["error"].as_str())));
        };
        let (Some(instance), Some(app_path), Some(token_path)) = (
            login["instance_url"].as_str(),
            login["app_path"].as_str(),
            login["token_path"].as_str(),
        ) else {
            return fail(Some(400), &login);
        };
        let origin = window().location().origin().unwrap_or_default();
        let redirect = signin::redirect_uri(&origin);
        let body = json!({ "instance_url": instance, "redirect_uri": redirect });
        let app = match request("POST", app_path, Some(&body)).await {
            Ok((s, v)) if (200..300).contains(&s) => v,
            Ok((s, v)) => return fail(Some(s), &v),
            Err(_) => return fail(None, &Value::Null),
        };
        let (Some(endpoint), Some(client_id)) = (
            app["authorize_endpoint"].as_str(),
            app["client_id"].as_str(),
        ) else {
            return fail(Some(500), &app);
        };
        let (Some(verifier), Some(state)) = (random(32), random(24)) else {
            return fail(Some(500), &json!({ "error": "no random numbers" }));
        };
        let pending = signin::Pending {
            state,
            verifier,
            instance_url: instance.to_owned(),
            token_path: token_path.to_owned(),
            redirect_uri: redirect,
            started_ms: js_sys::Date::now(),
        };
        let kept = window()
            .session_storage()
            .ok()
            .flatten()
            .is_some_and(|s| s.set_item(signin::PENDING_KEY, &pending.to_json()).is_ok());
        if !kept {
            return fail(Some(500), &json!({ "error": "no session storage" }));
        }
        let url = signin::authorize_url(
            endpoint,
            client_id,
            &pending.redirect_uri,
            app["scopes"].as_str().unwrap_or(""),
            &pending.state,
            &signin::challenge(&pending.verifier),
        );
        if window().location().assign(&url).is_err() {
            fail(
                Some(500),
                &json!({ "error": "can't open the sign-in page" }),
            );
        }
    }

    /// The page came back from an authorize page (`/web?code=…&state=…`):
    /// takes the address back to `/web` and hands the code to the pod, which
    /// adds the sign-in to this browser's session.
    async fn finish_sign_in(&self) {
        let location = window().location();
        let Some(ret) = signin::parse_return(&location.search().unwrap_or_default()) else {
            return;
        };
        if let Ok(h) = window().history() {
            let _ = h.replace_state_with_url(&JsValue::NULL, "", Some(signin::PAGE_PATH));
        }
        let storage = window().session_storage().ok().flatten();
        let pending = storage
            .as_ref()
            .and_then(|s| s.get_item(signin::PENDING_KEY).ok().flatten())
            .and_then(|raw| signin::Pending::parse(&raw));
        if let Some(s) = &storage {
            let _ = s.remove_item(signin::PENDING_KEY);
        }
        let code = match signin::check_return(&ret, pending.as_ref(), js_sys::Date::now()) {
            Ok(code) => code,
            Err(message) => return self.sign_in_status(Some(message)),
        };
        let Some(p) = pending else { return };
        self.sign_in_status(Some(signin::FINISHING));
        let status = match request("POST", &p.token_path, Some(&p.exchange(&code))).await {
            Ok((s, _)) if (200..300).contains(&s) => None,
            Ok((s, v)) => {
                log(&format!("sign-in: HTTP {s} {v}"));
                Some(signin::error_message(Some(s), v["error"].as_str()))
            }
            Err(_) => Some(signin::POD_UNREACHABLE),
        };
        self.sign_in_status(status);
    }

    /// The data path of layout `key`, if it is known.
    fn data_path(&self, key: &str) -> Option<String> {
        let st = self.lock();
        if let Some(l) = st.construct.layout(key) {
            return Some(l.data_path);
        }
        st.named
            .get(key)
            .map(|el| crate::construct::data_path(el, &Default::default()))
    }

    /// Fetches layout `key`'s data and brings it to the session.
    fn load_data(&self, key: &str) {
        let Some(path) = self.data_path(key) else {
            return;
        };
        let me = self.clone();
        let key = key.to_owned();
        spawn_local(async move {
            match get_json(&path)
                .await
                .and_then(|v| serde_json::from_value::<DataBundle>(v).map_err(|e| e.to_string()))
            {
                Ok(d) => {
                    let mut st = me.lock();
                    st.data.insert(key, d.clone());
                    st.push(Changes {
                        data: d,
                        layouts: Vec::new(),
                    });
                }
                Err(e) => log(&format!("loading {key}: {e}")),
            }
        });
    }

    /// The root moved: every open layout is drawn from the new one.
    async fn construct_moved(&self, root: &str) {
        if self.lock().construct.root() == Some(root) {
            return;
        }
        let named: Vec<String> = self.lock().named.values().cloned().collect();
        if let Err(e) = self.sync_objects(root, &named).await {
            log(&format!("the construct: {e}"));
            return;
        }
        let open: Vec<String> = self.lock().open.iter().cloned().collect();
        let mut layouts = Vec::new();
        for key in &open {
            if let Some(l) = self.lock().construct.layout(key) {
                layouts.push((key.clone(), l.node));
            }
            self.load_data(key);
        }
        self.lock().push(Changes {
            data: DataBundle::default(),
            layouts,
        });
    }

    /// Keeps `GET /v1/construct/events` open (a `fetch` body, so it carries
    /// the features and client headers), reconnecting when it drops.
    pub fn listen(&self) {
        let me = self.clone();
        spawn_local(async move {
            loop {
                if let Err(e) = me.stream_once().await {
                    log(&format!("events: {e}"));
                }
                sleep(2_000).await;
                // Whatever changed while the stream was down.
                let open: Vec<String> = me.lock().open.iter().cloned().collect();
                for k in &open {
                    me.load_data(k);
                }
            }
        });
    }

    async fn stream_once(&self) -> Result<(), String> {
        let init = RequestInit::new();
        let req = Request::new_with_str_and_init("/v1/construct/events", &init)
            .map_err(|e| format!("{e:?}"))?;
        let _ = req
            .headers()
            .set(protocol::FEATURES_HEADER, &features_header());
        let _ = req.headers().set("x-construct-client", "web");
        identify(&req.headers());
        let resp: Response = JsFuture::from(window().fetch_with_request(&req))
            .await
            .map_err(|e| format!("{e:?}"))?
            .dyn_into()
            .map_err(|_| "not a response")?;
        if !resp.ok() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let body = resp.body().ok_or("no body")?;
        let reader: web_sys::ReadableStreamDefaultReader =
            body.get_reader().dyn_into().map_err(|_| "no reader")?;
        let decoder = web_sys::TextDecoder::new().map_err(|e| format!("{e:?}"))?;
        let mut buf = String::new();
        loop {
            let chunk = JsFuture::from(reader.read())
                .await
                .map_err(|e| format!("{e:?}"))?;
            let done = js_sys::Reflect::get(&chunk, &"done".into())
                .ok()
                .and_then(|d| d.as_bool())
                .unwrap_or(true);
            if done {
                return Ok(());
            }
            let value =
                js_sys::Reflect::get(&chunk, &"value".into()).map_err(|e| format!("{e:?}"))?;
            let bytes: js_sys::Uint8Array = value.dyn_into().map_err(|_| "not bytes")?;
            let opts = web_sys::TextDecodeOptions::new();
            opts.set_stream(true);
            buf.push_str(
                &decoder
                    .decode_with_js_u8_array_and_options(&bytes, &opts)
                    .unwrap_or_default(),
            );
            for (event, data) in take_events(&mut buf) {
                let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                match event.as_str() {
                    "construct" => {
                        if let Some(root) = v["root"].as_str() {
                            self.construct_moved(root).await;
                        }
                    }
                    "data" => {
                        let open: Vec<String> = self.lock().open.iter().cloned().collect();
                        for k in &open {
                            self.load_data(k);
                        }
                    }
                    "notification" | "notifications_read" => {
                        self.load_device().await;
                        let device = self.lock().device("local:notifications");
                        self.lock().push(Changes {
                            data: device,
                            layouts: Vec::new(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Loader for WebLoader {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let found = {
            let st = self.lock();
            st.construct.layout(id).or_else(|| {
                let el = st.named.get(id)?;
                st.construct.element(el, &Default::default())
            })
        };
        let Some(l) = found else {
            // Not reached from home: by name, then the session opens it again.
            let me = self.clone();
            let id = id.to_owned();
            spawn_local(async move {
                let path = format!("/v1/construct/names/{}", crate::construct::encode(&id));
                if let Ok(v) = get_json(&path).await
                    && let Some(el) = v["el"].as_str()
                {
                    me.lock().named.insert(id, el.to_owned());
                    // The guard goes before the await: another task may lock
                    // meanwhile (one thread: it would never be let go).
                    let root = me.lock().construct.root().map(str::to_owned);
                    if let Some(root) = root {
                        let _ = me.sync_objects(&root, &[el.to_owned()]).await;
                    }
                    wake();
                }
            });
            return Err("Opening…".into());
        };
        let mut data = {
            let mut st = self.lock();
            st.open.insert(id.to_owned());
            st.data.get(id).cloned().unwrap_or_default()
        };
        let needs = serde_json::to_string(&l.node).unwrap_or_default();
        let device = self.lock().device(&needs);
        crate::merge(&mut data, device);
        self.load_data(id);
        Ok((l.node, data))
    }

    fn data(&self, path: &str) -> Result<DataBundle, String> {
        // Never in place: `fetch` loads every data path in the background.
        Err(format!("{path}: loading"))
    }

    fn fetch(&self, path: &str, done: Loaded) -> bool {
        let path = path.to_owned();
        spawn_local(async move {
            let r = get_json(&path)
                .await
                .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()));
            done(r);
            wake();
        });
        true
    }

    fn send(&self, write: Write, done: Done) -> bool {
        let me = self.clone();
        spawn_local(async move {
            let result = match request("POST", &write.path, Some(&write.body)).await {
                Ok((s, v)) if (200..300).contains(&s) => Ok(v),
                Ok((s, v)) => Err(format!(
                    "{}: HTTP {s} {}",
                    write.path,
                    v["error"].as_str().unwrap_or("")
                )),
                Err(e) => Err(e),
            };
            let mut refreshed = Vec::new();
            if result.is_ok() {
                for id in &write.refresh {
                    if let Some(p) = me.data_path(id)
                        && let Ok(v) = get_json(&p).await
                        && let Ok(d) = serde_json::from_value::<DataBundle>(v)
                    {
                        refreshed.push(d);
                    }
                }
            }
            done(Written { result, refreshed });
            wake();
        });
        true
    }

    fn changes(&self) -> Option<Changes> {
        self.lock().changes.take()
    }

    /// Settings' Sign in (and a layout's `action.auth.login`) goes to the
    /// upstream's authorize page and comes back to this page; Sign out ends
    /// this browser's session as it does a native client's unpinned one.
    fn auth(&self, intent: &Value) -> bool {
        if intent["command"] == "sign_out" {
            spawn_local(async {
                let _ = request("POST", "/web/logout", None).await;
                let _ = window().location().reload();
            });
            return true;
        }
        let login = {
            let st = self.lock();
            signin::login_intent(intent, st.device.get(device_data::SIGN_INS_PATH))
        };
        let Some(login) = login else {
            return false;
        };
        let me = self.clone();
        spawn_local(async move { me.begin_sign_in(login).await });
        true
    }
}

/// `bytes` random bytes from the browser's CSPRNG, base64url.
fn random(bytes: usize) -> Option<String> {
    let mut buf = vec![0u8; bytes];
    window()
        .crypto()
        .ok()?
        .get_random_values_with_u8_array(&mut buf)
        .ok()?;
    Some(signin::random_string(&buf))
}

/// Downloads images for the session: the pod's own through `fetch`, others
/// through the pod's image proxy (a canvas can't read another site's pixels
/// without CORS, decision `web-client`).
fn fetch_images(store: &construct_render::images::ImageStore, proxy: &str) {
    let proxy = proxy.to_owned();
    let origin = window().location().origin().unwrap_or_default();
    let s = store.clone();
    store.set_fetcher(move |url| {
        let s = s.clone();
        let path = if url.starts_with('/') || url.starts_with(&origin) {
            url.clone()
        } else {
            format!("{proxy}?url={}", crate::construct::encode(&url))
        };
        spawn_local(async move {
            let img = image_bytes(&path).await.and_then(|b| {
                let i = image::load_from_memory(&b).ok()?;
                Some(
                    if i.width().max(i.height()) > 1600 {
                        i.thumbnail(1600, 1600)
                    } else {
                        i
                    }
                    .to_rgba8(),
                )
            });
            s.insert(&url, img);
            wake();
        });
    });
}

/// A GET of the pod's bytes (an image through the proxy, a font): with
/// `x-construct-client: web`, without which the pod doesn't read the session
/// cookie and answers 401 (decision `web-client`).
async fn image_bytes(path: &str) -> Option<Vec<u8>> {
    let req = Request::new_with_str(path).ok()?;
    let _ = req.headers().set("x-construct-client", "web");
    let resp: Response = JsFuture::from(window().fetch_with_request(&req))
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    if !resp.ok() {
        log(&format!("{path}: {}", resp.status()));
        return None;
    }
    let buf = JsFuture::from(resp.array_buffer().ok()?).await.ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Fetches the fonts by name (`/web/fonts/<file>`): the faces before the
/// first frame, the fallbacks after it (decision `web-client`). With the
/// build's hash of the fonts (`constructFontVersion`, set by the page's
/// script) they are asked for as `?v=<hash>`, which the browser keeps until
/// a deploy changes them (decision `web-client-cache`).
async fn load_fonts(files: &[&str]) {
    let version = js_sys::Reflect::get(&js_sys::global(), &"constructFontVersion".into())
        .ok()
        .and_then(|v| v.as_string())
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit()));
    let query = version.map(|v| format!("?v={v}")).unwrap_or_default();
    for f in files {
        if let Some(b) = image_bytes(&format!("/web/fonts/{f}{query}")).await {
            construct_render::text::add_font(b);
        } else {
            log(&format!("couldn't load the font {f}"));
        }
    }
}

/// The app on a page: the canvas, a hidden text input for typing, and the
/// session drawn into the canvas.
/// What draws the frames: the GPU (WebGPU, else WebGL2) through the
/// renderer's wgpu pipeline, or, where the browser has neither, the CPU
/// rasterizer into a 2D canvas (decision `web-gpu`).
enum Drawer {
    Gpu(Box<GpuDrawer>),
    Cpu(CanvasRenderingContext2d),
}

struct GpuDrawer {
    gpu: Gpu,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

impl GpuDrawer {
    /// The GPU behind `canvas`: WebGL2, which every current browser has;
    /// WebGPU with `?webgpu` in the address (not yet checked on real
    /// hardware: headless Chromium's software WebGPU loses its device).
    async fn new(
        canvas: &HtmlCanvasElement,
        images: &construct_render::images::ImageStore,
    ) -> Result<GpuDrawer, String> {
        let webgpu = window()
            .location()
            .search()
            .unwrap_or_default()
            .contains("webgpu");
        let desc = wgpu::InstanceDescriptor {
            backends: if webgpu {
                wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL
            } else {
                wgpu::Backends::GL
            },
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        };
        let instance = wgpu::util::new_instance_with_webgpu_detection(desc).await;
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| e.to_string())?;
        let gpu = Gpu::with_instance(instance, Some(&surface))
            .await
            .map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&gpu.adapter);
        // A non-sRGB format blends as the CPU path does (as the desktop).
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or("the canvas supports no format")?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: canvas.width().max(1),
            height: canvas.height().max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &config);
        gpu.device
            .on_uncaptured_error(std::sync::Arc::new(|e| log(&format!("GPU error: {e}"))));
        let mut renderer = Renderer::new(&gpu, format);
        renderer.images = Some(images.clone());
        log(&format!("drawing on the GPU: {}", gpu.describe()));
        Ok(GpuDrawer {
            gpu,
            surface,
            config,
            renderer,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.gpu.device, &self.config);
    }

    /// Draws the session's scene; false when there was no frame to draw on.
    fn draw(&mut self, session: &mut Session) -> bool {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(_) | wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.gpu.device, &self.config);
                return false;
            }
            other => {
                log(&format!("no frame: {other:?}"));
                return false;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let px = (self.config.width, self.config.height);
        match session.scene_and_text() {
            Ok((scene, text)) => {
                if let Err(e) = self.renderer.render(&self.gpu, &view, px, scene, text) {
                    log(&format!("drawing: {e}"));
                }
            }
            Err(e) => log(&format!("drawing: {e}")),
        }
        self.gpu.queue.present(frame);
        true
    }
}

struct App {
    controller: Controller,
    canvas: HtmlCanvasElement,
    drawer: Drawer,
    input: HtmlInputElement,
    drawn: u64,
    /// Where the primary pointer went down, and when; a drag that moves
    /// scrolls, one that doesn't is a tap.
    down: Option<(f32, f32, f32, f32, Instant, bool)>,
    /// The finger's positions down the screen while it drags, for its speed
    /// when it lifts.
    tracker: VelocityTracker,
    /// A touch scroll still moving after the finger lifted (decision
    /// `web-touch-fling`): the renderer's fling, as the Android app flings,
    /// and where it started.
    fling: Option<(Flinging, f32, f32)>,
    /// Links to open in new tabs when the handler that tapped them lets the
    /// app go.
    links: Vec<String>,
    /// Which of the page's history entries is current: 0 the page itself,
    /// `k` the `k`th this app pushed, one for each step of the back stack
    /// ([`Session::back_steps`]), so the browser's Back closes the top one
    /// instead of leaving the page.
    history_at: usize,
    /// On a public link's page: a row's screen shows its own address
    /// (`/<handle>/<slug>/<row>`), so a visitor can copy it.
    public: Option<Public>,
}

/// The app, held by one event handler at a time. The browser can run the
/// page's other handlers (and its frame) inside a call the app makes:
/// Chrome draws a frame inside `window.open`. Those find the app held and
/// skip, rather than panic, which would stop the frame loop for good.
fn lease(app: &RefCell<App>) -> Option<Lease<'_>> {
    app.try_borrow_mut().ok().map(|a| Lease(Some(a)))
}

/// A handler's hold on the app; letting go opens the links it tapped.
struct Lease<'a>(Option<std::cell::RefMut<'a, App>>);

impl std::ops::Deref for Lease<'_> {
    type Target = App;
    fn deref(&self) -> &App {
        self.0.as_ref().expect("held until dropped")
    }
}

impl std::ops::DerefMut for Lease<'_> {
    fn deref_mut(&mut self) -> &mut App {
        self.0.as_mut().expect("held until dropped")
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let links = self
            .0
            .as_mut()
            .map(|a| std::mem::take(&mut a.links))
            .unwrap_or_default();
        // Let go first: the browser may run the page's handlers inside
        // `window.open`, and they must find the app free.
        self.0 = None;
        for url in links {
            let _ = window().open_with_url_and_target_and_features(&url, "_blank", "noopener");
        }
    }
}

/// When an event happened (seconds on the page's clock), not when it is
/// handled: while a frame is drawing, touches wait, and their speed must
/// come from when they were made.
fn event_secs(e: &PointerEvent) -> f64 {
    e.time_stamp() / 1000.0
}

/// Seconds on the page's clock.
fn secs() -> f64 {
    window().performance().map_or(0.0, |p| p.now() / 1000.0)
}

impl App {
    fn viewport(&self) -> Viewport {
        let w = window();
        let scale = w.device_pixel_ratio() as f32;
        let width = w
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(390.0) as f32;
        let height = w
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(844.0) as f32;
        Viewport {
            width,
            height,
            scale: scale.max(1.0),
        }
    }

    fn resize(&mut self) {
        let vp = self.viewport();
        self.canvas.set_width((vp.width * vp.scale) as u32);
        self.canvas.set_height((vp.height * vp.scale) as u32);
        let style = self.canvas.style();
        let _ = style.set_property("width", &format!("{}px", vp.width));
        let _ = style.set_property("height", &format!("{}px", vp.height));
        if let Drawer::Gpu(g) = &mut self.drawer {
            g.resize(self.canvas.width(), self.canvas.height());
        }
        self.controller.handle(Event::Resized {
            width_px: (vp.width * vp.scale) as u32,
            height_px: (vp.height * vp.scale) as u32,
            scale: vp.scale,
        });
        self.drawn = u64::MAX;
    }

    /// The mouse moved to a point, or left the canvas (`None`): the
    /// tappable under it is drawn hovered and the pointer is a hand.
    fn hover(&mut self, at: Option<(f32, f32)>) {
        let session = &mut self.controller.session;
        let changed = match at {
            Some((x, y)) => session.hover(x, y),
            None => session.hover_end(),
        };
        if changed {
            let cursor = if session.hovering() { "pointer" } else { "" };
            let _ = self.canvas.style().set_property("cursor", cursor);
            wake();
        }
    }

    fn event(&mut self, e: Event) {
        let out = self.controller.handle(e);
        // A link opens in a new tab, not over the pod, once the app is let
        // go (`Lease`): the browser runs the page's code inside `window.open`.
        for effect in out.iter().flat_map(|o| &o.effects) {
            match effect {
                construct_render::interact::Effect::OpenLink { url }
                    if url.starts_with("https://") || url.starts_with("http://") =>
                {
                    self.links.push(url.clone());
                }
                // Settings' "Leave pod on this device": this browser logs
                // out (its session ends; the pod code still works) and asks
                // for a code again.
                construct_render::interact::Effect::LeftPod => {
                    spawn_local(async {
                        let _ = request("POST", "/web/logout", None).await;
                        let _ = window().location().reload();
                    });
                }
                _ => {}
            }
        }
        self.sync_input();
        self.sync_history();
        wake();
    }

    /// One history entry for each step of the back stack: a screen opened
    /// pushes one (inside the tap, so the browser keeps it), and one closed
    /// in the app (its Back arrow, Escape) goes back over its entry.
    fn sync_history(&mut self) {
        let Ok(history) = window().history() else {
            return;
        };
        let want = self.controller.session.back_steps();
        while self.history_at < want {
            self.history_at += 1;
            let at = JsValue::from_f64(self.history_at as f64);
            let _ = history.push_state_with_url(&at, "", self.address(want).as_deref());
        }
        if want < self.history_at {
            let _ = history.go_with_delta(want as i32 - self.history_at as i32);
            self.history_at = want;
        }
    }

    /// The address of a history entry pushed for one step of the back
    /// stack, on a public link's page (`None`: the address stays): a row's
    /// screen opened from the list is `<base>/<row>`.
    fn address(&self, want: usize) -> Option<String> {
        let p = self.public.as_ref()?;
        if self.history_at != 1 || want != 1 {
            return None;
        }
        let route = self.controller.session.top_route();
        protocol::public_rows::row_with_params(&p.rows, route)
            .map(|r| format!("{}/{}", p.base, r.name))
    }

    /// The browser went back (or forward) to the history entry `at`: Back
    /// closes the back stack's top steps until as many are left.
    fn went_to(&mut self, at: usize) {
        self.history_at = at;
        while self.controller.session.back_steps() > at {
            let before = self.controller.session.back_steps();
            self.controller.unfocus();
            self.event(Event::Back);
            if self.controller.session.back_steps() >= before {
                break;
            }
        }
        self.sync_history();
    }

    /// Keeps the hidden input focused while a field is, so the keyboard (and
    /// a phone's on-screen one) types into it.
    fn sync_input(&self) {
        if self.controller.focused().is_some() {
            let _ = self.input.focus();
        } else {
            let _ = self.input.blur();
        }
    }

    /// Draws a frame when something changed.
    /// Moves a fling on by what this frame's share of it is.
    fn step_fling(&mut self) {
        let Some((f, x, y)) = self.fling.as_mut() else {
            return;
        };
        let (dy, done) = f.step(secs());
        let (x, y) = (*x, *y);
        if dy != 0.0 {
            self.controller.handle(Event::Scroll { x, y, dy });
        }
        if done {
            self.fling = None;
        }
    }

    fn frame(&mut self) {
        self.step_fling();
        let due = self
            .controller
            .session
            .next_wake()
            .is_some_and(|t| t <= Instant::now());
        // A slide, a spinner or a settling card draws every frame until done.
        let animating = self.controller.session.animating();
        let dirty = DIRTY.swap(false, Ordering::Relaxed)
            || self.controller.dirty
            || due
            || animating
            || self.fling.is_some();
        if !dirty {
            return;
        }
        self.controller.dirty = false;
        let session = &mut self.controller.session;
        if let Err(e) = session.scene() {
            log(&format!("drawing: {e}"));
            return;
        }
        if session.scenes_built() == self.drawn {
            return;
        }
        match &mut self.drawer {
            Drawer::Gpu(g) => {
                if g.draw(session) {
                    self.drawn = session.scenes_built();
                } else {
                    wake();
                }
            }
            Drawer::Cpu(ctx) => {
                self.drawn = session.scenes_built();
                match session.render_cpu() {
                    Ok(img) => {
                        let (w, h) = img.dimensions();
                        let data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
                            wasm_bindgen::Clamped(img.as_raw()),
                            w,
                            h,
                        );
                        if let Ok(d) = data {
                            let _ = ctx.put_image_data(&d, 0.0, 0.0);
                        }
                    }
                    Err(e) => log(&format!("drawing: {e}")),
                }
            }
        }
        if session.busy() {
            wake();
        }
    }
}

/// The frame loop's `requestAnimationFrame` callback.
type FrameCallback = Closure<dyn FnMut()>;

/// How far a pointer may move and still tap (points).
const TAP_SLOP: f32 = 8.0;
/// A held pointer this long is a long press.
const LONG_PRESS: Duration = Duration::from_millis(500);

/// Starts the app in the page's `<canvas id="construct">`.
pub async fn start() -> Result<(), JsValue> {
    load_fonts(&construct_render::text::FACE_FILES).await;
    let loader = WebLoader::default();
    // Back from an upstream's sign-in page: before the sign-ins are read.
    loader.finish_sign_in().await;
    loader.load_device().await;
    loader
        .load_root()
        .await
        .map_err(|e| JsValue::from_str(&e))?;
    let (root, data) = loader.layout(HOME).map_err(|e| JsValue::from_str(&e))?;
    let mut session = Session::new(HOME, root, data, Viewport::PHONE);
    session.loader = Some(Box::new(loader.clone()));
    fetch_images(&session.images, "/web/image");
    loader.listen();
    mount(session, None).await
}

/// A public link's page (decisions `public-web-links`,
/// `public-links-share-the-closure`): its copy, drawn read-only with no pod
/// behind it (nothing it draws can write); the layouts it opens come from
/// the copy. `base` is the page's path: `/s/<link>` or `/<handle>/<slug>`;
/// `row`, the name of one of its rows (`/<handle>/<slug>/<row>`, decision
/// `public-row-links`), opens that row's screen over the list.
pub async fn start_public(base: &str, row: Option<&str>) -> Result<(), JsValue> {
    load_fonts(&construct_render::text::FACE_FILES).await;
    let copy = get_json(&format!("{base}/view"))
        .await
        .map_err(|_| JsValue::from_str("This page isn't shared any more."))?;
    let mut session = construct_render::public::session(&copy, Viewport::PHONE)
        .map_err(|e| JsValue::from_str(&e))?;
    fetch_images(&session.images, &format!("{base}/image"));
    let public = Public {
        base: base.to_owned(),
        rows: protocol::public_rows::rows(&copy),
    };
    if let Some(name) = row {
        // The list is the page's first history entry, at the list's
        // address; the row's screen pushes its own over it (`sync_history`).
        if history_at() == 0
            && let Ok(h) = window().history()
        {
            let _ = h.replace_state_with_url(&JsValue::from_f64(0.0), "", Some(base));
        }
        // A name no row has (renamed, gone): the list.
        if let Some(r) = public.rows.iter().find(|r| r.name == name) {
            let _ = session.dispatch(&r.intent);
        }
    }
    mount(session, Some(public)).await
}

/// A public link's page: its path, and its rows by name, so the address
/// follows the screen a row opens.
struct Public {
    base: String,
    rows: Vec<protocol::public_rows::Row>,
}

/// Puts a session on the page's canvas and runs it.
async fn mount(session: Session, public: Option<Public>) -> Result<(), JsValue> {
    // A panic says what it was in the console (else only "unreachable").
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&format!("construct-web: {info}").into())
    }));
    session.images.set_waker(wake);
    let doc = window().document().ok_or("no document")?;
    let mut canvas: HtmlCanvasElement = doc
        .get_element_by_id("construct")
        .ok_or("no canvas")?
        .dyn_into()?;
    // Sized before the GPU takes the canvas, so its first frame fits.
    let ratio = window().device_pixel_ratio();
    let inner = |v: Result<JsValue, JsValue>| v.ok().and_then(|v| v.as_f64()).unwrap_or(1.0);
    canvas.set_width((inner(window().inner_width()) * ratio) as u32);
    canvas.set_height((inner(window().inner_height()) * ratio) as u32);
    // `?cpu` in the address draws on the CPU (to compare, or where a GPU
    // misbehaves).
    let want_gpu = !window()
        .location()
        .search()
        .unwrap_or_default()
        .contains("cpu");
    let gpu = if want_gpu {
        match GpuDrawer::new(&canvas, &session.images).await {
            Ok(g) => Some(g),
            Err(e) => {
                log(&format!("no GPU, drawing on the CPU: {e}"));
                // A canvas keeps the first kind of context it gave out: a
                // fresh one for the 2D context.
                let fresh: HtmlCanvasElement = canvas.clone_node()?.dyn_into()?;
                canvas.replace_with_with_node_1(&fresh)?;
                canvas = fresh;
                None
            }
        }
    } else {
        None
    };
    let drawer = match gpu {
        Some(g) => Drawer::Gpu(Box::new(g)),
        None => Drawer::Cpu(
            canvas
                .get_context("2d")?
                .ok_or("no 2d context")?
                .dyn_into()?,
        ),
    };
    let input: HtmlInputElement = doc
        .get_element_by_id("construct-input")
        .ok_or("no input")?
        .dyn_into()?;

    let app = Rc::new(RefCell::new(App {
        controller: Controller::new(session),
        canvas: canvas.clone(),
        drawer,
        input: input.clone(),
        drawn: u64::MAX,
        down: None,
        tracker: VelocityTracker::new(),
        fling: None,
        links: Vec::new(),
        history_at: history_at(),
        public,
    }));
    app.borrow_mut().resize();
    // A reload on one of this app's entries goes back to the page's own.
    app.borrow_mut().sync_history();
    let _ = doc.body().map(|b| b.class_list().add_1("ready"));

    // The fallbacks after the first frame: emoji and symbols appear when
    // they come.
    spawn_local(async {
        load_fonts(&construct_render::text::FALLBACK_FILES).await;
        wake();
    });

    listen(&window(), "resize", {
        let app = app.clone();
        move |_: web_sys::Event| {
            if let Some(mut a) = lease(&app) {
                a.resize();
            }
        }
    });
    listen(&canvas, "pointerdown", {
        let app = app.clone();
        move |e: PointerEvent| {
            if !e.is_primary() {
                return;
            }
            let (x, y) = (e.offset_x() as f32, e.offset_y() as f32);
            let Some(mut a) = lease(&app) else {
                return;
            };
            // A finger landing on a moving fling catches it: it stops, and
            // the touch is no tap.
            let caught = a.fling.take().is_some_and(|(f, _, _)| f.caught(secs()));
            a.tracker.clear();
            a.tracker.add(event_secs(&e), y);
            a.down = Some((x, y, x, y, Instant::now(), caught));
            // Moves outside the canvas still belong to this drag.
            let _ = a.canvas.set_pointer_capture(e.pointer_id());
        }
    });
    listen(&canvas, "pointermove", {
        let app = app.clone();
        move |e: PointerEvent| {
            let Some(mut a) = lease(&app) else {
                return;
            };
            let Some((x0, y0, _, ly, at, moved)) = a.down else {
                // A mouse with no button down: what it rests over is drawn
                // hovered (a finger never hovers).
                if e.pointer_type() == "mouse" {
                    a.hover(Some((e.offset_x() as f32, e.offset_y() as f32)));
                }
                return;
            };
            let (x, y) = (e.offset_x() as f32, e.offset_y() as f32);
            let moved = moved || (x - x0).hypot(y - y0) > TAP_SLOP;
            a.tracker.add(event_secs(&e), y);
            if moved && e.pointer_type() != "mouse" {
                // A finger drags the content: it moves with it.
                a.event(Event::Scroll {
                    x: x0,
                    y: y0,
                    dy: ly - y,
                });
            }
            a.down = Some((x0, y0, x, y, at, moved));
        }
    });
    listen(&canvas, "pointerup", {
        let app = app.clone();
        move |e: PointerEvent| {
            let Some(mut a) = lease(&app) else {
                return;
            };
            let Some((x, y, _, _, at, moved)) = a.down.take() else {
                return;
            };
            if !moved {
                if at.elapsed() >= LONG_PRESS {
                    a.event(Event::LongPress { x, y });
                } else {
                    a.event(Event::Tap { x, y });
                }
            } else if e.pointer_type() != "mouse" {
                // The content goes on at the finger's speed and slows down
                // (the finger moving up scrolls forward).
                let v = -a.tracker.velocity(event_secs(&e));
                if let Some(f) = Fling::new(v) {
                    a.fling = Some((Flinging::new(f, secs()), x, y));
                    wake();
                }
            }
        }
    });
    // A phone follows a tap with mouse events, and a mousedown on the canvas
    // (which takes no focus) moves focus to the page: the hidden input the
    // tap just focused blurs and the keyboard closes as it opens. The app
    // says where focus goes (`sync_input`), so the mousedown moves none.
    listen(&canvas, "mousedown", |e: web_sys::Event| {
        e.prevent_default()
    });
    listen(&canvas, "pointerleave", {
        let app = app.clone();
        move |_: PointerEvent| {
            if let Some(mut a) = lease(&app) {
                a.hover(None);
            }
        }
    });
    // The browser took the touch over (or it left): no tap, no fling.
    listen(&canvas, "pointercancel", {
        let app = app.clone();
        move |_: PointerEvent| {
            if let Some(mut a) = lease(&app) {
                a.down = None;
            }
        }
    });
    listen(&canvas, "wheel", {
        let app = app.clone();
        move |e: WheelEvent| {
            e.prevent_default();
            let (x, y) = (e.offset_x() as f32, e.offset_y() as f32);
            // Lines (Firefox) to points.
            let k = if e.delta_mode() == 1 { 40.0 } else { 1.0 };
            let Some(mut a) = lease(&app) else {
                return;
            };
            if e.delta_x().abs() > e.delta_y().abs() {
                a.event(Event::ScrollSideways {
                    x,
                    y,
                    dx: e.delta_x() as f32 * k,
                });
            } else {
                a.event(Event::Scroll {
                    x,
                    y,
                    dy: e.delta_y() as f32 * k,
                });
            }
        }
    });
    listen(&input, "input", {
        let app = app.clone();
        let input = input.clone();
        move |_: web_sys::Event| {
            let text = input.value();
            input.set_value("");
            if !text.is_empty()
                && let Some(mut a) = lease(&app)
            {
                a.event(Event::Text(text));
            }
        }
    });
    // The browser's Back (a phone's back gesture too) closes what is open
    // instead of leaving the page; with nothing open it leaves.
    listen(&window(), "popstate", {
        let app = app.clone();
        move |_: web_sys::Event| {
            if let Some(mut a) = lease(&app) {
                a.went_to(history_at());
            }
        }
    });
    listen(&window(), "keydown", {
        let app = app.clone();
        move |e: KeyboardEvent| {
            let Some(mut a) = lease(&app) else {
                return;
            };
            let typing = a.controller.focused().is_some();
            let shift = e.shift_key();
            let ev = match e.key().as_str() {
                "Escape" => Some(Event::Back),
                "Backspace" if typing => Some(Event::Backspace),
                "Delete" if typing => Some(Event::Edit(EditKey::Delete)),
                "ArrowLeft" if typing => Some(Event::Edit(EditKey::Move(
                    construct_render::interact::CursorMove::Left,
                    shift,
                ))),
                "ArrowRight" if typing => Some(Event::Edit(EditKey::Move(
                    construct_render::interact::CursorMove::Right,
                    shift,
                ))),
                "a" if typing && (e.ctrl_key() || e.meta_key()) => {
                    Some(Event::Edit(EditKey::SelectAll))
                }
                "Enter" if typing => Some(Event::Text("\n".into())),
                _ => None,
            };
            if let Some(ev) = ev {
                e.prevent_default();
                a.event(ev);
            }
        }
    });

    // The frame loop: draw when something changed.
    let tick: Rc<RefCell<Option<FrameCallback>>> = Rc::default();
    let again = tick.clone();
    *tick.borrow_mut() = Some(Closure::new(move || {
        // The next frame is asked for first: a frame that can't draw (or
        // fails) must not end the loop, or the page freezes.
        if let Some(cb) = again.borrow().as_ref() {
            let _ = window().request_animation_frame(cb.as_ref().unchecked_ref());
        }
        // Busy: the browser ran this frame from inside a handler (Chrome
        // draws a frame inside `window.open` as the new tab opens). The
        // handler wakes the next one.
        if let Some(mut a) = lease(&app) {
            a.frame();
        }
    }));
    if let Some(cb) = tick.borrow().as_ref() {
        window().request_animation_frame(cb.as_ref().unchecked_ref())?;
    }
    Ok(())
}

/// The current history entry's place: what this app pushed it with, 0 for
/// the page's own.
fn history_at() -> usize {
    window()
        .history()
        .ok()
        .and_then(|h| h.state().ok())
        .and_then(|v| v.as_f64())
        .map_or(0, |k| k.max(0.0) as usize)
}

/// Adds an event listener that lives as long as the page.
fn listen<E: JsCast + wasm_bindgen::convert::FromWasmAbi + 'static>(
    target: &web_sys::EventTarget,
    name: &str,
    f: impl FnMut(E) + 'static,
) {
    let cb = Closure::<dyn FnMut(E)>::new(f);
    let _ = target.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
    cb.forget();
}
