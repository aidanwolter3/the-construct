//! Following the runner, one task per open pod: take a snapshot when the
//! stored state is missing or old, then follow the event stream from the
//! cursor, reconnecting with backoff. Each event is applied to the stored
//! state, its notifications sent and its task rows updated. While the runner
//! can't be reached, the last state stays and the feeds say it's stale.
//!
//! The task holds the pod only while it works on an answer, so a pod that
//! closes ends it. A sign-in or sign-out wakes it ([`Ctl::changed`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use pod_core::{AppState, Pod};
use tokio::sync::Notify;

use crate::cap::{self, CapError, Event, Runner, SseParser};
use crate::state::Effect;
use crate::{AgentsAdapter, alerts, db, tasks};

/// A snapshot is taken again when the last good answer is older than this.
pub const SNAPSHOT_AFTER: Duration = Duration::from_secs(600);
/// Waits between attempts, doubling from the first to the last.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// While signed out, how often to look again (a sign-in wakes it sooner).
const IDLE: Duration = Duration::from_secs(300);

/// One pod's follower state ([`pod_core::AppState::local`]).
#[derive(Debug, Default)]
pub struct Ctl {
    wake: Notify,
    generation: AtomicU64,
    running: AtomicBool,
}

impl Ctl {
    /// The runner sign-in changed: start over.
    pub fn changed(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.wake.notify_one();
    }
}

/// Start `pod`'s follower unless it runs.
pub fn spawn(pod: &Pod) {
    let ctl = pod.local::<Ctl>();
    if ctl.running.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        ctl.running.store(false, Ordering::SeqCst);
        return;
    };
    let weak = Arc::downgrade(pod);
    rt.spawn(async move {
        run(weak, Arc::clone(&ctl)).await;
        ctl.running.store(false, Ordering::SeqCst);
    });
}

enum Outcome {
    /// The pod closed.
    Gone,
    /// Signed in or out meanwhile.
    Changed,
    /// The stream ended cleanly.
    Ended,
    Failed(CapError),
}

async fn wait(ctl: &Ctl, d: Duration) {
    tokio::select! {
        _ = ctl.wake.notified() => {}
        _ = tokio::time::sleep(d) => {}
    }
}

fn adapter(pod: &Pod) -> Option<&AgentsAdapter> {
    pod.adapter::<AgentsAdapter>()
}

