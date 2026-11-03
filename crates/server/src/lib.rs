//! The Construct pod as it ships: [`pod_core`] serving the compile-time
//! adapters (D40). Only this crate knows which apps the pod has; `main.rs`
//! runs it, and tests build the same pod with [`state_in_memory`] and friends.
//!
//! The core's modules are re-exported (`server::layouts`, `server::store`,
//! ...), `server::pixelfed` is the Pixelfed adapter crate and
//! `server::recipes` the recipes adapter crate, `server::collections` the
//! Collections adapter crate.

use std::path::Path;
use std::sync::Arc;

pub use app_collections as collections;
pub use app_pixelfed as pixelfed;
pub use app_pixelfed::{DEFAULT_PIXELFED_URL, PixelfedAdapter, PixelfedConfig};
pub use app_recipes as recipes;
pub use pod_core::*;
pub use pod_host as host;

/// The pod's adapters, in `GET /v1/apps` order: the Pixelfed feed (with the
/// `pixelfed` sign-in), the recipes, then the `pixelfed_demo` dev app; and
/// Collections (no apps of its own: data custom apps declare).
pub fn adapters(pixelfed: PixelfedConfig) -> Vec<Arc<dyn AppAdapter>> {
    vec![
        Arc::new(PixelfedAdapter::new(pixelfed)),
        Arc::new(app_recipes::RecipesAdapter::new()),
        app_pixelfed::apps::demo_adapter(),
        Arc::new(app_collections::CollectionsAdapter::new()),
    ]
}

/// The env var that lets `CONSTRUCT_OPEN_POD` serve a non-loopback address.
pub const INSECURE_OPEN_POD_VAR: &str = "CONSTRUCT_INSECURE_OPEN_POD_ON_NETWORK";

/// Whether `addr` (`CONSTRUCT_ADDR`) only accepts connections from this
/// machine.
pub fn is_loopback_addr(addr: &str) -> bool {
    addr.parse::<std::net::SocketAddr>()
        .is_ok_and(|a| a.ip().is_loopback())
        || addr
            .rsplit_once(':')
            .is_some_and(|(h, _)| h.eq_ignore_ascii_case("localhost"))
}

/// Security audit #31 (L2): the open pod gives every caller the whole pod,
/// sign-ins included. Refused behind Fly (`on_fly`, `FLY_APP_NAME`) always,
/// and on a non-loopback address unless `insecure_opt_in`
/// ([`INSECURE_OPEN_POD_VAR`]`=1`): anyone on the network would get it.
pub fn check_open_pod(addr: &str, on_fly: bool, insecure_opt_in: bool) -> Result<(), String> {
    if on_fly {
        return Err(
            "CONSTRUCT_OPEN_POD is refused on Fly (FLY_APP_NAME is set): \
                    every visitor would get the pod. Use pod tokens (`server pods create`)."
                .to_owned(),
        );
    }
    if !is_loopback_addr(addr) && !insecure_opt_in {
        return Err(format!(
            "CONSTRUCT_OPEN_POD is refused on {addr}: anyone who can reach it would get the \
             whole pod and its sign-ins. Bind 127.0.0.1, use a pod token (`server pods create`), \
             or set {INSECURE_OPEN_POD_VAR}=1 on a network you trust."
        ));
    }
    Ok(())
}

/// The per-pod settings for a new pod (D46): the recipes family collection
/// is imported only into pods made with `import_recipes`.
pub fn new_pod_settings(import_recipes: bool) -> Vec<(String, String)> {
    vec![(
        app_recipes::db::IMPORT_FAMILY_SETTING.to_owned(),
        if import_recipes { "1" } else { "0" }.to_owned(),
    )]
}

/// Extra settings of temporary pods (anyone can make one, D46): Pixelfed
/// only on the default instance, so a visitor can't make the server contact
/// hosts of their choosing (fetch URLs, register OAuth apps).
pub fn temporary_pod_settings() -> Vec<(String, String)> {
    vec![(
        app_pixelfed::OTHER_INSTANCES_SETTING.to_owned(),
        "0".to_owned(),
    )]
}

/// Builds each pod of a host (D46): every pod gets the same adapters (shared
/// objects; their data lives in each pod's store) and the same options.
pub fn pod_factory(
    pixelfed: PixelfedConfig,
    test_hooks: bool,
    session_ttl_secs: i64,
    show_dev_apps: bool,
) -> pod_host::PodFactory {
    pod_factory_with_push(pixelfed, test_hooks, session_ttl_secs, show_dev_apps, None)
}

