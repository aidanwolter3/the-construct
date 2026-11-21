//! Outbound HTTP to hosts a user picked (D45, D52): a typed Pixelfed
//! instance, a device's Web Push endpoint. Such a host must not reach the
//! pod's own network, so names resolve only to public addresses
//! ([`PublicOnlyResolver`]) and redirects go only to public `https://` names.
//! [`OutboundClient`] is the only way to call one: it checks each URL, pins
//! the resolver and caps the body it reads.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;

const USER_AGENT: &str = concat!("construct-pod/", env!("CARGO_PKG_VERSION"));
pub const MAX_REDIRECTS: usize = 5;

/// A public-looking host name: a dotted DNS name (no IP literal, no
/// `localhost`, `.local`, `.internal`, ...). The DNS answer is checked at
/// connect time too ([`PublicOnlyResolver`]).
pub fn is_public_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<IpAddr>().is_ok() || host.starts_with('[') {
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

/// An `https://<public host>` URL (default port; no IP literal).
pub fn is_public_https(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.port().is_none() && url.host_str().is_some_and(is_public_host)
}

/// A globally routable unicast address (not private, loopback, link-local,
/// CGNAT, ULA, documentation, multicast, ...).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local (fc00::/7, e.g. Fly's fdaa::)
                || (s[0] & 0xffc0) == 0xfe80 // link-local
                || (s[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x0064 && s[1] == 0xff9b) // NAT64: embeds an IPv4 address
                || s[0] == 0 // ::/16 (IPv4-compatible and other reserved)
                || s[0] == 0x2002) // 6to4: embeds an IPv4 address
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10
        || (a == 192 && b == 0 && c == 0) // 192.0.0/24
        || (a == 198 && (b == 18 || b == 19)) // benchmarking 198.18/15
        || a >= 240) // reserved
}

/// The system resolver, keeping only public addresses: a name a user typed
/// must not reach the pod's own network.
pub struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public_ip(a.ip()))
                .collect();
            if addrs.is_empty() {
                tracing::warn!(%host, "refused: the name has no public address");
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// The most of a response body an [`OutboundClient`] reads unless the
/// client or the request says otherwise.
pub const DEFAULT_BODY_CAP: usize = 4 * 1024 * 1024;

/// A client builder with the pod's user agent and timeouts, for a fixed host
/// the operator configured (the Messages API), never one a user picked: use
/// [`OutboundClient`] for those. Clippy refuses it (`clippy.toml`) except
/// where a call site allows it.
#[allow(clippy::disallowed_methods)] // the one place a reqwest client is built
pub fn fixed_host_builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .user_agent(USER_AGENT)
}

/// A request an [`OutboundClient`] refused or that failed.
#[derive(Debug)]
pub enum OutboundError {
    /// The URL is not one this client may call (scheme, host, IP literal,
    /// port).
    Refused(String),
    /// The body is longer than the cap (bytes).
    TooLarge(usize),
    /// The body is not the JSON asked for.
    Decode(serde_json::Error),
    Http(reqwest::Error),
}

impl OutboundError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Http(e) if e.is_timeout())
    }
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Http(e) if e.is_connect())
    }
    pub fn is_builder(&self) -> bool {
        matches!(self, Self::Http(e) if e.is_builder())
    }
    pub fn is_decode(&self) -> bool {
        matches!(self, Self::Decode(_)) || matches!(self, Self::Http(e) if e.is_decode())
    }
    pub fn is_too_large(&self) -> bool {
        matches!(self, Self::TooLarge(_))
    }
}