async fn run(weak: Weak<AppState>, ctl: Arc<Ctl>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let Some(pod) = weak.upgrade() else { return };
        let generation = ctl.generation.load(Ordering::SeqCst);
        let runner = pod.blocking(db::runner).await.ok().flatten();
        drop(pod);
        let Some(runner) = runner else {
            wait(&ctl, IDLE).await;
            continue;
        };
        match follow(&weak, &ctl, &runner, generation).await {
            Outcome::Gone => return,
            Outcome::Changed => backoff = BACKOFF_MIN,
            Outcome::Ended => {
                backoff = BACKOFF_MIN;
                wait(&ctl, BACKOFF_MIN).await;
            }
            Outcome::Failed(e) => {
                let Some(pod) = weak.upgrade() else { return };
                if ctl.generation.load(Ordering::SeqCst) == generation {
                    let msg = e.to_string();
                    let now = now_ms();
                    let marked = pod
                        .blocking(move |s| db::mark_unreachable(s, &msg, now))
                        .await;
                    if marked.is_ok() {
                        pod.publish_data_all(crate::NAME);
                    }
                    tracing::info!(error = %e, retry_in = ?backoff, "agent runner unreachable");
                }
                let wait_for = match adapter(&pod) {
                    Some(a) => backoff.min(a.config.backoff_max),
                    None => backoff,
                };
                drop(pod);
                wait(&ctl, wait_for).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Apply what an answer owes: notifications in the owner layout, then the
/// task rows. `seq` names the answer for dedupe keys.
pub async fn settle(pod: &Pod, effects: Vec<Effect>, seq: i64) {
    if effects.is_empty() {
        return;
    }
    let p = Arc::clone(pod);
    if let Ok(Some(app)) = pod.blocking(move |s| alerts::owner(s, p.catalog())).await {
        let items = alerts::notifications(&effects, &app, seq, pod_core::util::now_secs());
        if !items.is_empty()
            && let Err(e) = pod.notify(items).await
        {
            tracing::warn!(status = ?e.0, "agent notifications could not be stored");
        }
    }
    tasks::apply(pod, &effects).await;
}

async fn follow(weak: &Weak<AppState>, ctl: &Ctl, runner: &Runner, generation: u64) -> Outcome {
    let changed = || ctl.generation.load(Ordering::SeqCst) != generation;
    let Some(pod) = weak.upgrade() else {
        return Outcome::Gone;
    };
    let Some(a) = adapter(&pod) else {
        return Outcome::Gone;
    };
    let (clients, tasks_every) = (a.clients.clone(), a.config.tasks_every);
    let stored = pod.blocking(db::state).await.ok().flatten();
    let mut state = stored.as_ref().map(|(s, _)| s.clone()).unwrap_or_default();
    let fresh = stored.as_ref().is_some_and(|(s, h)| {
        s.cursor > 0
            && h.last_ok_at
                .is_some_and(|t| now_ms() - t < SNAPSHOT_AFTER.as_millis() as i64)
    });
    let mut data_rx = pod.subscribe_data();
    drop(pod);

    if !fresh {
        let snap = match cap::snapshot(&clients, runner).await {
            Ok(s) => s,
            Err(e) => return Outcome::Failed(e),
        };
        let Some(pod) = weak.upgrade() else {
            return Outcome::Gone;
        };
        if changed() {
            return Outcome::Changed;
        }
        let effects = state.apply_snapshot(&snap);
        let (st, now) = (state.clone(), now_ms());
        let _ = pod.blocking(move |s| db::save(s, &st, &[], now)).await;
        pod.publish_data_all(crate::NAME);
        settle(&pod, effects, state.cursor).await;
    }

    let mut resp = match cap::open_stream(&clients, runner, state.cursor).await {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(e),
    };
    {
        let Some(pod) = weak.upgrade() else {
            return Outcome::Gone;
        };
        if changed() {
            return Outcome::Changed;
        }
        let (st, now) = (state.clone(), now_ms());
        let _ = pod.blocking(move |s| db::save(s, &st, &[], now)).await;
        pod.publish_data_all(crate::NAME);
        tasks::launch_queued(&pod, &clients, runner).await;
    }
    let mut parser = SseParser::default();
    let mut tick = tokio::time::interval(tasks_every);
    tick.tick().await;
    loop {
        let mut launch = false;
        tokio::select! {
            chunk = resp.chunk() => {
                let bytes = match chunk {
                    Ok(Some(b)) => b,
                    Ok(None) => return Outcome::Ended,
                    Err(e) => {
                        return Outcome::Failed(CapError::Unreachable(if e.is_timeout() {
                            "the stream went quiet".into()
                        } else {
                            "the stream broke".into()
                        }))
                    }
                };
                let msgs = match parser.push(&bytes) {
                    Ok(m) => m,
                    Err(e) => return Outcome::Failed(CapError::Invalid(e)),
                };
                let events: Vec<Event> = msgs
                    .iter()
                    .filter_map(|m| serde_json::from_str(&m.data).ok())
                    .filter_map(|v| Event::parse(&v))
                    .filter(|e| e.seq > state.cursor)
                    .collect();
                if events.is_empty() {
                    continue;
                }
                let Some(pod) = weak.upgrade() else { return Outcome::Gone };
                if changed() {
                    return Outcome::Changed;
                }
                let mut owed = Vec::new();
                for e in &events {
                    let effects = state.apply(e);
                    if !effects.is_empty() {
                        owed.push((e.seq, effects));
                    }
                }
                let (st, now) = (state.clone(), now_ms());
                let saved = pod.blocking(move |s| db::save(s, &st, &events, now)).await;
                if saved.is_err() {
                    tracing::error!("the agent runner's state could not be saved");
                }
                pod.publish_data_all(crate::NAME);
                for (seq, effects) in owed {
                    settle(&pod, effects, seq).await;
                }
            }
            _ = ctl.wake.notified() => {
                if changed() {
                    return Outcome::Changed;
                }
            }
            _ = tick.tick() => launch = true,
            got = data_rx.recv() => match got {
                Ok(ev) => launch = ev.source == app_collections::NAME,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => launch = true,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Outcome::Gone,
            },
        }
        if launch {
            let Some(pod) = weak.upgrade() else {
                return Outcome::Gone;
            };
            tasks::launch_queued(&pod, &clients, runner).await;
        }
    }
}
