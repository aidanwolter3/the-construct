//! Notifications from apps (D47, docs/NOTIFICATIONS.md §6): the device
//! identity, device registration with the pod, the notification cache and
//! unread counts, the events-stream handlers and the blocking calls the UI
//! makes (list, mark read, channels, push subscription).
//!
//! The engine knows no app here either: a notification is an app id, a
//! channel id, ready-made text and a navigation target, as the pod sends it.
//! Notifications are private: they need a session, and without one the
//! engine simply has none (empty lists, zero counts, no errors from the
//! background paths). Signing out deletes the device on the pod and clears
//! the cache.
//!
//! Every call runs on the caller's thread (Kotlin calls them off the UI
//! thread) and never holds an engine lock during HTTP.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use protocol::{
    DEVICE_HEADER, Notification, NotificationEvent, NotificationList, PushPayload, ReadEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog::{LAYOUTS_OFFLINE, LayoutApiError, segment};
use crate::events::SseEvent;
use crate::store::{
    KV_NOTIFICATION_CHANNELS, KV_NOTIFICATION_UNREAD, NOTIFICATIONS_KEPT, Store, StoreOps,
};
use crate::{ConstructEngine, Inner, State, lock};

/// `engine_kv` key of this installation's device id.
pub const KV_DEVICE_ID: &str = "device_id";
/// `engine_kv` key of the remembered Web Push subscription (JSON).
pub const KV_PUSH_SUBSCRIPTION: &str = "push_subscription";
/// Notifications fetched when the events stream (re)connects, to catch up
/// on what the stream missed.
pub const REFRESH_LIMIT: u32 = 20;
/// Most notifications one list call asks for (the pod's limit).
pub const MAX_LIST_LIMIT: u32 = 100;
/// Platform sent until [`ConstructEngine::set_device_info`] says otherwise.
pub const DEFAULT_PLATFORM: &str = "unknown";
/// `reason` for calls that need a session while there is none.
pub const NOTIFICATIONS_SIGNED_OUT: &str = "Sign in to your pod to use notifications.";
/// `reason` when the server sends no Web Push.
pub const PUSH_UNAVAILABLE: &str = "This server doesn't send push notifications.";
const NOTIFY_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Called for each notification that arrives live on the events stream
/// (never for ones fetched, replayed or handed in by
/// [`ConstructEngine::handle_push_message`]), on the engine's events thread
/// with no engine lock held.
pub type NotificationListener = Arc<dyn Fn(&Notification) + Send + Sync>;

/// A Web Push subscription of this device (RFC 8030/8291).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

/// What the pod knows (or should know) about this device.
pub(crate) struct DeviceState {
    pub(crate) platform: String,
    pub(crate) name: String,
    /// The session token the device was last registered under (`None`:
    /// not registered, e.g. after a sign-out or a change of info).
    pub(crate) registered_for: Option<String>,
    /// Sent with every registration while set (so a new sign-in registers
    /// it again); persisted under [`KV_PUSH_SUBSCRIPTION`].
    pub(crate) push: Option<PushSubscription>,
}

impl DeviceState {
    pub(crate) fn new(push: Option<PushSubscription>) -> Self {
        Self {
            platform: DEFAULT_PLATFORM.to_owned(),
            name: String::new(),
            registered_for: None,
            push,
        }
    }
}

/// A mark-read the pod has not confirmed yet (sent again when the events
/// stream reconnects).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadReq {
    Ids(Vec<String>),
    /// Everything, or everything of one app.
    All(Option<String>),
}

impl ReadReq {
    fn body(&self) -> Value {
        match self {
            ReadReq::Ids(ids) => json!({ "ids": ids }),
            ReadReq::All(None) => json!({ "all": true }),
            ReadReq::All(Some(app)) => json!({ "all": true, "app": app }),
        }
    }
}

/// `[A-Za-z0-9_-]{8,64}`, what the pod accepts as a device id.
pub fn valid_device_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A new random device id (32 hex digits).
fn new_device_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS random number generator");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// This installation's device id: the stored one, else a new one (stored).
/// Without a store it lasts for this run only.
pub(crate) fn load_device_id(store: Option<&Store>) -> String {
    if let Some(s) = store {
        match s.kv_get(KV_DEVICE_ID) {
            Ok(Some(id)) if valid_device_id(&id) => return id,
            Ok(_) => {}
            Err(e) => eprintln!("[ConstructEngine] cannot read the device id: {e}"),
        }
    }
    let id = new_device_id();
    if let Some(s) = store
        && let Err(e) = s.kv_set(KV_DEVICE_ID, &id)
    {
        eprintln!("[ConstructEngine] cannot store the device id: {e}; it lasts until exit");
    }
    id
}