impl std::fmt::Display for OutboundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) => f.write_str(m),
            Self::TooLarge(cap) => write!(f, "the answer is over the {cap}-byte cap"),
            Self::Decode(e) => write!(f, "invalid JSON: {e}"),
            Self::Http(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for OutboundError {}

impl From<reqwest::Error> for OutboundError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

type RedirectFilter = Arc<dyn Fn(&reqwest::Url, &[reqwest::Url]) -> bool + Send + Sync>;

/// Builds an [`OutboundClient`] ([`OutboundClient::builder`]).
pub struct OutboundBuilder {
    timeout: Duration,
    read_timeout: Option<Duration>,
    private: bool,
    web: bool,
    redirects: Option<RedirectFilter>,
    cap: usize,
}

impl OutboundBuilder {
    /// `true`: any `http(s)` URL, private addresses and IP literals
    /// included, through the environment's proxy (hosts the operator
    /// configured, tests). `false` (the default): public names only.
    pub fn private(mut self, private: bool) -> Self {
        self.private = private;
        self
    }

    /// Web pages: `http://` and any port too (still public names only).
    /// Without it a URL must be `https://` on the default port.
    pub fn web(mut self) -> Self {
        self.web = true;
        self
    }

    /// Follow no redirect: a 3xx comes back as the response.
    pub fn no_redirects(mut self) -> Self {
        self.redirects = Some(Arc::new(|_, _| false));
        self
    }

    /// Follow a redirect only when `allow(target, previous)` says so (and,
    /// for public names, only to a public `https://` URL, at most
    /// [`MAX_REDIRECTS`] hops).
    pub fn redirect_if(
        mut self,
        allow: impl Fn(&reqwest::Url, &[reqwest::Url]) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.redirects = Some(Arc::new(allow));
        self
    }

    /// The most of a body read ([`DEFAULT_BODY_CAP`] by default;
    /// [`OutboundRequest::body_cap`] and [`OutboundResponse::body_cap`]
    /// change it for one request).
    pub fn body_cap(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    /// Drop a response that stays silent this long (a stream).
    pub fn read_timeout(mut self, d: Duration) -> Self {
        self.read_timeout = Some(d);
        self
    }

    #[allow(clippy::disallowed_methods)] // the checked client is built here
    pub fn build(self) -> OutboundClient {
        let private = self.private;
        let filter = self.redirects;
        let mut b = fixed_host_builder(self.timeout).redirect(redirect::Policy::custom(
            move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                let ok = private || is_public_https(attempt.url());
                if ok && filter.as_ref().is_none_or(|f| f(attempt.url(), attempt.previous())) {
                    attempt.follow()
                } else {
                    if !ok {
                        tracing::warn!(host = ?attempt.url().host_str(), "redirect to a non-public URL refused");
                    }
                    attempt.stop()
                }
            },
        ));
        if !private {
            // A proxy (HTTPS_PROXY & co.) resolves names itself, past
            // `PublicOnlyResolver`.
            b = b.no_proxy().dns_resolver(Arc::new(PublicOnlyResolver));
        }
        if let Some(d) = self.read_timeout {
            b = b.read_timeout(d);
        }
        OutboundClient {
            http: b.build().expect("an HTTP client"),
            private,
            web: self.web,
            cap: self.cap,
        }
    }
}

/// The only way the pod calls a host a user picked (a typed Pixelfed
/// instance, a push endpoint, an MCP server, an agent runner, a filler's or
/// the web proxy's page). Every request's URL is checked first (scheme, a
/// public host name and no IP literal, which would skip the resolver, the
/// port), names resolve to public addresses only ([`PublicOnlyResolver`]),
/// redirects go only to public `https://` names, and a body is read through
/// one cap ([`DEFAULT_BODY_CAP`] unless changed). Cheap to clone.
#[derive(Clone)]
pub struct OutboundClient {
    http: reqwest::Client,
    private: bool,
    web: bool,
    cap: usize,
}

impl std::fmt::Debug for OutboundClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboundClient")
            .field("private", &self.private)
            .field("web", &self.web)
            .field("cap", &self.cap)
            .finish()
    }
}

impl OutboundClient {
    /// A builder: public `https://` names on the default port, redirects
    /// followed (to public `https://` names), [`DEFAULT_BODY_CAP`].
    pub fn builder(timeout: Duration) -> OutboundBuilder {
        OutboundBuilder {
            timeout,
            read_timeout: None,
            private: false,
            web: false,
            redirects: None,
            cap: DEFAULT_BODY_CAP,
        }
    }

