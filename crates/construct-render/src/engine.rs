//! A live session over a pod run by the engine (`client-core`, feature
//! `remote`), as the Compose clients run theirs: home and every layout it
//! opens come from the engine's cache at once and are fetched again in the
//! background; writes go to its outbox, which keeps them until the pod has
//! them (offline too); and its event stream brings what other devices and
//! agents change while the layout is open.
//!
//! [`PodEngine`] is the session's [`Loader`]: every snapshot the engine
//! sends becomes [`Changes`] the session applies on its next frame (the
//! window is woken through the session's image store). The device data the
//! shared UI builds (`DeviceData`: the layouts list, the inbox, channels,
//! sign-ins and versions) is built from the engine's calls with the same
//! builders as [`crate::remote::PodClient`]; the layouts list is kept in a
//! file next to the engine's cache, so it shows offline too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use client_core::{
    ConstructEngine, EngineConfig, EngineOptions, RedirectMode, SecureStore, StateSnapshot,
};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};

use crate::images::ImageStore;
use crate::interact::{Changes, Loaded, Loader, Session};
use crate::layout::Viewport;
use crate::remote;
use crate::signin::SignIn;

/// The layout clients open first, and the engine's own workspace.
pub const HOME: &str = "home";
/// How long opening a layout or a data path the engine has never had waits
/// for the pod.
const LOAD_WAIT: Duration = Duration::from_secs(20);
/// How long a sign-in (`action.auth.session`) waits for the pod.
const SIGN_IN_WAIT: Duration = Duration::from_secs(25);
/// Notifications listed for the inbox.
const INBOX_LIMIT: u32 = 50;
/// The layouts list kept for offline starts, next to `cache.db`.
const APPS_FILE: &str = "apps.json";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A pod's engine and what a session over it needs. Cheap to clone: every
/// clone shares the engine.
#[derive(Clone)]
pub struct PodEngine {
    engine: Arc<ConstructEngine>,
    base: String,
    store: Arc<dyn SecureStore>,
    shared: Arc<Shared>,
    /// Upstream sign-in runs here (the platform gave a [`SignIn`]).
    signs_in: bool,
}

/// What the engine's observer and the session share.
#[derive(Default)]
struct Shared {
    /// What came in since the session last asked.
    news: Mutex<Option<Changes>>,
    /// What the last snapshot showed, to tell what changed.
    seen: Mutex<Seen>,
    /// The session's image store: waking it draws the next frame.
    images: Mutex<Option<ImageStore>>,
    /// Layout titles by id, for device data rows.
    titles: Mutex<HashMap<String, String>>,
    /// Versions picked on this device (`pin_version`), by layout.
    pins: Mutex<HashMap<String, i64>>,
    /// The upstreams to sign in to (`local:providers`), by id: their
    /// `action.auth.login` and whether they take another server.
    providers: Mutex<HashMap<String, Value>>,
    /// The layouts opened (their JSON, by id), for the device data they
    /// bind.
    needs: Mutex<HashMap<String, String>>,
    apps_file: Option<PathBuf>,
    /// The device data is being loaded again; `again`: once more after.
    loading: AtomicBool,
    again: AtomicBool,
}

#[derive(Default)]
struct Seen {
    workspace: String,
    layouts: HashMap<String, String>,
    notifications: String,
    auth: String,
}

impl Shared {
    /// Every open layout's JSON, to look for what they bind.
    fn needs(&self) -> String {
        lock(&self.needs).values().map(String::as_str).collect()
    }

    /// Notes a layout's JSON; true when it binds device data the others
    /// didn't.
    fn note(&self, id: &str, json: String) -> bool {
        let kinds = |n: &str| {
            (
                remote::needs_notifications(n),
                remote::needs_channels(n),
                remote::needs_sign_ins(n),
            )
        };
        let before = kinds(&self.needs());
        lock(&self.needs).insert(id.to_owned(), json);
        before != kinds(&self.needs())
    }

