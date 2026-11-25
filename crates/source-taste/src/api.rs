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
//! - `GET /api/map/tiles?x0=&y0=&x1=&y1=&n=`: the map of the catalog (0..1,
//!   y down) cut into an n × n grid over that box, `{bounds, n, tiles}`, each
//!   tile `{i, j, x0, y0, x1, y1, count, likes, skips, p, color, item}`;
//!   `GET /api/map/items?x0=&y0=&x1=&y1=&n=`: `{items}` inside the box.
//!
//! - `PUT /api/state {log, events, served}`, `GET /api/state`: the pod's
//!   history whole (decision `taste-history-in-the-pod`).
//!
//! Every request names the pod's history, `X-Taste-State: <log>/<n>` (n:
//! the reactions in it), once the pod keeps its own ([`StateLink`]). The
//! service answers from its copy when it holds exactly that version and 409
//! `state_unknown` when it doesn't: then the pod sends the whole history and
//! asks again, once ([`send`]).
//!
//! Everything it sends is read leniently and treated as untrusted display
//! text ([`crate::bundle`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use pod_core::net::{OutboundClient, OutboundError, OutboundRequest, OutboundResponse};
use serde_json::{Value, json};

/// The service the pod is signed in to: its base URL and the pod's token,
/// and the pod's history to name on each request (`None`: the service's own
/// state, before the pod has taken its history over).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub url: String,
    pub token: String,
    pub state: Option<StateLink>,
}

impl Service {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        Service {
            url: url.into(),
            token: token.into(),
            state: None,
        }
    }
}

/// The pod's history as a request names it: `<log>/<n>`, and how to read
/// the whole of it when the service doesn't hold that version.
#[derive(Clone)]
pub struct StateLink {
    pub tag: String,
    pub whole: Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Value>> + Send>> + Send + Sync>,
}

impl std::fmt::Debug for StateLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateLink").field("tag", &self.tag).finish()
    }
}

impl PartialEq for StateLink {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag
    }
}

impl Eq for StateLink {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The service refused the token (401).
    Unauthorized,
    /// The service doesn't hold the history the request named (409
    /// `state_unknown`), even after it was sent.
    StateUnknown,
    /// Another refusal: its status and the service's words.
    Status(u16, String),
    /// No answer, or one that can't be read.
    Unreachable(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "the taste service refused the pod's token"),
            ApiError::StateUnknown => {
                write!(f, "the taste service didn't take the pod's history")
            }
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
/// The most the pod reads of the whole map (every item's place: tens of
/// thousands of items, several megabytes).
pub const MAX_MAP_BODY: usize = 24 * 1024 * 1024;
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
    answer_capped(resp, MAX_BODY).await
}

async fn answer_capped(
    resp: Result<OutboundResponse, OutboundError>,
    max: usize,
) -> Result<Value, ApiError> {
    let resp = resp.map_err(|e| ApiError::Unreachable(short(&e)))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(ApiError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        let body = json_capped(resp, MAX_ERROR_BODY)
            .await
            .unwrap_or(Value::Null);
        if status == 409 && body.get("error").and_then(Value::as_str) == Some("state_unknown") {
            return Err(ApiError::StateUnknown);
        }
        let detail = body
            .get("detail")
            .or_else(|| body.get("error"))
            .and_then(Value::as_str)
            .map(|s| crate::bundle::clean(s, 200))
            .unwrap_or_default();
        return Err(ApiError::Status(status, detail));
    }
    json_capped(resp, max).await
}

/// The `items` list of an answer.
fn items(v: Value) -> Result<Vec<Value>, ApiError> {
    match v.get("items").and_then(Value::as_array) {
        Some(a) => Ok(a.iter().filter(|i| i.is_object()).cloned().collect()),
        None => Err(ApiError::Unreachable("not a list of items".into())),
    }
}

/// Send the request `build` makes, with the token and the pod's history
/// named. When the service doesn't hold that history, send it the whole of
/// it and ask again, once.
async fn send(
    c: &OutboundClient,
    s: &Service,
    max: usize,
    build: impl Fn() -> OutboundRequest,
) -> Result<Value, ApiError> {
    let named = |r: OutboundRequest| {
        let r = r.bearer_auth(&s.token);
        match &s.state {
            Some(link) => r.header("x-taste-state", link.tag.as_str()),
            None => r,
        }
    };
    match answer_capped(named(build()).send().await, max).await {
        Err(ApiError::StateUnknown) if s.state.is_some() => {
            let whole = s.state.as_ref().map(|l| (l.whole)());
            let Some(history) = (match whole {
                Some(f) => f.await,
                None => None,
            }) else {
                return Err(ApiError::Unreachable(
                    "the pod couldn't read its taste history".into(),
                ));
            };
            put_state(c, s, &history).await?;
            answer_capped(named(build()).send().await, max).await
        }
        other => other,
    }
}

/// `PUT /api/state`: the pod's history whole (`{log, events, served}`).
pub async fn put_state(c: &OutboundClient, s: &Service, history: &Value) -> Result<(), ApiError> {
    let u = url(s, &["state"])?;
    answer(
        c.request(reqwest::Method::PUT, u)
            .timeout(SUMMARY_TIMEOUT)
            .json(history)
            .bearer_auth(&s.token)
            .send()
            .await,
    )
    .await
    .map(|_| ())
}