/// The remembered push subscription, if any.
pub(crate) fn load_push(store: Option<&Store>) -> Option<PushSubscription> {
    let raw = store?.kv_get(KV_PUSH_SUBSCRIPTION).ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// A failed pod call, before it becomes a [`LayoutApiError`].
#[derive(Debug)]
enum CallError {
    Offline(String),
    /// Status, the body's `error` code (`""` if none), the body's start.
    Status(u16, String, String),
    Unusable(String),
}

impl CallError {
    fn code(&self) -> &str {
        match self {
            CallError::Status(_, code, _) => code,
            _ => "",
        }
    }

    /// Worth trying again later (no answer, or the pod had a problem).
    fn transient(&self) -> bool {
        match self {
            CallError::Offline(_) => true,
            CallError::Status(s, ..) => *s >= 500 || *s == 408 || *s == 429,
            CallError::Unusable(_) => false,
        }
    }

    fn into_api(self) -> LayoutApiError {
        match self {
            CallError::Offline(_) => LayoutApiError::Offline {
                reason: LAYOUTS_OFFLINE.to_owned(),
            },
            CallError::Unusable(_) => LayoutApiError::Server {
                status: 200,
                reason: "The pod sent an answer this app doesn't understand.".to_owned(),
            },
            CallError::Status(status, code, _) => match (status, code.as_str()) {
                (_, "push_unavailable") => LayoutApiError::NotFound {
                    reason: PUSH_UNAVAILABLE.to_owned(),
                },
                (_, "push_endpoint_not_allowed") => LayoutApiError::Server {
                    status,
                    reason: "The pod doesn't accept this push service.".to_owned(),
                },
                (401, _) => LayoutApiError::Unauthorized {
                    reason: NOTIFICATIONS_SIGNED_OUT.to_owned(),
                },
                (404, "app_not_found") => LayoutApiError::NotFound {
                    reason: "That app doesn't exist.".to_owned(),
                },
                (404, _) => LayoutApiError::NotFound {
                    reason: "This pod doesn't support notifications.".to_owned(),
                },
                (400, "unknown_channel") => LayoutApiError::Server {
                    status,
                    reason: "That notification channel doesn't exist.".to_owned(),
                },
                (s, "") if s >= 500 => LayoutApiError::Server {
                    status,
                    reason: format!("The pod had a problem (HTTP {s}). Please try again."),
                },
                (s, "") => LayoutApiError::Server {
                    status,
                    reason: format!("The pod answered HTTP {s}."),
                },
                (s, c) => LayoutApiError::Server {
                    status,
                    reason: format!("The pod answered HTTP {s} ({c})."),
                },
            },
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Offline(e) => write!(f, "unreachable: {e}"),
            CallError::Status(s, _, body) => write!(f, "HTTP {s}: {body}"),
            CallError::Unusable(e) => write!(f, "unusable answer: {e}"),
        }
    }
}

fn signed_out() -> LayoutApiError {
    LayoutApiError::Unauthorized {
        reason: NOTIFICATIONS_SIGNED_OUT.to_owned(),
    }
}

fn empty_list(unread_total: u32, unread_by_app: BTreeMap<String, u32>) -> NotificationList {
    NotificationList {
        notifications: Vec::new(),
        unread_total,
        unread_by_app,
        next_before: None,
    }
}

/// One request to the pod with the session and the device header.
fn call(
    method: reqwest::Method,
    url: &str,
    token: Option<&str>,
    device: &str,
    body: Option<&Value>,
) -> Result<String, CallError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(NOTIFY_HTTP_TIMEOUT)
        .build()
        .map_err(|e| CallError::Offline(format!("client build: {e}")))?;
    let mut req = client
        .request(method.clone(), url)
        .header(DEVICE_HEADER, device);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().map_err(|e| CallError::Offline(e.to_string()))?;
    let status = resp.status().as_u16();
    let text = resp.text().map_err(|e| CallError::Offline(e.to_string()))?;
    if (200..300).contains(&status) {
        return Ok(text);
    }
    let code = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    Err(CallError::Status(
        status,
        code,
        text.trim().chars().take(200).collect(),
    ))
}