    fn push(&self, c: Changes) {
        {
            let mut news = lock(&self.news);
            match news.as_mut() {
                Some(n) => {
                    n.data.entities.extend(c.data.entities);
                    n.data.queries.extend(c.data.queries);
                    for (id, root) in c.layouts {
                        n.layouts.retain(|(i, _)| *i != id);
                        n.layouts.push((id, root));
                    }
                }
                None => *news = Some(c),
            }
        }
        if let Some(images) = lock(&self.images).as_ref() {
            images.wake();
        }
    }
}

/// A snapshot's entities and queries.
fn bundle(snap: &StateSnapshot) -> DataBundle {
    DataBundle {
        entities: serde_json::from_str(&snap.entities_json).unwrap_or_default(),
        queries: serde_json::from_str(&snap.queries_json).unwrap_or_default(),
    }
}

/// `local://connection` as the shared UI builds it.
fn connection(snap: &StateSnapshot) -> Value {
    let online = snap.online_known.then_some(snap.is_online);
    let pending = snap.pending_actions;
    let mut label = match online {
        Some(true) => "Online",
        Some(false) => "Offline",
        None => "Connecting…",
    }
    .to_owned();
    if pending > 0 {
        label.push_str(&format!(" · {pending} pending"));
    }
    json!({
        "online": online == Some(true), "pending": pending, "label": label,
        "attention": online == Some(false) || pending > 0,
        "color": if online == Some(true) { "#2E7D32" } else { "#9E9E9E" },
    })
}

/// `local://session` as the shared UI builds it, from the engine's auth state.
fn session_entity(snap: &StateSnapshot, base: &str) -> Value {
    let auth: Value = serde_json::from_str(&snap.auth_json).unwrap_or_default();
    let text = |k: &str| auth[k].as_str().unwrap_or("").to_owned();
    let status = match auth["state"].as_str() {
        Some("pending") => auth["message"].as_str().unwrap_or("Signing in…").to_owned(),
        Some("error") => text("error"),
        _ => String::new(),
    };
    json!({ "open": auth["session"] == true, "server": base, "error": text("error"),
        "message": text("message"), "status": status })
}

/// Whether the engine has a pod session.
fn signed_in(snap: &StateSnapshot) -> bool {
    serde_json::from_str::<Value>(&snap.auth_json).is_ok_and(|a| a["session"] == true)
}

/// The engine's unread count.
fn unread(snap: &StateSnapshot) -> i64 {
    serde_json::from_str::<Value>(&snap.notifications_json)
        .ok()
        .and_then(|v| v["unread_total"].as_i64())
        .unwrap_or(0)
}

/// What a page shows in place of home until the engine has it (a first
/// start, before the pod answers): the connection's state.
fn opening_page(base: &str) -> UiNode {
    serde_json::from_value(json!({
        "type": "core://Page", "id": "opening", "root": { "type": "core://Column",
        "padding_h_dp": 16, "padding_v_dp": 16, "spacing_dp": 12, "children": [
            { "type": "core://Text", "value_expr": "Opening your pod…", "style": "Title" },
            { "type": "core://Text", "value_expr": base, "style": "Body" },
            { "type": "core://Text", "value_expr": "${local://connection#label}", "style": "Caption" },
            { "type": "core://Button", "label": "Sign in with another code",
              "on_click": { "intent": "action.device", "command": "leave_pod" } }
        ] } }))
    .expect("a valid page")
}

/// Where a pod's engine keeps its cache under `dir`: a folder per server.
pub fn storage_for(dir: &Path, base: &str) -> PathBuf {
    let name: String = base
        .split("://")
        .last()
        .unwrap_or(base)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(name)
}

impl PodEngine {
    /// The engine of the pod at `base`, its cache under `storage` (in memory
    /// when `None`) and its session in `store`.
    pub fn open(base: &str, storage: Option<&Path>, store: Arc<dyn SecureStore>) -> PodEngine {
        PodEngine::open_with(base, storage, store, EngineConfig::default())
    }

    /// [`PodEngine::open`] with the engine's timing (tests shorten it).
    pub fn open_with(
        base: &str,
        storage: Option<&Path>,
        store: Arc<dyn SecureStore>,
        config: EngineConfig,
    ) -> PodEngine {
        PodEngine::open_signing_in(base, storage, store, config, None)
    }

