//! HTTP clients for Pixelfed (Mastodon API) instances and their media.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{StatusCode, redirect};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{Instance, is_public_host};
use crate::{MediaPolicy, MediaRoute};

/// Total timeout for API calls.
pub const API_TIMEOUT: Duration = Duration::from_secs(10);
/// Media downloads may take longer than API calls.
pub const MEDIA_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout of one photo upload to the instance.
pub const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);

pub const APP_NAME: &str = "The Construct";
pub const SCOPES: &str = "read write";

#[derive(Debug)]
pub enum UpstreamError {
    /// Connect error, timeout, or an unreadable / undecodable response.
    Unreachable(String),
    /// A non-2xx response (body kept for error details, truncated).
    Status(StatusCode, String),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "upstream unreachable: {e}"),
            Self::Status(s, body) => write!(f, "upstream answered {s}: {body}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisteredApp {
    pub client_id: String,
    pub client_secret: String,
}

/// `POST /oauth/token` response. Parsed leniently: only `access_token` is
/// required. Pixelfed (Laravel Passport) answers `{token_type, expires_in,
/// access_token, refresh_token, created_at}` with `created_at` as an ISO 8601
/// string (Mastodon: unix seconds) and no `scope`; `created_at` is not used.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub refresh_token: Option<String>,
    /// Lifetime in seconds (Pixelfed: 31536000, one year).
    #[serde(default, deserialize_with = "lenient_secs")]
    pub expires_in: Option<i64>,
}

fn lenient_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s),
        _ => None,
    })
}

fn lenient_secs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let secs = match Option::<Value>::deserialize(d)? {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    Ok(secs.filter(|s| *s > 0))
}

/// The pod's HTTP clients (built once; cheap to clone). Instances and
/// media the operator configured use the `trusted` pair, which may reach
/// private addresses (a local fake, a Fly-internal host). Instances users
/// type under `PIXELFED_ALLOWED_INSTANCES=*`, and media outside the
/// allowlist, use the `public` pair, whose resolver drops every non-public
/// address and which follows redirects only to public HTTPS names (D45).
#[derive(Debug, Clone)]
pub struct Clients {
    trusted_api: reqwest::Client,
    trusted_media: reqwest::Client,
    public_api: reqwest::Client,
    public_media: reqwest::Client,
    policy: MediaPolicy,
}

const USER_AGENT: &str = concat!("construct-pod/", env!("CARGO_PKG_VERSION"));
const MAX_REDIRECTS: usize = 5;

fn builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .user_agent(USER_AGENT)
}

/// An `https://<public host>` URL (default port; no IP literal).
fn public_https(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.port().is_none() && url.host_str().is_some_and(is_public_host)
}

