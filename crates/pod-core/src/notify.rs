//! Notifications from apps (D52, `docs/notifications.md`).
//!
//! Adapters report notifications ([`AppState::notify`]), either when the host
//! scheduler polls them ([`Source::poll_notifications`], driven by
//! [`AppState::poll_due`]) or from their own routes. The core stores each once
//! (`UNIQUE(layout, dedupe_key)`), announces it on every open events stream
//! of the pod and sends it by Web Push to registered devices that want its
//! channel and have no stream open. The core never knows what a notification
//! means: it has an app id, a channel the app's adapter declared, ready-made
//! text and a navigation target in the app's own layout routes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use protocol::{
    DEVICE_HEADER, Notification, NotificationChannel, NotificationEvent, NotificationList,
    PushPayload, ReadEvent,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::notify_store::{ChannelId, CoreStore, ListQuery, NotificationStore, ReadSel, ToStore};
use crate::push::{PushError, Subscription};
use crate::session::{Caller, sign_in_required};
use crate::util::now_secs;
use crate::{ApiError, AppState, Pod, blocking, json_error, parse_json_body};

/// Notifications kept per pod (the oldest go first).
pub const KEEP_COUNT: i64 = 500;
/// Notifications older than this (by arrival) go.
pub const KEEP_SECS: i64 = 30 * 86_400;
/// Devices kept per pod (the least recently seen go first).
pub const MAX_DEVICES: i64 = 20;
/// A device not seen for this long is no reason to poll.
pub const DEVICE_ACTIVE_SECS: i64 = 30 * 86_400;
/// What [`AppState::poll_due`] answers for a pod an adapter keeps open
/// ([`crate::Source::keeps_open`]) and with nothing to poll: due at
/// once, so the host opens it whenever the server starts. It never
/// changes, so the host doesn't write it again on every pass.
pub const KEEP_OPEN_WAKE: i64 = 0;
/// One adapter poll may take this long.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest backoff after failed polls.
pub const MAX_BACKOFF_SECS: i64 = 3600;
/// Consecutive failed deliveries before a device's subscription is dropped.
pub const MAX_PUSH_FAILURES: i64 = 5;
/// Web Push deliveries per pod per minute.
pub const PUSH_PER_MINUTE: u32 = 60;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;
const MAX_TITLE: usize = 200;
const MAX_BODY: usize = 1000;

/// A notification an adapter reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewNotification {
    /// The layout (or blueprint) whose channel it is.
    pub layout: String,
    /// A channel the source declared for that layout.
    pub channel: String,
    /// Stable per upstream event (e.g. `<instance>:<id>`); reporting the same
    /// key again does nothing.
    pub dedupe_key: String,
    pub title: String,
    pub body: String,
    /// Absolute, or a pod path starting with `/` (clients prefix their base
    /// URL); must load without a bearer ([`PollCtx::public_prefix`]).
    pub image_url: Option<String>,
    /// An `action.navigation.open` target in the app's layout routes.
    pub target_uri: Option<String>,
    pub group_key: Option<String>,
    /// Unix seconds (when it happened upstream).
    pub created_at: i64,
    /// The entity it is about (an entity URI), when there is one: the
    /// notification goes when that does ([`AppState::notifications_gone`];
    /// decision `notifications-in-a-collection`).
    pub subject: Option<String>,
    /// Store it as already read and announce nothing (a first poll's
    /// backlog, so the inbox has history without a burst of alerts).
    pub quiet: bool,
}

/// What a poll is told.
#[derive(Debug, Clone, Default)]
pub struct PollCtx {
    /// The pod's public path prefix (D46, `""` when served on its own): put
    /// it in front of image paths so clients load them without a bearer.
    pub public_prefix: String,
}

/// How a poll went.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PollStatus {
    #[default]
    Ok,
    /// Try again later, with backoff (upstream down, timeout).
    Transient(String),
    /// The sign-in this adapter polls with is gone (revoked upstream). Polls
    /// stop until a session completes that sign-in again.
    SignInGone(String),
}

/// What [`Source::poll_notifications`](crate::Source::poll_notifications) returns.
#[derive(Debug, Clone, Default)]
pub struct PollOutcome {
    pub new: Vec<NewNotification>,
    pub status: PollStatus,
    /// Poll again after this instead of the adapter's interval.
    pub next_in: Option<Duration>,
}

/// What the pod's events streams carry besides layout versions.
#[derive(Debug, Clone, PartialEq, Eq)]
// A broadcast message, cloned per subscriber; a box would not save anything.
#[allow(clippy::large_enum_variant)]
pub enum NotifyEvent {
    New(NotificationEvent),
    Read(ReadEvent),
}

/// Valid channel and app ids: `[a-z0-9_]{1,32}` for channels.
pub fn valid_channel_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn valid_device_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ---------- devices ----------