    /// Whether this client may call `url`.
    pub fn allows(&self, url: &reqwest::Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return false;
        }
        if self.private {
            return true;
        }
        if self.web {
            url.host_str().is_some_and(is_public_host)
        } else {
            is_public_https(url)
        }
    }

    /// `url` parsed, if this client may call it.
    pub fn check(&self, url: &str) -> Result<reqwest::Url, OutboundError> {
        let u = reqwest::Url::parse(url.trim())
            .map_err(|_| OutboundError::Refused("not a URL".into()))?;
        if self.allows(&u) {
            Ok(u)
        } else {
            tracing::warn!(host = ?u.host_str(), "outbound request to a non-public URL refused");
            Err(OutboundError::Refused(if self.web {
                format!(
                    "{} is not a public website",
                    u.host_str().unwrap_or("the URL")
                )
            } else {
                format!(
                    "{} is not an https:// address on a public host name (default port)",
                    u.host_str().unwrap_or("the URL")
                )
            }))
        }
    }

    /// The body cap of this client's requests.
    pub fn body_cap(&self) -> usize {
        self.cap
    }

    pub fn request(&self, method: reqwest::Method, url: impl AsRef<str>) -> OutboundRequest {
        OutboundRequest {
            inner: self
                .check(url.as_ref())
                .map(|u| self.http.request(method, u)),
            cap: self.cap,
        }
    }

    pub fn get(&self, url: impl AsRef<str>) -> OutboundRequest {
        self.request(reqwest::Method::GET, url)
    }

    pub fn post(&self, url: impl AsRef<str>) -> OutboundRequest {
        self.request(reqwest::Method::POST, url)
    }

    pub fn head(&self, url: impl AsRef<str>) -> OutboundRequest {
        self.request(reqwest::Method::HEAD, url)
    }
}

/// A request of an [`OutboundClient`]; a refused URL fails at
/// [`send`](Self::send).
pub struct OutboundRequest {
    inner: Result<reqwest::RequestBuilder, OutboundError>,
    cap: usize,
}