impl Clients {
    pub fn new(policy: MediaPolicy) -> Self {
        let trusted_api = builder(API_TIMEOUT).build().expect("reqwest client");
        let p = policy.clone();
        let trusted_media = builder(MEDIA_TIMEOUT)
            .redirect(redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    attempt.error("too many media redirects")
                } else if p.route_url(attempt.url()) == Some(MediaRoute::Trusted) {
                    attempt.follow()
                } else {
                    // A public target is fetched again with the public client.
                    attempt.stop()
                }
            }))
            .build()
            .expect("reqwest client");
        // The public clients never use a proxy (HTTPS_PROXY & co.): a proxy
        // resolves names itself, past `PublicOnlyResolver`.
        let public_api = builder(API_TIMEOUT)
            .no_proxy()
            .dns_resolver(Arc::new(PublicOnlyResolver))
            .redirect(redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if public_https(attempt.url()) {
                    attempt.follow()
                } else {
                    tracing::warn!(host = ?attempt.url().host_str(), "redirect to a non-public URL refused");
                    attempt.stop()
                }
            }))
            .build()
            .expect("reqwest client");
        let p = policy.clone();
        let public_media = builder(MEDIA_TIMEOUT)
            .no_proxy()
            .dns_resolver(Arc::new(PublicOnlyResolver))
            .redirect(redirect::Policy::custom(move |attempt| {
                // Only to other public URLs: a trusted origin may be private
                // (or an IP literal, which skips the resolver).
                if attempt.previous().len() >= MAX_REDIRECTS {
                    attempt.error("too many media redirects")
                } else if p.route_url(attempt.url()) == Some(MediaRoute::Public) {
                    attempt.follow()
                } else {
                    tracing::warn!(host = ?attempt.url().host_str(), "media redirect to a host outside the allowlist refused");
                    attempt.stop()
                }
            }))
            .build()
            .expect("reqwest client");
        Self {
            trusted_api,
            trusted_media,
            public_api,
            public_media,
            policy,
        }
    }

    /// The API client for `instance`.
    pub fn api(&self, instance: &Instance) -> Upstream {
        Upstream {
            http: if instance.trusted {
                self.trusted_api.clone()
            } else {
                self.public_api.clone()
            },
            instance_url: instance.url.trim_end_matches('/').to_owned(),
        }
    }

    /// GET a media URL (no credentials are sent: media are public and the URL
    /// may be on another host) with the client its [`MediaRoute`] calls for.
    /// A redirect to a URL the policy refuses is not followed (the 3xx comes
    /// back as a `Status` error); one from a trusted origin to a public URL
    /// is fetched with the public client.
    pub async fn fetch_media(
        &self,
        url: &str,
        route: MediaRoute,
    ) -> Result<reqwest::Response, UpstreamError> {
        let client = match route {
            MediaRoute::Trusted => &self.trusted_media,
            MediaRoute::Public => &self.public_media,
        };
        let mut resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        if route == MediaRoute::Trusted && resp.status().is_redirection() {
            let next = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .and_then(|l| resp.url().join(l).ok())
                .filter(|u| self.policy.route_url(u) == Some(MediaRoute::Public));
            if let Some(next) = next {
                resp = self
                    .public_media
                    .get(next)
                    .send()
                    .await
                    .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
            }
        }
        if !resp.status().is_success() {
            return Err(UpstreamError::Status(resp.status(), String::new()));
        }
        Ok(resp)
    }
}

/// The system resolver, keeping only public addresses: a name a user typed
/// (or a federated post's media host) must not reach the pod's own network.
struct PublicOnlyResolver;

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

/// API client for one instance ([`Clients::api`]).
#[derive(Debug, Clone)]
pub struct Upstream {
    http: reqwest::Client,
    instance_url: String,
}

