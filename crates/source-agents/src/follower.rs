//! Following the runner, one task per pod (decision `agents-cap-v3`): take
//! a snapshot only when there is no cursor yet (or the runner says the
//! cursor is older than its log, 410), else catch up from the stored cursor
//! with the event log's pages (`/v3/events`), then follow the stream from
//! there, reconnecting with backoff. Each event's record goes into the
//! mirror ([`crate::state`]), its notifications are sent, and the cursor is
//! saved with it, so a gap of any length (within the log's 30 days) is
//! replayed and notified. The linked task rows follow their runs
//! ([`tasks::sync`]) and the pod's schedules fire ([`schedules::tick`]) on
//! every pass, connected or not. While the runner can't be reached, the
//! last state stays and the feeds say it's stale.
//!
//! It starts when the pod opens ([`crate::AgentsAdapter`]'s `resume`), and
//! a pod signed in to a runner is opened when the server starts and kept
//! open (`Source::keeps_open`), so it runs whether or not anyone uses
//! the pod. The task holds the pod only while it works on an answer, so a
//! pod that closes ends it. A sign-in or sign-out wakes it
//! ([`Ctl::changed`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use pod_core::{AppState, Pod};
use tokio::sync::Notify;

use crate::cap::{self, CapError, Clients, Event, Runner, SseParser};
use crate::schedules::{self, Notice};
use crate::state::{Effect, State};
use crate::{AgentsAdapter, alerts, db, tasks};

/// Waits between attempts, doubling from the first to the last.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// While signed out, how often to look again (a sign-in wakes it sooner).
const IDLE: Duration = Duration::from_secs(300);
/// The most pages read in one catch-up before following the stream (the
/// stream carries on from there).
const MAX_PAGES: usize = 200;

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
        let Some(runner) = runner else {
            // No runner: the schedules only move on.
            if let Some(a) = adapter(&pod) {
                let clients = a.clients.clone();
                schedules::tick(&pod, &clients, None, now_ms()).await;
            }
            drop(pod);
            wait(&ctl, IDLE).await;
            continue;
        };
        drop(pod);
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
                let (wait_for, clients) = match adapter(&pod) {
                    Some(a) => (backoff.min(a.config.backoff_max), Some(a.clients.clone())),
                    None => (backoff, None),
                };
                // The timer runs while the runner is away: occurrences
                // wait to be sent, and are missed after 2 h.
                if let Some(clients) = clients {
                    let pass = schedules::tick(&pod, &clients, Some(&runner), now_ms()).await;
                    settle(&pod, Vec::new(), pass.notices).await;
                }
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

/// Send the notifications an answer (and the timer) owe, in the owner
/// layout.
pub async fn settle(pod: &Pod, effects: Vec<Effect>, notices: Vec<Notice>) {
    if effects.is_empty() && notices.is_empty() {
        return;
    }
    let p = Arc::clone(pod);
    if let Ok(Some(app)) = pod.blocking(move |s| alerts::owner(s, p.catalog())).await {
        let items = alerts::notifications(&effects, &notices, &app, pod_core::util::now_secs());
        if !items.is_empty()
            && let Err(e) = pod.notify(items).await
        {
            tracing::warn!(status = ?e.0, "agent notifications could not be stored");
        }
    }
}

/// Save `state`, tell the clients and send what it owes.
async fn store(pod: &Pod, state: &State, effects: Vec<Effect>) {
    let (st, now) = (state.clone(), now_ms());
    if pod.blocking(move |s| db::save(s, &st, now)).await.is_err() {
        tracing::error!("the agent runner's state could not be saved");
    }
    pod.publish_data_all(crate::NAME);
    settle(pod, effects, Vec::new()).await;
}

/// Bring `state` up to the runner's log: a snapshot when there is no cursor
/// (or it is too old), else the log's pages from the cursor. Each page is
/// saved (with its cursor) before the next is read.
async fn catch_up(
    weak: &Weak<AppState>,
    clients: &Clients,
    runner: &Runner,
    state: &mut State,
    changed: &(dyn Fn() -> bool + Sync),
) -> Result<(), Outcome> {
    let mut snapshot = state.cursor <= 0;
    for _ in 0..MAX_PAGES {
        if snapshot {
            let snap = cap::snapshot(clients, runner)
                .await
                .map_err(Outcome::Failed)?;
            let pod = weak.upgrade().ok_or(Outcome::Gone)?;
            if changed() {
                return Err(Outcome::Changed);
            }
            let effects = state.apply_snapshot(&snap);
            store(&pod, state, effects).await;
            snapshot = false;
            continue;
        }
        let page = match cap::events(clients, runner, state.cursor, cap::PAGE_LIMIT).await {
            Ok(p) => p,
            Err(CapError::CursorTooOld) => {
                tracing::info!("the runner's log no longer reaches the cursor: a snapshot");
                snapshot = true;
                continue;
            }
            Err(e) => return Err(Outcome::Failed(e)),
        };
        let pod = weak.upgrade().ok_or(Outcome::Gone)?;
        if changed() {
            return Err(Outcome::Changed);
        }
        let events: Vec<Event> = page
            .events
            .into_iter()
            .filter(|e| e.seq > state.cursor)
            .collect();
        let effects = state.apply_events(&events, now_ms());
        state.cursor = state.cursor.max(page.next);
        store(&pod, state, effects).await;
        if !page.more {
            return Ok(());
        }
    }
    Ok(())
}

/// One pass of the work beside the stream: the linked tasks and the timer.
async fn pass(pod: &Pod, clients: &Clients, runner: &Runner, state: &mut State) {
    tasks::sync(pod, clients, runner, state).await;
    let pass = schedules::tick(pod, clients, Some(runner), now_ms()).await;
    let mut effects = Vec::new();
    for r in &pass.runs {
        effects.extend(state.apply_run(r));
    }
    if !pass.runs.is_empty() {
        let (st, now) = (state.clone(), now_ms());
        let _ = pod.blocking(move |s| db::save(s, &st, now)).await;
        pod.publish_data_all(crate::NAME);
    }
    settle(pod, effects, pass.notices).await;
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
    let mut state = stored.map(|(s, _)| s).unwrap_or_default();
    let mut data_rx = pod.subscribe_data();
    drop(pod);

    if let Err(o) = catch_up(weak, &clients, runner, &mut state, &changed).await {
        return o;
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
        store(&pod, &state, Vec::new()).await;
        pass(&pod, &clients, runner, &mut state).await;
    }
    let mut parser = SseParser::default();
    let mut tick = tokio::time::interval(tasks_every);
    tick.tick().await;
    loop {
        let mut again = false;
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
                let owed = state.apply_events(&events, now_ms());
                store(&pod, &state, owed).await;
                again = true;
            }
            _ = ctl.wake.notified() => {
                if changed() {
                    return Outcome::Changed;
                }
            }
            _ = tick.tick() => again = true,
            got = data_rx.recv() => match got {
                Ok(ev) => again = ev.source == source_collections::NAME,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => again = true,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Outcome::Gone,
            },
        }
        if again {
            let Some(pod) = weak.upgrade() else {
                return Outcome::Gone;
            };
            pass(&pod, &clients, runner, &mut state).await;
        }
    }
}
