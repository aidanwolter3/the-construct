//! Which Pixelfed instances the adapter talks to, and who may own the pod.

use crate::MediaAllowlist;

/// Default Pixelfed instance: the local fake (`crates/fake-pixelfed`).
pub const DEFAULT_PIXELFED_URL: &str = "http://127.0.0.1:8081";

/// Which Pixelfed instances the pod talks to, and who may own it (D45).
///
/// One instance is the default (`PIXELFED_INSTANCE_URL`): the signed-out
/// screen's "Sign in with Pixelfed" button uses it, and sessions from before
/// D45 belong to it. Others are accepted only when listed in
/// `PIXELFED_ALLOWED_INSTANCES`, or with `*` there, any public HTTPS one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelfedConfig {
    /// Base URL of the default instance for server->upstream calls
    /// (`PIXELFED_INSTANCE_URL`).
    pub instance_url: String,
    /// Base URL for the default instance's browser-facing links such as the
    /// authorize endpoint (`PIXELFED_PUBLIC_URL`, default = instance URL).
    pub public_url: String,
    /// Other instances users may sign in to (`PIXELFED_ALLOWED_INSTANCES`,
    /// comma-separated base URLs). The pod trusts them like the default one:
    /// they may be on a private network.
    pub allowed_instances: Vec<String>,
    /// `*` in `PIXELFED_ALLOWED_INSTANCES`: any public HTTPS instance a user
    /// types. The pod then only connects to public IP addresses for them.
    pub any_instance: bool,
    /// Explicit owner of the default instance (`PIXELFED_OWNER_ACCOUNT_ID`):
    /// only this upstream account id may sign in there. Otherwise (and on
    /// every other instance) the first account to sign in is pinned.
    pub owner_account_id: Option<String>,
    /// `PIXELFED_ALLOW_ACCOUNT_CHANGE=1`: on the default and listed
    /// instances, any account may sign in and replace the stored one (the
    /// pre-pinning behaviour). Never on a user-typed instance: there, another
    /// person's sign-in must not replace the stored account.
    pub allow_account_change: bool,
    /// Extra media hosts the proxy may fetch (`PIXELFED_MEDIA_HOSTS`,
    /// comma-separated `host`, `host:port` or `https://host[:port]`), e.g. a CDN.
    pub media_hosts: Vec<String>,
    /// Extra exact redirect URIs accepted by `/v1/auth/pixelfed/app`
    /// (`PIXELFED_EXTRA_REDIRECTS`, comma-separated).
    pub extra_redirects: Vec<String>,
}

