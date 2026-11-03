//! [`PixelfedAdapter`]: the `pixelfed_home` app and the `pixelfed` sign-in.

use std::sync::{Arc, RwLock};

use axum::Router;
use pod_core::layouts::LayoutSource;
use pod_core::{AppAdapter, AppInfo, BoxFuture, Hydration, Pod, RequestCtx, SignInProvider};
use protocol::WorkspaceSpec;

use crate::config::{Instance, PixelfedConfig};
use crate::upstream::{Clients, Upstream};
use crate::{MediaPolicy, apps};

/// The sign-in this adapter provides (recorded on sessions).
pub const SIGN_IN: &str = "pixelfed";

/// What the adapter's handlers read per request: the config and what is
/// built from it.
pub struct Runtime {
    pub config: PixelfedConfig,
    /// [`PixelfedConfig::media_allowlist`], parsed once, plus any public
    /// HTTPS media when users may sign in to any instance.
    pub media: MediaPolicy,
    pub clients: Clients,
}

impl Runtime {
    fn new(config: PixelfedConfig) -> Self {
        let media = MediaPolicy {
            allow: config.media_allowlist(),
            any_public_https: config.any_instance,
        };
        Self {
            clients: Clients::new(media.clone()),
            media,
            config,
        }
    }

    /// The API client for `instance`.
    pub fn api(&self, instance: &Instance) -> Upstream {
        self.clients.api(instance)
    }

    /// [`PixelfedConfig::resolve`] in a pod whose settings may forbid other
    /// instances than the default (`others` false: temporary pods, D46).
    pub fn resolve(&self, typed: &str, others: bool) -> Option<Instance> {
        let instance = self.config.resolve(typed)?;
        (others || instance.url == self.config.instance_url).then_some(instance)
    }

    /// The media policy of a pod: without other instances, only the
    /// configured origins (no public HTTPS media from anywhere).
    pub fn media_for(&self, others: bool) -> MediaPolicy {
        MediaPolicy {
            allow: self.media.allow.clone(),
            any_public_https: others && self.media.any_public_https,
        }
    }
}

/// The Pixelfed adapter (one owner per instance; each session uses the
/// instance it signed in to, D45): sign-in with PKCE (the pod keeps the
/// upstream token, D17), the home feed, likes, bookmarks, comments and the
/// media proxy.
pub struct PixelfedAdapter {
    runtime: RwLock<Arc<Runtime>>,
}

impl PixelfedAdapter {
    pub fn new(config: PixelfedConfig) -> Self {
        Self {
            runtime: RwLock::new(Arc::new(Runtime::new(config))),
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

    fn migrations(&self) -> &'static [pod_core::store::Migration] {
        crate::db::MIGRATIONS
    }

    fn public_paths(&self) -> &'static [&'static str] {
        // Images load without the bearer; keys are opaque hashes of public
        // upstream media (D17).
        &["/v1/pixelfed/media/"]
    }

    fn sign_ins(&self) -> Vec<SignInProvider> {
        vec![SignInProvider { id: SIGN_IN }]
    }

    fn signed_out(&self, _sign_in: &str, app: &AppInfo, _req: &RequestCtx) -> WorkspaceSpec {
        crate::layout::signed_out(&self.runtime().config, app.id, app.title)
    }

    fn hydrate<'a>(
        &'a self,
        pod: &'a Pod,
        _app_id: &'a str,
        req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(crate::routes::home_feed(self, pod, req))
    }

    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        Box::pin(crate::posting::resume(pod))
    }
}