impl OutboundRequest {
    /// Change the request with reqwest's builder (a multipart form, ...);
    /// its URL stays the one checked.
    pub fn with(
        mut self,
        f: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Self {
        self.inner = self.inner.map(f);
        self
    }

    pub fn header<K, V>(self, key: K, value: V) -> Self
    where
        reqwest::header::HeaderName: TryFrom<K>,
        <reqwest::header::HeaderName as TryFrom<K>>::Error: Into<axum::http::Error>,
        reqwest::header::HeaderValue: TryFrom<V>,
        <reqwest::header::HeaderValue as TryFrom<V>>::Error: Into<axum::http::Error>,
    {
        self.with(|r| r.header(key, value))
    }

    pub fn bearer_auth(self, token: impl std::fmt::Display) -> Self {
        self.with(|r| r.bearer_auth(token))
    }

    pub fn basic_auth(
        self,
        user: impl std::fmt::Display,
        password: Option<impl std::fmt::Display>,
    ) -> Self {
        self.with(|r| r.basic_auth(user, password))
    }

    pub fn query<T: serde::Serialize + ?Sized>(self, query: &T) -> Self {
        self.with(|r| r.query(query))
    }

    pub fn form<T: serde::Serialize + ?Sized>(self, form: &T) -> Self {
        self.with(|r| r.form(form))
    }

    pub fn json<T: serde::Serialize + ?Sized>(self, json: &T) -> Self {
        self.with(|r| r.json(json))
    }

    pub fn body(self, body: impl Into<reqwest::Body>) -> Self {
        self.with(|r| r.body(body))
    }

    pub fn timeout(self, d: Duration) -> Self {
        self.with(|r| r.timeout(d))
    }

    /// The most of this answer's body read.
    pub fn body_cap(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    pub async fn send(self) -> Result<OutboundResponse, OutboundError> {
        let resp = self.inner?.send().await?;
        Ok(OutboundResponse {
            inner: resp,
            cap: self.cap,
            read: 0,
        })
    }
}

/// An answer whose body is read through the cap.
#[derive(Debug)]
pub struct OutboundResponse {
    inner: reqwest::Response,
    cap: usize,
    read: usize,
}

impl OutboundResponse {
    pub fn status(&self) -> reqwest::StatusCode {
        self.inner.status()
    }

    pub fn headers(&self) -> &reqwest::header::HeaderMap {
        self.inner.headers()
    }

    pub fn url(&self) -> &reqwest::Url {
        self.inner.url()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    /// Read at most `cap` bytes of the body (`usize::MAX`: a stream read
    /// for as long as it lasts, which the caller bounds itself).
    pub fn body_cap(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    /// The next part of the body; [`OutboundError::TooLarge`] once the body
    /// passes the cap.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, OutboundError> {
        match self.inner.chunk().await? {
            Some(c) => {
                self.read = self.read.saturating_add(c.len());
                if self.read > self.cap {
                    return Err(OutboundError::TooLarge(self.cap));
                }
                Ok(Some(c))
            }
            None => Ok(None),
        }
    }

    /// The whole body (refused past the cap).
    pub async fn bytes(mut self) -> Result<Vec<u8>, OutboundError> {
        if self.content_length().is_some_and(|n| n > self.cap as u64) {
            return Err(OutboundError::TooLarge(self.cap));
        }
        let mut out = Vec::new();
        while let Some(c) = self.chunk().await? {
            out.extend_from_slice(&c);
        }
        Ok(out)
    }

    /// The body as text (UTF-8, invalid bytes replaced).
    pub async fn text(self) -> Result<String, OutboundError> {
        Ok(String::from_utf8_lossy(&self.bytes().await?).into_owned())
    }

    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, OutboundError> {
        serde_json::from_slice(&self.bytes().await?).map_err(OutboundError::Decode)
    }

    /// The body as a stream that fails once it passes the cap.
    pub fn into_stream(
        self,
    ) -> impl futures_util::Stream<Item = Result<Bytes, OutboundError>> + Send + 'static {
        futures_util::stream::unfold(Some(self), |resp| async move {
            let mut resp = resp?;
            match resp.chunk().await {
                Ok(Some(c)) => Some((Ok(c), Some(resp))),
                Ok(None) => None,
                Err(e) => Some((Err(e), None)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_hosts() {
        for ok in ["social.example", "a.b.example.com", "push.example.org."] {
            assert!(is_public_host(ok), "{ok}");
        }
        for bad in [
            "localhost",
            "x.local",
            "10.0.0.1",
            "[::1]",
            "host",
            "a.internal",
            "0x7f.1",
            "1.2.3.4",
        ] {
            assert!(!is_public_host(bad), "{bad}");
        }
    }

    #[test]
    fn outbound_urls() {
        let t = Duration::from_secs(1);
        let https = OutboundClient::builder(t).build();
        let web = OutboundClient::builder(t).web().build();
        let private = OutboundClient::builder(t).private(true).build();
        let ok = |c: &OutboundClient, u: &str| c.check(u).is_ok();
        assert!(ok(&https, "https://social.example/api"));
        for bad in [
            "http://social.example/",
            "https://social.example:8443/",
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://localhost/",
            "https://fdaa.internal/",
            "ftp://social.example/",
            "nonsense",
        ] {
            assert!(!ok(&https, bad), "{bad}");
        }
        assert!(ok(&web, "http://news.example:8080/a"));
        assert!(!ok(&web, "http://10.0.0.1/"));
        assert!(ok(&private, "http://127.0.0.1:9/x"));
        assert!(!ok(&private, "file:///etc/passwd"));
    }

    #[tokio::test]
    async fn refused_urls_fail_at_send() {
        let c = OutboundClient::builder(Duration::from_secs(1)).build();
        let e = c
            .get("https://169.254.169.254/latest")
            .send()
            .await
            .unwrap_err();
        assert!(matches!(e, OutboundError::Refused(_)), "{e}");
    }

    #[tokio::test]
    async fn bodies_stop_at_the_cap() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app =
            axum::Router::new().route("/big", axum::routing::get(|| async { vec![b'x'; 2048] }));
        tokio::spawn(async move { axum::serve(listener, app).await });
        let c = OutboundClient::builder(Duration::from_secs(5))
            .private(true)
            .body_cap(1024)
            .build();
        let url = format!("http://{addr}/big");
        let e = c.get(&url).send().await.unwrap().bytes().await.unwrap_err();
        assert!(e.is_too_large(), "{e}");
        let ok = c
            .get(&url)
            .body_cap(4096)
            .send()
            .await
            .unwrap()
            .bytes()
            .await;
        assert_eq!(ok.unwrap().len(), 2048);
    }

    #[test]
    fn public_ips() {
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_public_ip("10.1.2.3".parse().unwrap()));
        assert!(!is_public_ip("fdaa::1".parse().unwrap()));
        assert!(!is_public_ip("::ffff:127.0.0.1".parse().unwrap()));
    }
}
