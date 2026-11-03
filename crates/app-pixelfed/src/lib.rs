//! The Pixelfed adapter (M6, D40): the `pixelfed_home` app, the `pixelfed`
//! sign-in and the `pixelfed_demo` dev app, plugged into the pod through
//! [`pod_core::AppAdapter`] ([`PixelfedAdapter`], [`apps::demo_adapter`]).
//!
//! Mastodon-API JSON -> normalized Construct entities: everything in this
//! module (except [`upstream`], [`routes`], [`adapter`] and [`db`]) is pure: it
//! turns upstream JSON into URI-keyed [`EntityRecord`]s and an ordered timeline.
//! Input is parsed leniently (every optional Mastodon field may be absent or
//! null; a status that cannot be parsed at all is skipped and logged).
//!
//! Entity shapes (docs/M6 contract):
//! - `pixelfed://posts/<id>`: id, author_uri, author_name, author_handle (`@acct`),
//!   author_avatar_url, media_uris, caption, content_warning, content_warning_label
//!   ("CW: ..."), sensitive, like_count, is_liked, is_bookmarked, reply_count,
//!   created_at (unix s), url, and `reblogged_by_uri` / `reblogged_by_name` when the
//!   timeline entry was a reblog (the post is the INNER status, so actions target the
//!   inner id). The author fields are copies, so layouts bind them on the post (D36).
//!   `cover_url` / `cover_blurhash` (the first media's preview, else its image) and
//!   `media_count` are copies too, for the cards view (D48).
//! - `pixelfed://users/<id>`: id, username (acct), display_name, avatar_url.
//! - `pixelfed://media/<id>`: id, type, url, preview_url, aspect_ratio, alt_text, blurhash.
//!
//! Every image URL is rewritten through [`MediaUrls`] to the pod's media proxy
//! (or dropped to null when its origin is not in the [`MediaAllowlist`]).

pub mod adapter;
pub mod apps;
pub mod config;
pub mod db;
pub mod layout;
pub mod routes;
pub mod upstream;

pub use adapter::{PixelfedAdapter, SIGN_IN};

/// Pod setting (D46): `"0"` keeps the pod on the default instance: no
/// typed-in instances and no media from other hosts. A pod host sets it on
/// temporary pods, which anyone can create, so they can't make the server
/// contact hosts of their choosing.
pub const OTHER_INSTANCES_SETTING: &str = "pixelfed.other_instances";
pub use config::{DEFAULT_PIXELFED_URL, Instance, PixelfedConfig};

use std::collections::{BTreeMap, HashSet};

use protocol::EntityRecord;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const POST_PREFIX: &str = "pixelfed://posts/";
pub const USER_PREFIX: &str = "pixelfed://users/";
pub const MEDIA_PREFIX: &str = "pixelfed://media/";
/// Query key the pixelfed_home FeedList renders.
pub const HOME_QUERY: &str = "timeline:home";

// ---------- lenient upstream types ----------

/// Mastodon ids are strings, but some servers emit numbers; accept both.
fn de_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) if !s.is_empty() => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!("invalid id: {other}"))),
    }
}

/// A list whose unparseable items are skipped (logged); null/absent/non-array -> empty.
fn de_lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let Some(Value::Array(items)) = Option::<Value>::deserialize(d)? else {
        return Ok(Vec::new());
    };
    Ok(items
        .into_iter()
        .filter_map(|v| match T::deserialize(&v) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(error = %e, id = %v["id"], "skipping unparseable Pixelfed list item");
                None
            }
        })
        .collect())
}

/// A count that may be a number, a numeric string or null/absent.
fn de_lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

/// A flag that may be a bool, 0/1, "true"/"false"/"0"/"1" or null/absent.
fn de_lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Bool(b)) => Some(b),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0),
        Some(Value::String(s)) => match s.trim() {
            "true" | "1" => Some(true),
            "false" | "0" | "" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

/// A dimension that may be a number (integer or float), a numeric string or null.
fn de_lenient_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

/// An optional id: a string or a number; anything else (null, absent, ...) is None.
fn de_opt_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

/// A string field that some servers send as another JSON type: non-strings
/// (numbers, objects, ...) read as absent rather than failing the status.
fn de_lenient_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) => Some(s),
        _ => None,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub username: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub acct: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub display_name: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub avatar: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub avatar_static: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaSize {
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub width: Option<f64>,
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub height: Option<f64>,
    /// Pixelfed sends an integer (`1`) for square images.
    #[serde(default, deserialize_with = "de_lenient_f64")]
    pub aspect: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaMeta {
    #[serde(default, deserialize_with = "de_lenient_obj")]
    pub original: Option<MediaSize>,
    /// Absent on Pixelfed.
    #[serde(default, deserialize_with = "de_lenient_obj")]
    pub small: Option<MediaSize>,
}

/// An object field that may be null, absent or of the wrong shape (-> None).
fn de_lenient_obj<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    Ok(Option::<Value>::deserialize(d)?.and_then(|v| T::deserialize(v).ok()))
}

#[derive(Debug, Clone, Deserialize)]
pub struct MediaAttachment {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, rename = "type", deserialize_with = "de_lenient_str")]
    pub kind: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub preview_url: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub remote_url: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub blurhash: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_obj")]
    pub meta: Option<MediaMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub uri: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub created_at: Option<String>,
    /// Set on a reply (a comment).
    #[serde(default, deserialize_with = "de_opt_id")]
    pub in_reply_to_id: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub content: Option<String>,
    /// Pixelfed sometimes carries a plain-text rendition.
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub content_text: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub spoiler_text: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_bool")]
    pub sensitive: Option<bool>,
    #[serde(default, deserialize_with = "de_lenient_i64")]
    pub replies_count: Option<i64>,
    /// Pixelfed's `_pe` entity (also used for a reblog's inner status, even in
    /// Mastodon mode) names it `reply_count`. Not a serde alias: some objects
    /// carry both, which an alias would reject as a duplicate field.
    #[serde(default, deserialize_with = "de_lenient_i64")]
    pub reply_count: Option<i64>,
    #[serde(default, deserialize_with = "de_lenient_i64")]
    pub favourites_count: Option<i64>,
    #[serde(default, deserialize_with = "de_lenient_bool")]
    pub favourited: Option<bool>,
    #[serde(default, deserialize_with = "de_lenient_bool")]
    pub bookmarked: Option<bool>,
    pub account: Account,
    /// An attachment that does not parse is dropped (logged), not the status.
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub media_attachments: Vec<MediaAttachment>,
    /// A malformed reblog object reads as no reblog (the wrapper is then
    /// shown as a plain status) rather than dropping the entry.
    #[serde(default, deserialize_with = "de_lenient_obj")]
    pub reblog: Option<Box<Status>>,
}

impl Status {
    /// The status actions apply to: the reblogged one for a reblog wrapper.
    pub fn target(&self) -> &Status {
        self.reblog.as_deref().unwrap_or(self)
    }
}

