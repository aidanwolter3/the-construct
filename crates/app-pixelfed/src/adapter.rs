//! [`PixelfedAdapter`]: the `pixelfed_home` app and the `pixelfed` sign-in.

use std::sync::{Arc, RwLock};

use axum::Router;
use pod_core::layouts::LayoutSource;
use pod_core::{AppAdapter, AppInfo, BoxFuture, Hydration, Pod, RequestCtx, SignInProvider};
use protocol::WorkspaceSpec;

use crate::config::PixelfedConfig;
use crate::upstream::Upstream;
use crate::{MediaAllowlist, apps};

/// The sign-in this adapter provides (recorded on sessions).
pub const SIGN_IN: &str = "pixelfed";

/// What the adapter's handlers read per request: the config and what is
/// built from it.
pub struct Runtime {
    pub config: PixelfedConfig,
    /// [`PixelfedConfig::media_allowlist`], parsed once.
    pub media_allow: MediaAllowlist,
    pub upstream: Upstream,
}

impl Runtime {
    fn new(config: PixelfedConfig) -> Self {
        let media_allow = config.media_allowlist();
        Self {
            upstream: Upstream::new(&config.instance_url, media_allow.clone()),
            media_allow,
            config,
        }
    }
}

/// The Pixelfed adapter (single tenant): sign-in with PKCE (the pod keeps the
/// upstream token, D17), the home feed, likes, bookmarks, comments and the
/// media proxy.
pub struct PixelfedAdapter {
    runtime: RwLock<Arc<Runtime>>,
    /// Serializes upstream token refreshes (Passport rotates the refresh
    /// token, so two concurrent refreshes would revoke each other).
    pub(crate) refresh: tokio::sync::Mutex<()>,
}

impl PixelfedAdapter {
    pub fn new(config: PixelfedConfig) -> Self {
        Self {
            runtime: RwLock::new(Arc::new(Runtime::new(config))),
            refresh: tokio::sync::Mutex::new(()),
        }
    }

    /// The current runtime (cheap: one `Arc` clone).
    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn config(&self) -> PixelfedConfig {
        self.runtime().config.clone()
    }

    /// Point the adapter at another instance (tests that start the fake
    /// upstream after the pod's state). Requests already running keep the
    /// old runtime.
    pub fn configure(&self, config: PixelfedConfig) {
        *self.runtime.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(Runtime::new(config));
    }
}

impl AppAdapter for PixelfedAdapter {
    fn name(&self) -> &'static str {
        "pixelfed"
    }

    fn apps(&self) -> Vec<AppInfo> {
        vec![apps::HOME]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        apps::HOME_FILES
    }

    fn legacy_files(&self) -> &'static [(&'static str, u32)] {
        apps::HOME_LEGACY
    }

    fn router(&self) -> Router<Pod> {
        crate::routes::router()
    }

    fn sign_ins(&self) -> Vec<SignInProvider> {
        vec![SignInProvider { id: SIGN_IN }]
    }

    fn signed_out(&self, _sign_in: &str, app: &AppInfo, _req: &RequestCtx) -> WorkspaceSpec {
        crate::layout::signed_out(&self.runtime().config.instance_url, app.id, app.title)
    }

    fn hydrate<'a>(
        &'a self,
        pod: &'a Pod,
        _app_id: &'a str,
        req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(crate::routes::home_feed(self, pod, req))
    }
}