    /// [`PodEngine::open_with`] that signs in to upstreams the platform's
    /// way ([`SignIn`]: its browser and redirect); without one, a sign-in
    /// isn't taken.
    pub fn open_signing_in(
        base: &str,
        storage: Option<&Path>,
        store: Arc<dyn SecureStore>,
        config: EngineConfig,
        sign_in: Option<&SignIn>,
    ) -> PodEngine {
        let base = base.trim_end_matches('/').to_owned();
        let dir = storage.map(|d| storage_for(d, &base));
        if let Some(d) = &dir
            && let Err(e) = std::fs::create_dir_all(d)
        {
            eprintln!("construct-render: {}: {e}", d.display());
        }
        let path = dir
            .as_ref()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        let engine = Arc::new(ConstructEngine::init_with_options(
            &path,
            &base,
            HOME,
            EngineOptions {
                config,
                secure_store: Some(store.clone()),
                url_opener: sign_in.map(|s| s.opener.clone()),
                redirect: sign_in
                    .map(|s| (s.redirect)(&base))
                    .unwrap_or(RedirectMode::Loopback),
                ..EngineOptions::default()
            },
        ));
        let platform = if cfg!(target_os = "android") {
            "android"
        } else {
            "desktop"
        };
        engine.set_device_info(platform, "The Construct (Rust)");
        let shared = Arc::new(Shared {
            apps_file: dir.map(|d| d.join(APPS_FILE)),
            ..Shared::default()
        });
        let pod = PodEngine {
            engine,
            base,
            store,
            shared,
            signs_in: sign_in.is_some(),
        };
        // The observer holds the engine weakly: the engine owns it.
        let weak = Arc::downgrade(&pod.engine);
        let (shared, base) = (pod.shared.clone(), pod.base.clone());
        pod.engine.subscribe(move |snap| {
            observe(&weak, &shared, &base, &snap);
        });
        pod
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Whether the engine has a pod session (else home is the pod's
    /// sign-in screen).
    pub fn signed_in(&self) -> bool {
        signed_in(&self.engine.snapshot())
    }

    /// Leaves the pod on this device: the engine forgets its session and
    /// shows the pod's sign-in screen once the pod answers.
    pub fn forget_session(&self) {
        self.engine.forget_session();
    }

    /// Fetches home again in the background (Try again).
    pub fn refresh(&self) {
        let engine = self.engine.clone();
        std::thread::spawn(move || engine.refresh());
        self.reload_device();
    }

    /// The session token the engine keeps (after a sign-in).
    fn token(&self) -> Option<String> {
        let raw = self
            .store
            .get(&client_core::auth::session_key(&self.base))?;
        match serde_json::from_str::<Value>(&raw) {
            Ok(v) => v["token"].as_str().map(str::to_owned),
            Err(_) => Some(raw),
        }
    }

    /// A live session on home: as the engine has it now (from its cache
    /// offline, the pod's sign-in screen without a session), then whatever
    /// it hears of.
    pub fn session(&self, viewport: Viewport) -> Session {
        // What came in before is in the snapshot too.
        lock(&self.shared.news).take();
        let snap = self.engine.snapshot();
        let root = serde_json::from_str::<UiNode>(&snap.workspace_json)
            .unwrap_or_else(|_| opening_page(&self.base));
        let mut data = bundle(&snap);
        let apps = self.cached_apps();
        merge(&mut data, apps);
        merge(&mut data, self.device_now(&snap));
        let mut s = Session::new(HOME, root, data, viewport);
        {
            // Under the observer's lock: home may have come in since the
            // snapshot, and its newer JSON stays.
            let seen = lock(&self.shared.seen);
            if seen.workspace.is_empty() {
                self.shared.note(HOME, snap.workspace_json.clone());
            }
        }
        *lock(&self.shared.images) = Some(s.images.clone());
        s.loader = Some(Box::new(self.clone()));
        remote::fetch_images(&s.images);
        self.reload_device();
        s
    }

    /// The device data a snapshot gives at once: connection, session,
    /// device, servers and the inbox's count.
    fn device_now(&self, snap: &StateSnapshot) -> DataBundle {
        let mut out = remote::base_device(
            &self.base,
            connection(snap),
            session_entity(snap, &self.base),
        );
        out.entities
            .insert("local://inbox".into(), remote::inbox_entity(unread(snap)));
        out
    }

    /// The layouts list kept from the last time the pod gave it.
    fn cached_apps(&self) -> DataBundle {
        let Some(v) = self
            .shared
            .apps_file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            return DataBundle::default();
        };
        remote::layouts_bundle(&v, &mut lock(&self.shared.titles))
    }

    /// Loads the device data again in the background: the layouts list,
    /// and the notifications, channels and sign-ins the open layouts bind.
    fn reload_device(&self) {
        reload_device(Arc::downgrade(&self.engine), self.shared.clone());
    }

    /// Notes a layout's JSON for the device data it binds.
    fn needs(&self, id: &str, root: &UiNode) {
        if self
            .shared
            .note(id, serde_json::to_string(root).unwrap_or_default())
        {
            self.reload_device();
        }
    }

    /// Waits until `done` answers for a snapshot, or `wait` passes.
    fn wait_for<T>(
        &self,
        wait: Duration,
        mut done: impl FnMut(&StateSnapshot) -> Option<T>,
    ) -> Option<T> {
        let until = Instant::now() + wait;
        loop {
            if let Some(t) = done(&self.engine.snapshot()) {
                return Some(t);
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn versions(&self, app: &str) -> Result<DataBundle, String> {
        let v: Value = self
            .engine
            .app_versions(app, None, Some(100))
            .map_err(|e| e.reason().to_owned())
            .and_then(|b| serde_json::from_str(&b).map_err(|e| e.to_string()))?;
        let pinned = lock(&self.shared.pins).get(app).copied();
        let title = lock(&self.shared.titles)
            .get(app)
            .cloned()
            .unwrap_or_else(|| app.to_owned());
        Ok(remote::versions_bundle(
            app,
            &v,
            pinned,
            &title,
            remote::now_secs(),
        ))
    }
}

fn merge(into: &mut DataBundle, from: DataBundle) {
    into.entities.extend(from.entities);
    into.queries.extend(from.queries);
}

/// A snapshot from the engine (on one of its threads): what changed goes to
/// the session.
fn observe(engine: &Weak<ConstructEngine>, shared: &Arc<Shared>, base: &str, snap: &StateSnapshot) {
    let mut c = Changes {
        data: bundle(snap),
        layouts: Vec::new(),
    };
    let mut reload = false;
    {
        let mut seen = lock(&shared.seen);
        if snap.workspace_json != seen.workspace {
            seen.workspace = snap.workspace_json.clone();
            if let Ok(root) = serde_json::from_str::<UiNode>(&snap.workspace_json) {
                c.layouts.push((HOME.to_owned(), root));
                shared.note(HOME, snap.workspace_json.clone());
                reload = true;
            }
        }
        let layouts: HashMap<String, Value> =
            serde_json::from_str(&snap.layouts_json).unwrap_or_default();
        for (id, l) in layouts {
            let text = l["workspace"].to_string();
            if l["workspace"].is_null() || seen.layouts.get(&id) == Some(&text) {
                continue;
            }
            if let Ok(root) = serde_json::from_value::<UiNode>(l["workspace"].clone()) {
                c.layouts.push((id.clone(), root));
                shared.note(&id, text.clone());
                reload = true;
            }
            seen.layouts.insert(id, text);
        }
        if snap.notifications_json != seen.notifications {
            seen.notifications = snap.notifications_json.clone();
            reload = true;
        }
        if snap.auth_json != seen.auth {
            seen.auth = snap.auth_json.clone();
            reload = true;
        }
    }
    let mut device = remote::base_device(base, connection(snap), session_entity(snap, base));
    device.queries.clear();
    merge(&mut c.data, device);
    // The count at once; the list follows.
    c.data
        .entities
        .insert("local://inbox".into(), remote::inbox_entity(unread(snap)));
    shared.push(c);
    if reload {
        reload_device(engine.clone(), shared.clone());
    }
}

/// The layouts list, and what the open layouts bind of the notifications,
/// channels and sign-ins, from the engine on a background thread (one at a
/// time; asked again while it runs, it runs once more after).
fn reload_device(engine: Weak<ConstructEngine>, shared: Arc<Shared>) {
    if shared.loading.swap(true, Ordering::SeqCst) {
        shared.again.store(true, Ordering::SeqCst);
        return;
    }
    std::thread::spawn(move || {
        loop {
            shared.again.store(false, Ordering::SeqCst);
            let Some(pod) = engine.upgrade() else {
                break;
            };
            let data = device_data(&pod, &shared);
            drop(pod);
            shared.push(Changes {
                data,
                layouts: Vec::new(),
            });
            if shared.again.load(Ordering::SeqCst) {
                continue;
            }
            shared.loading.store(false, Ordering::SeqCst);
            // Asked between the check and now, and no other thread took it.
            if !shared.again.load(Ordering::SeqCst) || shared.loading.swap(true, Ordering::SeqCst) {
                break;
            }
        }
    });
}

fn device_data(engine: &ConstructEngine, shared: &Shared) -> DataBundle {
    let mut out = DataBundle::default();
    match engine.list_apps() {
        Ok(body) => {
            // Whole or not at all: a new start may read it meanwhile.
            let tmp = shared.apps_file.as_ref().map(|f| f.with_extension("tmp"));
            if let (Some(f), Some(tmp)) = (&shared.apps_file, tmp)
                && let Err(e) = std::fs::write(&tmp, &body).and_then(|()| std::fs::rename(&tmp, f))
            {
                eprintln!("construct-render: {}: {e}", f.display());
            }
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                merge(
                    &mut out,
                    remote::layouts_bundle(&v, &mut lock(&shared.titles)),
                );
            }
        }
        // Offline: the list kept from the last time stays.
        Err(e) => eprintln!("construct-render: layouts: {e}"),
    }
    let needs = shared.needs();
    let titles = lock(&shared.titles).clone();
    if remote::needs_notifications(&needs) {
        // From the pod, else the engine's cache.
        match engine.list_notifications(None, INBOX_LIMIT, None) {
            Ok(list) => {
                let v = serde_json::to_value(&list).unwrap_or_default();
                merge(
                    &mut out,
                    remote::notifications_bundle(&v, &titles, remote::now_secs()),
                );
            }
            Err(e) => eprintln!("construct-render: notifications: {e}"),
        }
    }
    if remote::needs_channels(&needs)
        && let Ok(v) = engine
            .notification_channels()
            .map(|b| serde_json::from_str::<Value>(&b).unwrap_or_default())
    {
        merge(&mut out, remote::channels_bundle(&v, &titles));
    }
    if remote::needs_sign_ins(&needs)
        && let Ok(v) = engine
            .sign_ins()
            .map(|b| serde_json::from_str::<Value>(&b).unwrap_or_default())
    {
        *lock(&shared.providers) = v["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p["id"].as_str()?.to_owned(), p.clone())))
            .collect();
        merge(&mut out, remote::sign_ins_bundle(&v));
    }
    out
}

impl Loader for PodEngine {
    /// The engine's copy at once when it has one (its fetch goes on in the
    /// background and comes in through [`Loader::changes`]); else waits for
    /// the pod.
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        let key = format!("{}{id}", client_core::LAYOUT_LOAD_PREFIX);
        if id != HOME {
            self.engine.open_layout(id);
        }
        let got = self.wait_for(LOAD_WAIT, |snap| {
            if id == HOME {
                return serde_json::from_str::<UiNode>(&snap.workspace_json)
                    .ok()
                    .map(|r| Ok((r, bundle(snap))));
            }
            let layouts: Value = serde_json::from_str(&snap.layouts_json).unwrap_or_default();
            let l = &layouts[id];
            if let Ok(root) = serde_json::from_value::<UiNode>(l["workspace"].clone()) {
                return Some(Ok((root, bundle(snap))));
            }
            if l["gone"] == true {
                return Some(Err(client_core::LOAD_NOT_FOUND.to_owned()));
            }
            let loads: Value = serde_json::from_str(&snap.loads_json).unwrap_or_default();
            (loads[&key]["state"] == "error").then(|| {
                Err(loads[&key]["error"]
                    .as_str()
                    .unwrap_or(client_core::LOAD_OFFLINE)
                    .to_owned())
            })
        });
        let (root, mut data) = got.unwrap_or_else(|| Err(client_core::LOAD_OFFLINE.to_owned()))?;
        self.needs(id, &root);
        merge(&mut data, self.device_now(&self.engine.snapshot()));
        Ok((root, data))
    }

    /// `action.data.load` through the engine (its answer is cached and
    /// shown offline); waits for it.
    fn data(&self, path: &str) -> Result<DataBundle, String> {
        self.engine
            .dispatch(&json!({ "intent": "action.data.load", "path": path }).to_string());
        let got = self.wait_for(LOAD_WAIT, |snap| {
            let loads: Value = serde_json::from_str(&snap.loads_json).unwrap_or_default();
            match loads[path]["state"].as_str() {
                Some("ready") => Some(Ok(bundle(snap))),
                Some("error") => Some(Err(loads[path]["error"]
                    .as_str()
                    .unwrap_or(client_core::LOAD_OFFLINE)
                    .to_owned())),
                _ => None,
            }
        });
        got.unwrap_or_else(|| Err(client_core::LOAD_OFFLINE.to_owned()))
    }

    fn fetch(&self, path: &str, done: Loaded) -> bool {
        let pod = self.clone();
        let path = path.to_owned();
        std::thread::spawn(move || done(Loader::data(&pod, &path)));
        true
    }

    fn device(&self, command: &str, app: &str, id: &str) -> Option<Result<DataBundle, String>> {
        match command {
            "show_versions" => Some(self.versions(app)),
            "pin_version" => {
                let n = id.parse::<i64>().ok()?;
                lock(&self.shared.pins).insert(app.to_owned(), n);
                Some(self.versions(app))
            }
            "follow_latest" => {
                lock(&self.shared.pins).remove(app);
                Some(self.versions(app))
            }
            _ => None,
        }
    }

    /// The engine opens the session (it keeps it in its secure store and
    /// loads home with it); answers its token once it has it.
    fn open_session(&self, path: &str, payload: &Value) -> Option<Result<String, String>> {
        let intent = json!({ "intent": "action.auth.session", "path": path, "payload": payload });
        self.engine.dispatch(&intent.to_string());
        let auth = |snap: &StateSnapshot| -> Value {
            serde_json::from_str(&snap.auth_json).unwrap_or_default()
        };
        if auth(&self.engine.snapshot())["state"] != "pending" {
            return Some(Err("That isn't a pod address.".to_owned()));
        }
        let answer = self.wait_for(SIGN_IN_WAIT, |snap| {
            let a = auth(snap);
            match a["state"].as_str() {
                Some("pending") => None,
                Some("error") => Some(Err(a["error"]
                    .as_str()
                    .unwrap_or("That pod code didn't open a pod.")
                    .to_owned())),
                _ if a["session"] == true => Some(Ok(())),
                _ => Some(Err("That pod code didn't open a pod.".to_owned())),
            }
        });
        Some(match answer {
            Some(Ok(())) => self
                .token()
                .ok_or_else(|| "the pod answered no session".to_owned()),
            Some(Err(e)) => Err(e),
            None => Err(client_core::LOAD_OFFLINE.to_owned()),
        })
    }

    /// Mutations and creates go to the engine's outbox; notifications are
    /// marked read by the engine (sent again when the pod is back).
    fn take(&self, intent: &Value) -> bool {
        match intent["intent"].as_str() {
            Some("action.mutation.optimistic" | "action.mutation.create") => {
                self.engine.dispatch(&intent.to_string());
                true
            }
            Some("action.device") => {
                let ids: Option<Vec<String>> = match intent["command"].as_str() {
                    Some("mark_read") => match intent["id"].as_str() {
                        Some(id) => Some(vec![id.to_owned()]),
                        None => return false,
                    },
                    Some("mark_all_read") => None,
                    _ => return false,
                };
                let pod = self.clone();
                std::thread::spawn(move || {
                    let r = match &ids {
                        Some(ids) => pod.engine.mark_notifications_read(ids),
                        None => pod.engine.mark_all_notifications_read(None),
                    };
                    if let Err(e) = r {
                        eprintln!("construct-render: marking read: {e}");
                    }
                    pod.reload_device();
                });
                true
            }
            _ => false,
        }
    }

    fn changes(&self) -> Option<Changes> {
        lock(&self.shared.news).take()
    }

    /// The engine's sign-in (D19): `action.auth.login` registers the
    /// client with the pod and opens the browser; `sign_in` starts a
    /// provider's (`local:providers`), on another server when it takes one
    /// and the layout passes it (`value`); `sign_out` signs out through
    /// the pod. `set_server`: this device has only its own pod to pick.
    fn auth(&self, intent: &Value) -> bool {
        if !self.signs_in {
            return false;
        }
        let login = match (intent["intent"].as_str(), intent["command"].as_str()) {
            (Some("action.auth.login"), _) => intent.clone(),
            (Some("action.device"), Some("sign_in")) => {
                let id = intent["id"].as_str().unwrap_or("");
                let Some(p) = lock(&self.shared.providers).get(id).cloned() else {
                    eprintln!("construct-render: sign_in: no provider {id:?}");
                    return false;
                };
                let mut start = p["intent"].clone();
                if start["intent"] != "action.auth.login" {
                    return false;
                }
                let server = intent["value"].as_str().unwrap_or("").trim();
                if !server.is_empty() && !p["server_placeholder"].is_null() {
                    start["instance_url"] = json!(server);
                }
                start
            }
            (Some("action.device"), Some("sign_out")) => {
                self.engine.logout();
                self.reload_device();
                return true;
            }
            (Some("action.device"), Some("set_server")) => {
                let id = intent["id"].as_str().unwrap_or("");
                return id == "current" || id == self.base;
            }
            _ => return false,
        };
        self.engine.dispatch(&login.to_string());
        true
    }

    fn redirect(&self, url: &str) -> bool {
        self.engine.handle_auth_redirect(url)
    }
}

/// A [`SecureStore`] in a file only this user can read (desktops without a
/// keychain): a JSON object of keys and values.
pub struct FileStore {
    file: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    pub fn new(file: PathBuf) -> FileStore {
        FileStore {
            file,
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> serde_json::Map<String, Value> {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn write(&self, map: &serde_json::Map<String, Value>) {
        if let Some(d) = self.file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let text = Value::Object(map.clone()).to_string();
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let result = opts
            .open(&self.file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        if let Err(e) = result {
            eprintln!("construct-render: {}: {e}", self.file.display());
        }
    }
}

impl SecureStore for FileStore {
    fn get(&self, key: &str) -> Option<String> {
        let _g = lock(&self.lock);
        self.read().get(key)?.as_str().map(str::to_owned)
    }

    fn set(&self, key: &str, value: &str) {
        let _g = lock(&self.lock);
        let mut map = self.read();
        map.insert(key.to_owned(), json!(value));
        self.write(&map);
    }

    fn delete(&self, key: &str) {
        let _g = lock(&self.lock);
        let mut map = self.read();
        if map.remove(key).is_some() {
            self.write(&map);
        }
    }
}

/// The desktop's secure store: the login Keychain on macOS, else a private
/// file in `dir` (`secure.json`), else memory.
pub fn desktop_store(dir: Option<&Path>) -> Arc<dyn SecureStore> {
    if cfg!(target_os = "macos") {
        return Arc::from(client_core::auth::default_secure_store());
    }
    match dir {
        Some(d) => Arc::new(FileStore::new(d.join("secure.json"))),
        None => Arc::new(client_core::MemoryStore::new()),
    }
}
