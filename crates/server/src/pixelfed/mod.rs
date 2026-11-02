//! Pixelfed adapter (M6): Mastodon-API JSON -> normalized Canvas entities.
//!
//! Everything in this module (except [`upstream`] and [`routes`]) is pure: it
//! turns upstream JSON into URI-keyed [`EntityRecord`]s and an ordered timeline.
//! Input is parsed leniently (every optional Mastodon field may be absent or
//! null; a status that cannot be parsed at all is skipped and logged).
//!
//! Entity shapes (docs/M6 contract):
//! - `pixelfed://posts/<id>`: id, author_uri, media_uris, caption, content_warning,
//!   sensitive, like_count, is_liked, is_bookmarked, reply_count, created_at (unix s),
//!   url, and `reblogged_by_uri` when the timeline entry was a reblog (the post is the
//!   INNER status, so actions target the inner id).
//! - `pixelfed://users/<id>`: id, username (acct), display_name, avatar_url.
//! - `pixelfed://media/<id>`: id, type, url, preview_url, aspect_ratio, alt_text, blurhash.
//!
//! Every image URL is rewritten through [`MediaUrls`] to the pod's media proxy.

pub mod layout;
pub mod routes;
pub mod upstream;

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

/// `null` -> default (serde's `default` only covers an absent field).
fn de_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub acct: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub avatar_static: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaSize {
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    #[serde(default)]
    pub aspect: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaMeta {
    #[serde(default)]
    pub original: Option<MediaSize>,
    #[serde(default)]
    pub small: Option<MediaSize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MediaAttachment {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub preview_url: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub blurhash: Option<String>,
    #[serde(default)]
    pub meta: Option<MediaMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    /// Pixelfed sometimes carries a plain-text rendition.
    #[serde(default)]
    pub content_text: Option<String>,
    #[serde(default)]
    pub spoiler_text: Option<String>,
    #[serde(default)]
    pub sensitive: Option<bool>,
    #[serde(default)]
    pub replies_count: Option<i64>,
    #[serde(default)]
    pub favourites_count: Option<i64>,
    #[serde(default)]
    pub favourited: Option<bool>,
    #[serde(default)]
    pub bookmarked: Option<bool>,
    pub account: Account,
    #[serde(default, deserialize_with = "de_null_default")]
    pub media_attachments: Vec<MediaAttachment>,
    #[serde(default)]
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

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Rewrites upstream image URLs to `<proxy_base>/<key>` and remembers
/// key -> upstream URL for the URLs it has emitted.
#[derive(Debug, Clone)]
pub struct MediaUrls {
    proxy_base: String,
    pub keys: BTreeMap<String, String>,
}

impl MediaUrls {
    /// `proxy_base` like `http://10.0.2.2:8080/v1/pixelfed/media` (no trailing slash).
    pub fn new(proxy_base: &str) -> Self {
        Self {
            proxy_base: proxy_base.trim_end_matches('/').to_owned(),
            keys: BTreeMap::new(),
        }
    }

    pub fn rewrite(&mut self, url: &str) -> String {
        let key = media_key(url);
        let out = format!("{}/{key}", self.proxy_base);
        self.keys.insert(key, url.to_owned());
        out
    }

    fn rewrite_opt(&mut self, url: Option<&str>) -> Value {
        match url.map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => Value::String(self.rewrite(u)),
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
    post.set("author_uri", author_uri);
    post.set("media_uris", media_uris);
    post.set("caption", caption);
    post.set("content_warning", json!(content_warning));
    post.set("sensitive", s.sensitive.unwrap_or(false));
    post.set("reply_count", s.replies_count.unwrap_or(0));
    post.set(
        "created_at",
        json!(s.created_at.as_deref().and_then(parse_rfc3339)),
    );
    post.set("url", json!(s.url.as_deref().or(s.uri.as_deref())));
    post
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
        let decoded = after
            .find(';')
            .filter(|&semi| semi <= 10)
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

    const FIXTURE: &str = include_str!("../../../../fixtures/pixelfed/timeline_home.json");
    const BASE: &str = "http://127.0.0.1:8081";
    const PROXY: &str = "http://10.0.2.2:8080/v1/pixelfed/media";

    fn fixture_timeline() -> (Timeline, MediaUrls) {
        let raw: Vec<Value> = serde_json::from_str(&FIXTURE.replace("{{BASE}}", BASE)).unwrap();
        let mut media = MediaUrls::new(PROXY);
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

    #[test]
    fn lenient_parsing_of_minimal_and_broken_statuses() {
        let raw = vec![
            json!({"id": 7, "account": {"id": "1"}, "media_attachments": null,
                   "content": null, "spoiler_text": null}),
            json!({"no_id": true}),
            json!({"id": "8", "account": {"id": "1", "acct": "x@remote", "avatar": ""},
                   "media_attachments": [{"id": "9", "meta": {"original": {"width": 1280, "height": 720}}}]}),
        ];
        let mut media = MediaUrls::new(PROXY);
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