/// `GET /api/state` without naming a history: the service's own, which the
/// pod takes over once ([`crate::db::take_over`]).
pub async fn own_state(c: &OutboundClient, s: &Service) -> Result<Value, ApiError> {
    let u = url(s, &["state"])?;
    let v = answer_capped(
        c.get(u)
            .timeout(SUMMARY_TIMEOUT)
            .bearer_auth(&s.token)
            .send()
            .await,
        MAX_MAP_BODY,
    )
    .await?;
    if !v.get("events").is_some_and(Value::is_array) {
        return Err(ApiError::Unreachable("not a taste history".into()));
    }
    Ok(v)
}

async fn get(
    c: &OutboundClient,
    s: &Service,
    segments: &[&str],
    query: &[(&str, String)],
) -> Result<Value, ApiError> {
    let u = url(s, segments)?;
    send(c, s, MAX_BODY, || c.get(u.clone()).query(query)).await
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

/// `GET /api/taste/<index>?n=<n>`: one taste to browse (its likes, then the
/// catalog's nearest pieces not yet liked or disliked, up to `n` in all).
pub async fn one_taste(
    c: &OutboundClient,
    s: &Service,
    index: usize,
    n: usize,
) -> Result<Value, ApiError> {
    get(
        c,
        s,
        &["taste", &index.to_string()],
        &[("n", n.to_string())],
    )
    .await
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
    let body = json!({"id": id, "kind": kind});
    let v = send(c, s, MAX_BODY, || c.post(u.clone()).json(&body)).await?;
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

/// A box of the map, `0..1` each way (y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Bounds {
    fn query(&self) -> Vec<(&'static str, String)> {
        vec![
            ("x0", self.x0.to_string()),
            ("y0", self.y0.to_string()),
            ("x1", self.x1.to_string()),
            ("y1", self.y1.to_string()),
        ]
    }
}

/// The whole map: every item's id and place (0..1, y down), the user's
/// likes and skips (by index) and the model's like chance for each (`None`
/// before it has enough reactions).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapData {
    pub ids: Vec<String>,
    pub xy: Vec<[f32; 2]>,
    pub liked: std::collections::HashSet<usize>,
    pub skipped: std::collections::HashSet<usize>,
    pub p: Option<Vec<f32>>,
}

impl MapData {
    /// Reads `GET /api/map`'s answer; `None` when it isn't one.
    pub fn from_json(v: &Value) -> Option<MapData> {
        let ids: Vec<String> = v
            .get("ids")?
            .as_array()?
            .iter()
            .map(|i| i.as_str().map(str::to_owned))
            .collect::<Option<_>>()?;
        let xy: Vec<[f32; 2]> = v
            .get("xy")?
            .as_array()?
            .iter()
            .map(|p| {
                let a = p.as_array()?;
                Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            })
            .collect::<Option<_>>()?;
        if xy.len() != ids.len() {
            return None;
        }
        let rows = |k: &str| -> std::collections::HashSet<usize> {
            v.get(k)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .map(|r| r as usize)
                .filter(|r| *r < ids.len())
                .collect()
        };
        let p = v.get("p").and_then(Value::as_array).and_then(|a| {
            (a.len() == ids.len()).then(|| {
                a.iter()
                    .map(|x| x.as_f64().map_or(f32::NAN, |x| x as f32))
                    .collect()
            })
        });
        Some(MapData {
            liked: rows("liked"),
            skipped: rows("skipped"),
            ids,
            xy,
            p,
        })
    }

    /// The index of item `id`.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|i| i == id)
    }
}

/// `GET /api/map`: the whole map ([`MapData`]).
pub async fn map(c: &OutboundClient, s: &Service) -> Result<MapData, ApiError> {
    let u = url(s, &["map"])?;
    let v = send(c, s, MAX_MAP_BODY, || c.get(u.clone())).await?;
    MapData::from_json(&v).ok_or_else(|| ApiError::Unreachable("not a map".into()))
}

/// `GET /api/map/tiles`: the box cut into an `n` × `n` grid.
pub async fn map_tiles(
    c: &OutboundClient,
    s: &Service,
    b: Bounds,
    n: usize,
) -> Result<Value, ApiError> {
    let mut q = b.query();
    q.push(("n", n.to_string()));
    let v = get(c, s, &["map", "tiles"], &q).await?;
    if !v.get("tiles").is_some_and(Value::is_array) {
        return Err(ApiError::Unreachable("not a map".into()));
    }
    Ok(v)
}

/// `GET /api/map/items`: pieces inside the box.
pub async fn map_items(
    c: &OutboundClient,
    s: &Service,
    b: Bounds,
    n: usize,
) -> Result<Vec<Value>, ApiError> {
    let mut q = b.query();
    q.push(("n", n.to_string()));
    items(get(c, s, &["map", "items"], &q).await?)
}

/// `POST /api/taste/summary`: `{text, likes}`.
pub async fn summary(c: &OutboundClient, s: &Service) -> Result<Value, ApiError> {
    let u = url(s, &["taste", "summary"])?;
    send(c, s, MAX_BODY, || {
        c.post(u.clone()).timeout(SUMMARY_TIMEOUT)
    })
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
        let s = Service::new("https://taste.example", "t");
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
