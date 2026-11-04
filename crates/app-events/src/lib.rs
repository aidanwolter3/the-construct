//! The Events app (decision `collection-fillers`): every upcoming event of
//! the websites the user watches, the ones an alert matches first, and a
//! notification for each match.
//!
//! The data is the Collections source's: the system collection `events`
//! ([`events`]), with one fetch filler per website (`extract: events`, or
//! `llm`) and alerts for the rules. Claude adds and changes sites and alerts
//! with the Collections MCP tools.

pub mod events;

use std::sync::Arc;

use pod_core::layouts::LayoutSource;
use pod_core::store::Migration;
use pod_core::{
    AppAdapter, AppInfo, AuthPolicy, BoxFuture, Hydration, Pod, RequestCtx, SourceManifest,
};
use protocol::{Importance, NotificationChannel};
use rusqlite::Transaction;

/// The adapter name.
pub const NAME: &str = "events";
/// The app.
pub const APP_ID: &str = "events";
/// Its notification channel (alerts on `events` notify on it).
pub const CHANNEL: &str = "matches";

/// The Events app: the `events` collection.
pub const APP: AppInfo = AppInfo {
    id: APP_ID,
    title: "Events",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[(app_collections::NAME, events::COLLECTION)],
};

/// The app's data comes from the Collections source; it declares none of
/// its own.
pub const MANIFEST: SourceManifest = SourceManifest::new("Events");

// `FILES`: every `layouts/*.json` of this crate, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The app's id and adapter before decision `rename-apps-and-folders`.
const OLD_NAME: &str = "watcher";

/// Append-only: never edit or reorder a step that has shipped.
pub static MIGRATIONS: &[Migration] = &[
    // 1 (decision `rename-apps-and-folders`): the app was `watcher`. Its
    // stored data moves to the new id, and the old Watcher's tables (read
    // once, long ago, when its sites moved into `events`) go.
    Migration::Rust(from_watcher),
];

fn from_watcher(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS watcher_matches;
         DROP TABLE IF EXISTS watcher_events;
         DROP TABLE IF EXISTS watcher_rules;
         DROP TABLE IF EXISTS watcher_targets;
         DELETE FROM pod_settings WHERE key = 'watcher.imported';",
    )?;
    tx.execute(
        "DELETE FROM adapter_migrations WHERE adapter = ?1",
        [OLD_NAME],
    )?;
    pod_core::store::rename_app_on(tx, OLD_NAME, APP_ID)
}

/// The Events adapter.
#[derive(Debug, Default)]
pub struct EventsAdapter;

impl EventsAdapter {
    pub fn new() -> Self {
        Self
    }
}

const INSTRUCTIONS: &str = "The Events app (app events) shows the pod's events collection: every \
upcoming event of the websites the user watches, matched ones first, with a notification for each \
match. A website is a filler of events and a rule is an alert, both changed with apply_change on \
collection events (describe_collection events shows them with their ids; collections_status shows each \
site's health). To watch a site: try it with collections_preview {url, extract: \"events\"} (an ics feed \
or a page with schema.org events; zone for times without an offset), then apply_change {op: set_filler, \
filler: {kind: \"fetch\", title, url, extract: \"events\", zone, map: {source_id: \"id\", title: \
\"title\", start: \"start\", end: \"end\", venue: \"venue\", description: \"description\", url: \"url\", \
tickets_url: \"tickets_url\"}, key: \"source_id\", upcoming: \"start\", every_minutes: 360}}. Add \
details: {link: \"url\", extract: \"events\", map: {description: \"description\"}, text: \"description\"} \
(and drop description from map) when the listing lacks what alerts need (lineups) and each event's \
page has it. A page without structured events: extract \"llm\" with fields [title, start, end, venue, \
description, url] and a hint (a model reads each changed page). Then a rule: apply_change {op: \
add_alert, filler: <the site's id>, any: [phrases], none?: [phrases], days?, within_days?}; each site has \
its own alerts. The events fields can't be renamed or removed.";

impl AppAdapter for EventsAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn apps(&self) -> Vec<AppInfo> {
        vec![APP]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }

    fn migrations(&self) -> &'static [Migration] {
        MIGRATIONS
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn feed<'a>(
        &'a self,
        _pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move { pod_core::unknown_feed(NAME, feed) })
    }

    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        let pod = Arc::clone(pod);
        Box::pin(async move {
            let _ = pod
                .blocking(|s| {
                    events::prepare(s).inspect_err(|e| {
                        tracing::error!(error = %e, "the events collection could not be prepared");
                    })
                })
                .await;
        })
    }

    fn notification_channels(&self) -> Vec<NotificationChannel> {
        vec![NotificationChannel {
            app_id: APP_ID.into(),
            id: CHANNEL.into(),
            title: "Matching events".into(),
            description: "A watched website lists an event one of your alerts matches.".into(),
            default_enabled: true,
            importance: Importance::default(),
        }]
    }

    fn mcp_instructions(&self) -> Option<&'static str> {
        Some(INSTRUCTIONS)
    }
}
