//! Where a request's time goes (issue #576): how long it waited for the
//! store's one connection ([`crate::store::Store`], behind a lock) against
//! how long it held it, per kind of request.
//!
//! Every HTTP request runs in a [`Call`] named after its route (`GET
//! /v1/construct/data/{id}`; `internal …` when the pod's own tools sent it;
//! `mcp <tool>` once an MCP call names its tool), every filler run in one
//! named `fill <collection>/<filler>`; store work outside any call counts
//! as `background`. The call travels to the blocking threads the request
//! runs store work on ([`crate::blocking`], [`spawn_blocking`]), so the
//! lock's every take is counted against it ([`record`]).
//!
//! Logged under the target `pod_timing`:
//!
//! - `slow request`: a call that took [`SLOW_CALL`] or longer, or waited
//!   [`SLOW_WAIT`] or longer for the lock, with its totals and its longest
//!   hold and where it was taken;
//! - `long store hold`: one take of the lock held [`LONG_HOLD`] or longer
//!   (whoever made everyone else wait), with the call and the code that
//!   took it;
//! - `store lock by kind`: about once a minute, per kind, the calls,
//!   takes, waiting and holding since the last summary.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A call this long or longer is logged.
pub const SLOW_CALL: Duration = Duration::from_millis(500);
/// A call that waited this long or longer for the lock, in all, is logged.
pub const SLOW_WAIT: Duration = Duration::from_millis(100);
/// One hold of the lock this long or longer is logged.
pub const LONG_HOLD: Duration = Duration::from_millis(250);
/// How often the per-kind summary is logged (at the first take after).
const SUMMARY_EVERY: Duration = Duration::from_secs(60);

/// The kind store work outside any call counts as.
const BACKGROUND: &str = "background";

/// One request's (or background job's) use of the store.
#[derive(Debug)]
pub struct Call {
    kind: Mutex<String>,
    started: Instant,
    takes: AtomicU64,
    wait_us: AtomicU64,
    hold_us: AtomicU64,
    max_hold_us: AtomicU64,
    max_hold_at: Mutex<Option<&'static Location<'static>>>,
}

impl Call {
    pub fn new(kind: impl Into<String>) -> Arc<Call> {
        Arc::new(Call {
            kind: Mutex::new(kind.into()),
            started: Instant::now(),
            takes: AtomicU64::new(0),
            wait_us: AtomicU64::new(0),
            hold_us: AtomicU64::new(0),
            max_hold_us: AtomicU64::new(0),
            max_hold_at: Mutex::new(None),
        })
    }

    pub fn kind(&self) -> String {
        self.kind.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Name the call more exactly (an MCP call, once its tool is known).
    pub fn set_kind(&self, kind: impl Into<String>) {
        *self.kind.lock().unwrap_or_else(|e| e.into_inner()) = kind.into();
    }

    fn add(&self, wait: Duration, hold: Duration, at: &'static Location<'static>) {
        self.takes.fetch_add(1, Ordering::Relaxed);
        self.wait_us.fetch_add(us(wait), Ordering::Relaxed);
        self.hold_us.fetch_add(us(hold), Ordering::Relaxed);
        if self.max_hold_us.fetch_max(us(hold), Ordering::Relaxed) < us(hold) {
            *self.max_hold_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
        }
    }

    /// The call's totals so far.
    pub fn totals(&self) -> Totals {
        Totals {
            takes: self.takes.load(Ordering::Relaxed),
            wait: Duration::from_micros(self.wait_us.load(Ordering::Relaxed)),
            hold: Duration::from_micros(self.hold_us.load(Ordering::Relaxed)),
            max_hold: Duration::from_micros(self.max_hold_us.load(Ordering::Relaxed)),
        }
    }

    /// Log the call if it was slow, and count it in its kind's summary.
    fn finish(&self) {
        let took = self.started.elapsed();
        let t = self.totals();
        let kind = self.kind();
        summary()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .call(&kind, took, &t);
        if took >= SLOW_CALL || t.wait >= SLOW_WAIT {
            let at = *self.max_hold_at.lock().unwrap_or_else(|e| e.into_inner());
            tracing::info!(
                target: "pod_timing",
                kind = %kind,
                total_ms = ms(took),
                lock_wait_ms = ms(t.wait),
                lock_hold_ms = ms(t.hold),
                lock_takes = t.takes,
                max_hold_ms = ms(t.max_hold),
                max_hold_at = %at.map(|l| l.to_string()).unwrap_or_default(),
                "slow request"
            );
        }
    }
}

/// What a call spent on the store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub takes: u64,
    pub wait: Duration,
    pub hold: Duration,
    pub max_hold: Duration,
}

fn us(d: Duration) -> u64 {
    d.as_micros().min(u64::MAX as u128) as u64
}

/// Milliseconds with a tenth, for logs and answers.
pub fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 10_000.0).round() / 10.0
}

tokio::task_local! {
    static CALL: Arc<Call>;
}

thread_local! {
    static BLOCKING: RefCell<Option<Arc<Call>>> = const { RefCell::new(None) };
}