/// An instance a sign-in or session uses, as the pod stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// Canonical base URL (`scheme://host[:port]`, no trailing slash); the
    /// default instance keeps its configured spelling. Keys the stored rows.
    pub url: String,
    /// Configured by the operator (default or listed), so it may be on a
    /// private network. Not trusted: a user-typed instance under `*`, reached
    /// only on public addresses.
    pub trusted: bool,
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
            allowed_instances: Vec::new(),
            any_instance: false,
            owner_account_id: None,
            allow_account_change: false,
            media_hosts: Vec::new(),
            extra_redirects: Vec::new(),
        }
    }

    /// From `PIXELFED_INSTANCE_URL`, `PIXELFED_PUBLIC_URL`,
    /// `PIXELFED_ALLOWED_INSTANCES`, `PIXELFED_OWNER_ACCOUNT_ID`,
    /// `PIXELFED_ALLOW_ACCOUNT_CHANGE`, `PIXELFED_MEDIA_HOSTS` and
    /// `PIXELFED_EXTRA_REDIRECTS`.
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
        let mut config = Self {
            owner_account_id: var("PIXELFED_OWNER_ACCOUNT_ID"),
            allow_account_change: var("PIXELFED_ALLOW_ACCOUNT_CHANGE")
                .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            media_hosts: list("PIXELFED_MEDIA_HOSTS"),
            extra_redirects: list("PIXELFED_EXTRA_REDIRECTS"),
            ..Self::new(&instance, var("PIXELFED_PUBLIC_URL").as_deref())
        };
        for entry in list("PIXELFED_ALLOWED_INSTANCES") {
            if entry == "*" {
                config.any_instance = true;
            } else {
                match canonical_base(&entry) {
                    Some(url) => config.allowed_instances.push(url),
                    None => tracing::warn!(
                        entry,
                        "ignoring PIXELFED_ALLOWED_INSTANCES entry that is not a base URL"
                    ),
                }
            }
        }
        config
    }

    /// Whether users may sign in to instances other than the default.
    pub fn other_instances(&self) -> bool {
        self.any_instance || !self.allowed_instances.is_empty()
    }

    /// The default instance.
    pub fn default_instance(&self) -> Instance {
        Instance {
            url: self.instance_url.clone(),
            trusted: true,
        }
    }

    /// The instance a sign-in names (`pixelfed.social`, `https://pixelfed.social/`,
    /// the default's instance or public URL, ...), if the pod may use it. A
    /// value without a scheme means `https://`. `None`: not a base URL, or an
    /// instance this pod does not accept.
    pub fn resolve(&self, typed: &str) -> Option<Instance> {
        let typed = typed.trim();
        let base = normalize_base(typed);
        if base.eq_ignore_ascii_case(&self.instance_url)
            || base.eq_ignore_ascii_case(&self.public_url)
        {
            return Some(self.default_instance());
        }
        let url = canonical_base(typed)?;
        if url.eq_ignore_ascii_case(&self.instance_url)
            || url.eq_ignore_ascii_case(&self.public_url)
        {
            return Some(self.default_instance());
        }
        if self.allowed_instances.contains(&url) {
            return Some(Instance { url, trusted: true });
        }
        if self.any_instance && is_public_base(&url) {
            return Some(Instance {
                url,
                trusted: false,
            });
        }
        None
    }

    /// The only account id that may sign in to `instance`, if one is set.
    pub fn required_owner(&self, instance: &Instance) -> Option<&str> {
        self.owner_account_id
            .as_deref()
            .filter(|_| instance.url == self.instance_url)
    }

    /// Whether a sign-in to `instance` replaces a pinned owner.
    pub fn replaces_owner(&self, instance: &Instance) -> bool {
        self.required_owner(instance).is_some() || (self.allow_account_change && instance.trusted)
    }

    /// The browser-facing base of `instance` (the default's public URL).
    pub fn public_base<'a>(&'a self, instance: &'a Instance) -> &'a str {
        if instance.url == self.instance_url {
            &self.public_url
        } else {
            &instance.url
        }
    }

    /// Origins the media proxy may fetch with the trusted client: the
    /// default's instance and public URLs, the allowed instances and
    /// [`Self::media_hosts`]. Under `*`, other public HTTPS media are
    /// allowed too (see [`crate::MediaPolicy`]).
    pub fn media_allowlist(&self) -> MediaAllowlist {
        MediaAllowlist::new(
            [self.instance_url.as_str(), self.public_url.as_str()]
                .into_iter()
                .chain(self.allowed_instances.iter().map(String::as_str)),
            self.media_hosts.iter().map(String::as_str),
        )
    }
}

fn normalize_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

/// `scheme://host[:port]` (lowercase host, default port dropped) for a base
/// URL with no path, query, fragment or credentials. No scheme: `https://`.
pub fn canonical_base(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() || typed.chars().any(char::is_whitespace) {
        return None;
    }
    let with_scheme = if typed.contains("://") {
        typed.to_owned()
    } else {
        format!("https://{typed}")
    };
    let u = reqwest::Url::parse(&with_scheme).ok()?;
    if !matches!(u.scheme(), "http" | "https")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || !matches!(u.path(), "" | "/")
    {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    Some(match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    })
}

/// A public-looking host name: a dotted DNS name (no IP literal, no
/// `localhost`, `.local`, `.internal`, ...). The DNS answer is checked at
/// connect time too ([`crate::upstream`]).
pub fn is_public_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|l| {
            l.is_empty()
                || l.len() > 63
                || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return false;
    }
    // A numeric last label is an IP address in some spelling (`0x7f.1`).
    let tld = labels[labels.len() - 1];
    if tld.bytes().all(|b| b.is_ascii_digit()) || tld.starts_with("0x") {
        return false;
    }
    !matches!(
        tld,
        "localhost" | "local" | "internal" | "lan" | "home" | "intranet" | "corp" | "arpa"
    )
}

