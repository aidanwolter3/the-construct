//! Dry-run writes (decision `journeys-and-dry-runs`): a journey walk
//! (`walk_journey`) asks what a tap's write *would* write without writing
//! it. The walk sends the write through the pod's own router, as a device
//! would, with a [`DryRun`] in the request's task; a source that says it can
//! ([`crate::Source::dry_runs`]) runs its real write path in a transaction it
//! always rolls back, first putting back the rows earlier steps of the walk
//! would have written ([`DryRun::overlay`]), and records the rows the write
//! leaves ([`DryRun::record`]). Nothing that runs after a commit runs
//! (fillers, model fields, reminders, alerts, events to devices, idempotency
//! records, agent links): there is no commit.
//!
//! A path no source dry-runs is never sent at all: the walk marks that step
//! `unverified`.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// A row a dry run wrote (and rolled back): its collection, id, its values
/// after the write (`None`: deleted), and its stored row, to put back in a
/// later dry run of the same walk.
#[derive(Debug, Clone)]
pub struct Row {
    /// The data set it is in (a source's name for it, e.g. a collection's).
    pub collection: String,
    pub id: String,
    /// Its entity URI, as the source serves it.
    pub uri: String,
    pub values: Option<serde_json::Map<String, Value>>,
    /// The stored row, as the source keeps it (opaque to the core).
    pub stored: Value,
}

/// One walk's dry runs: the rows its earlier writes would have written, and
/// what the current one writes.
#[derive(Debug, Default)]
pub struct DryRun {
    pub overlay: Vec<Row>,
    written: Mutex<Vec<Row>>,
    /// Every source list the write left, `(collection, ids)`, for the walk
    /// to show (a created row joins its list).
    lists: Mutex<Vec<(String, Vec<String>)>>,
}

impl DryRun {
    pub fn new(overlay: Vec<Row>) -> Arc<DryRun> {
        Arc::new(DryRun {
            overlay,
            ..DryRun::default()
        })
    }

    /// Note a row the write left.
    pub fn record(&self, row: Row) {
        let mut w = self.written.lock().unwrap_or_else(|e| e.into_inner());
        w.retain(|r| !(r.collection == row.collection && r.id == row.id));
        w.push(row);
    }

    /// Note a collection's live ids after the write.
    pub fn record_list(&self, collection: &str, ids: Vec<String>) {
        let mut l = self.lists.lock().unwrap_or_else(|e| e.into_inner());
        l.retain(|(c, _)| c != collection);
        l.push((collection.to_owned(), ids));
    }

    pub fn written(&self) -> Vec<Row> {
        self.written
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn lists(&self) -> Vec<(String, Vec<String>)> {
        self.lists.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

tokio::task_local! {
    pub static DRY_RUN: Arc<DryRun>;
}

thread_local! {
    static BLOCKING: RefCell<Option<Arc<DryRun>>> = const { RefCell::new(None) };
}

/// The dry run the current task's request is, if any.
pub fn current() -> Option<Arc<DryRun>> {
    DRY_RUN.try_with(Arc::clone).ok()
}

/// Holds a dry run on this (blocking) thread until dropped ([`enter`]).
pub struct Entered(Option<Arc<DryRun>>);

impl Drop for Entered {
    fn drop(&mut self) {
        let prev = self.0.take();
        BLOCKING.with(|g| *g.borrow_mut() = prev);
    }
}

/// Hold `d` on this thread while the guard lives ([`active`]).
pub fn enter(d: Option<Arc<DryRun>>) -> Entered {
    Entered(BLOCKING.with(|g| std::mem::replace(&mut *g.borrow_mut(), d)))
}

/// The dry run held on this thread ([`enter`]), else the current task's.
pub fn active() -> Option<Arc<DryRun>> {
    BLOCKING.with(|g| g.borrow().clone()).or_else(current)
}
