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
use construct_render::interact::{Changes, Done, Loaded, Loader, Session, Write, Written};
use construct_render::layout::Viewport;
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

/// The frame must be drawn again (something arrived).
static DIRTY: AtomicBool = AtomicBool::new(true);

fn wake() {
    DIRTY.store(true, Ordering::Relaxed);
}

fn window() -> web_sys::Window {
    web_sys::window().expect("a window")
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
        device_data::device(&base, needs, &titles, |p| {
            self.device
                .get(p)
                .cloned()
                .ok_or_else(|| "not loaded".to_owned())
        })
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

async fn image_bytes(path: &str) -> Option<Vec<u8>> {
    let resp: Response = JsFuture::from(window().fetch_with_str(path))
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    if !resp.ok() {
        return None;
    }
    let buf = JsFuture::from(resp.array_buffer().ok()?).await.ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Fetches the fonts by name (`/web/fonts/<file>`): the faces before the
/// first frame, the fallbacks after it (decision `web-client`).
async fn load_fonts(files: &[&str]) {
    for f in files {
        if let Some(b) = image_bytes(&format!("/web/fonts/{f}")).await {
            construct_render::text::add_font(b);
        } else {
            log(&format!("couldn't load the font {f}"));
        }
    }
}

/// The app on a page: the canvas, a hidden text input for typing, and the
/// session drawn into the canvas.
struct App {
    controller: Controller,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    input: HtmlInputElement,
    drawn: u64,
    /// Where the primary pointer went down, and when; a drag that moves
    /// scrolls, one that doesn't is a tap.
    down: Option<(f32, f32, f32, f32, Instant, bool)>,
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
        self.controller.handle(Event::Resized {
            width_px: (vp.width * vp.scale) as u32,
            height_px: (vp.height * vp.scale) as u32,
            scale: vp.scale,
        });
        self.drawn = u64::MAX;
    }

    fn event(&mut self, e: Event) {
        let out = self.controller.handle(e);
        // A link opens in a new tab, not over the pod.
        for effect in out.iter().flat_map(|o| &o.effects) {
            if let construct_render::interact::Effect::OpenLink { url } = effect
                && (url.starts_with("https://") || url.starts_with("http://"))
            {
                let _ = window().open_with_url_and_target_and_features(url, "_blank", "noopener");
            }
        }
        self.sync_input();
        wake();
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
    fn frame(&mut self) {
        let due = self
            .controller
            .session
            .next_wake()
            .is_some_and(|t| t <= Instant::now());
        // A slide, a spinner or a settling card draws every frame until done.
        let animating = self.controller.session.animating();
        let dirty =
            DIRTY.swap(false, Ordering::Relaxed) || self.controller.dirty || due || animating;
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
                    let _ = self.ctx.put_image_data(&d, 0.0, 0.0);
                }
            }
            Err(e) => log(&format!("drawing: {e}")),
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
    mount(session)
}

/// A public link's page (decision `public-web-links`): its copy, drawn
/// read-only with no pod behind it (nothing it draws can write).
pub async fn start_public(link: &str) -> Result<(), JsValue> {
    load_fonts(&construct_render::text::FACE_FILES).await;
    let base = format!("/s/{}", crate::construct::encode(link));
    let copy = get_json(&format!("{base}/view"))
        .await
        .map_err(|_| JsValue::from_str("This page isn't shared any more."))?;
    let root: UiNode = serde_json::from_value(copy["node"].clone())
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let data: DataBundle = serde_json::from_value(copy["data"].clone()).unwrap_or_default();
    let mut session = Session::new("public", root, data, Viewport::PHONE);
    if let Some(item) = copy["item"].as_str() {
        session.set_root(Default::default(), Some(item.to_owned()));
    }
    fetch_images(&session.images, &format!("{base}/image"));
    mount(session)
}

/// Puts a session on the page's canvas and runs it.
fn mount(session: Session) -> Result<(), JsValue> {
    session.images.set_waker(wake);
    let doc = window().document().ok_or("no document")?;
    let canvas: HtmlCanvasElement = doc
        .get_element_by_id("construct")
        .ok_or("no canvas")?
        .dyn_into()?;
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or("no 2d context")?
        .dyn_into()?;
    let input: HtmlInputElement = doc
        .get_element_by_id("construct-input")
        .ok_or("no input")?
        .dyn_into()?;

    let app = Rc::new(RefCell::new(App {
        controller: Controller::new(session),
        canvas: canvas.clone(),
        ctx,
        input: input.clone(),
        drawn: u64::MAX,
        down: None,
    }));
    app.borrow_mut().resize();
    let _ = doc.body().map(|b| b.class_list().add_1("ready"));

    // The fallbacks after the first frame: emoji and symbols appear when
    // they come.
    spawn_local(async {
        load_fonts(&construct_render::text::FALLBACK_FILES).await;
        wake();
    });

    listen(&window(), "resize", {
        let app = app.clone();
        move |_: web_sys::Event| app.borrow_mut().resize()
    });
    listen(&canvas, "pointerdown", {
        let app = app.clone();
        move |e: PointerEvent| {
            if !e.is_primary() {
                return;
            }
            let (x, y) = (e.offset_x() as f32, e.offset_y() as f32);
            app.borrow_mut().down = Some((x, y, x, y, Instant::now(), false));
        }
    });
    listen(&canvas, "pointermove", {
        let app = app.clone();
        move |e: PointerEvent| {
            let mut a = app.borrow_mut();
            let Some((x0, y0, _, ly, at, moved)) = a.down else {
                return;
            };
            let (x, y) = (e.offset_x() as f32, e.offset_y() as f32);
            let moved = moved || (x - x0).hypot(y - y0) > TAP_SLOP;
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
        move |_: PointerEvent| {
            let mut a = app.borrow_mut();
            if let Some((x, y, _, _, at, moved)) = a.down.take()
                && !moved
            {
                if at.elapsed() >= LONG_PRESS {
                    a.event(Event::LongPress { x, y });
                } else {
                    a.event(Event::Tap { x, y });
                }
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
            let mut a = app.borrow_mut();
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
            if !text.is_empty() {
                app.borrow_mut().event(Event::Text(text));
            }
        }
    });
    listen(&window(), "keydown", {
        let app = app.clone();
        move |e: KeyboardEvent| {
            let mut a = app.borrow_mut();
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
        app.borrow_mut().frame();
        if let Some(cb) = again.borrow().as_ref() {
            let _ = window().request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }));
    if let Some(cb) = tick.borrow().as_ref() {
        window().request_animation_frame(cb.as_ref().unchecked_ref())?;
    }
    Ok(())
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
