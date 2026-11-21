//! A live session over a pod run by the engine (`client-core`, feature
//! `remote`): home and every layout it
//! opens come from the engine's cache at once and are fetched again in the
//! background; writes go to its outbox, which keeps them until the pod has
//! them (offline too); and its event stream brings what other devices and
//! agents change while the layout is open.
//!
//! [`PodEngine`] is the session's [`Loader`]: every snapshot the engine
//! sends becomes [`Changes`] the session applies on its next frame (the
//! window is woken through the session's image store). The device data the
//! shared UI builds (`DeviceData`: the inbox, channels and sign-ins) is
//! built from the engine's calls with the same builders as
//! [`crate::remote::PodClient`]; the layouts' titles that name their rows
//! come from the construct's elements the engine has (decision
//! `one-layout-system`).

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

/// A server the device offers (`local:servers`): id, title, URL.
pub type ServerOption = (String, String, String);

/// The layout clients open first, and the engine's own workspace.
pub const HOME: &str = "home";
/// How long opening a layout or a data path the engine has never had waits
/// for the pod.
const LOAD_WAIT: Duration = Duration::from_secs(20);
/// How long a sign-in (`action.auth.session`) waits for the pod.
const SIGN_IN_WAIT: Duration = Duration::from_secs(25);
/// Notifications listed for the inbox.
const INBOX_LIMIT: u32 = 50;

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
    /// The construct's edge graph, when it changed ([`Loader::edges`]).
    edges: Mutex<Option<crate::interact::Edges>>,
    /// What the last snapshot showed, to tell what changed.
    seen: Mutex<Seen>,
    /// The data the session was last sent (its snapshot JSON too), so a
    /// snapshot sends only what changed: comparing the whole cache on the
    /// UI thread cost a phone about 20 ms a snapshot.
    sent: Mutex<Sent>,
    /// The session's image store: waking it draws the next frame.
    images: Mutex<Option<ImageStore>>,
    /// The servers the platform offers (`local:servers`).
    servers: Mutex<Vec<ServerOption>>,
    /// Where the inbox's next older page starts, and the older pages
    /// loaded so far (kept under the newest page when it is listed again).
    older: Mutex<(Option<String>, Vec<String>)>,
    /// The upstreams to sign in to (`local:providers`), by id: their
    /// `action.auth.login` and whether they take another server.
    providers: Mutex<HashMap<String, Value>>,
    /// The layouts opened (their JSON, by id), for the device data they
    /// bind.
    needs: Mutex<HashMap<String, String>>,
    /// The device data is being loaded again; `again`: once more after.
    loading: AtomicBool,
    again: AtomicBool,
}

#[derive(Default)]
struct Sent {
    entities_json: String,
    queries_json: String,
    data: DataBundle,
}

impl Sent {
    /// What `now` changes of what was sent (entities and queries that
    /// differ or are new), and now it is what was sent. Nothing is
    /// removed, as the session's merge removes nothing.
    fn changes(&mut self, now: DataBundle) -> DataBundle {
        let mut out = DataBundle::default();
        for (uri, e) in now.entities {
            if self.data.entities.get(&uri) != Some(&e) {
                out.entities.insert(uri.clone(), e.clone());
                self.data.entities.insert(uri, e);
            }
        }
        for (key, q) in now.queries {
            if self.data.queries.get(&key) != Some(&q) {
                out.queries.insert(key.clone(), q.clone());
                self.data.queries.insert(key, q);
            }
        }
        out
    }

    /// The snapshot's own entities and queries that changed; parsed only
    /// when its JSON did.
    fn snapshot_changes(&mut self, snap: &StateSnapshot) -> DataBundle {
        if snap.entities_json == self.entities_json && snap.queries_json == self.queries_json {
            return DataBundle::default();
        }
        self.entities_json.clone_from(&snap.entities_json);
        self.queries_json.clone_from(&snap.queries_json);
        self.changes(bundle(snap))
    }
}