/// An `https://<public host>` base on the default port.
fn is_public_base(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https" && u.port().is_none() && u.host_str().is_some_and(is_public_host)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> PixelfedConfig {
        PixelfedConfig {
            any_instance: true,
            allowed_instances: vec!["http://localhost:8090".into()],
            ..PixelfedConfig::new("http://127.0.0.1:8081/", Some("http://10.0.2.2:8081"))
        }
    }

    #[test]
    fn canonical_bases() {
        for (typed, want) in [
            ("pixelfed.social", "https://pixelfed.social"),
            (" Pixelfed.Social/ ", "https://pixelfed.social"),
            ("https://pixelfed.social:443", "https://pixelfed.social"),
            ("http://localhost:8090/", "http://localhost:8090"),
            ("pix.example:8443", "https://pix.example:8443"),
        ] {
            assert_eq!(canonical_base(typed).as_deref(), Some(want), "{typed}");
        }
        for bad in [
            "",
            "pixelfed social",
            "https://pixelfed.social/path",
            "https://pixelfed.social/?q=1",
            "https://u:p@pixelfed.social",
            "ftp://pixelfed.social",
            "javascript:alert(1)",
        ] {
            assert_eq!(canonical_base(bad), None, "{bad}");
        }
    }

    #[test]
    fn public_hosts() {
        for ok in ["pixelfed.social", "pix.example.org", "a-b.example"] {
            assert!(is_public_host(ok), "{ok}");
        }
        for bad in [
            "localhost",
            "127.0.0.1",
            "10.0.0.1",
            "[::1]",
            "::1",
            "169.254.169.254",
            "0x7f.1",
            "2130706433",
            "printer.local",
            "metadata.google.internal",
            "pod.localhost",
            "under_score.example",
            "nodot",
            "1.2.3.4.5",
        ] {
            assert!(!is_public_host(bad), "{bad}");
        }
    }

    #[test]
    fn resolve_default_listed_and_any() {
        let c = open();
        let default = Some(c.default_instance());
        assert_eq!(c.resolve("http://127.0.0.1:8081"), default);
        assert_eq!(c.resolve("http://127.0.0.1:8081/"), default);
        assert_eq!(c.resolve("http://10.0.2.2:8081"), default);
        assert_eq!(
            c.resolve("localhost:8090"),
            None,
            "no scheme means https, which is not the listed http instance"
        );
        assert_eq!(
            c.resolve("http://LOCALHOST:8090/"),
            Some(Instance {
                url: "http://localhost:8090".into(),
                trusted: true
            })
        );
        assert_eq!(
            c.resolve("Pixelfed.Social"),
            Some(Instance {
                url: "https://pixelfed.social".into(),
                trusted: false
            })
        );
        for bad in [
            "http://pixelfed.social", // not https
            "https://pixelfed.social:8443",
            "https://192.168.1.10",
            "https://localhost",
            "http://127.0.0.1:9999",
            "pixelfed.social/x",
        ] {
            assert_eq!(c.resolve(bad), None, "{bad}");
        }
        // Without `*` only the default and the listed ones.
        let closed = PixelfedConfig {
            any_instance: false,
            ..open()
        };
        assert_eq!(closed.resolve("pixelfed.social"), None);
        assert!(closed.resolve("http://localhost:8090").is_some());
        assert!(!PixelfedConfig::default().other_instances());
        assert!(closed.other_instances());
    }

    #[test]
    fn owner_rules_per_instance() {
        let c = PixelfedConfig {
            owner_account_id: Some("7".into()),
            allow_account_change: true,
            ..open()
        };
        let default = c.default_instance();
        let listed = c.resolve("http://localhost:8090").unwrap();
        let typed = c.resolve("pixelfed.social").unwrap();
        assert_eq!(c.required_owner(&default), Some("7"));
        assert_eq!(c.required_owner(&listed), None);
        assert!(c.replaces_owner(&default) && c.replaces_owner(&listed));
        assert!(
            !c.replaces_owner(&typed),
            "a typed instance always keeps its first account"
        );
    }

    #[test]
    fn public_base_maps_only_the_default() {
        let c = open();
        assert_eq!(c.public_base(&c.default_instance()), "http://10.0.2.2:8081");
        let other = c.resolve("pixelfed.social").unwrap();
        assert_eq!(c.public_base(&other), "https://pixelfed.social");
    }
}