/// Devices of sessions that no longer exist go (with their choices).
fn prune_devices_on(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM devices WHERE token_hash NOT IN (SELECT token_hash FROM sessions)",
        [],
    )?;
    conn.execute(
        "DELETE FROM device_channel_prefs WHERE device_id NOT IN (SELECT id FROM devices)",
        [],
    )?;
    Ok(())
}

/// A device that may get a Web Push delivery.
#[derive(Debug, Clone)]
struct PushTarget {
    device: String,
    sub: Subscription,
}

/// Devices with a subscription whose session is still valid.
fn push_targets_on(conn: &Connection, ttl_secs: i64) -> rusqlite::Result<Vec<PushTarget>> {
    prune_devices_on(conn)?;
    let cutoff = now_secs().saturating_sub(ttl_secs);
    let rows: Vec<(String, String, String, String)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT d.id, d.push_endpoint, d.push_p256dh, d.push_auth FROM devices d
             JOIN sessions s ON s.token_hash = d.token_hash
             WHERE d.push_endpoint IS NOT NULL AND (s.created_at > ?1 OR s.pinned = 1)",
        )?;
        stmt.query_map([cutoff], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    Ok(rows
        .into_iter()
        .map(|(device, endpoint, p256dh, auth)| PushTarget {
            device,
            sub: Subscription {
                endpoint,
                p256dh,
                auth,
            },
        })
        .collect())
}

