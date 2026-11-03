//! [`StaticAdapter`]: a layout-only app (layout files plus fixed data).

use axum::Router;
use protocol::{DataBundle, WorkspaceSpec};

use crate::adapter::{AppAdapter, AppInfo, BoxFuture, Hydration, Pod, RequestCtx};
use crate::layouts::LayoutSource;

/// An app with no upstream: its layout files and one fixed bundle of
/// entities and queries, served to everyone (D40). Optional extra routes
/// (e.g. a stateless endpoint its buttons call) come with
/// [`Self::with_router`].
pub struct StaticAdapter {
    name: &'static str,
    app: AppInfo,
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
            files,
            legacy: &[],
            data: DataBundle {
                entities: data.entities.clone(),
                queries: data.queries.clone(),
            },
            router: Router::new,
        }
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

    fn hydrate<'a>(
        &'a self,
        _pod: &'a Pod,
        _app_id: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move { Hydration::Data(self.data.clone()) })
    }
}