// ---------- media URL rewriting ----------

/// Opaque, stable proxy key for an upstream URL (first 128 bits of SHA-256, hex).
pub fn media_key(url: &str) -> String {
    hex(&Sha256::digest(url.as_bytes())[..16])
}

pub use pod_core::util::hex;

/// The origins the media proxy may fetch (SSRF guard): media URLs come from
/// upstream content, which a federated post can fill with any URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaAllowlist {
    /// (lowercase host, port). `None`: the URL must use its scheme's default port.
    origins: Vec<(String, Option<u16>)>,
}

impl MediaAllowlist {
    /// `bases`: full URLs (their host and effective port). `hosts`: `host`,
    /// `host:port` or a full `scheme://host[:port]` URL.
    pub fn new<'a>(
        bases: impl IntoIterator<Item = &'a str>,
        hosts: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut origins = Vec::new();
        for base in bases {
            match reqwest::Url::parse(base.trim()) {
                Ok(u) if u.host_str().is_some() => origins.push((
                    u.host_str().unwrap_or_default().to_ascii_lowercase(),
                    u.port_or_known_default(),
                )),
                _ => tracing::warn!(base, "unparseable Pixelfed base URL in media allowlist"),
            }
        }
        for host in hosts {
            let host = host.trim();
            let parsed = if host.contains("://") {
                reqwest::Url::parse(host).map(|u| {
                    let port = u.port_or_known_default();
                    (u, port)
                })
            } else {
                reqwest::Url::parse(&format!("http://{host}")).map(|u| {
                    let port = u.port();
                    (u, port)
                })
            };
            match parsed {
                Ok((u, port)) if u.host_str().is_some() => {
                    origins.push((u.host_str().unwrap_or_default().to_ascii_lowercase(), port))
                }
                _ => tracing::warn!(host, "ignoring unparseable PIXELFED_MEDIA_HOSTS entry"),
            }
        }
        Self { origins }
    }

    pub fn allows_url(&self, url: &reqwest::Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return false;
        };
        self.origins.iter().any(|(h, port)| {
            *h == host
                && match port {
                    Some(p) => url.port_or_known_default() == Some(*p),
                    None => url.port().is_none(),
                }
        })
    }

    pub fn allows(&self, url: &str) -> bool {
        reqwest::Url::parse(url).is_ok_and(|u| self.allows_url(&u))
    }
}

/// How the pod may fetch a media URL ([`MediaPolicy::route`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaRoute {
    /// An origin in the [`MediaAllowlist`] (configured by the operator).
    Trusted,
    /// Any other public HTTPS URL, allowed when users may sign in to any
    /// instance (D45): fetched only from public IP addresses.
    Public,
}

/// Which media URLs the pod rewrites and proxies: the [`MediaAllowlist`],
/// plus (with `any_public_https`) every `https://<public host>` URL, since
/// a user-chosen instance may serve media from any CDN.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaPolicy {
    pub allow: MediaAllowlist,
    pub any_public_https: bool,
}

impl From<&MediaAllowlist> for MediaPolicy {
    fn from(allow: &MediaAllowlist) -> Self {
        Self {
            allow: allow.clone(),
            any_public_https: false,
        }
    }
}

impl From<&MediaPolicy> for MediaPolicy {
    fn from(p: &MediaPolicy) -> Self {
        p.clone()
    }
}

impl MediaPolicy {
    pub fn route_url(&self, url: &reqwest::Url) -> Option<MediaRoute> {
        if self.allow.allows_url(url) {
            Some(MediaRoute::Trusted)
        } else if self.any_public_https
            && url.scheme() == "https"
            && url.port().is_none()
            && url.host_str().is_some_and(config::is_public_host)
        {
            Some(MediaRoute::Public)
        } else {
            None
        }
    }

    pub fn route(&self, url: &str) -> Option<MediaRoute> {
        reqwest::Url::parse(url)
            .ok()
            .and_then(|u| self.route_url(&u))
    }

    pub fn allows(&self, url: &str) -> bool {
        self.route(url).is_some()
    }
}

/// Warn once per host (per process) about media URLs outside the allowlist.
fn log_dropped_media_host(url: &str) {
    static LOGGED: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "<unparseable>".to_owned());
    let mut guard = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    if guard.get_or_insert_with(HashSet::new).insert(host.clone()) {
        tracing::warn!(
            %host,
            "media URL host is not in the allowlist (PIXELFED_MEDIA_HOSTS); dropped (logged once per host)"
        );
    }
}

/// Rewrites upstream image URLs to `<proxy_base>/<key>` and remembers
/// key -> upstream URL for the URLs it has emitted. URLs outside the
/// [`MediaPolicy`] are dropped (null), so the client shows a placeholder.
#[derive(Debug, Clone)]
pub struct MediaUrls {
    proxy_base: String,
    allow: MediaPolicy,
    pub keys: BTreeMap<String, String>,
}

impl MediaUrls {
    /// `proxy_base` like `http://10.0.2.2:8080/v1/pixelfed/media` (no trailing slash).
    pub fn new(proxy_base: &str, policy: impl Into<MediaPolicy>) -> Self {
        Self {
            proxy_base: proxy_base.trim_end_matches('/').to_owned(),
            allow: policy.into(),
            keys: BTreeMap::new(),
        }
    }

    /// The proxy URL for `url`, or `None` if its origin is not allowed.
    pub fn rewrite(&mut self, url: &str) -> Option<String> {
        if !self.allow.allows(url) {
            log_dropped_media_host(url);
            return None;
        }
        let key = media_key(url);
        let out = format!("{}/{key}", self.proxy_base);
        self.keys.insert(key, url.to_owned());
        Some(out)
    }

    fn rewrite_opt(&mut self, url: Option<&str>) -> Value {
        match url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .and_then(|u| self.rewrite(u))
        {
            Some(u) => Value::String(u),
            None => Value::Null,
        }
    }
}

// ---------- normalization ----------

/// A normalized timeline: entities plus the ordered post URIs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    pub entities: BTreeMap<String, EntityRecord>,
    pub order: Vec<String>,
}

pub fn post_uri(id: &str) -> String {
    format!("{POST_PREFIX}{id}")
}

pub fn user_uri(id: &str) -> String {
    format!("{USER_PREFIX}{id}")
}

pub fn media_uri(id: &str) -> String {
    format!("{MEDIA_PREFIX}{id}")
}