/// Best-effort `DELETE /v1/devices/{id}` (sign-out), logged.
pub(crate) fn delete_device(base_url: &str, device: &str, token: &str) {
    let url = format!("{base_url}/v1/devices/{device}");
    match call(reqwest::Method::DELETE, &url, Some(token), device, None) {
        Ok(_) => println!("[ConstructEngine] device removed from the pod"),
        Err(e) => eprintln!("[ConstructEngine] DELETE {url} failed (ignored): {e}"),
    }
}

impl State {
    /// See [`crate::StateSnapshot::notifications_json`].
    pub(crate) fn notifications_json(&self) -> String {
        if self.session.is_none() {
            return json!({ "unread_total": 0, "unread_by_app": {} }).to_string();
        }
        json!({ "unread_total": self.unread_total, "unread_by_app": self.unread_by_app })
            .to_string()
    }

    /// The unread counts cached by an earlier run (shown offline).
    pub(crate) fn load_unread(&mut self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(raw)) = store.kv_get(KV_NOTIFICATION_UNREAD)
            && let Ok(ev) = serde_json::from_str::<ReadEvent>(&raw)
        {
            self.unread_total = ev.unread_total;
            self.unread_by_app = ev.unread_by_app;
        }
    }

    /// Set the unread counts (memory and store). Does not bump `seq`.
    fn set_unread(&mut self, total: u32, by_app: BTreeMap<String, u32>) {
        self.unread_total = total;
        self.unread_by_app = by_app;
        self.unread_by_app.retain(|_, n| *n > 0);
        if let Some(store) = &self.store {
            let raw = json!({ "unread_total": total, "unread_by_app": self.unread_by_app });
            if let Err(e) = store.kv_set(KV_NOTIFICATION_UNREAD, &raw.to_string()) {
                eprintln!("[ConstructEngine] cannot cache the unread counts: {e}");
            }
        }
    }

    fn unread_event(&self) -> ReadEvent {
        ReadEvent {
            unread_total: self.unread_total,
            unread_by_app: self.unread_by_app.clone(),
        }
    }

    /// Store a notification (keeping the newest [`NOTIFICATIONS_KEPT`]).
    /// Returns whether it was not cached before (a store failure counts as
    /// new, logged).
    fn cache_notification(&mut self, n: &Notification) -> bool {
        let Some(store) = self.store.as_mut() else {
            return true;
        };
        let result = store.transaction(|tx| {
            let new = tx.upsert_notification(n)?;
            tx.prune_notifications(NOTIFICATIONS_KEPT)?;
            Ok(new)
        });
        result.unwrap_or_else(|e| {
            eprintln!("[ConstructEngine] cannot cache notification {}: {e}", n.id);
            true
        })
    }

    /// Store a fetched page (no listener calls).
    fn cache_notifications(&mut self, list: &[Notification]) {
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let result = store.transaction(|tx| {
            for n in list {
                tx.upsert_notification(n)?;
            }
            tx.prune_notifications(NOTIFICATIONS_KEPT)?;
            Ok(())
        });
        if let Err(e) = result {
            eprintln!("[ConstructEngine] cannot cache notifications: {e}");
        }
    }

    /// Make cached read flags agree with authoritative counts: an app (or
    /// everything) with no unread notifications has none unread here.
    fn sync_read_flags(&mut self) {
        let Some(store) = &self.store else { return };
        let result = (|| {
            if self.unread_total == 0 {
                store.mark_notifications_read(None, None)?;
                return Ok(());
            }
            for app in store.unread_notification_apps()? {
                if !self.unread_by_app.contains_key(&app) {
                    store.mark_notifications_read(None, Some(&app))?;
                }
            }
            Ok::<_, crate::store::StoreError>(())
        })();
        if let Err(e) = result {
            eprintln!("[ConstructEngine] cannot update cached read flags: {e}");
        }
    }

    /// Mark read locally (optimistic): cached flags and counts.
    fn mark_read_locally(&mut self, req: &ReadReq) {
        let marked = match &self.store {
            Some(store) => {
                let r = match req {
                    ReadReq::Ids(ids) => store.mark_notifications_read(Some(ids), None),
                    ReadReq::All(app) => store.mark_notifications_read(None, app.as_deref()),
                };
                r.unwrap_or_else(|e| {
                    eprintln!("[ConstructEngine] cannot mark cached notifications read: {e}");
                    BTreeMap::new()
                })
            }
            None => BTreeMap::new(),
        };
        let mut by_app = self.unread_by_app.clone();
        let total = match req {
            ReadReq::All(None) => {
                by_app.clear();
                0
            }
            ReadReq::All(Some(app)) => {
                let n = by_app.remove(app).unwrap_or(0);
                self.unread_total.saturating_sub(n)
            }
            ReadReq::Ids(_) => {
                let mut total = self.unread_total;
                for (app, n) in marked {
                    if let Some(c) = by_app.get_mut(&app) {
                        *c = c.saturating_sub(n);
                    }
                    total = total.saturating_sub(n);
                }
                total
            }
        };
        self.set_unread(total, by_app);
    }

    /// Sign-out: forget the cached notifications, channels and counts.
    pub(crate) fn clear_notifications(&mut self) {
        self.unread_total = 0;
        self.unread_by_app.clear();
        if let Some(store) = &self.store {
            match store.delete_notification_cache() {
                Ok(n) if n > 0 => println!("[ConstructEngine] removed {n} cached notifications"),
                Ok(_) => {}
                Err(e) => eprintln!("[ConstructEngine] cannot clear the notification cache: {e}"),
            }
        }
    }
}

