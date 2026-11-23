//! The taste service's side of the wire: its HTTP API, every request with
//! the pod's bearer token for it.
//!
//! - `GET /api/feed?n=<n>`: `{items}`, a new page each call (the service
//!   records what it served).
//! - `POST /api/event {id, kind}`: `like`, `skip` or `clear`; `{ok, likes, skips}`.
//! - `GET /api/item/<id>`, `.../similar`, `.../contrast`, `.../steer?q=`: an
//!   item, or `{items}`.
//! - `GET /api/search?q=`, `GET /api/likes`: `{items}`.
//! - `GET /api/taste`: `{likes, skips, tastes, summary}`; `POST
//!   /api/taste/summary`: `{text, likes}`.
//!
//! Everything it sends is read leniently and treated as untrusted display
//! text ([`crate::bundle`]).

use std::time::Duration;

use pod_core::net::{OutboundClient, OutboundError, OutboundResponse};
use serde_json::{Value, json};

/// The service the pod is signed in to: its base URL and the pod's token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The service refused the token (401).
    Unauthorized,
    /// Another refusal: its status and the service's words.
    Status(u16, String),
    /// No answer, or one that can't be read.
    Unreachable(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "the taste service refused the pod's token"),
            ApiError::Status(s, d) if d.is_empty() => write!(f, "the taste service answered {s}"),
            ApiError::Status(s, d) => write!(f, "the taste service answered {s}: {d}"),
            ApiError::Unreachable(e) => write!(f, "the taste service can't be reached: {e}"),
        }
    }
}

/// The request timeout. Writing a taste summary asks a model, so it gets
/// longer ([`SUMMARY_TIMEOUT`]).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
pub const SUMMARY_TIMEOUT: Duration = Duration::from_secs(120);
/// The most the pod reads of one answer.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_ERROR_BODY: usize = 16 * 1024;

/// The HTTP client. It follows no redirects: the token goes to the service
/// and nowhere else.
pub fn client(allow_private: bool) -> OutboundClient {
    OutboundClient::builder(REQUEST_TIMEOUT)
        .private(allow_private)
        .no_redirects()
        .body_cap(MAX_BODY)
        .build()
}

/// A service URL as typed: `Ok(base)` for an `https://` URL of a public host
/// (any `http(s)://` URL when `allow_private`), without path, query or
/// credentials.
pub fn check_url(url: &str, allow_private: bool) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(url).map_err(|_| "that is not a URL".to_owned())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the URL must not carry a user name or password".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() || parsed.path() != "/" {
        return Err("the URL is the service's base address, e.g. https://taste.example".into());
    }
    let ok = if allow_private {
        matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
    } else {
        pod_core::net::is_public_https(&parsed)
    };
    if !ok {
        return Err("the URL must be https:// on a public host name".into());
    }
    Ok(url.to_owned())
}

/// The host (and port) of a URL, for showing.
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str().map(|h| match u.port() {
                Some(p) => format!("{h}:{p}"),
                None => h.to_owned(),
            })
        })
        .unwrap_or_default()
}

/// `<base>/api/<segments...>`, each segment percent-encoded (item ids hold
/// `:` and `.`).
fn url(service: &Service, segments: &[&str]) -> Result<reqwest::Url, ApiError> {
    let mut u = reqwest::Url::parse(&service.url)
        .map_err(|_| ApiError::Unreachable("the service's URL is not a URL".into()))?;
    u.path_segments_mut()
        .map_err(|_| ApiError::Unreachable("the service's URL is not a base".into()))?
        .pop_if_empty()
        .push("api")
        .extend(segments);
    Ok(u)
}

async fn json_capped(resp: OutboundResponse, max: usize) -> Result<Value, ApiError> {
    let bytes = resp.body_cap(max).bytes().await.map_err(|e| {
        ApiError::Unreachable(if e.is_too_large() {
            "the answer is too long".into()
        } else {
            short(&e)
        })
    })?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::Unreachable("unreadable answer".into()))
}

async fn answer(resp: Result<OutboundResponse, OutboundError>) -> Result<Value, ApiError> {
    let resp = resp.map_err(|e| ApiError::Unreachable(short(&e)))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(ApiError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        let body = json_capped(resp, MAX_ERROR_BODY)
            .await
            .unwrap_or(Value::Null);
        let detail = body
            .get("detail")
            .or_else(|| body.get("error"))
            .and_then(Value::as_str)
            .map(|s| crate::bundle::clean(s, 200))
            .unwrap_or_default();
        return Err(ApiError::Status(status, detail));
    }
    json_capped(resp, MAX_BODY).await
}

/// The `items` list of an answer.
fn items(v: Value) -> Result<Vec<Value>, ApiError> {
    match v.get("items").and_then(Value::as_array) {
        Some(a) => Ok(a.iter().filter(|i| i.is_object()).cloned().collect()),
        None => Err(ApiError::Unreachable("not a list of items".into())),
    }
}