/// The call the current task is part of, else the one this (blocking)
/// thread works for ([`enter`]). The task's comes first: a request the pod
/// sends itself from a blocking thread (a preview's reads) is a call of its
/// own.
pub fn current() -> Option<Arc<Call>> {
    CALL.try_with(Arc::clone)
        .ok()
        .or_else(|| BLOCKING.with(|g| g.borrow().clone()))
}

/// Run `fut` as a call of `kind`, logging it when it ends ([`Call::finish`]).
pub async fn scope<F: std::future::Future>(kind: impl Into<String>, fut: F) -> F::Output {
    let call = Call::new(kind);
    let out = CALL.scope(call.clone(), fut).await;
    call.finish();
    out
}

/// Holds a call on this (blocking) thread until dropped ([`enter`]).
pub struct Entered(Option<Arc<Call>>);

impl Drop for Entered {
    fn drop(&mut self) {
        let prev = self.0.take();
        BLOCKING.with(|g| *g.borrow_mut() = prev);
    }
}

/// Count this thread's store work against `call` until the guard drops.
pub fn enter(call: Option<Arc<Call>>) -> Entered {
    Entered(BLOCKING.with(|g| std::mem::replace(&mut *g.borrow_mut(), call)))
}

/// [`tokio::task::spawn_blocking`], counting the work against the current
/// call.
pub fn spawn_blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let call = current();
    tokio::task::spawn_blocking(move || {
        let _held = enter(call);
        f()
    })
}

/// One take of the store's lock: waited `wait` for it, held it `hold`,
/// taken at `at`.
pub fn record(wait: Duration, hold: Duration, at: &'static Location<'static>) {
    let call = current();
    let kind = match &call {
        Some(c) => {
            c.add(wait, hold, at);
            None
        }
        None => Some(BACKGROUND),
    };
    if hold >= LONG_HOLD {
        tracing::info!(
            target: "pod_timing",
            kind = %kind.map(str::to_owned).unwrap_or_else(|| call.as_ref().map(|c| c.kind()).unwrap_or_default()),
            hold_ms = ms(hold),
            wait_ms = ms(wait),
            at = %at,
            "long store hold"
        );
    }
    let mut s = summary().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = kind {
        s.take(k, wait, hold);
    }
    s.maybe_log();
}

/// The per-kind numbers since the last summary.
#[derive(Default)]
struct Summary {
    since: Option<Instant>,
    kinds: BTreeMap<String, KindTotals>,
}

#[derive(Default)]
struct KindTotals {
    calls: u64,
    slowest: Duration,
    takes: u64,
    wait: Duration,
    hold: Duration,
    max_hold: Duration,
}

impl Summary {
    fn call(&mut self, kind: &str, took: Duration, t: &Totals) {
        let k = self.kinds.entry(kind.to_owned()).or_default();
        k.calls += 1;
        k.slowest = k.slowest.max(took);
        k.takes += t.takes;
        k.wait += t.wait;
        k.hold += t.hold;
        k.max_hold = k.max_hold.max(t.max_hold);
    }

    fn take(&mut self, kind: &str, wait: Duration, hold: Duration) {
        let k = self.kinds.entry(kind.to_owned()).or_default();
        k.takes += 1;
        k.wait += wait;
        k.hold += hold;
        k.max_hold = k.max_hold.max(hold);
    }

    fn maybe_log(&mut self) {
        let since = *self.since.get_or_insert_with(Instant::now);
        if since.elapsed() < SUMMARY_EVERY {
            return;
        }
        for (kind, k) in std::mem::take(&mut self.kinds) {
            if k.takes == 0 && k.slowest < SLOW_CALL {
                continue;
            }
            tracing::info!(
                target: "pod_timing",
                kind = %kind,
                calls = k.calls,
                slowest_ms = ms(k.slowest),
                lock_takes = k.takes,
                lock_wait_ms = ms(k.wait),
                lock_hold_ms = ms(k.hold),
                max_hold_ms = ms(k.max_hold),
                window_s = since.elapsed().as_secs(),
                "store lock by kind"
            );
        }
        self.since = Some(Instant::now());
    }
}

fn summary() -> &'static Mutex<Summary> {
    static S: std::sync::OnceLock<Mutex<Summary>> = std::sync::OnceLock::new();
    S.get_or_init(Mutex::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_call_counts_its_blocking_work() {
        let call = Call::new("test");
        let c = call.clone();
        CALL.scope(c, async {
            spawn_blocking(|| {
                record(
                    Duration::from_millis(3),
                    Duration::from_millis(7),
                    Location::caller(),
                )
            })
            .await
            .unwrap();
            record(
                Duration::from_millis(1),
                Duration::from_millis(2),
                Location::caller(),
            );
        })
        .await;
        let t = call.totals();
        assert_eq!(t.takes, 2);
        assert_eq!(t.wait, Duration::from_millis(4));
        assert_eq!(t.hold, Duration::from_millis(9));
        assert_eq!(t.max_hold, Duration::from_millis(7));
        assert!(
            call.max_hold_at
                .lock()
                .unwrap()
                .is_some_and(|l| l.file().ends_with("timing.rs"))
        );
    }

    #[test]
    fn outside_a_call_nothing_is_counted_against_one() {
        assert!(current().is_none());
        record(Duration::ZERO, Duration::ZERO, Location::caller());
    }
}
