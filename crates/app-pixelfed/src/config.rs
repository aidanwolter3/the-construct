//! Which Pixelfed instance the adapter talks to, and who may own the pod.

use crate::MediaAllowlist;

/// Default Pixelfed instance: the local fake (`crates/fake-pixelfed`).
pub const DEFAULT_PIXELFED_URL: &str = "http://127.0.0.1:8081";

/// Which Pixelfed instance the pod talks to (single tenant), and who may own it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelfedConfig {
    /// Base URL for server->upstream calls (`PIXELFED_INSTANCE_URL`).
    pub instance_url: String,
    /// Base URL for browser-facing links such as the authorize endpoint
    /// (`PIXELFED_PUBLIC_URL`, default = instance URL).
    pub public_url: String,
    /// Explicit owner (`PIXELFED_OWNER_ACCOUNT_ID`): only this upstream
    /// account id may sign in. Unset: the first account to sign in is pinned.
    pub owner_account_id: Option<String>,
    /// `PIXELFED_ALLOW_ACCOUNT_CHANGE=1`: any account may sign in and replace
    /// the stored one (the pre-pinning behaviour).
    pub allow_account_change: bool,
    /// Extra media hosts the proxy may fetch (`PIXELFED_MEDIA_HOSTS`,
    /// comma-separated `host`, `host:port` or `https://host[:port]`), e.g. a CDN.
    pub media_hosts: Vec<String>,
    /// Extra exact redirect URIs accepted by `/v1/auth/pixelfed/app`
    /// (`PIXELFED_EXTRA_REDIRECTS`, comma-separated).
    pub extra_redirects: Vec<String>,
}

impl Default for PixelfedConfig {
    fn default() -> Self {
        Self::new(DEFAULT_PIXELFED_URL, None)
    }
}

impl PixelfedConfig {
    pub fn new(instance_url: &str, public_url: Option<&str>) -> Self {
        let instance_url = normalize_base(instance_url);
        let public_url = public_url
            .map(normalize_base)
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| instance_url.clone());
        Self {
            instance_url,
            public_url,
            owner_account_id: None,
            allow_account_change: false,
            media_hosts: Vec::new(),
            extra_redirects: Vec::new(),
        }
    }

    /// From `PIXELFED_INSTANCE_URL`, `PIXELFED_PUBLIC_URL`,
    /// `PIXELFED_OWNER_ACCOUNT_ID`, `PIXELFED_ALLOW_ACCOUNT_CHANGE`,
    /// `PIXELFED_MEDIA_HOSTS` and `PIXELFED_EXTRA_REDIRECTS`.
    pub fn from_env() -> Self {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let list = |k: &str| -> Vec<String> {
            var(k)
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let instance =
            var("PIXELFED_INSTANCE_URL").unwrap_or_else(|| DEFAULT_PIXELFED_URL.to_owned());
        Self {
            owner_account_id: var("PIXELFED_OWNER_ACCOUNT_ID"),
            allow_account_change: var("PIXELFED_ALLOW_ACCOUNT_CHANGE")
                .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            media_hosts: list("PIXELFED_MEDIA_HOSTS"),
            extra_redirects: list("PIXELFED_EXTRA_REDIRECTS"),
            ..Self::new(&instance, var("PIXELFED_PUBLIC_URL").as_deref())
        }
    }

    /// Origins the media proxy may fetch: the instance and public URLs plus
    /// [`Self::media_hosts`].
    pub fn media_allowlist(&self) -> MediaAllowlist {
        MediaAllowlist::new(
            [self.instance_url.as_str(), self.public_url.as_str()],
            self.media_hosts.iter().map(String::as_str),
        )
    }

    /// Only the configured instance (by its instance or public URL) is accepted
    /// in auth calls: the pod never talks to a client-chosen host.
    pub fn accepts(&self, url: &str) -> bool {
        let url = normalize_base(url);
        url.eq_ignore_ascii_case(&self.instance_url) || url.eq_ignore_ascii_case(&self.public_url)
    }
}

fn normalize_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}