impl Inner {
    fn session_is(&self, token: &str) -> bool {
        lock(&self.state).session.as_ref().map(|s| s.token.as_str()) == Some(token)
    }

    fn pod_call(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> Result<String, CallError> {
        let url = format!("{}{path}", self.base_url);
        let result = call(method.clone(), &url, Some(token), &self.device_id, body);
        if let Err(e) = &result {
            eprintln!("[ConstructEngine] notifications: {method} {url} failed: {e}");
        }
        result
    }

    /// `PUT /v1/devices/{id}` unless the device is registered under the
    /// current session already (`force`: always). Includes the remembered
    /// push subscription; if the pod refuses that, registers without it.
    pub(crate) fn ensure_device_registered(&self, force: bool) -> Result<(), LayoutApiError> {
        let _io = lock(&self.device_io);
        let token = self.session_token().ok_or_else(signed_out)?;
        let (platform, name, push, done) = {
            let d = lock(&self.device);
            (
                d.platform.clone(),
                d.name.clone(),
                d.push.clone(),
                d.registered_for.as_deref() == Some(token.as_str()),
            )
        };
        if done && !force {
            return Ok(());
        }
        let path = format!("/v1/devices/{}", self.device_id);
        let mut body = json!({ "platform": platform, "name": name });
        if let Some(p) = &push {
            body["push"] = json!(p);
        }
        let mut result = self.pod_call(reqwest::Method::PUT, &path, &token, Some(&body));
        if push.is_some()
            && let Err(e) = &result
            && (e.code().starts_with("push_") || e.code() == "invalid_request")
        {
            eprintln!(
                "[ConstructEngine] the pod refused the push subscription ({}); registering without it",
                e.code()
            );
            if let Some(o) = body.as_object_mut() {
                o.remove("push");
            }
            result = self.pod_call(reqwest::Method::PUT, &path, &token, Some(&body));
        }
        match result {
            Ok(answer) => {
                let push_on = serde_json::from_str::<Value>(&answer)
                    .ok()
                    .and_then(|v| v.get("push").and_then(Value::as_bool))
                    .unwrap_or(false);
                println!(
                    "[ConstructEngine] device registered with the pod (platform {platform}, push {})",
                    if push_on { "on" } else { "off" }
                );
                if self.session_is(&token) {
                    lock(&self.device).registered_for = Some(token);
                }
                Ok(())
            }
            Err(e) => Err(e.into_api()),
        }
    }

    /// The events stream (re)connected with `token`: send mark-reads the pod
    /// has not confirmed, register the device if needed, and catch up on
    /// notifications and counts the stream missed (no listener calls).
    /// Failures are logged only. Events thread.
    pub(crate) fn on_stream_connected(&self, token: &str) {
        self.flush_pending_reads(token);
        if self.signal.is_shut_down() || !self.session_is(token) {
            return;
        }
        let _ = self.ensure_device_registered(false);
        if self.signal.is_shut_down() {
            return;
        }
        let path = format!("/v1/notifications?limit={REFRESH_LIMIT}");
        if let Ok(body) = self.pod_call(reqwest::Method::GET, &path, token, None) {
            match serde_json::from_str::<NotificationList>(&body) {
                Ok(list) => self.merge_list(token, &list),
                Err(e) => eprintln!("[ConstructEngine] notifications: unusable list: {e}"),
            }
        }
    }

    fn flush_pending_reads(&self, token: &str) {
        let pending = std::mem::take(&mut *lock(&self.pending_reads));
        let mut rest = pending.into_iter();
        while let Some(req) = rest.next() {
            match self.post_read(token, &req) {
                Ok(_) => {}
                Err(e) if e.transient() => {
                    let mut q = lock(&self.pending_reads);
                    let newer = std::mem::take(&mut *q);
                    q.push(req);
                    q.extend(rest);
                    q.extend(newer);
                    return;
                }
                // Refused: dropped (the refetch brings the pod's view).
                Err(_) => {}
            }
        }
    }

    /// Cache a fetched page and take its counts (if `token` is still the
    /// session), then notify.
    fn merge_list(&self, token: &str, list: &NotificationList) {
        let snap = {
            let mut st = lock(&self.state);
            if st.session.as_ref().map(|s| s.token.as_str()) != Some(token) {
                return;
            }
            st.cache_notifications(&list.notifications);
            st.set_unread(list.unread_total, list.unread_by_app.clone());
            st.sync_read_flags();
            st.seq += 1;
            st.snapshot()
        };
        self.notify(snap);
    }

    /// Store a notification that arrived (stream or push) and update the
    /// counts. Returns whether it was new and unread (`None`: signed out,
    /// nothing stored).
    fn take_notification(&self, ev: &NotificationEvent) -> Option<bool> {
        let n = &ev.notification;
        let (snap, fresh) = {
            let mut st = lock(&self.state);
            st.session.as_ref()?;
            let new = st.cache_notification(n);
            let mut by_app = st.unread_by_app.clone();
            if new && !n.read {
                *by_app.entry(n.app_id.clone()).or_insert(0) += 1;
            }
            st.set_unread(ev.unread_total, by_app);
            st.seq += 1;
            (st.snapshot(), new && !n.read)
        };
        self.notify(snap);
        Some(fresh)
    }

    /// Counts after a mark-read (stream event or the POST's answer).
    fn apply_read(&self, ev: &ReadEvent) {
        let snap = {
            let mut st = lock(&self.state);
            if st.session.is_none() {
                return;
            }
            st.set_unread(ev.unread_total, ev.unread_by_app.clone());
            st.sync_read_flags();
            st.seq += 1;
            st.snapshot()
        };
        self.notify(snap);
    }

    /// A `notification` or `notifications_read` stream event. Returns
    /// whether `ev` was one of them. Events thread.
    pub(crate) fn on_notification_event(&self, ev: &SseEvent) -> bool {
        match ev.event.as_str() {
            protocol::notify::NOTIFICATION_EVENT => {
                match serde_json::from_str::<NotificationEvent>(&ev.data) {
                    Ok(event) => {
                        if self.take_notification(&event) == Some(true) {
                            self.call_listener(&event.notification);
                        }
                    }
                    Err(e) => eprintln!(
                        "[ConstructEngine] events: unusable notification event ignored: {e}"
                    ),
                }
                true
            }
            protocol::notify::NOTIFICATIONS_READ_EVENT => {
                match serde_json::from_str::<ReadEvent>(&ev.data) {
                    Ok(event) => self.apply_read(&event),
                    Err(e) => eprintln!(
                        "[ConstructEngine] events: unusable notifications_read event ignored: {e}"
                    ),
                }
                true
            }
            _ => false,
        }
    }

    fn call_listener(&self, n: &Notification) {
        let listener = lock(&self.notification_listener).clone();
        if let Some(f) = listener
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(n))).is_err()
        {
            eprintln!("[ConstructEngine] notification listener panicked; ignored");
        }
    }

    fn post_read(&self, token: &str, req: &ReadReq) -> Result<ReadEvent, CallError> {
        let body = self.pod_call(
            reqwest::Method::POST,
            "/v1/notifications/read",
            token,
            Some(&req.body()),
        )?;
        let ev: ReadEvent =
            serde_json::from_str(&body).map_err(|e| CallError::Unusable(e.to_string()))?;
        if self.session_is(token) {
            self.apply_read(&ev);
        }
        Ok(ev)
    }

    /// Optimistic mark-read, then the POST. An unanswered (or 5xx) POST is
    /// queued for the next stream connection and the local counts returned.
    fn mark_read(&self, req: ReadReq) -> Result<ReadEvent, LayoutApiError> {
        let Some(token) = self.session_token() else {
            return Ok(ReadEvent {
                unread_total: 0,
                unread_by_app: BTreeMap::new(),
            });
        };
        let (snap, local) = {
            let mut st = lock(&self.state);
            st.mark_read_locally(&req);
            st.seq += 1;
            (st.snapshot(), st.unread_event())
        };
        self.notify(snap);
        match self.post_read(&token, &req) {
            Ok(ev) => Ok(ev),
            Err(e) if e.transient() => {
                lock(&self.pending_reads).push(req);
                Ok(local)
            }
            Err(e) => Err(e.into_api()),
        }
    }

    /// Notifications from the cache (offline answer of a list call).
    fn cached_list(
        &self,
        before: Option<&str>,
        limit: u32,
        app: Option<&str>,
    ) -> Result<NotificationList, LayoutApiError> {
        let st = lock(&self.state);
        let mut list = empty_list(st.unread_total, st.unread_by_app.clone());
        if let Some(store) = &st.store {
            list.notifications = store
                .load_notifications(before, limit as usize, app)
                .map_err(|e| LayoutApiError::Server {
                    status: 0,
                    reason: format!("The notification cache failed: {e}"),
                })?;
        }
        if limit > 0 && list.notifications.len() == limit as usize {
            list.next_before = list.notifications.last().map(|n| n.id.clone());
        }
        Ok(list)
    }

    /// Update `enabled` of one channel in the cached channel list.
    fn cache_channel_choice(&self, app_id: &str, channel: &str, enabled: bool) {
        let st = lock(&self.state);
        let Some(store) = &st.store else { return };
        let Ok(Some(raw)) = store.kv_get(KV_NOTIFICATION_CHANNELS) else {
            return;
        };
        let Ok(mut v) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        if let Some(list) = v.get_mut("channels").and_then(Value::as_array_mut) {
            for c in list {
                if c["app_id"] == app_id && c["id"] == channel {
                    c["enabled"] = json!(enabled);
                }
            }
        }
        let _ = store.kv_set(KV_NOTIFICATION_CHANNELS, &v.to_string());
    }

    fn remember_push(&self, push: Option<PushSubscription>) {
        lock(&self.device).push = push.clone();
        let st = lock(&self.state);
        if let Some(store) = &st.store {
            let r = match &push {
                Some(p) => store.kv_set(
                    KV_PUSH_SUBSCRIPTION,
                    &serde_json::to_string(p).unwrap_or_default(),
                ),
                None => store.kv_delete(KV_PUSH_SUBSCRIPTION),
            };
            if let Err(e) = r {
                eprintln!("[ConstructEngine] cannot store the push subscription: {e}");
            }
        }
    }
}