#[derive(Default)]
struct Seen {
    workspace: String,
    layouts: HashMap<String, String>,
    notifications: String,
    auth: String,
    construct: String,
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

    /// `local://inbox`: the unread count, and whether older notifications
    /// can be loaded (`next_before`).
    fn inbox(&self, unread: i64) -> protocol::EntityRecord {
        let mut e = remote::inbox_entity(unread);
        let next = lock(&self.older).0.clone();
        e.set("has_older", next.is_some());
        e.set("next_before", next.unwrap_or_default());
        e
    }

    fn push(&self, c: Changes) {
        {
            let mut news = lock(&self.news);
            match news.as_mut() {
                Some(n) => n.absorb(c),
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
        let shared = Arc::new(Shared::default());
        let pod = PodEngine {
            engine,
            base,
            store,
            shared,
            signs_in: sign_in.is_some(),
        };
        // The observer holds the engine weakly: the engine owns it. The
        // engine calls it on the thread that changed its state, often the
        // UI thread (a tap's write, a layout opening), so snapshots go to a
        // thread of their own, the newest first, and parsing them never
        // holds a frame up. It ends when the engine drops the sender.
        let weak = Arc::downgrade(&pod.engine);
        let (shared, base) = (pod.shared.clone(), pod.base.clone());
        let (tx, rx) = std::sync::mpsc::channel::<StateSnapshot>();
        let spawned = std::thread::Builder::new()
            .name("construct-observe".into())
            .spawn(move || {
                while let Ok(mut snap) = rx.recv() {
                    // Snapshots are whole: only the newest counts.
                    while let Ok(newer) = rx.try_recv() {
                        snap = newer;
                    }
                    observe(&weak, &shared, &base, &snap);
                }
            });
        if let Err(e) = spawned {
            eprintln!("construct-render: no observer thread: {e}");
        }
        pod.engine.subscribe(move |snap| {
            let _ = tx.send(snap);
        });
        pod
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// The servers this device offers, for `local:servers`.
    pub fn set_servers(&self, servers: Vec<ServerOption>) {
        *lock(&self.shared.servers) = servers;
    }

    /// Whether the engine has a pod session (else home is the pod's
    /// sign-in screen).
    pub fn signed_in(&self) -> bool {
        signed_in(&self.engine.snapshot())
    }

    /// Leaves the pod on this device: the engine forgets its session and
    /// shows the pod's sign-in screen once the pod answers.
    pub fn forget_session(&self) {
        // The inbox starts afresh.
        *lock(&self.shared.older) = (None, Vec::new());
        self.engine.forget_session();
    }

    /// Fetches home again in the background (Try again).
    pub fn refresh(&self) {
        let engine = self.engine.clone();
        std::thread::spawn(move || engine.refresh());
        self.reload_device();
    }

    /// The pod's VAPID key (base64url); an error when it sends no push.
    /// Blocking.
    pub fn push_public_key(&self) -> Result<String, String> {
        self.engine.push_public_key().map_err(|e| e.to_string())
    }

    /// Sends this device's Web Push subscription to the pod (`crate::push`).
    /// Blocking.
    pub fn register_push(&self, endpoint: &str, p256dh: &str, auth: &str) -> Result<(), String> {
        self.engine
            .register_push(endpoint, p256dh, auth)
            .map_err(|e| e.to_string())
    }

    /// Stops Web Push to this device. Blocking.
    pub fn unregister_push(&self) -> Result<(), String> {
        self.engine.unregister_push().map_err(|e| e.to_string())
    }

    /// A decrypted push message: the engine caches the notification and
    /// takes its unread count (the open session's inbox and bell follow).
    pub fn handle_push_message(&self, json: &str) {
        self.engine.handle_push_message(json);
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
        // The session starts from this snapshot: later ones send changes.
        {
            let mut sent = lock(&self.shared.sent);
            *sent = Sent::default();
            sent.snapshot_changes(&snap);
        }
        let root = serde_json::from_str::<UiNode>(&snap.workspace_json)
            .unwrap_or_else(|_| opening_page(&self.base));
        let mut data = bundle(&snap);
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
        let engine = Arc::downgrade(&self.engine);
        let pod = remote::ImagePod {
            base: self.base.clone(),
            bearer: Arc::new(move || engine.upgrade().and_then(|e| e.session_bearer())),
        };
        remote::fetch_images(&s.images, Some(self.engine.attachments_dir()), Some(pod));
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
            .insert("local://inbox".into(), self.shared.inbox(unread(snap)));
        let servers = lock(&self.shared.servers).clone();
        if !servers.is_empty() {
            let mut uris = Vec::new();
            for (id, title, url) in servers {
                let uri = format!("local://servers/{id}");
                if let Ok(e) = serde_json::from_value(json!({ "id": id, "title": title, "url": url,
                    "selected": url.trim_end_matches('/') == self.base }))
                {
                    out.entities.insert(uri.clone(), e);
                }
                uris.push(uri);
            }
            out.entities.remove("local://servers/current");
            out.queries.insert("local:servers".into(), uris);
        }
        out
    }

    /// Loads the device data again in the background: the notifications,
    /// channels and sign-ins the open layouts bind.
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
}

fn merge(into: &mut DataBundle, from: DataBundle) {
    into.entities.extend(from.entities);
    into.queries.extend(from.queries);
}

/// A snapshot from the engine (on one of its threads): what changed goes to
/// the session.
fn observe(engine: &Weak<ConstructEngine>, shared: &Arc<Shared>, base: &str, snap: &StateSnapshot) {
    let mut c = Changes {
        data: lock(&shared.sent).snapshot_changes(snap),
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
        // A layout the engine forgot (leaving the pod or signing out clears
        // them) or hasn't loaded again is new when it comes back, even with
        // the same JSON: the session has to be told.
        seen.layouts
            .retain(|id, _| layouts.get(id).is_some_and(|l| !l["workspace"].is_null()));
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
        // The construct's edge graph (decision `content-addressed-construct`).
        if snap.construct_json != seen.construct {
            seen.construct = snap.construct_json.clone();
            // The layouts' titles may have changed.
            reload = true;
            if let Ok(v) = serde_json::from_str::<Value>(&snap.construct_json)
                && let Ok(reach) = serde_json::from_value(v["reach"].clone())
            {
                *lock(&shared.edges) = Some(reach);
                if let Some(images) = lock(&shared.images).as_ref() {
                    images.wake();
                }
            }
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
    // The count at once; the list follows.
    device
        .entities
        .insert("local://inbox".into(), shared.inbox(unread(snap)));
    merge(&mut c.data, lock(&shared.sent).changes(device));
    // Nothing new: no frame is drawn for it.
    if !c.data.entities.is_empty() || !c.data.queries.is_empty() || !c.layouts.is_empty() {
        shared.push(c);
    }
    if reload {
        reload_device(engine.clone(), shared.clone());
    }
}

/// What the open layouts bind of the notifications, channels and sign-ins,
/// from the engine on a background thread (one at a time; asked again while
/// it runs, it runs once more after).
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
    let needs = shared.needs();
    let titles: HashMap<String, String> = engine.layout_titles().into_iter().collect();
    if remote::needs_notifications(&needs) {
        // From the pod, else the engine's cache.
        match engine.list_notifications(None, INBOX_LIMIT, None) {
            Ok(list) => {
                let v = serde_json::to_value(&list).unwrap_or_default();
                let mut page = remote::notifications_bundle(&v, &titles, remote::now_secs());
                // Older pages loaded before stay under the newest.
                let mut older = lock(&shared.older);
                if older.1.is_empty() {
                    older.0 = list.next_before.clone();
                }
                if let Some(q) = page.queries.get_mut("local:notifications") {
                    for u in &older.1 {
                        if !q.contains(u) {
                            q.push(u.clone());
                        }
                    }
                }
                drop(older);
                merge(&mut out, page);
                let unread = i64::from(list.unread_total);
                out.entities
                    .insert("local://inbox".into(), shared.inbox(unread));
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

/// The form scope the kept `local://` fields are stored under (feature
/// `local_keep`).
const KEPT_SCOPE: &str = "local-keep";
/// The most `local://` fields a device keeps.
const MAX_KEPT: usize = 200;

impl Loader for PodEngine {
    /// The engine's copy at once when it has one (its fetch goes on in the
    /// background and comes in through [`Loader::changes`]); else waits for
    /// the pod. Only the layout: the session already has the engine's
    /// entities and device data (from its first snapshot and every change
    /// since). Snapshots and the whole cache parsed again on the tap cost a
    /// phone about 50 ms before the screen could open.
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        if id != HOME {
            self.engine.open_layout(id);
        }
        // The layout alone (`layout_now`), not a snapshot: a snapshot
        // serializes the engine's whole state, and this runs on the tap.
        let until = Instant::now() + LOAD_WAIT;
        let got = loop {
            let now = self.engine.layout_now(id);
            if let Some(root) = now.root {
                break Ok(root);
            }
            if now.gone {
                break Err(client_core::LOAD_NOT_FOUND.to_owned());
            }
            if let Some(e) = now.error {
                break Err(e);
            }
            if Instant::now() >= until {
                break Err(client_core::LOAD_OFFLINE.to_owned());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let root = got?;
        self.needs(id, &root);
        Ok((root, DataBundle::default()))
    }

    /// Tells the engine what's on screen, as its load keys (decision
    /// `scoped-data-events`).
    fn foregrounded(&self) {
        self.engine.app_foregrounded();
    }

    fn visible(&self, shown: &[crate::interact::Shown]) {
        use crate::interact::Shown;
        let keys = shown
            .iter()
            .map(|s| match s {
                Shown::Layout(id) => format!("{}{id}", client_core::LAYOUT_LOAD_PREFIX),
                Shown::Data(path) => path.clone(),
            })
            .collect();
        self.engine.set_visible(keys);
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

    fn device_command(&self, command: &str, args: &Value) -> Option<Result<DataBundle, String>> {
        let text = |k: &str| args[k].as_str().unwrap_or("").trim().to_owned();
        let reason = |e: client_core::LayoutApiError| e.reason().to_owned();
        Some(match command {
            "toggle_channel" => {
                let on = args["enabled"].as_bool().unwrap_or(true);
                let layout = text("layout");
                let r = self.engine.set_channel_enabled(&layout, &text("id"), on);
                self.reload_device();
                r.map(|()| DataBundle::default()).map_err(reason)
            }
            "load_older_notifications" => {
                let before = text("before");
                self.engine
                    .list_notifications(Some(&before), INBOX_LIMIT, None)
                    .map_err(reason)
                    .map(|list| {
                        let titles: HashMap<String, String> =
                            self.engine.layout_titles().into_iter().collect();
                        let v = serde_json::to_value(&list).unwrap_or_default();
                        let page = remote::notifications_bundle(&v, &titles, remote::now_secs());
                        {
                            let mut older = lock(&self.shared.older);
                            older.0 = list.next_before.clone();
                            if let Some(q) = page.queries.get("local:notifications") {
                                older.1.extend(q.iter().cloned());
                            }
                        }
                        let mut out = page;
                        if let Ok(e) = serde_json::from_value(json!({
                            "has_older": list.next_before.is_some(),
                            "next_before": list.next_before.unwrap_or_default() }))
                        {
                            out.entities.insert("local://inbox".into(), e);
                        }
                        out
                    })
            }
            _ => return None,
        })
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

    /// The engine sets the fields too: its snapshots (the next one comes
    /// with the write that usually follows) would otherwise put the old
    /// values back before the pod's answer brings the new ones.
    fn local_set(&self, intent: &Value) {
        self.engine.dispatch(&intent.to_string());
    }

    /// Kept in the engine's store beside the remembered form values, one
    /// value per `<uri>#<field>`, as JSON.
    fn keep_local(&self, entity_uri: &str, field: &str, value: &Value) {
        let key = format!("{entity_uri}#{field}");
        let kept = self.engine.remembered_form_values(KEPT_SCOPE);
        if kept.len() >= MAX_KEPT && !kept.contains_key(&key) {
            eprintln!("[construct] local keep {key}: {MAX_KEPT} values are kept already; not kept");
            return;
        }
        // An empty value forgets it (the engine stores no empty values).
        let text = match value {
            Value::Null => String::new(),
            v => v.to_string(),
        };
        self.engine
            .remember_form_value(KEPT_SCOPE, &key, Some(&text));
    }

    fn kept_locals(&self) -> Vec<(String, String, Value)> {
        self.engine
            .remembered_form_values(KEPT_SCOPE)
            .into_iter()
            .filter_map(|(key, text)| {
                let (uri, field) = key.rsplit_once('#')?;
                let value = serde_json::from_str(&text).ok()?;
                Some((uri.to_owned(), field.to_owned(), value))
            })
            .collect()
    }

    fn changes(&self) -> Option<Changes> {
        lock(&self.shared.news).take()
    }

    fn edges(&self) -> Option<crate::interact::Edges> {
        lock(&self.shared.edges).take()
    }

    /// The engine's sign-in (D19): `action.auth.login` registers the
    /// client with the pod and opens the browser; `sign_in` starts a
    /// provider's (`local:providers`), on another server when it takes one
    /// and the layout passes it (`value`); `sign_out` signs out through
    /// the pod.
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
            _ => return false,
        };
        self.engine.dispatch(&login.to_string());
        true
    }

    fn redirect(&self, url: &str) -> bool {
        self.engine.handle_auth_redirect(url)
    }

    /// The engine stages it (decoded, upright, scaled, re-encoded without
    /// metadata) on a thread of its own: it takes a moment.
    fn stage(&self, file: &std::path::Path, done: crate::interact::StageDone) -> bool {
        let engine = self.engine.clone();
        let path = file.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            done(
                engine
                    .stage_attachment(&path)
                    .map(|a| crate::interact::Staged {
                        id: a.id,
                        uri: a.uri,
                        url: a.url,
                        width: a.width,
                        height: a.height,
                    }),
            )
        });
        true
    }

    fn discard(&self, id: &str) {
        self.engine.discard_attachment(id);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_of(entities: &[(&str, &str)], queries: &[(&str, &[&str])]) -> DataBundle {
        let mut b = DataBundle::default();
        for (uri, title) in entities {
            let mut e = protocol::EntityRecord::default();
            e.set("title", *title);
            b.entities.insert((*uri).to_owned(), e);
        }
        for (key, uris) in queries {
            b.queries.insert(
                (*key).to_owned(),
                uris.iter().map(|u| (*u).to_owned()).collect(),
            );
        }
        b
    }

    #[test]
    fn a_snapshot_sends_only_what_changed() {
        let mut sent = Sent::default();
        let first = bundle_of(
            &[("t://1", "a"), ("t://2", "b")],
            &[("q", &["t://1", "t://2"])],
        );
        // The first is sent whole.
        assert_eq!(sent.changes(first.clone()), first);
        // The same again sends nothing.
        assert_eq!(sent.changes(first), DataBundle::default());
        // One row changed, one added, the query grew.
        let next = bundle_of(
            &[("t://1", "a"), ("t://2", "B"), ("t://3", "c")],
            &[("q", &["t://1", "t://2", "t://3"])],
        );
        let out = sent.changes(next);
        assert_eq!(
            out.entities.keys().collect::<Vec<_>>(),
            vec!["t://2", "t://3"]
        );
        assert_eq!(out.queries["q"].len(), 3);
    }
}