impl Upstream {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.instance_url)
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        req: reqwest::RequestBuilder,
    ) -> Result<T, UpstreamError> {
        let resp = req
            .send()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            let mut text = String::from_utf8_lossy(&body).into_owned();
            text.truncate(500);
            return Err(UpstreamError::Status(status, text));
        }
        serde_json::from_slice(&body)
            .map_err(|e| UpstreamError::Unreachable(format!("invalid JSON from upstream: {e}")))
    }

    /// `POST /api/v1/apps`.
    pub async fn register_app(&self, redirect_uri: &str) -> Result<RegisteredApp, UpstreamError> {
        Self::send_json(self.http.post(self.url("/api/v1/apps")).form(&[
            ("client_name", APP_NAME),
            ("redirect_uris", redirect_uri),
            ("scopes", SCOPES),
        ]))
        .await
    }

    /// `POST /oauth/token` (authorization_code + PKCE verifier).
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse, UpstreamError> {
        Self::send_json(self.http.post(self.url("/oauth/token")).form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code_verifier", code_verifier),
            ("scope", SCOPES),
        ]))
        .await
    }

    /// `POST /oauth/token` (refresh_token grant). Passport rotates both
    /// tokens: the old access and refresh tokens stop working.
    pub async fn refresh_token(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<TokenResponse, UpstreamError> {
        Self::send_json(self.http.post(self.url("/oauth/token")).form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("scope", SCOPES),
        ]))
        .await
    }

    /// `POST /oauth/token` (client_credentials grant, scope read): checks that
    /// the upstream still knows a cached app. Passport issues an app-only
    /// token (unused); an unknown client or wrong secret is 401 invalid_client.
    pub async fn client_credentials(
        &self,
        client_id: &str,
        client_secret: &str,
    ) -> Result<(), UpstreamError> {
        Self::send_json::<Value>(self.http.post(self.url("/oauth/token")).form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("scope", "read"),
        ]))
        .await
        .map(|_| ())
    }

    pub async fn verify_credentials(&self, token: &str) -> Result<Value, UpstreamError> {
        Self::send_json(
            self.http
                .get(self.url("/api/v1/accounts/verify_credentials"))
                .bearer_auth(token),
        )
        .await
    }

    pub async fn home_timeline(
        &self,
        token: &str,
        limit: u32,
    ) -> Result<Vec<Value>, UpstreamError> {
        Self::send_json(
            self.http
                .get(self.url("/api/v1/timelines/home"))
                // Pixelfed hides boosts from the home timeline unless asked
                // (Mastodon includes them by default). The pod fetches one
                // page; a client following the Link header would have to
                // re-add this, as Pixelfed's Link URLs drop it.
                .query(&[
                    ("limit", limit.to_string().as_str()),
                    ("include_reblogs", "true"),
                ])
                .bearer_auth(token),
        )
        .await
    }

    /// `POST /api/v1/statuses/{id}/{action}` (favourite, unfavourite, bookmark,
    /// unbookmark). `id` must already be validated as path-safe.
    pub async fn status_action(
        &self,
        token: &str,
        id: &str,
        action: &str,
    ) -> Result<Value, UpstreamError> {
        Self::send_json(
            self.http
                .post(self.url(&format!("/api/v1/statuses/{id}/{action}")))
                .bearer_auth(token),
        )
        .await
    }

    /// `GET /api/v1/statuses/{id}/context` (`{ancestors, descendants}`). `id`
    /// must already be validated as path-safe.
    pub async fn status_context(&self, token: &str, id: &str) -> Result<Value, UpstreamError> {
        Self::send_json(
            self.http
                .get(self.url(&format!("/api/v1/statuses/{id}/context")))
                .bearer_auth(token),
        )
        .await
    }

    /// `POST /api/v1/statuses` as a reply (a comment) to `in_reply_to_id`.
    /// `idempotency_key`, when given, is sent as `Idempotency-Key` so a
    /// retried request does not post twice (Mastodon semantics).
    pub async fn create_reply(
        &self,
        token: &str,
        in_reply_to_id: &str,
        text: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Value, UpstreamError> {
        let mut req = self
            .http
            .post(self.url("/api/v1/statuses"))
            .bearer_auth(token)
            .form(&[("status", text), ("in_reply_to_id", in_reply_to_id)]);
        if let Some(key) = idempotency_key {
            req = req.header("Idempotency-Key", key);
        }
        Self::send_json(req).await
    }

    /// `GET /api/v1/statuses/{id}`. `id` must be path-safe.
    pub async fn get_status(&self, token: &str, id: &str) -> Result<Value, UpstreamError> {
        Self::send_json(
            self.http
                .get(self.url(&format!("/api/v1/statuses/{id}")))
                .bearer_auth(token),
        )
        .await
    }

    /// Upload one photo: `POST /api/v2/media` (the answer may still be
    /// processing: 202, `url` null), or `/api/v1/media` when the instance has
    /// no v2 (404). Returns the attachment JSON.
    pub async fn upload_media(
        &self,
        token: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<Value, UpstreamError> {
        let form = |bytes: Vec<u8>| -> Result<reqwest::multipart::Form, UpstreamError> {
            let ext = match content_type {
                "image/png" => "png",
                "image/webp" => "webp",
                _ => "jpg",
            };
            let part = reqwest::multipart::Part::bytes(bytes)
                .file_name(format!("photo.{ext}"))
                .mime_str(content_type)
                .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
            Ok(reqwest::multipart::Form::new().part("file", part))
        };
        let send = |path: &str, bytes: Vec<u8>| -> Result<reqwest::RequestBuilder, UpstreamError> {
            Ok(self
                .http
                .post(self.url(path))
                .timeout(UPLOAD_TIMEOUT)
                .bearer_auth(token)
                .multipart(form(bytes)?))
        };
        match Self::send_json(send("/api/v2/media", bytes.clone())?).await {
            Err(UpstreamError::Status(StatusCode::NOT_FOUND, _)) => {
                Self::send_json(send("/api/v1/media", bytes)?).await
            }
            other => other,
        }
    }

    /// `GET /api/v1/media/{id}`: the attachment, `Ok(None)` while it is still
    /// processing (206, or no `url` yet). `id` must be path-safe.
    pub async fn media_status(
        &self,
        token: &str,
        id: &str,
    ) -> Result<Option<Value>, UpstreamError> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/media/{id}")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Unreachable(e.to_string()))?;
        if status == StatusCode::PARTIAL_CONTENT {
            return Ok(None);
        }
        if !status.is_success() {
            let mut text = String::from_utf8_lossy(&body).into_owned();
            text.truncate(500);
            return Err(UpstreamError::Status(status, text));
        }
        let v: Value = serde_json::from_slice(&body)
            .map_err(|e| UpstreamError::Unreachable(format!("invalid JSON from upstream: {e}")))?;
        Ok(v["url"].is_string().then_some(v))
    }

    /// `POST /api/v1/statuses` with `media_ids[]`: a new post.
    pub async fn create_post(
        &self,
        token: &str,
        caption: &str,
        media_ids: &[String],
        idempotency_key: &str,
    ) -> Result<Value, UpstreamError> {
        let mut form: Vec<(&str, &str)> = vec![("status", caption), ("visibility", "public")];
        form.extend(media_ids.iter().map(|id| ("media_ids[]", id.as_str())));
        Self::send_json(
            self.http
                .post(self.url("/api/v1/statuses"))
                .bearer_auth(token)
                .header("Idempotency-Key", idempotency_key)
                .form(&form),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_response_parsing_is_lenient() {
        // Real Pixelfed 0.15.2 (Laravel Passport): ISO created_at, no scope.
        let t: TokenResponse = serde_json::from_str(
            r#"{"token_type":"Bearer","expires_in":31536000,"access_token":"eyJ0","refresh_token":"def5",
                "created_at":"2026-11-02T03:24:18+00:00"}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "eyJ0");
        assert_eq!(t.refresh_token.as_deref(), Some("def5"));
        assert_eq!(t.expires_in, Some(31_536_000));
        // Mastodon: integer created_at, scope, no refresh token / expiry.
        let t: TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","token_type":"Bearer","scope":"read write","created_at":1793472130}"#,
        )
        .unwrap();
        assert_eq!((t.refresh_token, t.expires_in), (None, None));
        // Odd types for optional fields are ignored, not fatal.
        let t: TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","refresh_token":null,"expires_in":"3600","created_at":null}"#,
        )
        .unwrap();
        assert_eq!((t.refresh_token, t.expires_in), (None, Some(3600)));
        assert!(serde_json::from_str::<TokenResponse>(r#"{"token_type":"Bearer"}"#).is_err());
    }

    #[test]
    fn public_ips() {
        for ok in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
        ] {
            assert!(is_public_ip(ok.parse().unwrap()), "{ok}");
        }
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "198.18.0.1",
            "192.0.0.8",
            "::1",
            "::",
            "fdaa:0:1::2",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(bad.parse().unwrap()), "{bad}");
        }
    }

    #[tokio::test]
    async fn public_client_refuses_private_names() {
        let clients = Clients::new(MediaPolicy::default());
        let api = clients.api(&Instance {
            url: "https://localhost".into(),
            trusted: false,
        });
        match api.verify_credentials("t").await {
            Err(UpstreamError::Unreachable(e)) => assert!(e.contains("error"), "{e}"),
            other => panic!("expected a refused connection, got {other:?}"),
        }
    }
}