/// Normalize a `GET /api/v1/timelines/home` response (a JSON array). Entries
/// that do not parse as a status are skipped (logged). A reblog becomes its
/// inner post (with `reblogged_by_uri`); order follows the outer list and a
/// post seen twice keeps its first position.
pub fn normalize_timeline(statuses: &[Value], media: &mut MediaUrls) -> Timeline {
    let mut out = Timeline::default();
    let mut seen = HashSet::new();
    for raw in statuses {
        let status: Status = match Status::deserialize(raw) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, id = %raw["id"], "skipping unparseable Pixelfed status");
                continue;
            }
        };
        let target = status.target();
        let uri = post_uri(&target.id);
        if !seen.insert(uri.clone()) {
            continue;
        }
        let mut post = normalize_post(target, media, &mut out.entities);
        if status.reblog.is_some() {
            let by = add_user(&status.account, media, &mut out.entities);
            post.set(
                "reblogged_by_name",
                out.entities[&by]
                    .get("display_name")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            post.set("reblogged_by_uri", by);
        }
        out.entities.insert(uri.clone(), post);
        out.order.push(uri);
    }
    out
}

/// Normalize one status (not a reblog wrapper; see [`Status::target`]) into
/// its post record, adding its author and media entities to `entities`.
pub fn normalize_post(
    s: &Status,
    media: &mut MediaUrls,
    entities: &mut BTreeMap<String, EntityRecord>,
) -> EntityRecord {
    let author_uri = add_user(&s.account, media, entities);
    let media_uris: Vec<String> = s
        .media_attachments
        .iter()
        .map(|m| {
            let uri = media_uri(&m.id);
            entities.insert(uri.clone(), normalize_media(m, media));
            uri
        })
        .collect();
    let caption = match s.content.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(html) => html_to_text(html),
        None => s.content_text.clone().unwrap_or_default().trim().to_owned(),
    };
    let content_warning = s
        .spoiler_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let mut post = post_state(s);
    // The author's display fields on the post itself, so a layout binds them
    // as `$item.author_name` etc. (D36).
    let author = &entities[&author_uri];
    post.set(
        "author_name",
        author.get("display_name").cloned().unwrap_or(Value::Null),
    );
    post.set(
        "author_handle",
        format!("@{}", author.get_str("username").unwrap_or_default()),
    );
    post.set(
        "author_avatar_url",
        author.get("avatar_url").cloned().unwrap_or(Value::Null),
    );
    post.set("author_uri", author_uri);
    // The first media, for the cards view (D48).
    let first = media_uris.first().map(|uri| &entities[uri]);
    let cover = first.and_then(|m| {
        ["preview_url", "url"]
            .into_iter()
            .find_map(|k| m.get(k).filter(|v| v.is_string()).cloned())
    });
    post.set("cover_url", cover.unwrap_or(Value::Null));
    post.set(
        "cover_blurhash",
        first
            .and_then(|m| m.get("blurhash").cloned())
            .unwrap_or(Value::Null),
    );
    post.set("media_count", media_uris.len());
    post.set("media_uris", media_uris);
    post.set("caption", caption);
    post.set("content_warning", json!(content_warning));
    post.set(
        "content_warning_label",
        json!(content_warning.map(content_warning_label)),
    );
    post.set("sensitive", s.sensitive.unwrap_or(false));
    post.set(
        "reply_count",
        s.replies_count.or(s.reply_count).unwrap_or(0).max(0),
    );
    post.set(
        "created_at",
        json!(s.created_at.as_deref().and_then(parse_rfc3339)),
    );
    post.set("url", json!(s.url.as_deref().or(s.uri.as_deref())));
    post
}

/// Deepest nesting level reported for a comment; deeper replies are shown at this level.
pub const MAX_COMMENT_DEPTH: usize = 4;

pub const COMMENT_PREFIX: &str = "pixelfed://comments/";

pub fn comment_uri(id: &str) -> String {
    format!("{COMMENT_PREFIX}{id}")
}

/// The query key of the post screen (D48): the one post.
pub fn post_query(post_id: &str) -> String {
    format!("post:{post_id}")
}

/// The post screen's data (D48) from a `GET /api/v1/statuses/<id>` response:
/// the post (the inner one of a reblog) with its author and media, and
/// `queries["post:<id>"]` listing it. `None` when it is not a status.
pub fn normalize_post_screen(
    id: &str,
    raw: &Value,
    media: &mut MediaUrls,
) -> Option<protocol::DataBundle> {
    let status = Status::deserialize(raw)
        .inspect_err(|e| tracing::warn!(error = %e, %id, "unparseable Pixelfed status"))
        .ok()?;
    let target = status.target();
    let mut entities = BTreeMap::new();
    let post = normalize_post(target, media, &mut entities);
    let uri = post_uri(&target.id);
    entities.insert(uri.clone(), post);
    Some(protocol::DataBundle {
        entities,
        queries: BTreeMap::from([(post_query(id), vec![uri])]),
    })
}

/// The query key of a post's comments (the comments screen lists it).
pub fn comments_query(post_id: &str) -> String {
    format!("comments:{post_id}")
}

/// "just now", "5 min", "3 h", "2 d", else the date (`2026-10-31`).
pub fn relative_time(created: i64, now: i64) -> String {
    let d = (now - created).max(0);
    match d {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{} min", d / 60),
        3_600..86_400 => format!("{} h", d / 3_600),
        86_400..604_800 => format!("{} d", d / 86_400),
        _ => {
            let days = created.div_euclid(86_400);
            let (y, m, dd) = civil_from_days(days);
            format!("{y:04}-{m:02}-{dd:02}")
        }
    }
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// One comment (a reply status) as an entity (`pixelfed://comments/<id>`):
/// `id, post_id, in_reply_to_id, depth, author_name, author_handle (@acct),
/// avatar_url, text, created_at (unix s), meta ("@acct · 3 h"), like_count,
/// is_liked, is_own`.
pub fn normalize_comment(
    s: &Status,
    post_id: &str,
    depth: usize,
    own_account_id: &str,
    now: i64,
    media: &mut MediaUrls,
) -> EntityRecord {
    let text = match s.content.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(html) => html_to_text(html),
        None => s.content_text.clone().unwrap_or_default().trim().to_owned(),
    };
    let author = normalize_account(&s.account, media);
    let handle = format!("@{}", author.get_str("username").unwrap_or_default());
    let created = s.created_at.as_deref().and_then(parse_rfc3339);
    let meta = match created {
        Some(c) => format!("{handle} · {}", relative_time(c, now)),
        None => handle.clone(),
    };
    let mut c = EntityRecord::new();
    c.set("id", s.id.as_str());
    c.set("post_id", post_id);
    c.set("in_reply_to_id", json!(s.in_reply_to_id));
    c.set("depth", depth);
    c.set(
        "author_name",
        author.get("display_name").cloned().unwrap_or(Value::Null),
    );
    c.set("author_handle", handle);
    c.set(
        "avatar_url",
        author.get("avatar_url").cloned().unwrap_or(Value::Null),
    );
    c.set("text", text);
    c.set("created_at", json!(created));
    c.set("meta", meta);
    c.set("like_count", s.favourites_count.unwrap_or(0).max(0));
    c.set("is_liked", s.favourited.unwrap_or(false));
    c.set("is_own", s.account.id == own_account_id);
    c
}

