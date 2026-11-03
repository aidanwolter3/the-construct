//! [`StaticAdapter`]: a layout-only app (layout files plus fixed data).

use axum::Router;
use protocol::{DataBundle, WorkspaceSpec};

use crate::adapter::{AppAdapter, AppInfo, BoxFuture, Hydration, Pod, RequestCtx};
use crate::layouts::LayoutSource;
use crate::source::{FeedSpec, SourceManifest};

/// The one feed of a [`StaticAdapter`]: list `(name, FEED)` in the app's
/// [`AppInfo::feeds`].
pub const FEED: &str = "all";

static FEEDS: &[FeedSpec] = &[FeedSpec {
    name: FEED,
    query_keys: &[],
    doc: "The app's fixed data.",
}];

/// An app with no upstream: its layout files and one fixed bundle of
/// entities and queries, served to everyone (D40). Optional extra routes
/// (e.g. a stateless endpoint its buttons call) come with
/// [`Self::with_router`]. Its app lists `(name, FEED)` among its feeds.
pub struct StaticAdapter {
    name: &'static str,
    app: AppInfo,
    manifest: SourceManifest,
    files: &'static [LayoutSource],
    legacy: &'static [(&'static str, u32)],
    data: DataBundle,
    router: fn() -> Router<Pod>,
}

impl StaticAdapter {
    /// `name`: the adapter name ([`AppAdapter::name`]). `data`: the entities
    /// and queries every workspace fetch returns (its `workspace` and
    /// `version` are ignored: the layout comes from `files`).
    pub fn new(
        name: &'static str,
        app: AppInfo,
        files: &'static [LayoutSource],
        data: &WorkspaceSpec,
    ) -> Self {
        Self {
            name,
            app,
            manifest: SourceManifest {
                feeds: FEEDS,
                ..SourceManifest::new(app.title)
            },
            files,
            legacy: &[],
            data: DataBundle {
                entities: data.entities.clone(),
                queries: data.queries.clone(),
            },
            router: Router::new,
        }
    }

    /// What it offers layouts ([`AppAdapter::manifest`]); by default only
    /// the [`FEED`] feed, which answers the fixed data. Declare the routes
    /// of [`Self::with_router`] here. The manifest's feeds all answer the
    /// fixed data.
    pub fn with_manifest(mut self, manifest: SourceManifest) -> Self {
        self.manifest = manifest;
        self
    }

    /// See [`AppAdapter::legacy_files`].
    pub fn with_legacy_files(mut self, legacy: &'static [(&'static str, u32)]) -> Self {
        self.legacy = legacy;
        self
    }

    /// Extra routes for this app ([`AppAdapter::router`]).
    pub fn with_router(mut self, router: fn() -> Router<Pod>) -> Self {
        self.router = router;
        self
    }
}

impl AppAdapter for StaticAdapter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn apps(&self) -> Vec<AppInfo> {
        vec![self.app]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        self.files
    }

    fn legacy_files(&self) -> &'static [(&'static str, u32)] {
        self.legacy
    }

    fn router(&self) -> Router<Pod> {
        (self.router)()
    }

    fn manifest(&self) -> SourceManifest {
        self.manifest
    }

    fn feed<'a>(
        &'a self,
        _pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            if self.manifest.feed(feed).is_none() {
                return crate::unknown_feed(self.name, feed);
            }
            Hydration::Data(self.data.clone())
        })
    }
}