/// Whether any device with a valid session was seen recently (a reason to
/// poll upstreams at all).
fn has_active_device_on(conn: &Connection, ttl_secs: i64) -> rusqlite::Result<bool> {
    let cutoff = now_secs().saturating_sub(ttl_secs);
    Ok(conn
        .query_row(
            "SELECT 1 FROM devices d JOIN sessions s ON s.token_hash = d.token_hash
             WHERE d.last_seen > ?1 AND (s.created_at > ?2 OR s.pinned = 1) LIMIT 1",
            params![now_secs() - DEVICE_ACTIVE_SECS, cutoff],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

#[derive(Debug, Clone, Default)]
struct PollRow {
    next_at: i64,
    failures: i64,
    stopped_for: Option<String>,
}

fn poll_row_on(conn: &Connection, adapter: &str) -> rusqlite::Result<PollRow> {
    Ok(conn
        .query_row(
            "SELECT next_at, failures, stopped_for FROM notification_polls WHERE adapter = ?1",
            [adapter],
            |r| {
                Ok(PollRow {
                    next_at: r.get(0)?,
                    failures: r.get(1)?,
                    stopped_for: r.get(2)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

fn save_poll_on(
    conn: &Connection,
    adapter: &str,
    row: &PollRow,
    error: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO notification_polls (adapter, next_at, failures, last_error, stopped_for)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(adapter) DO UPDATE SET next_at = excluded.next_at,
             failures = excluded.failures, last_error = excluded.last_error,
             stopped_for = excluded.stopped_for",
        params![adapter, row.next_at, row.failures, error, row.stopped_for],
    )?;
    Ok(())
}

// ---------- the pipeline ----------

/// Per-pod notification state kept in [`AppState`].
pub(crate) struct Notifier {
    pub(crate) tx: tokio::sync::broadcast::Sender<NotifyEvent>,
    /// Devices with an open events stream (they show notifications
    /// themselves, so they get no Web Push): device id -> open streams.
    live: std::sync::Mutex<HashMap<String, usize>>,
    poll_lock: tokio::sync::Mutex<()>,
    /// `(minute, deliveries in it)`.
    budget: std::sync::Mutex<(i64, u32)>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(crate::layouts::CHANNEL_CAPACITY).0,
            live: std::sync::Mutex::new(HashMap::new()),
            poll_lock: tokio::sync::Mutex::new(()),
            budget: std::sync::Mutex::new((0, 0)),
        }
    }
}

/// Held while a device's events stream is open ([`AppState::device_live`]).
pub struct LiveDevice {
    pod: Pod,
    device: String,
}

impl Drop for LiveDevice {
    fn drop(&mut self) {
        let mut live = self
            .pod
            .notifier
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(n) = live.get_mut(&self.device) {
            *n -= 1;
            if *n == 0 {
                live.remove(&self.device);
            }
        }
    }
}

/// The calling device ([`DEVICE_HEADER`]), when well-formed.
pub fn device_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(DEVICE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|d| valid_device_id(d))
        .map(str::to_owned)
}

impl AppState {
    /// Every adapter's channels: those declared at startup, then those
    /// declared now for live custom apps (decision `collection-reminders`).
    /// Reads the store (blocking).
    pub fn notification_channels(&self) -> Vec<NotificationChannel> {
        let mut out: Vec<NotificationChannel> = self
            .catalog()
            .adapters()
            .iter()
            .flat_map(|a| a.notification_channels())
            .collect();
        let senders: Vec<_> = self
            .catalog()
            .adapters()
            .iter()
            .filter(|a| a.notifies_layouts())
            .collect();
        if senders.is_empty() {
            return out;
        }
        let live: Vec<String> = match self.store().with_conn(crate::layouts::pod_layouts) {
            Ok(layouts) => layouts.into_iter().map(|a| a.id).collect(),
            Err(e) => {
                tracing::error!(error = %e, "the pod's layouts could not be read");
                return out;
            }
        };
        for a in senders {
            for c in a.layout_notification_channels(self.store(), self.catalog()) {
                let fits = live.contains(&c.layout)
                    && valid_channel_id(&c.id)
                    && !out.iter().any(|o| o.layout == c.layout && o.id == c.id);
                if fits {
                    out.push(c);
                } else {
                    tracing::warn!(adapter = a.name(), layout = %c.layout, channel = %c.id,
                        "layout channel dropped");
                }
            }
        }
        out
    }

    /// The layouts notifications may belong to: blueprints and the pod's
    /// live layouts.
    fn layout_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .catalog()
            .blueprints()
            .map(|a| a.id.to_owned())
            .collect();
        match self.store().with_conn(crate::layouts::pod_layouts) {
            Ok(layouts) => ids.extend(layouts.into_iter().map(|a| a.id)),
            Err(e) => tracing::error!(error = %e, "the pod's layouts could not be read"),
        }
        ids
    }

    /// Where the pod keeps its notifications: the first source that offers
    /// a store ([`crate::Source::notification_store`]), else the core's
    /// own tables.
    pub fn notification_store(&self) -> Arc<dyn NotificationStore> {
        self.catalog()
            .adapters()
            .iter()
            .find_map(|a| a.notification_store())
            .unwrap_or_else(|| Arc::new(CoreStore))
    }

    /// [`NotificationStore::fill_layouts`] with the pod's layouts as they
    /// are now (when the pod opens). Blocking.
    pub fn fill_notification_layouts(self: &Pod) -> rusqlite::Result<usize> {
        let opens = self
            .store()
            .with_conn(|c| crate::layouts::NotificationOpens::read(self.catalog(), c))?;
        self.notification_store().fill_layouts(self, &opens)
    }

    /// Every declared channel, and those switched off pod-wide (by the
    /// user, or by default). Tells the store which channels there are.
    /// Blocking.
    pub fn channel_states(
        self: &Pod,
    ) -> rusqlite::Result<(Vec<NotificationChannel>, HashSet<ChannelId>)> {
        let channels = self.notification_channels();
        let store = self.notification_store();
        store.channels_declared(self, &channels)?;
        let switches = store.switches(self)?;
        let hidden = channels
            .iter()
            .filter(|c| {
                !switches
                    .get(&(c.layout.clone(), c.id.clone()))
                    .copied()
                    .unwrap_or(c.default_enabled)
            })
            .map(|c| (c.layout.clone(), c.id.clone()))
            .collect();
        Ok((channels, hidden))
    }

    /// The unread counts of what is shown (switched-off channels left
    /// out). Blocking.
    fn unread_now(self: &Pod) -> rusqlite::Result<ReadEvent> {
        let (_, hidden) = self.channel_states()?;
        let (unread_total, unread_by_layout) =
            self.notification_store()
                .unread(self, &self.layout_ids(), &hidden)?;
        Ok(ReadEvent {
            unread_total,
            unread_by_layout,
        })
    }

    /// Tell every open events stream the unread counts now (a
    /// `notifications_read` event), after a store changed notifications
    /// itself: a row edited or deleted, a channel switched.
    pub async fn announce_unread(self: &Pod) {
        let p = self.clone();
        if let Ok(ev) = blocking(self, move |_| p.unread_now()).await {
            let _ = self.notifier.tx.send(NotifyEvent::Read(ev));
        }
    }

    /// The entities `subjects` (URIs) are gone: their notifications go
    /// too (decision `notifications-in-a-collection`). For sources whose
    /// things disappear upstream (a deleted post); a store that keeps its
    /// own rows may remove them as they go.
    pub async fn notifications_gone(self: &Pod, subjects: Vec<String>) -> Result<usize, ApiError> {
        if subjects.is_empty() {
            return Ok(0);
        }
        let p = self.clone();
        let n = blocking(self, move |_| {
            p.notification_store().remove_about(&p, &subjects)
        })
        .await?;
        if n > 0 {
            self.announce_unread().await;
        }
        Ok(n)
    }

    /// Every stored notification, newest first, switched off or not (for
    /// tooling and tests). Blocking.
    pub fn stored_notifications(self: &Pod) -> rusqlite::Result<Vec<Notification>> {
        self.notification_store().list(
            self,
            &ListQuery {
                before: None,
                layout: None,
                limit: usize::MAX - 1,
                layouts: Vec::new(),
                hidden: HashSet::new(),
            },
        )
    }

    pub fn subscribe_notifications(&self) -> tokio::sync::broadcast::Receiver<NotifyEvent> {
        self.notifier.tx.subscribe()
    }

    /// Mark `device` as having an open events stream until the guard drops.
    pub fn device_live(self: &Pod, device: &str) -> LiveDevice {
        *self
            .notifier
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(device.to_owned())
            .or_default() += 1;
        LiveDevice {
            pod: self.clone(),
            device: device.to_owned(),
        }
    }

    /// Whether `device` has an events stream open with a valid session (it
    /// hears arrivals live, so it gets no Web Push).
    pub fn device_is_live(&self, device: &str) -> bool {
        self.notifier
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(device)
    }

    /// Take one delivery from this minute's budget.
    fn take_push_budget(&self) -> bool {
        let minute = now_secs() / 60;
        let mut b = self
            .notifier
            .budget
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if b.0 != minute {
            *b = (minute, 0);
        }
        if b.1 >= PUSH_PER_MINUTE {
            return false;
        }
        b.1 += 1;
        true
    }

    /// Report notifications: store the new ones, announce them on open
    /// events streams and send them by Web Push. Items for an app or channel
    /// no adapter declared are dropped (logged). Returns the stored rows that
    /// were new (quiet ones included).
    pub async fn notify(
        self: &Pod,
        items: Vec<NewNotification>,
    ) -> Result<Vec<Notification>, ApiError> {
        Ok(self.notify_inner(items).await?.0)
    }

    /// [`Self::notify`], then wait until every Web Push delivery finished
    /// (tooling that exits right after, such as `server pods notify-test`).
    /// Returns the new rows and how many deliveries were attempted.
    pub async fn notify_and_deliver(
        self: &Pod,
        items: Vec<NewNotification>,
    ) -> Result<(Vec<Notification>, usize), ApiError> {
        let (rows, deliveries) = self.notify_inner(items).await?;
        let count = deliveries.len();
        for d in deliveries {
            let _ = d.await;
        }
        Ok((rows, count))
    }

    async fn notify_inner(
        self: &Pod,
        items: Vec<NewNotification>,
    ) -> Result<(Vec<Notification>, Vec<tokio::task::JoinHandle<()>>), ApiError> {
        let channels = self.notification_channels();
        let mut items: Vec<NewNotification> = items
            .into_iter()
            .filter(|n| {
                let known = channels
                    .iter()
                    .any(|c| c.layout == n.layout && c.id == n.channel);
                if !known {
                    tracing::warn!(app = %n.layout, channel = %n.channel, "notification for an undeclared channel dropped");
                }
                known && !n.dedupe_key.is_empty() && n.dedupe_key.len() <= 256
            })
            .map(|mut n| {
                n.title = truncate(&n.title, MAX_TITLE);
                n.body = truncate(&n.body, MAX_BODY);
                n.image_url = n.image_url.filter(|u| u.len() <= 1024);
                n.target_uri = n.target_uri.filter(|u| u.len() <= 512);
                n.group_key = n.group_key.map(|g| truncate(&g, 128));
                n
            })
            .collect();
        if items.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        items.sort_by_key(|n| n.created_at);
        let p = self.clone();
        let (inserted, hidden, unread_total) = blocking(self, move |s| {
            let opens = s.with_conn(|c| crate::layouts::NotificationOpens::read(p.catalog(), c))?;
            let items: Vec<ToStore> = items
                .into_iter()
                .map(|item| ToStore {
                    open: opens.open(&item.layout, item.target_uri.as_deref()),
                    item,
                })
                .collect();
            let (_, hidden) = p.channel_states()?;
            let store = p.notification_store();
            let inserted = store.insert(&p, &items)?;
            let (unread_total, _) = store.unread(&p, &p.layout_ids(), &hidden)?;
            Ok((inserted, hidden, unread_total))
        })
        .await?;
        // A switched-off channel's are stored (a layout may still show
        // them) but alert no one.
        let fresh: Vec<Notification> = inserted
            .iter()
            .filter(|(n, quiet)| {
                !quiet && !hidden.contains(&(n.channel_layout.clone(), n.channel.clone()))
            })
            .map(|(n, _)| n.clone())
            .collect();
        if !fresh.is_empty() {
            tracing::info!(count = fresh.len(), "new notifications");
        }
        for n in &fresh {
            let _ = self.notifier.tx.send(NotifyEvent::New(NotificationEvent {
                notification: n.clone(),
                unread_total,
            }));
        }
        let deliveries = if fresh.is_empty() {
            Vec::new()
        } else {
            self.push(fresh, unread_total).await
        };
        Ok((inserted.into_iter().map(|(n, _)| n).collect(), deliveries))
    }

    /// Send `fresh` to every device that wants it and has no stream open.
    async fn push(
        self: &Pod,
        fresh: Vec<Notification>,
        unread_total: u32,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        let mut deliveries = Vec::new();
        let Some(sender) = self.push_sender() else {
            return deliveries;
        };
        let ttl = self.session_ttl_secs();
        let targets = match blocking(self, move |s| s.with_conn(|c| push_targets_on(c, ttl))).await
        {
            Ok(t) => t,
            Err(_) => return deliveries,
        };
        for t in targets {
            if self.device_is_live(&t.device) {
                continue;
            }
            for n in &fresh {
                if !self.take_push_budget() {
                    tracing::warn!("push budget for this minute used up; delivery skipped");
                    return deliveries;
                }
                let payload = push_payload(n, unread_total);
                let (pod, sender, target) = (self.clone(), sender.clone(), t.clone());
                deliveries.push(tokio::spawn(async move {
                    let result = sender.send(&target.sub, &payload).await;
                    pod.record_delivery(&target.device, &target.sub.endpoint, result)
                        .await;
                }));
            }
        }
        deliveries
    }

    async fn record_delivery(
        self: &Pod,
        device: &str,
        endpoint: &str,
        result: Result<(), PushError>,
    ) {
        let (device, endpoint) = (device.to_owned(), endpoint.to_owned());
        let failed = result.as_ref().err().cloned();
        if let Some(e) = &failed {
            tracing::warn!(error = %e, "push delivery failed");
        }
        let _ = blocking(self, move |s| {
            s.with_conn(|c| {
                // Only touch the subscription the delivery went to (the
                // device may have re-registered meanwhile).
                let same = "id = ?1 AND push_endpoint = ?2";
                match failed {
                    None => c.execute(
                        &format!("UPDATE devices SET push_failures = 0 WHERE {same}"),
                        params![device, endpoint],
                    ),
                    Some(PushError::Gone | PushError::BadSubscription(_)) => c.execute(
                        &format!(
                            "UPDATE devices SET push_endpoint = NULL, push_p256dh = NULL,
                                 push_auth = NULL, push_failures = 0 WHERE {same}"
                        ),
                        params![device, endpoint],
                    ),
                    Some(PushError::Failed(_)) => c.execute(
                        &format!(
                            "UPDATE devices SET
                                 push_failures = push_failures + 1,
                                 push_endpoint = CASE WHEN push_failures + 1 >= ?3 THEN NULL ELSE push_endpoint END,
                                 push_p256dh = CASE WHEN push_failures + 1 >= ?3 THEN NULL ELSE push_p256dh END,
                                 push_auth = CASE WHEN push_failures + 1 >= ?3 THEN NULL ELSE push_auth END
                             WHERE {same}"
                        ),
                        params![device, endpoint, MAX_PUSH_FAILURES],
                    ),
                }
            })
        })
        .await;
    }

    /// Run every polling adapter that is due (one at a time, each within
    /// [`POLL_TIMEOUT`]), and return when the next one is due (unix seconds;
    /// `None`: nothing to poll, e.g. no device registered). Called by the host
    /// scheduler; overlapping calls return at once.
    pub async fn poll_due(self: &Pod) -> Option<i64> {
        let pod = self.clone();
        let keep = blocking(self, move |s| {
            Ok(pod.catalog().adapters().iter().any(|a| a.keeps_open(s)))
        })
        .await
        .unwrap_or(false);
        let idle = keep.then_some(KEEP_OPEN_WAKE);
        let pollers: Vec<_> = self
            .catalog()
            .adapters()
            .iter()
            .filter_map(|a| a.poll_interval().map(|i| (a.clone(), i)))
            .collect();
        if pollers.is_empty() {
            return idle;
        }
        let Ok(_guard) = self.notifier.poll_lock.try_lock() else {
            return Some(now_secs() + 30);
        };
        let ttl = self.session_ttl_secs();
        if !blocking(self, move |s| s.with_conn(|c| has_active_device_on(c, ttl)))
            .await
            .unwrap_or(false)
        {
            return idle;
        }
        let ctx = PollCtx {
            public_prefix: self.public_prefix().to_owned(),
        };
        let mut next: Option<i64> = None;
        for (adapter, interval) in pollers {
            let name = adapter.name();
            let Ok(mut row) = blocking(self, move |s| s.with_conn(|c| poll_row_on(c, name))).await
            else {
                continue;
            };
            if row.stopped_for.is_some() {
                continue;
            }
            let now = now_secs();
            if row.next_at > now {
                next = Some(next.map_or(row.next_at, |n| n.min(row.next_at)));
                continue;
            }
            let outcome =
                match tokio::time::timeout(POLL_TIMEOUT, adapter.poll_notifications(self, &ctx))
                    .await
                {
                    Ok(o) => o,
                    Err(_) => PollOutcome {
                        status: PollStatus::Transient("timed out".into()),
                        ..Default::default()
                    },
                };
            if let Err(e) = self.notify(outcome.new).await {
                tracing::error!(adapter = name, status = ?e.0, "storing polled notifications failed");
            }
            let now = now_secs();
            let interval = interval.as_secs().max(30) as i64;
            let error = match outcome.status {
                PollStatus::Ok => {
                    row.failures = 0;
                    row.next_at = now
                        + outcome
                            .next_in
                            .map_or(interval, |d| d.as_secs().max(10) as i64);
                    None
                }
                PollStatus::Transient(e) => {
                    row.failures += 1;
                    let backoff = (60i64 << (row.failures - 1).min(10)).min(MAX_BACKOFF_SECS);
                    row.next_at = now + backoff.max(interval);
                    tracing::warn!(adapter = name, failures = row.failures, error = %e, "notification poll failed");
                    Some(e)
                }
                PollStatus::SignInGone(sign_in) => {
                    tracing::info!(adapter = name, %sign_in, "notification polls stop until the sign-in is back");
                    row.stopped_for = Some(sign_in);
                    Some("sign-in gone".to_owned())
                }
            };
            if row.stopped_for.is_none() {
                next = Some(next.map_or(row.next_at, |n| n.min(row.next_at)));
            }
            let saved = row.clone();
            let _ = blocking(self, move |s| {
                s.with_conn(|c| save_poll_on(c, name, &saved, error.as_deref()))
            })
            .await;
        }
        next.or(idle)
    }

    /// When [`Self::poll_due`] has work next, without polling (`None`: no
    /// polling adapter, no active device, or every poll stopped).
    pub async fn next_wake(self: &Pod) -> Option<i64> {
        let names: Vec<&'static str> = self
            .catalog()
            .adapters()
            .iter()
            .filter(|a| a.poll_interval().is_some())
            .map(|a| a.name())
            .collect();
        if names.is_empty() {
            return None;
        }
        let ttl = self.session_ttl_secs();
        blocking(self, move |s| {
            s.with_conn(|c| {
                if !has_active_device_on(c, ttl)? {
                    return Ok(None);
                }
                let mut next: Option<i64> = None;
                for n in names {
                    let row = poll_row_on(c, n)?;
                    if row.stopped_for.is_none() {
                        next = Some(next.map_or(row.next_at, |x| x.min(row.next_at)));
                    }
                }
                Ok(next)
            })
        })
        .await
        .ok()
        .flatten()
    }
}

/// The Web Push plaintext for `n`, under [`crate::push::MAX_PLAINTEXT`]
/// (the body is shortened when needed).
fn push_payload(n: &Notification, unread_total: u32) -> Vec<u8> {
    let mut n = n.clone();
    loop {
        let bytes = serde_json::to_vec(&PushPayload {
            v: 1,
            event: NotificationEvent {
                notification: n.clone(),
                unread_total,
            },
        })
        .unwrap_or_default();
        if bytes.len() <= crate::push::MAX_PLAINTEXT || (n.body.is_empty() && n.image_url.is_none())
        {
            return bytes;
        }
        if n.body.is_empty() {
            n.image_url = None;
        } else {
            let keep = n.body.chars().count() / 2;
            n.body = truncate(&n.body, keep);
            if keep < 4 {
                n.body.clear();
            }
        }
    }
}

// ---------- routes ----------

pub(crate) fn router() -> Router<Pod> {
    Router::new()
        .route("/v1/notifications", get(list))
        .route("/v1/notifications/read", post(mark_read))
        .route("/v1/notifications/channels", get(channels))
        .route(
            "/v1/devices/{id}",
            put(register_device).delete(delete_device),
        )
        .route("/v1/devices/{id}/channels", put(set_channels))
        .route("/v1/push/key", get(push_key))
}

/// The caller's session, or the usual 401.
async fn session_of(state: &Pod, headers: &HeaderMap) -> Result<crate::Session, Box<Response>> {
    match state.caller(headers).await {
        Ok(Caller::Signed(s)) => Ok(s),
        Ok(other) => Err(Box::new(match other {
            // Notifications need a pod session, not an app sign-in: a
            // missing bearer is 401 like any private route.
            Caller::Anonymous => crate::session::unauthorized(),
            c => sign_in_required(&c),
        })),
        Err(e) => Err(Box::new(e.into_response())),
    }
}

fn bad_request(detail: &str) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_request", "detail": detail }),
    )
}

/// `GET /v1/notifications?limit=0..=100 (50)&before=<id>&layout=<layout id>`:
/// newest first, with unread counts (`limit=0`: counts only). Switched-off
/// channels are left out of both.
async fn list(
    State(state): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = session_of(&state, &headers).await {
        return *r;
    }
    let limit = match q.get("limit").map(|v| v.trim().parse::<usize>()) {
        None => DEFAULT_LIMIT,
        Some(Ok(n)) if n <= MAX_LIMIT => n,
        Some(_) => return bad_request("limit is 0..=100"),
    };
    let before = match q.get("before").map(|v| v.trim().to_owned()) {
        None => None,
        Some(b) if !b.is_empty() && b.len() <= 64 => Some(b),
        Some(_) => return bad_request("before is a notification id"),
    };
    let layout = q.get("layout").cloned();
    let layouts = state.layout_ids();
    if let Some(a) = &layout
        && !layouts.contains(a)
    {
        return json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "layout_not_found" }),
        );
    }
    let p = state.clone();
    let result = blocking(&state, move |_| {
        let (_, hidden) = p.channel_states()?;
        let store = p.notification_store();
        let (total, by) = store.unread(&p, &layouts, &hidden)?;
        let mut rows = if limit == 0 {
            Vec::new()
        } else {
            store.list(
                &p,
                &ListQuery {
                    before,
                    layout,
                    limit,
                    layouts,
                    hidden,
                },
            )?
        };
        // The layout each opens, where the store doesn't say.
        if rows.iter().any(|n| n.layout.is_none()) {
            let opens = p
                .store()
                .with_conn(|c| crate::layouts::NotificationOpens::read(p.catalog(), c))?;
            for n in rows.iter_mut().filter(|n| n.layout.is_none()) {
                n.layout = Some(opens.open(&n.channel_layout, n.target_uri.as_deref()));
            }
        }
        Ok((rows, total, by))
    })
    .await;
    let (mut rows, unread_total, unread_by_layout) = match result {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_before = if more {
        rows.last().map(|n| n.id.clone())
    } else {
        None
    };
    Json(NotificationList {
        notifications: rows,
        unread_total,
        unread_by_layout,
        next_before,
    })
    .into_response()
}