/// The comments under `post_id` from a `GET /api/v1/statuses/{id}/context` response, in
/// thread order: each comment followed by its replies (depth first), siblings in the
/// upstream's order. Works whether the upstream returns the whole tree (Mastodon) or only
/// direct replies; a reply whose parent is missing is shown at the top level, after the rest.
/// Unparseable entries are skipped (logged).
pub fn normalize_comments(
    post_id: &str,
    context: &Value,
    own_account_id: &str,
    now: i64,
    media: &mut MediaUrls,
) -> Vec<EntityRecord> {
    let statuses: Vec<Status> = context["descendants"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|raw| match Status::deserialize(raw) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!(error = %e, id = %raw["id"], "skipping unparseable Pixelfed comment");
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let statuses: Vec<&Status> = statuses
        .iter()
        .filter(|s| s.id != post_id && seen.insert(s.id.as_str()))
        .collect();
    let ids: HashSet<&str> = statuses.iter().map(|s| s.id.as_str()).collect();
    // Parent id -> children, in upstream order. Orphans hang off the post.
    let mut children: BTreeMap<&str, Vec<&Status>> = BTreeMap::new();
    let mut orphans = Vec::new();
    for s in &statuses {
        match s.in_reply_to_id.as_deref() {
            Some(p) if p == post_id || ids.contains(p) => children.entry(p).or_default().push(s),
            _ => orphans.push(*s),
        }
    }
    let mut out = Vec::with_capacity(statuses.len());
    let mut emitted = HashSet::new();
    let mut stack: Vec<(&Status, usize)> = orphans.iter().rev().map(|s| (*s, 0)).collect();
    stack.extend(
        children
            .get(post_id)
            .into_iter()
            .flatten()
            .rev()
            .map(|s| (*s, 0)),
    );
    while let Some((s, depth)) = stack.pop() {
        if !emitted.insert(s.id.as_str()) {
            continue; // a cycle in malformed data
        }
        out.push(normalize_comment(
            s,
            post_id,
            depth,
            own_account_id,
            now,
            media,
        ));
        let next = (depth + 1).min(MAX_COMMENT_DEPTH);
        stack.extend(
            children
                .get(s.id.as_str())
                .into_iter()
                .flatten()
                .rev()
                .map(|c| (*c, next)),
        );
    }
    out
}

/// Most extra `context` calls one comments screen makes for replies to comments.
pub const MAX_REPLY_FETCHES: usize = 50;

/// Extra `context` calls in flight at once.
const REPLY_FETCH_CONCURRENCY: usize = 6;

/// A status id as text (Pixelfed sends strings, some servers numbers).
fn raw_status_id(raw: &Value) -> Option<String> {
    match &raw["id"] {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The comments in `descendants` that report more replies than `descendants` holds and
/// were not `asked` for yet.
fn comments_missing_replies(descendants: &[Value], asked: &HashSet<String>) -> Vec<String> {
    let parsed: Vec<Status> = descendants
        .iter()
        .filter_map(|raw| Status::deserialize(raw).ok())
        .collect();
    let mut present: BTreeMap<&str, i64> = BTreeMap::new();
    for s in &parsed {
        if let Some(p) = s.in_reply_to_id.as_deref() {
            *present.entry(p).or_default() += 1;
        }
    }
    parsed
        .iter()
        .filter(|s| {
            let want = s.replies_count.or(s.reply_count).unwrap_or(0);
            want > present.get(s.id.as_str()).copied().unwrap_or(0) && !asked.contains(&s.id)
        })
        .map(|s| s.id.clone())
        .collect()
}

/// `context` (the post's `GET /api/v1/statuses/{id}/context`) with the replies to its
/// comments added to `descendants`. Pixelfed's `context` lists only the direct replies, so
/// each comment that reports more replies than are present is asked for its own `context`
/// with `fetch` (`None`: that call failed; its replies stay missing), level by level, at
/// most [`MAX_REPLY_FETCHES`] calls. A Mastodon-style whole thread needs no extra call.
pub async fn complete_comment_thread<F, Fut>(post_id: &str, mut context: Value, fetch: F) -> Value
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    let Some(obj) = context.as_object_mut() else {
        return context;
    };
    let mut all = match obj.remove("descendants") {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    };
    let mut known: HashSet<String> = all.iter().filter_map(raw_status_id).collect();
    let mut asked = HashSet::from([post_id.to_owned()]);
    let mut budget = MAX_REPLY_FETCHES;
    loop {
        let mut wanted = comments_missing_replies(&all, &asked);
        if wanted.is_empty() {
            break;
        }
        if budget == 0 {
            tracing::info!(post_id, skipped = wanted.len(), "comment thread cut short");
            break;
        }
        wanted.truncate(budget);
        budget -= wanted.len();
        asked.extend(wanted.iter().cloned());
        for chunk in wanted.chunks(REPLY_FETCH_CONCURRENCY) {
            let replies =
                futures_util::future::join_all(chunk.iter().map(|id| fetch(id.clone()))).await;
            for mut reply_context in replies.into_iter().flatten() {
                let Some(Value::Array(items)) = reply_context
                    .as_object_mut()
                    .and_then(|o| o.remove("descendants"))
                else {
                    continue;
                };
                for s in items {
                    if raw_status_id(&s).is_some_and(|id| known.insert(id)) {
                        all.push(s);
                    }
                }
            }
        }
    }
    obj.insert("descendants".to_owned(), Value::Array(all));
    context
}

/// "CW: " + the warning, unless the author already wrote a "CW" word.
pub fn content_warning_label(text: &str) -> String {
    let t = text.trim();
    let prefixed = t.len() >= 2
        && t[..2].eq_ignore_ascii_case("cw")
        && t[2..].chars().next().is_none_or(|c| !c.is_alphanumeric());
    if prefixed {
        t.to_owned()
    } else {
        format!("CW: {t}")
    }
}

/// The mutable interaction state of a post: id, is_liked, like_count, is_bookmarked.
pub fn post_state(s: &Status) -> EntityRecord {
    let mut post = EntityRecord::new();
    post.set("id", s.id.as_str());
    post.set("like_count", s.favourites_count.unwrap_or(0).max(0));
    post.set("is_liked", s.favourited.unwrap_or(false));
    post.set("is_bookmarked", s.bookmarked.unwrap_or(false));
    post
}

fn add_user(
    a: &Account,
    media: &mut MediaUrls,
    entities: &mut BTreeMap<String, EntityRecord>,
) -> String {
    let uri = user_uri(&a.id);
    if !entities.contains_key(&uri) {
        entities.insert(uri.clone(), normalize_account(a, media));
    }
    uri
}

pub fn normalize_account(a: &Account, media: &mut MediaUrls) -> EntityRecord {
    let nonempty = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let username = nonempty(&a.acct)
        .or_else(|| nonempty(&a.username))
        .unwrap_or_else(|| a.id.clone());
    let display_name = nonempty(&a.display_name).unwrap_or_else(|| username.clone());
    let avatar = nonempty(&a.avatar).or_else(|| nonempty(&a.avatar_static));
    let mut user = EntityRecord::new();
    user.set("id", a.id.as_str());
    user.set("username", username);
    user.set("display_name", display_name);
    user.set("avatar_url", media.rewrite_opt(avatar.as_deref()));
    user
}

pub fn normalize_media(m: &MediaAttachment, media: &mut MediaUrls) -> EntityRecord {
    let url = m
        .url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .or(m.remote_url.as_deref());
    let mut rec = EntityRecord::new();
    rec.set("id", m.id.as_str());
    rec.set("type", m.kind.as_deref().unwrap_or("image"));
    rec.set("url", media.rewrite_opt(url));
    rec.set("preview_url", media.rewrite_opt(m.preview_url.as_deref()));
    rec.set("aspect_ratio", aspect_ratio(m.meta.as_ref()));
    rec.set(
        "alt_text",
        json!(
            m.description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
        ),
    );
    rec.set(
        "blurhash",
        json!(m.blurhash.as_deref().filter(|b| !b.is_empty())),
    );
    rec
}

/// `meta.original.aspect`, else original width/height (rounded to 4 places),
/// else the same from `meta.small`, else 1.0.
pub fn aspect_ratio(meta: Option<&MediaMeta>) -> f64 {
    let valid = |x: f64| x.is_finite() && x > 0.0;
    let from = |size: Option<&MediaSize>| -> Option<f64> {
        let size = size?;
        if let Some(a) = size.aspect.filter(|a| valid(*a)) {
            return Some(a);
        }
        match (size.width, size.height) {
            (Some(w), Some(h)) if valid(w) && valid(h) => {
                Some((w / h * 10_000.0).round() / 10_000.0)
            }
            _ => None,
        }
    };
    meta.and_then(|m| from(m.original.as_ref()).or_else(|| from(m.small.as_ref())))
        .unwrap_or(1.0)
}

// ---------- text helpers ----------

/// Mastodon status HTML -> plain text: `<br>` -> newline, paragraphs separated
/// by a blank line, other tags dropped (their text kept), entities decoded.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..lt]));
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            // Not a tag after all: keep the text.
            out.push_str(&decode_entities(&rest[lt..]));
            rest = "";
            break;
        };
        let tag = after[..gt].trim();
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("br", _) => out.push('\n'),
            ("p", false) | ("p", true) => {
                let trimmed = out.trim_end_matches([' ', '\t', '\n']).len();
                out.truncate(trimmed);
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
            }
            _ => {}
        }
        rest = &after[gt + 1..];
    }
    out.push_str(&decode_entities(rest));
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Decode the HTML entities that appear in Mastodon content (named basics and
/// numeric). Unknown entities are left as-is.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // Only look a few bytes ahead (the longest entity is `#x10FFFF`), so
        // a long run of `&` without `;` stays linear.
        let decoded = after
            .bytes()
            .take(11)
            .position(|b| b == b';')
            .and_then(|semi| entity(&after[..semi]).map(|c| (c, semi)));
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let num = name.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => num.parse().ok()?,
            };
            return char::from_u32(code);
        }
    })
}