impl ConstructEngine {
    /// Set (replace) the listener for notifications that arrive live on the
    /// events stream. Called on the engine's events thread, no locks held;
    /// a panic in it is contained.
    pub fn set_notification_listener<F: Fn(&Notification) + Send + Sync + 'static>(&self, f: F) {
        *lock(&self.inner.notification_listener) = Some(Arc::new(f));
    }

    /// What the pod shows for this device (`platform`: 1-32 characters,
    /// e.g. "android"; `name`: up to 64 characters). Re-registers the
    /// device in the background when it changed and there is a session.
    pub fn set_device_info(&self, platform: &str, name: &str) {
        let platform = platform.trim();
        let platform = if platform.is_empty() || platform.len() > 32 {
            DEFAULT_PLATFORM.to_owned()
        } else {
            platform.to_owned()
        };
        let name: String = name.trim().chars().take(64).collect();
        {
            let mut d = lock(&self.inner.device);
            if d.platform == platform && d.name == name {
                return;
            }
            d.platform = platform;
            d.name = name;
            d.registered_for = None;
        }
        if self.inner.session_token().is_none() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let spawned = std::thread::Builder::new()
            .name("construct-engine-device".into())
            .spawn(move || {
                let _ = inner.ensure_device_registered(false);
            });
        if let Err(e) = spawned {
            eprintln!("[ConstructEngine] cannot start the device registration: {e}");
        }
    }

    /// This installation's device id (sent as `x-construct-device`).
    pub fn device_id(&self) -> String {
        self.inner.device_id.clone()
    }

    /// One page of notifications, newest first (`limit` 0..=100; `before`:
    /// the previous page's `next_before`; `app_id`: one app only). From the
    /// pod, whose answer is cached; when the pod can't be reached (or had a
    /// problem), from the cache. Signed out: an empty list. Blocking.
    pub fn list_notifications(
        &self,
        before: Option<&str>,
        limit: u32,
        app_id: Option<&str>,
    ) -> Result<NotificationList, LayoutApiError> {
        let limit = limit.min(MAX_LIST_LIMIT);
        let Some(token) = self.inner.session_token() else {
            return Ok(empty_list(0, BTreeMap::new()));
        };
        let mut path = format!("/v1/notifications?limit={limit}");
        if let Some(b) = before {
            path.push_str(&format!("&before={}", segment(b)));
        }
        if let Some(a) = app_id {
            path.push_str(&format!("&app={}", segment(a)));
        }
        match self
            .inner
            .pod_call(reqwest::Method::GET, &path, &token, None)
        {
            Ok(body) => {
                let list: NotificationList = serde_json::from_str(&body)
                    .map_err(|e| CallError::Unusable(e.to_string()).into_api())?;
                self.inner.merge_list(&token, &list);
                Ok(list)
            }
            Err(e) if e.transient() => self.inner.cached_list(before, limit, app_id),
            Err(e) => Err(e.into_api()),
        }
    }

    /// Mark notifications read: at once locally (cache, counts, observers),
    /// then on the pod. Returns the pod's counts, or the local ones when the
    /// pod could not be reached (the request is sent again when the events
    /// stream reconnects). Signed out: zero counts. Blocking.
    pub fn mark_notifications_read(&self, ids: &[String]) -> Result<ReadEvent, LayoutApiError> {
        self.inner.mark_read(ReadReq::Ids(ids.to_vec()))
    }

    /// Mark every notification read (of `app_id` only, when given); like
    /// [`Self::mark_notifications_read`].
    pub fn mark_all_notifications_read(
        &self,
        app_id: Option<&str>,
    ) -> Result<ReadEvent, LayoutApiError> {
        self.inner
            .mark_read(ReadReq::All(app_id.map(str::to_owned)))
    }

    /// `{"channels": [{app_id, id, title, description, default_enabled,
    /// importance, enabled}]}`: every app's channels with this device's
    /// choices. From the pod (cached); the cached list when the pod can't be
    /// reached. Signed out: no channels. Blocking.
    pub fn notification_channels(&self) -> Result<String, LayoutApiError> {
        let Some(token) = self.inner.session_token() else {
            return Ok(json!({ "channels": [] }).to_string());
        };
        match self.inner.pod_call(
            reqwest::Method::GET,
            "/v1/notifications/channels",
            &token,
            None,
        ) {
            Ok(body) => {
                let ok = serde_json::from_str::<Value>(&body)
                    .is_ok_and(|v| v.get("channels").is_some_and(Value::is_array));
                if !ok {
                    return Err(CallError::Unusable("no channels array".into()).into_api());
                }
                let st = lock(&self.inner.state);
                if st.session.as_ref().map(|s| s.token.as_str()) == Some(token.as_str())
                    && let Some(store) = &st.store
                    && let Err(e) = store.kv_set(KV_NOTIFICATION_CHANNELS, &body)
                {
                    eprintln!("[ConstructEngine] cannot cache the channels: {e}");
                }
                Ok(body)
            }
            Err(e) if e.transient() => {
                let cached = lock(&self.inner.state)
                    .store
                    .as_ref()
                    .and_then(|s| s.kv_get(KV_NOTIFICATION_CHANNELS).ok().flatten());
                cached.ok_or_else(|| e.into_api())
            }
            Err(e) => Err(e.into_api()),
        }
    }

    /// Turn one channel on or off for this device (registers the device
    /// first if needed). Blocking.
    pub fn set_channel_enabled(
        &self,
        app_id: &str,
        channel: &str,
        enabled: bool,
    ) -> Result<(), LayoutApiError> {
        let token = self.inner.session_token().ok_or_else(signed_out)?;
        self.inner.ensure_device_registered(false)?;
        let path = format!("/v1/devices/{}/channels", self.inner.device_id);
        let body = json!({ protocol::channel_key(app_id, channel): enabled });
        let mut result = self
            .inner
            .pod_call(reqwest::Method::PUT, &path, &token, Some(&body));
        if let Err(e) = &result
            && e.code() == "device_not_found"
        {
            // The pod forgot the device (e.g. pruned): register it again.
            self.inner.ensure_device_registered(true)?;
            result = self
                .inner
                .pod_call(reqwest::Method::PUT, &path, &token, Some(&body));
        }
        result.map_err(CallError::into_api)?;
        self.inner.cache_channel_choice(app_id, channel, enabled);
        Ok(())
    }

    /// The server's VAPID public key (base64url), needed to subscribe to
    /// Web Push. `NotFound` ([`PUSH_UNAVAILABLE`]) when the server sends no
    /// push. Needs a session. Blocking.
    pub fn push_public_key(&self) -> Result<String, LayoutApiError> {
        let token = self.inner.session_token().ok_or_else(signed_out)?;
        let body = self
            .inner
            .pod_call(reqwest::Method::GET, "/v1/push/key", &token, None)
            .map_err(CallError::into_api)?;
        serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("vapid_public_key")?.as_str().map(str::to_owned))
            .ok_or_else(|| CallError::Unusable("no vapid_public_key".into()).into_api())
    }

    /// Send this device's Web Push subscription to the pod. It is
    /// remembered and sent again with every registration (e.g. after the
    /// next sign-in). Signed out: only remembered. Blocking.
    pub fn register_push(
        &self,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
    ) -> Result<(), LayoutApiError> {
        let sub = PushSubscription {
            endpoint: endpoint.to_owned(),
            p256dh: p256dh.to_owned(),
            auth: auth.to_owned(),
        };
        let Some(token) = self.inner.session_token() else {
            self.inner.remember_push(Some(sub));
            return Ok(());
        };
        let _io = lock(&self.inner.device_io);
        let (platform, name) = {
            let d = lock(&self.inner.device);
            (d.platform.clone(), d.name.clone())
        };
        let path = format!("/v1/devices/{}", self.inner.device_id);
        let body = json!({ "platform": platform, "name": name, "push": sub });
        self.inner
            .pod_call(reqwest::Method::PUT, &path, &token, Some(&body))
            .map_err(CallError::into_api)?;
        self.inner.remember_push(Some(sub));
        if self.inner.session_is(&token) {
            lock(&self.inner.device).registered_for = Some(token);
        }
        println!("[ConstructEngine] push subscription registered with the pod");
        Ok(())
    }

    /// Stop Web Push to this device (forgets the remembered subscription).
    /// Blocking.
    pub fn unregister_push(&self) -> Result<(), LayoutApiError> {
        self.inner.remember_push(None);
        let Some(token) = self.inner.session_token() else {
            return Ok(());
        };
        let _io = lock(&self.inner.device_io);
        let (platform, name) = {
            let d = lock(&self.inner.device);
            (d.platform.clone(), d.name.clone())
        };
        let path = format!("/v1/devices/{}", self.inner.device_id);
        let body = json!({ "platform": platform, "name": name, "push": null });
        self.inner
            .pod_call(reqwest::Method::PUT, &path, &token, Some(&body))
            .map_err(CallError::into_api)?;
        if self.inner.session_is(&token) {
            lock(&self.inner.device).registered_for = Some(token);
        }
        Ok(())
    }

    /// A decrypted Web Push message (a [`PushPayload`] JSON): cache the
    /// notification and take its unread count (observers are notified),
    /// and return it; `None` if the payload is not one. Never calls the
    /// notification listener (the platform shows push messages itself).
    /// Signed out: returned, but not cached.
    pub fn handle_push_message(&self, payload_json: &str) -> Option<Notification> {
        let payload: PushPayload = match serde_json::from_str(payload_json) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[ConstructEngine] push message ignored: {e}");
                return None;
            }
        };
        self.inner.take_notification(&payload.event);
        Some(payload.event.notification)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids_are_valid_and_stored() {
        let id = new_device_id();
        assert!(valid_device_id(&id), "{id}");
        assert_ne!(id, new_device_id());
        assert!(!valid_device_id("short"));
        assert!(!valid_device_id("has.dots.in.it"));
        let store = Store::open_in_memory().unwrap();
        let a = load_device_id(Some(&store));
        assert_eq!(load_device_id(Some(&store)), a);
        assert_ne!(load_device_id(None), a);
    }

    #[test]
    fn errors_map_to_short_sentences() {
        let e = |s, c: &str| CallError::Status(s, c.into(), String::new()).into_api();
        assert_eq!(
            e(404, "push_unavailable"),
            LayoutApiError::NotFound {
                reason: PUSH_UNAVAILABLE.into()
            }
        );
        assert!(matches!(e(401, ""), LayoutApiError::Unauthorized { .. }));
        assert!(matches!(
            e(503, ""),
            LayoutApiError::Server { status: 503, .. }
        ));
        assert!(CallError::Offline("x".into()).transient());
        assert!(CallError::Status(502, String::new(), String::new()).transient());
        assert!(!CallError::Status(400, String::new(), String::new()).transient());
    }

    #[test]
    fn read_request_bodies() {
        assert_eq!(
            ReadReq::Ids(vec!["1".into()]).body(),
            json!({ "ids": ["1"] })
        );
        assert_eq!(ReadReq::All(None).body(), json!({ "all": true }));
        assert_eq!(
            ReadReq::All(Some("a".into())).body(),
            json!({ "all": true, "app": "a" })
        );
    }
}