/// `POST /v1/notifications/read {"ids": ["12", …]}` or `{"all": true,
/// "layout"?: "<layout id>"}` -> `{unread_total, unread_by_layout}`; announced as a
/// `notifications_read` event on the pod's streams.
async fn mark_read(State(state): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = session_of(&state, &headers).await {
        return *r;
    }
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let ids: Option<Vec<String>> = match payload.get("ids") {
        None => None,
        Some(Value::Array(a)) if a.len() <= 500 => {
            let parsed: Option<Vec<String>> = a
                .iter()
                .map(|v| match v {
                    Value::String(s) if !s.is_empty() && s.len() <= 64 => Some(s.clone()),
                    Value::Number(n) => n.as_i64().map(|n| n.to_string()),
                    _ => None,
                })
                .collect();
            match parsed {
                Some(p) => Some(p),
                None => return bad_request("ids are notification ids"),
            }
        }
        Some(_) => return bad_request("ids is a list of up to 500 ids"),
    };
    let all = payload.get("all").and_then(Value::as_bool) == Some(true);
    let layout = payload
        .get("layout")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let sel = match ids {
        Some(ids) => ReadSel::Ids(ids),
        None if all => ReadSel::All(layout),
        None => return bad_request("send ids or all: true"),
    };
    let p = state.clone();
    let result = blocking(&state, move |_| {
        p.notification_store().mark_read(&p, &sel)?;
        p.unread_now()
    })
    .await;
    match result {
        Ok(ev) => {
            let _ = state.notifier.tx.send(NotifyEvent::Read(ev.clone()));
            Json(ev).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// `GET /v1/notifications/channels` -> `{"channels": [NotificationChannel +
/// "enabled"]}`: every layout's channels and whether each is on (for the
/// whole pod; decision `notifications-in-a-collection`).
async fn channels(State(state): State<Pod>, headers: HeaderMap) -> Response {
    if let Err(r) = session_of(&state, &headers).await {
        return *r;
    }
    let p = state.clone();
    let (channels, hidden) = match blocking(&state, move |_| p.channel_states()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let list: Vec<Value> = channels
        .into_iter()
        .map(|c| {
            let enabled = !hidden.contains(&(c.layout.clone(), c.id.clone()));
            let mut v = serde_json::to_value(&c).unwrap_or_default();
            v["enabled"] = json!(enabled);
            v
        })
        .collect();
    Json(json!({ "channels": list })).into_response()
}

/// `PUT /v1/devices/{id} {"platform": "android"|…, "name"?: "…", "push"?:
/// {"endpoint", "p256dh", "auth"} | null}`: register (or refresh) the calling
/// device under the caller's session. Without `push` the device's
/// subscription is kept; `"push": null` removes it.
async fn register_device(
    State(state): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let session = match session_of(&state, &headers).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if !valid_device_id(&id) {
        return bad_request("device id is 8-64 letters, digits, '-' or '_'");
    }
    let payload = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let platform = payload
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if platform.is_empty() || platform.len() > 32 {
        return bad_request("platform is 1-32 characters");
    }
    let name = truncate(
        payload.get("name").and_then(Value::as_str).unwrap_or(""),
        64,
    );
    // None: keep; Some(None): remove; Some(Some(sub)): set.
    let push: Option<Option<Subscription>> = match payload.get("push") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(p) => {
            let field = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_owned);
            let (Some(endpoint), Some(p256dh), Some(auth)) =
                (field("endpoint"), field("p256dh"), field("auth"))
            else {
                return bad_request("push needs endpoint, p256dh and auth");
            };
            let Some(sender) = state.push_sender() else {
                return json_error(StatusCode::CONFLICT, json!({ "error": "push_unavailable" }));
            };
            if !sender.endpoint_allowed(&endpoint) {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "push_endpoint_not_allowed" }),
                );
            }
            let sub = Subscription {
                endpoint,
                p256dh,
                auth,
            };
            if let Err(e) = crate::push::PushSender::check_subscription(&sub) {
                return bad_request(&e);
            }
            Some(Some(sub))
        }
    };
    let (platform, hash) = (platform.to_owned(), session.token_hash.clone());
    let result = blocking(&state, move |s| {
        s.transaction(|tx| {
            let now = now_secs();
            tx.execute(
                "INSERT INTO devices (id, token_hash, platform, name, created_at, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(id) DO UPDATE SET token_hash = excluded.token_hash,
                     platform = excluded.platform, name = excluded.name,
                     last_seen = excluded.last_seen",
                params![id, hash, platform, name, now],
            )?;
            match push {
                None => {}
                Some(None) => {
                    tx.execute(
                        "UPDATE devices SET push_endpoint = NULL, push_p256dh = NULL,
                             push_auth = NULL, push_failures = 0 WHERE id = ?1",
                        [&id],
                    )?;
                }
                Some(Some(sub)) => {
                    tx.execute(
                        "UPDATE devices SET push_endpoint = ?2, push_p256dh = ?3,
                             push_auth = ?4, push_failures = 0 WHERE id = ?1",
                        params![id, sub.endpoint, sub.p256dh, sub.auth],
                    )?;
                }
            }
            prune_devices_on(tx)?;
            tx.execute(
                "DELETE FROM devices WHERE id IN (SELECT id FROM devices
                     ORDER BY last_seen DESC LIMIT -1 OFFSET ?1)",
                [MAX_DEVICES],
            )?;
            let has_push: bool = tx.query_row(
                "SELECT push_endpoint IS NOT NULL FROM devices WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )?;
            Ok(has_push)
        })
    })
    .await;
    match result {
        Ok(has_push) => Json(json!({ "ok": true, "push": has_push })).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `DELETE /v1/devices/{id}`: forget the device (sign-out on that device).
async fn delete_device(
    State(state): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = session_of(&state, &headers).await {
        return *r;
    }
    let result = blocking(&state, move |s| {
        s.transaction(|tx| {
            let n = tx.execute("DELETE FROM devices WHERE id = ?1", [&id])?;
            tx.execute(
                "DELETE FROM device_channel_prefs WHERE device_id = ?1",
                [&id],
            )?;
            Ok(n > 0)
        })
    })
    .await;
    match result {
        Ok(deleted) => Json(json!({ "ok": true, "deleted": deleted })).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `PUT /v1/devices/{id}/channels {"<layout>.<channel>": bool, …}`: turn
/// channels on or off, for the whole pod (decision
/// `notifications-in-a-collection`; the path names the calling device, as
/// before). Unknown channels are refused.
async fn set_channels(
    State(state): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = session_of(&state, &headers).await {
        return *r;
    }
    let payload = match parse_json_body(&headers, &body) {
        Ok(Value::Object(m)) => m,
        Ok(_) => return bad_request("send an object of \"<app>.<channel>\": bool"),
        Err(e) => return e.into_response(),
    };
    let known = state.notification_channels();
    let mut choices = Vec::new();
    for (key, v) in payload {
        let Some(enabled) = v.as_bool() else {
            return bad_request("values are true or false");
        };
        let Some(c) = known.iter().find(|c| c.key() == key) else {
            return json_error(
                StatusCode::BAD_REQUEST,
                json!({ "error": "unknown_channel", "channel": key }),
            );
        };
        choices.push((c.layout.clone(), c.id.clone(), enabled));
    }
    let p = state.clone();
    let result = blocking(&state, move |s| {
        let exists = s.with_conn(|c| {
            Ok::<bool, rusqlite::Error>(
                c.query_row("SELECT 1 FROM devices WHERE id = ?1", [&id], |_| Ok(()))
                    .optional()?
                    .is_some(),
            )
        })?;
        if !exists {
            return Ok(false);
        }
        p.notification_store().channels_declared(&p, &known)?;
        p.notification_store().set_switches(&p, &choices)?;
        Ok(true)
    })
    .await;
    match result {
        Ok(true) => {
            state.announce_unread().await;
            Json(json!({ "ok": true })).into_response()
        }
        Ok(false) => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "device_not_found" }),
        ),
        Err(e) => e.into_response(),
    }
}

/// `GET /v1/push/key` -> `{"vapid_public_key": "<base64url>"}`, or 404
/// `push_unavailable` when the server sends no Web Push.
async fn push_key(State(state): State<Pod>, headers: HeaderMap) -> Response {
    if let Err(r) = session_of(&state, &headers).await {
        return *r;
    }
    match state.push_sender() {
        Some(s) => Json(json!({ "vapid_public_key": s.public_key() })).into_response(),
        None => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "push_unavailable" }),
        ),
    }
}

/// A shared push sender for every pod of a server.
pub type SharedPush = Arc<crate::push::PushSender>;