/// RFC 3339 / ISO 8601 timestamp (`2026-10-31T18:42:10.000000Z`, `...+02:00`)
/// -> unix seconds. `None` if it does not parse.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hm = rest[1..].replace(':', "");
            if hm.len() != 4 || !hm.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            sign * (hm[..2].parse::<i64>().ok()? * 3600 + hm[2..].parse::<i64>().ok()? * 60)
        }
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// Days since 1970-01-01 (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../fixtures/pixelfed/timeline_home.json");
    const BASE: &str = "http://127.0.0.1:8081";
    const PROXY: &str = "http://10.0.2.2:8080/v1/pixelfed/media";

    /// The fake's and the recorded instance's origins.
    fn allow() -> MediaAllowlist {
        MediaAllowlist::new([BASE, REAL_BASE], [])
    }

    #[test]
    fn media_allowlist_matches_host_and_port() {
        let a = MediaAllowlist::new(
            ["http://localhost:8090", "https://pixelfed.example/"],
            ["cdn.example", "s3.example:9000", "https://img.example"],
        );
        for ok in [
            "http://localhost:8090/storage/a.jpg",
            "http://LOCALHOST:8090/x",
            "https://pixelfed.example/storage/a.jpg",
            "https://pixelfed.example:443/a",
            "https://cdn.example/a.jpg",
            "http://cdn.example/a.jpg",
            "https://s3.example:9000/b/a.jpg",
            "https://img.example/a",
        ] {
            assert!(a.allows(ok), "{ok}");
        }
        for bad in [
            "http://localhost:22/",
            "http://localhost/",
            "http://pixelfed.example/a", // port 80, not the configured 443
            "https://cdn.example:8443/a",
            "https://s3.example/a",
            "http://169.254.169.254/latest/meta-data",
            "https://evil.example/a.jpg",
            "https://cdn.example.evil.example/a",
            "file:///etc/passwd",
            "ftp://cdn.example/a",
            "not a url",
        ] {
            assert!(!a.allows(bad), "{bad}");
        }
    }

    #[test]
    fn media_policy_routes() {
        let closed = MediaPolicy::from(&allow());
        let open = MediaPolicy {
            any_public_https: true,
            ..closed.clone()
        };
        let ok = format!("{BASE}/storage/a.jpg");
        assert_eq!(closed.route(&ok), Some(MediaRoute::Trusted));
        assert_eq!(open.route(&ok), Some(MediaRoute::Trusted));
        let cdn = "https://cdn.pixelfed.example/m/a.jpg";
        assert_eq!(closed.route(cdn), None);
        assert_eq!(open.route(cdn), Some(MediaRoute::Public));
        for bad in [
            "http://cdn.pixelfed.example/a.jpg",
            "https://cdn.pixelfed.example:8443/a.jpg",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/a",
            "https://localhost/a",
            "file:///etc/passwd",
        ] {
            assert_eq!(open.route(bad), None, "{bad}");
        }
    }

    #[test]
    fn media_outside_the_allowlist_is_dropped() {
        let raw = vec![json!({
            "id": "1",
            "account": {"id": "2", "avatar": "http://169.254.169.254/latest/meta-data/"},
            "media_attachments": [{
                "id": "3",
                "url": format!("{BASE}/storage/ok.png"),
                "preview_url": "https://evil.example/p.png"
            }]
        })];
        let mut media = MediaUrls::new(PROXY, &allow());
        let tl = normalize_timeline(&raw, &mut media);
        assert_eq!(
            tl.entities["pixelfed://users/2"].get("avatar_url"),
            Some(&Value::Null)
        );
        let m = &tl.entities["pixelfed://media/3"];
        assert_eq!(m.get("preview_url"), Some(&Value::Null));
        proxied(&media, &m.0["url"], "/storage/ok.png");
        assert_eq!(media.keys.len(), 1, "only the allowed URL got a proxy key");
    }

    fn fixture_timeline() -> (Timeline, MediaUrls) {
        let raw: Vec<Value> = serde_json::from_str(&FIXTURE.replace("{{BASE}}", BASE)).unwrap();
        let mut media = MediaUrls::new(PROXY, &allow());
        let tl = normalize_timeline(&raw, &mut media);
        (tl, media)
    }

    fn proxied(media: &MediaUrls, v: &Value, upstream_path: &str) {
        let url = v.as_str().unwrap_or_else(|| panic!("not a URL: {v}"));
        let key = url
            .strip_prefix(&format!("{PROXY}/"))
            .unwrap_or_else(|| panic!("not rewritten to the proxy: {url}"));
        assert_eq!(media.keys[key], format!("{BASE}{upstream_path}"));
    }

    #[test]
    fn test_pixelfed_timeline_normalization() {
        let (tl, media) = fixture_timeline();
        let e = &tl.entities;

        // Order: outer timeline order, the reblog replaced by its inner post.
        assert_eq!(
            tl.order,
            [
                "pixelfed://posts/781254316723458049",
                "pixelfed://posts/781101134560870401",
                "pixelfed://posts/781233309876543489",
                "pixelfed://posts/781220014433221633",
                "pixelfed://posts/781198765432109057",
            ]
        );
        assert!(
            !e.contains_key("pixelfed://posts/781249870123446273"),
            "outer reblog wrapper is not a post"
        );

        // Carousel: 3 media in order, aspect 1.0, alt text.
        let carousel = &e["pixelfed://posts/781254316723458049"];
        assert_eq!(
            carousel.get_str_list("media_uris").unwrap(),
            [
                "pixelfed://media/781254316501123073",
                "pixelfed://media/781254316501123074",
                "pixelfed://media/781254316501123075",
            ]
        );
        for (i, uri) in carousel
            .get_str_list("media_uris")
            .unwrap()
            .iter()
            .enumerate()
        {
            let m = &e[*uri];
            assert_eq!(m.get_f64("aspect_ratio"), Some(1.0));
            assert!(
                m.get_str("alt_text")
                    .unwrap()
                    .ends_with(&format!("number {}", i + 1))
            );
            assert_eq!(m.get_str("type"), Some("image"));
            proxied(
                &media,
                &m.0["url"],
                &format!("/storage/carousel-{}.png", i + 1),
            );
            proxied(
                &media,
                &m.0["preview_url"],
                &format!("/storage/carousel-{}_thumb.png", i + 1),
            );
        }
        assert_eq!(
            e["pixelfed://media/781254316501123073"].get_str("blurhash"),
            Some("00Ojn}")
        );
        assert_eq!(
            carousel.get_str("caption"),
            Some("Sunrise over Boulder, three ways & one coffee later #sunrise #colorado")
        );
        assert_eq!(
            carousel.get_str("author_uri"),
            Some("pixelfed://users/698765432100012345")
        );
        assert_eq!(carousel.get_i64("like_count"), Some(27));
        assert_eq!(carousel.get_bool("is_liked"), Some(false));
        assert_eq!(carousel.get_bool("is_bookmarked"), Some(false));
        assert_eq!(carousel.get_i64("reply_count"), Some(4));
        assert_eq!(carousel.get_i64("created_at"), Some(1_793_472_130)); // 2026-10-31T18:42:10Z
        assert_eq!(
            carousel.get_str("url"),
            Some("http://127.0.0.1:8081/p/alice/781254316723458049")
        );
        assert_eq!(carousel.get("content_warning"), Some(&Value::Null));
        assert_eq!(carousel.get_bool("sensitive"), Some(false));
        assert!(carousel.get("reblogged_by_uri").is_none());

        // Reblog: the inner post, attributed to its author, with reblogged_by_uri.
        let reblog = &e["pixelfed://posts/781101134560870401"];
        assert_eq!(reblog.get_str("id"), Some("781101134560870401"));
        assert_eq!(
            reblog.get_str("author_uri"),
            Some("pixelfed://users/701234567890123456")
        );
        assert_eq!(
            reblog.get_str("reblogged_by_uri"),
            Some("pixelfed://users/699112233445566778")
        );
        assert_eq!(reblog.get_i64("like_count"), Some(318));
        assert_eq!(
            reblog.get_str("caption"),
            Some("Tram lines, Lisbon. #streetphotography")
        );
        assert_eq!(
            e["pixelfed://media/781101134401234945"].get_f64("aspect_ratio"),
            Some(1.3333)
        );

        // Landscape single: aspect 1.7778, alt_text null, favourited.
        let landscape = &e["pixelfed://posts/781233309876543489"];
        assert_eq!(landscape.get_bool("is_liked"), Some(true));
        assert_eq!(
            landscape.get_str("caption"),
            Some("Golden hour ride.\nLegs are toast.")
        );
        let m = &e["pixelfed://media/781233309701234689"];
        assert_eq!(m.get_f64("aspect_ratio"), Some(1.7778));
        assert_eq!(m.get("alt_text"), Some(&Value::Null));

        // Text-only: no media, multi-paragraph caption, decoded entities, bookmarked.
        let text = &e["pixelfed://posts/781220014433221633"];
        assert_eq!(text.get_str_list("media_uris"), Some(vec![]));
        assert_eq!(
            text.get_str("caption"),
            Some(
                "Thanks @alice for the tips on night shooting!\n\
                 Notes are up at https://example.com/night-notes\n\n\
                 Settings: f/1.8 & 1/60s, ISO 3200 <3 #nightphotography"
            )
        );
        assert_eq!(text.get_bool("is_bookmarked"), Some(true));

        // CW / sensitive.
        let cw = &e["pixelfed://posts/781198765432109057"];
        assert_eq!(cw.get_str("content_warning"), Some("Snake photo"));
        assert_eq!(cw.get_bool("sensitive"), Some(true));
        assert_eq!(
            e["pixelfed://media/781198765301987329"].get_f64("aspect_ratio"),
            Some(0.8)
        );

        // Users deduped: alice, bob (reblogger + author), carol.
        let users: Vec<&String> = e.keys().filter(|k| k.starts_with(USER_PREFIX)).collect();
        assert_eq!(users.len(), 3, "{users:?}");
        let bob = &e["pixelfed://users/699112233445566778"];
        assert_eq!(bob.get_str("username"), Some("bob"));
        assert_eq!(bob.get_str("display_name"), Some("Bob 🚲"));
        proxied(&media, &bob.0["avatar_url"], "/storage/avatar-bob.png");

        // 5 posts + 6 media + 3 users; every emitted URL has a key.
        assert_eq!(e.len(), 14);
        let media_count = e.keys().filter(|k| k.starts_with(MEDIA_PREFIX)).count();
        assert_eq!(media_count, 6);
        // 6 urls + 6 previews + 3 avatars (deduped by URL).
        assert_eq!(media.keys.len(), 15);
    }

    /// Real Pixelfed 0.15.2 responses recorded from the local instance
    /// (`scripts/pixelfed_dev.sh record`; D20).
    const RECORDED_REBLOGS: &str =
        include_str!("../../../fixtures/pixelfed/recorded/timeline_home_reblogs.json");
    const RECORDED_PLAIN: &str =
        include_str!("../../../fixtures/pixelfed/recorded/timeline_home.json");
    const RECORDED_PE: &str =
        include_str!("../../../fixtures/pixelfed/recorded/timeline_home_pe.json");
    const REAL_BASE: &str = "http://localhost:8090";

    fn normalize_recorded(json: &str) -> (Timeline, MediaUrls) {
        let raw: Vec<Value> = serde_json::from_str(json).unwrap();
        let mut media = MediaUrls::new(PROXY, &allow());
        let tl = normalize_timeline(&raw, &mut media);
        assert_eq!(tl.order.len(), raw.len(), "no recorded status was skipped");
        (tl, media)
    }

    #[test]
    fn test_pixelfed_recorded_timeline_normalization() {
        let (tl, media) = normalize_recorded(RECORDED_REBLOGS);
        let e = &tl.entities;
        let alice = "pixelfed://users/1022706462152876034";
        let bob = "pixelfed://users/1022706465058381827";

        // include_reblogs=true: alice's boost of bob's post comes first, as bob's post.
        assert_eq!(
            tl.order,
            [
                "pixelfed://posts/1022706474904072196",
                "pixelfed://posts/1022706476640768008",
                "pixelfed://posts/1022706476322123783",
                "pixelfed://posts/1022706476008685574",
                "pixelfed://posts/1022706475674484741",
            ]
        );
        assert!(!e.contains_key("pixelfed://posts/1022706489856569353"));
        // The inner object is Pixelfed's `_pe` entity (pf_type, liked_by, reply_count, mime).
        let boosted = &e["pixelfed://posts/1022706474904072196"];
        assert_eq!(boosted.get_str("author_uri"), Some(bob));
        assert_eq!(boosted.get_str("reblogged_by_uri"), Some(alice));
        assert_eq!(
            boosted.get_str("caption"),
            Some("Bob's post, boosted by alice")
        );
        assert_eq!(boosted.get_i64("reply_count"), Some(0));
        assert_eq!(boosted.get_i64("like_count"), Some(0));
        assert_eq!(boosted.get_i64("created_at"), Some(1_793_589_024)); // ...T03:10:24.000Z
        let teal = &e["pixelfed://media/1"];
        assert_eq!(teal.get_f64("aspect_ratio"), Some(1.0));
        assert_eq!(
            teal.get_str("alt_text"),
            Some("Teal square with the number 7")
        );
        assert_eq!(e[bob].get_str("display_name"), Some("Bob Example"));

        // Carousel: media 2, 3, 4 in order; integer aspect `1` -> 1.0; the
        // content has no <p> wrapper and a hashtag link whose text is kept.
        let carousel = &e["pixelfed://posts/1022706475674484741"];
        assert_eq!(
            carousel.get_str_list("media_uris").unwrap(),
            [
                "pixelfed://media/2",
                "pixelfed://media/3",
                "pixelfed://media/4"
            ]
        );
        for (uri, alt) in [
            ("pixelfed://media/2", "Red square with the number 1"),
            ("pixelfed://media/3", "Blue square with the number 2"),
            ("pixelfed://media/4", "Green square with the number 3"),
        ] {
            assert_eq!(e[uri].get_str("alt_text"), Some(alt));
            assert_eq!(e[uri].get_f64("aspect_ratio"), Some(1.0));
            assert_eq!(e[uri].get_str("type"), Some("image"));
        }
        assert_eq!(
            carousel.get_str("caption"),
            Some("Carousel: one, two, three #construct")
        );
        assert_eq!(carousel.get_str("author_uri"), Some(alice));
        assert!(carousel.get("reblogged_by_uri").is_none());

        // Portrait: empty alt "" -> null; aspect 0.8.
        let portrait = &e["pixelfed://media/5"];
        assert_eq!(portrait.get("alt_text"), Some(&Value::Null));
        assert_eq!(portrait.get_f64("aspect_ratio"), Some(0.8));
        assert_eq!(e["pixelfed://media/6"].get_f64("aspect_ratio"), Some(1.5));

        // CW post.
        let cw = &e["pixelfed://posts/1022706476640768008"];
        assert_eq!(cw.get_str("content_warning"), Some("CW: orange"));
        assert_eq!(cw.get_bool("sensitive"), Some(true));
        assert_eq!(
            cw.get_str("url"),
            Some("http://localhost:8090/p/alice/1022706476640768008")
        );

        // Every image URL is proxied and maps back to the instance.
        let m = &e["pixelfed://media/2"];
        let key = m
            .get_str("url")
            .unwrap()
            .strip_prefix(&format!("{PROXY}/"))
            .unwrap();
        assert!(media.keys[key].starts_with(&format!("{REAL_BASE}/storage/m/_v2/")));
        let key = e[alice]
            .get_str("avatar_url")
            .unwrap()
            .strip_prefix(&format!("{PROXY}/"))
            .unwrap();
        assert_eq!(
            media.keys[key],
            format!("{REAL_BASE}/storage/avatars/default.jpg")
        );

        // Without include_reblogs the boost is absent; the `_pe` variant (no
        // avatar_static, reply_count instead of replies_count, media mime)
        // normalizes to the same entities.
        let (plain, _) = normalize_recorded(RECORDED_PLAIN);
        assert_eq!(plain.order, tl.order[1..]);
        let (pe, _) = normalize_recorded(RECORDED_PE);
        assert_eq!(pe.order, plain.order);
        assert_eq!(pe.entities, plain.entities);
        for (uri, rec) in &plain.entities {
            assert_eq!(Some(rec), e.get(uri), "{uri}");
        }
    }

    #[test]
    fn lenient_field_types() {
        let raw = vec![json!({
            "id": "1", "account": {"id": 2, "avatar": 5, "display_name": null},
            "sensitive": 0, "favourited": "1", "bookmarked": null,
            "favourites_count": "12", "reply_count": 3, "content": 42,
            "reblog": "garbage",
            "media_attachments": [
                {"id": 9, "description": "", "meta": {"original": {"width": "1080", "height": 1350}, "small": null}},
                {"no_id": true},
                {"id": "10", "meta": "bad"}
            ]
        })];
        let mut media = MediaUrls::new(PROXY, &allow());
        let tl = normalize_timeline(&raw, &mut media);
        let p = &tl.entities["pixelfed://posts/1"];
        assert_eq!(p.get_bool("sensitive"), Some(false));
        assert_eq!(p.get_bool("is_liked"), Some(true));
        assert_eq!(p.get_bool("is_bookmarked"), Some(false));
        assert_eq!(p.get_i64("like_count"), Some(12));
        assert_eq!(p.get_i64("reply_count"), Some(3));
        assert_eq!(p.get_str("caption"), Some(""));
        assert!(p.get("reblogged_by_uri").is_none());
        assert_eq!(
            p.get_str_list("media_uris").unwrap(),
            ["pixelfed://media/9", "pixelfed://media/10"]
        );
        assert_eq!(
            tl.entities["pixelfed://media/9"].get_f64("aspect_ratio"),
            Some(0.8)
        );
        assert_eq!(
            tl.entities["pixelfed://media/9"].get("alt_text"),
            Some(&Value::Null)
        );
        assert_eq!(
            tl.entities["pixelfed://media/10"].get_f64("aspect_ratio"),
            Some(1.0)
        );
        assert_eq!(
            tl.entities["pixelfed://users/2"].get("avatar_url"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn lenient_parsing_of_minimal_and_broken_statuses() {
        let raw = vec![
            json!({"id": 7, "account": {"id": "1"}, "media_attachments": null,
                   "content": null, "spoiler_text": null}),
            json!({"no_id": true}),
            json!({"id": "8", "account": {"id": "1", "acct": "x@remote", "avatar": ""},
                   "media_attachments": [{"id": "9", "meta": {"original": {"width": 1280, "height": 720}}}]}),
        ];
        let mut media = MediaUrls::new(PROXY, &allow());
        let tl = normalize_timeline(&raw, &mut media);
        assert_eq!(tl.order, ["pixelfed://posts/7", "pixelfed://posts/8"]);
        let p = &tl.entities["pixelfed://posts/7"];
        assert_eq!(p.get_str("caption"), Some(""));
        assert_eq!(p.get("created_at"), Some(&Value::Null));
        assert_eq!(p.get("url"), Some(&Value::Null));
        assert_eq!(p.get_i64("like_count"), Some(0));
        let m = &tl.entities["pixelfed://media/9"];
        assert_eq!(m.get_f64("aspect_ratio"), Some(1.7778));
        assert_eq!(m.get("url"), Some(&Value::Null));
        let u = &tl.entities["pixelfed://users/1"];
        assert_eq!(u.get_str("username"), Some("1"), "first occurrence wins");
        assert_eq!(aspect_ratio(None), 1.0);
    }

    #[test]
    fn html_and_time_helpers() {
        assert_eq!(
            html_to_text("<p>a &amp; b</p><p>c<br>d<br/>e</p>"),
            "a & b\n\nc\nd\ne"
        );
        assert_eq!(
            html_to_text("1 < 2 &#38; &#x3C;3 &bogus; &"),
            "1 < 2 & <3 &bogus; &"
        );
        assert_eq!(html_to_text(""), "");
        assert_eq!(
            decode_entities("&#x1F600; &#128512;"),
            "\u{1F600} \u{1F600}"
        );
        // Many `&` and a `;` only at the very end: linear, and nothing decoded.
        let amps = format!("{};", "&".repeat(200_000));
        let t = std::time::Instant::now();
        assert_eq!(decode_entities(&amps), amps);
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-10-31T18:42:10.000000Z"),
            Some(1_793_472_130)
        );
        assert_eq!(
            parse_rfc3339("2026-10-31T20:42:10+02:00"),
            Some(1_793_472_130)
        );
        assert_eq!(
            parse_rfc3339("2000-02-29T00:00:00.5-00:30"),
            Some(951_784_200)
        );
        assert_eq!(parse_rfc3339("yesterday"), None);
        assert_eq!(media_key("x"), media_key("x"));
        assert_eq!(media_key("x").len(), 32);
    }
}

#[cfg(test)]
mod comment_tests {
    use super::{MAX_REPLY_FETCHES, complete_comment_thread, relative_time};
    use serde_json::{Value, json};
    use std::sync::Mutex;

    fn reply(id: &str, parent: &str, replies: i64) -> Value {
        json!({ "id": id, "in_reply_to_id": parent, "replies_count": replies,
                "account": { "id": "1", "username": "a" } })
    }

    fn ids(context: &Value) -> Vec<&str> {
        context["descendants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn thread_asks_each_comment_with_missing_replies() {
        // Pixelfed: one level per call. "c" reports a reply that is gone (stale count),
        // "d" fails; neither is asked twice.
        let asked = Mutex::new(Vec::new());
        let root = json!({ "ancestors": [], "descendants": [
            reply("a", "p", 1), reply("c", "p", 1), reply("d", "p", 2) ] });
        let done = complete_comment_thread("p", root, |id| {
            asked.lock().unwrap().push(id.clone());
            async move {
                match id.as_str() {
                    "a" => Some(json!({ "descendants": [reply("b", "a", 1)] })),
                    "b" => Some(json!({ "descendants": [reply("e", "b", 0)] })),
                    "c" => Some(json!({ "descendants": [] })),
                    _ => None,
                }
            }
        })
        .await;
        assert_eq!(ids(&done), ["a", "c", "d", "b", "e"]);
        assert_eq!(done["ancestors"], json!([]));
        assert_eq!(*asked.lock().unwrap(), ["a", "c", "d", "b"]);
    }

    #[tokio::test]
    async fn a_whole_thread_needs_no_more_calls_and_calls_are_capped() {
        let whole = json!({ "descendants": [reply("a", "p", 1), reply("b", "a", 0)] });
        let done =
            complete_comment_thread("p", whole.clone(), |_| async { panic!("no call expected") })
                .await;
        assert_eq!(done, whole);

        // An endless chain stops after MAX_REPLY_FETCHES calls.
        let calls = Mutex::new(0);
        let chain = json!({ "descendants": [reply("0", "p", 1)] });
        let done = complete_comment_thread("p", chain, |id| {
            *calls.lock().unwrap() += 1;
            let next = (id.parse::<usize>().unwrap() + 1).to_string();
            async move { Some(json!({ "descendants": [reply(&next, &id, 1)] })) }
        })
        .await;
        assert_eq!(*calls.lock().unwrap(), MAX_REPLY_FETCHES);
        assert_eq!(ids(&done).len(), MAX_REPLY_FETCHES + 1);

        // Not an object: unchanged.
        let odd = complete_comment_thread("p", json!([1]), |_| async { None }).await;
        assert_eq!(odd, json!([1]));
    }

    #[test]
    fn relative_time_labels() {
        let now = 1_793_589_858;
        assert_eq!(relative_time(now - 5, now), "just now");
        assert_eq!(relative_time(now + 30, now), "just now");
        assert_eq!(relative_time(now - 300, now), "5 min");
        assert_eq!(relative_time(now - 3 * 3_600, now), "3 h");
        assert_eq!(relative_time(now - 2 * 86_400, now), "2 d");
        assert_eq!(relative_time(951_782_400, now), "2000-02-29");
    }
}

#[cfg(test)]
mod label_tests {
    use super::content_warning_label;

    #[test]
    fn content_warning_labels() {
        assert_eq!(content_warning_label("Snake photo"), "CW: Snake photo");
        assert_eq!(content_warning_label(" cw: spiders "), "cw: spiders");
        assert_eq!(content_warning_label("CW"), "CW");
        assert_eq!(content_warning_label("Cwtch"), "CW: Cwtch");
    }
}
