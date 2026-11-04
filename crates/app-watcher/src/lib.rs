//! The Events app (app id `watcher`; decision `collection-fillers`): every
//! upcoming event of the websites the user watches, the ones an alert
//! matches first, and a notification for each match.
//!
//! It was the Watcher (decision `watcher`), a source with its own tables,
//! extractors and rules. Now the data is the Collections source's: the
//! system collection `events` ([`events`]), with one fetch filler per
//! website (`extract: events`, or `llm`) and alerts for the rules. Claude
//! adds and changes sites and alerts with the Collections MCP tools. The app
//! keeps its id, so it stays where people pinned it, and its notification
//! channel (`matches`), so their choice for it holds. When a pod starts, its
//! Watcher sites and rules move into `events` once ([`events::import`]).
//!
//! The Watcher's routes and feeds stay declared for its older layout
//! versions (0001, 0002), and answer that it is gone; its tables stay,
//! unread.

pub mod events;
pub mod legacy;

pub use web_extract::{html, when};

use std::sync::Arc;

use axum::{
    Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::layouts::LayoutSource;
use pod_core::store::Migration;
use pod_core::{
    AppAdapter, AppInfo, AuthPolicy, BoxFuture, EntityDoc, FeedSpec, Hydration, PathSpec, Pod,
    RequestCtx, SourceManifest,
};
use protocol::{DataBundle, Importance, NotificationChannel};
use serde_json::json;

/// The adapter name: the prefix of its old tables and routes, and the
/// source id of its old feeds.
pub const NAME: &str = "watcher";
/// The app.
pub const APP_ID: &str = "watcher";
/// Its notification channel (alerts on `events` notify on it).
pub const CHANNEL: &str = "matches";

pub const MATCHES_FEED: &str = "matches";
pub const TARGETS_FEED: &str = "targets";

/// The Events app: the `events` collection.
pub const APP: AppInfo = AppInfo {
    id: APP_ID,
    title: "Events",
    icon: "dashboard",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[(app_collections::NAME, events::COLLECTION)],
};

const GONE: &str = "Retired: the Watcher became the Events app (the events collection).";

/// The Watcher's old routes and feeds, declared for its older layout
/// versions; every one answers that it is gone.
pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Watcher (retired)",
    sign_in: None,
    entity_schemes: &["watcher"],
    feeds: &[
        FeedSpec {
            name: MATCHES_FEED,
            query_keys: &["watcher:matches"],
            doc: GONE,
        },
        FeedSpec {
            name: TARGETS_FEED,
            query_keys: &["watcher:targets", "watcher:rules"],
            doc: GONE,
        },
    ],
    data_paths: &[
        PathSpec {
            pattern: "/v1/watcher/screens/event?id={id}",
            query_keys: &["watcher:event:{id}"],
            doc: GONE,
        },
        PathSpec {
            pattern: "/v1/watcher/screens/target?id={id}",
            query_keys: &[
                "watcher:target:{id}",
                "watcher:target:{id}:rules",
                "watcher:target:{id}:events",
            ],
            doc: GONE,
        },
    ],
    actions: &[
        PathSpec {
            pattern: "/v1/watcher/targets/create",
            query_keys: &[],
            doc: GONE,
        },
        PathSpec {
            pattern: "/v1/watcher/targets/update",
            query_keys: &[],
            doc: GONE,
        },
        PathSpec {
            pattern: "/v1/watcher/targets/refresh",
            query_keys: &[],
            doc: GONE,
        },
        PathSpec {
            pattern: "/v1/watcher/rules/create",
            query_keys: &[],
            doc: GONE,
        },
        PathSpec {
            pattern: "/v1/watcher/rules/update",
            query_keys: &[],
            doc: GONE,
        },
        PathSpec {
            pattern: "/v1/watcher/events/seen",
            query_keys: &[],
            doc: GONE,
        },
    ],
    entities: &[EntityDoc {
        uri: "watcher://{kind}/{id}",
        doc: GONE,
        fields: &[],
    }],
    dev: false,
};

// `FILES`: every `layouts/watcher/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The Events adapter.
#[derive(Debug, Default)]
pub struct WatcherAdapter {
    /// Imported sites may be on private addresses (tests; the operator's
    /// `CONSTRUCT_FILL_PRIVATE_HOSTS=1`).
    allow_private_hosts: bool,
}

impl WatcherAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Let imported sites be on private hosts.
    pub fn with_private_hosts(mut self, allow: bool) -> Self {
        self.allow_private_hosts = allow;
        self
    }
}

const INSTRUCTIONS: &str = "The Events app (app watcher) shows the pod's events collection: every \
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

impl AppAdapter for WatcherAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn apps(&self) -> Vec<AppInfo> {
        vec![APP]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }

    fn router(&self) -> Router<Pod> {
        Router::new()
            .route("/v1/watcher/screens/event", get(gone))
            .route("/v1/watcher/screens/target", get(gone))
            .route("/v1/watcher/targets/create", post(gone))
            .route("/v1/watcher/targets/update", post(gone))
            .route("/v1/watcher/targets/refresh", post(gone))
            .route("/v1/watcher/rules/create", post(gone))
            .route("/v1/watcher/rules/update", post(gone))
            .route("/v1/watcher/events/seen", post(gone))
    }

    fn migrations(&self) -> &'static [Migration] {
        legacy::MIGRATIONS
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
        Box::pin(async move {
            match feed {
                // An older layout version: nothing to show.
                MATCHES_FEED | TARGETS_FEED => Hydration::Data(DataBundle::default()),
                other => pod_core::unknown_feed(NAME, other),
            }
        })
    }

    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        let allow = self.allow_private_hosts;
        let pod = Arc::clone(pod);
        Box::pin(async move {
            match pod
                .blocking(move |s| {
                    events::prepare(s, allow).map_err(|e| {
                        tracing::error!(error = %e, "the events collection could not be prepared");
                        e
                    })
                })
                .await
            {
                Ok(moved) if !moved.is_empty() => {
                    tracing::info!(sites = moved.len(), "the Watcher's sites moved to Events");
                    app_collections::fill::spawn(&pod, events::COLLECTION, None, false);
                }
                _ => {}
            }
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

/// Every old Watcher route: 410, it is gone.
async fn gone() -> Response {
    pod_core::json_error(
        StatusCode::GONE,
        json!({ "error": "gone", "detail": "The Watcher became the Events app; update the app to see your events." }),
    )
    .into_response()
}