/// [`pod_factory`] whose pods send notifications by Web Push with `push`
/// (one sender, and so one VAPID key, for the whole server; D52).
pub fn pod_factory_with_push(
    pixelfed: PixelfedConfig,
    test_hooks: bool,
    session_ttl_secs: i64,
    show_dev_apps: bool,
    push: Option<Arc<push::PushSender>>,
) -> pod_host::PodFactory {
    let adapters = adapters(pixelfed);
    Arc::new(move |store| {
        Ok(AppState::try_new(store, adapters.clone())?
            .with_test_hooks(test_hooks)
            .with_session_ttl_secs(session_ttl_secs)
            .with_dev_apps(show_dev_apps)
            .with_push(push.clone()))
    })
}

/// The Web Push sender from the environment (D52), or `None` with
/// `CONSTRUCT_PUSH=off`. The VAPID key is `CONSTRUCT_VAPID_PRIVATE_KEY`, else
/// one kept in `<data_dir>/host.db`; the contact is `CONSTRUCT_VAPID_SUBJECT`.
/// With test hooks, deliveries may go to local endpoints (test receivers).
pub fn push_from_env(
    data_dir: &Path,
    test_hooks: bool,
) -> Result<Option<Arc<push::PushSender>>, String> {
    let var = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    if var("CONSTRUCT_PUSH").as_deref() == Some("off") {
        return Ok(None);
    }
    let key = pod_host::vapid_private_key(data_dir, var("CONSTRUCT_VAPID_PRIVATE_KEY").as_deref())?;
    let subject =
        var("CONSTRUCT_VAPID_SUBJECT").unwrap_or_else(|| DEFAULT_VAPID_SUBJECT.to_owned());
    Ok(Some(Arc::new(push::PushSender::new(
        &key, &subject, test_hooks,
    )?)))
}

/// `CONSTRUCT_VAPID_SUBJECT` when unset: push services may use it to reach
/// the operator.
pub const DEFAULT_VAPID_SUBJECT: &str = "https://github.com/aidanwolter3/the-construct";

/// Every layout file the pod ships, per app in number order.
pub fn layout_files() -> Vec<layouts::LayoutSource> {
    let mut files = app_pixelfed::apps::files();
    files.extend_from_slice(app_recipes::FILES);
    files
}

/// A catalog of the pod's apps (no store), for checking layout files.
pub fn catalog() -> Catalog {
    Catalog::new(adapters(PixelfedConfig::default())).expect("the pod's adapters are valid")
}

/// Force-parse all embedded fixtures. Call at startup to fail fast.
pub fn validate_fixtures() {
    app_pixelfed::apps::validate_fixtures();
}

/// The pod over `store` with the default Pixelfed config. Panics on a
/// layout sync error.
pub fn state_new(store: store::Store) -> AppState {
    AppState::new(store, adapters(PixelfedConfig::default()))
}

/// The pod over a private in-memory database (tests).
pub fn state_in_memory() -> AppState {
    AppState::in_memory(adapters(PixelfedConfig::default()))
}

/// The pod backed by `<dir>/server.db`.
pub fn open_dir(dir: &Path) -> Result<AppState, Box<dyn std::error::Error + Send + Sync>> {
    AppState::open_dir(dir, adapters(PixelfedConfig::default()))
}

/// Router over a fresh in-memory pod, hooks off (tests).
pub fn app_in_memory() -> axum::Router {
    app(Arc::new(state_in_memory()))
}

/// Configure the Pixelfed adapter of a built pod.
pub trait WithPixelfed: Sized {
    /// Point the Pixelfed adapter at another instance.
    fn with_pixelfed(self, config: PixelfedConfig) -> Self;
    /// The Pixelfed adapter's current config.
    fn pixelfed_config(&self) -> PixelfedConfig;
}

impl WithPixelfed for AppState {
    fn with_pixelfed(self, config: PixelfedConfig) -> Self {
        self.adapter::<PixelfedAdapter>()
            .expect("the pod has the Pixelfed adapter")
            .configure(config);
        self
    }

    fn pixelfed_config(&self) -> PixelfedConfig {
        self.adapter::<PixelfedAdapter>()
            .expect("the pod has the Pixelfed adapter")
            .config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pod_bind_addresses() {
        for a in [
            "127.0.0.1:8080",
            "127.1.2.3:1",
            "[::1]:8080",
            "localhost:8080",
        ] {
            assert!(is_loopback_addr(a), "{a}");
            assert!(check_open_pod(a, false, false).is_ok(), "{a}");
            assert!(check_open_pod(a, true, true).is_err(), "Fly: {a}");
        }
        for a in [
            "0.0.0.0:8080",
            "[::]:8080",
            "192.168.1.5:8080",
            "pod.example:8080",
            "bad",
        ] {
            assert!(!is_loopback_addr(a), "{a}");
            assert!(check_open_pod(a, false, false).is_err(), "{a}");
            assert!(check_open_pod(a, false, true).is_ok(), "opt-in: {a}");
            assert!(check_open_pod(a, true, true).is_err(), "Fly: {a}");
        }
    }
}