async fn get(
    c: &OutboundClient,
    s: &Service,
    segments: &[&str],
    query: &[(&str, String)],
) -> Result<Value, ApiError> {
    let u = url(s, segments)?;
    answer(c.get(u).query(query).bearer_auth(&s.token).send().await).await
}

/// `GET /api/taste`: also how a sign-in checks the URL and token.
pub async fn taste(c: &OutboundClient, s: &Service) -> Result<Value, ApiError> {
    let v = get(c, s, &["taste"], &[]).await?;
    if !v.get("tastes").is_some_and(Value::is_array) {
        return Err(ApiError::Unreachable(
            "that address doesn't answer like a taste service".into(),
        ));
    }
    Ok(v)
}

/// `GET /api/feed?n=<n>`: the next page.
pub async fn feed(c: &OutboundClient, s: &Service, n: usize) -> Result<Vec<Value>, ApiError> {
    items(get(c, s, &["feed"], &[("n", n.to_string())]).await?)
}

/// `POST /api/event {id, kind}`: the service's like and skip counts after it.
pub async fn event(
    c: &OutboundClient,
    s: &Service,
    id: &str,
    kind: &str,
) -> Result<(i64, i64), ApiError> {
    let u = url(s, &["event"])?;
    let v = answer(
        c.post(u)
            .json(&json!({"id": id, "kind": kind}))
            .bearer_auth(&s.token)
            .send()
            .await,
    )
    .await?;
    let n = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    Ok((n("likes"), n("skips")))
}

/// `GET /api/item/<id>`.
pub async fn item(c: &OutboundClient, s: &Service, id: &str) -> Result<Value, ApiError> {
    get(c, s, &["item", id], &[]).await
}

/// `GET /api/item/<id>/<list>?n=<n>` for `similar` and `contrast`.
pub async fn related(
    c: &OutboundClient,
    s: &Service,
    id: &str,
    list: &str,
    n: usize,
) -> Result<Vec<Value>, ApiError> {
    items(get(c, s, &["item", id, list], &[("n", n.to_string())]).await?)
}

/// `GET /api/item/<id>/steer?q=<q>&n=<n>`: "this, but <q>".
pub async fn steer(
    c: &OutboundClient,
    s: &Service,
    id: &str,
    q: &str,
    n: usize,
) -> Result<Vec<Value>, ApiError> {
    items(
        get(
            c,
            s,
            &["item", id, "steer"],
            &[("q", q.to_owned()), ("n", n.to_string())],
        )
        .await?,
    )
}

/// `GET /api/search?q=<q>&n=<n>`.
pub async fn search(
    c: &OutboundClient,
    s: &Service,
    q: &str,
    n: usize,
) -> Result<Vec<Value>, ApiError> {
    items(
        get(
            c,
            s,
            &["search"],
            &[("q", q.to_owned()), ("n", n.to_string())],
        )
        .await?,
    )
}

/// `GET /api/likes`.
pub async fn likes(c: &OutboundClient, s: &Service) -> Result<Vec<Value>, ApiError> {
    items(get(c, s, &["likes"], &[]).await?)
}

/// `POST /api/taste/summary`: `{text, likes}`.
pub async fn summary(c: &OutboundClient, s: &Service) -> Result<Value, ApiError> {
    let u = url(s, &["taste", "summary"])?;
    answer(
        c.post(u)
            .timeout(SUMMARY_TIMEOUT)
            .bearer_auth(&s.token)
            .send()
            .await,
    )
    .await
}

/// An error without the request URL.
fn short(e: &OutboundError) -> String {
    if let OutboundError::Refused(_) = e {
        "its address is not allowed".into()
    } else if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "no connection".into()
    } else if e.is_decode() {
        "unreadable answer".into()
    } else {
        "request failed".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_encode_item_ids() {
        let s = Service {
            url: "https://taste.example".into(),
            token: "t".into(),
        };
        let u = url(&s, &["item", "shop.example:12 3", "similar"]).unwrap();
        assert_eq!(
            u.as_str(),
            "https://taste.example/api/item/shop.example:12%203/similar"
        );
    }

    #[test]
    fn service_urls_are_public_https_bases() {
        assert_eq!(
            check_url(" https://taste.example/ ", false).unwrap(),
            "https://taste.example"
        );
        assert!(check_url("http://taste.example", false).is_err());
        assert!(check_url("https://127.0.0.1", false).is_err());
        assert!(check_url("https://taste.example/api", false).is_err());
        assert!(check_url("https://a:b@taste.example", false).is_err());
        assert!(check_url("http://127.0.0.1:9", true).is_ok());
    }
}
