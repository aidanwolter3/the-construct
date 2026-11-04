//! Notifications from Pixelfed (D52, docs/notifications.md).
//!
//! The pod core polls this adapter ([`poll`]) while the pod has a registered
//! device. For each stored upstream account (one per instance) it reads
//! `GET /api/v1/notifications` from the instance's cursor on and turns each
//! upstream notification into a [`NewNotification`] with ready-made text, a
//! navigation target in the `pixelfed` layout routes and the actor's
//! avatar through the media proxy. The mapping functions are pure.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

use pod_core::util::now_secs;
use pod_core::{NewNotification, Pod, PollCtx, PollOutcome, PollStatus, blocking};
use protocol::{Importance, NotificationChannel};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;

use crate::adapter::{PixelfedAdapter, Runtime, SIGN_IN};
use crate::config::Instance;
use crate::db::{PixelfedAccount, PixelfedStore};
use crate::routes::{MEDIA_PATH, others_allowed, save_media_keys, with_token};
use crate::upstream::UpstreamError;
use crate::{
    Account, MediaUrls, Status, de_id, de_lenient_obj, de_lenient_str, de_opt_id, html_to_text,
    normalize_account, parse_rfc3339,
};

/// How often the host polls this adapter.
pub const POLL_INTERVAL: Duration = Duration::from_secs(120);
/// Notifications asked for per page.
pub const PAGE_LIMIT: u32 = 40;
/// Pages read per instance per poll; the rest waits for the next poll.
pub const MAX_PAGES: usize = 3;
/// The first poll of an instance stores this many, quietly (inbox history).
pub const FIRST_POLL_LIMIT: u32 = 20;
/// Poll again this soon when a poll stopped at [`MAX_PAGES`].
pub const CATCH_UP_IN: Duration = Duration::from_secs(30);
/// Longest post excerpt in the body of a like or a boost.
const EXCERPT_CHARS: usize = 140;
/// Most status lookups one poll makes to find the posts replies are under.
pub const MAX_ROOT_LOOKUPS: usize = 40;

/// Channel ids (never change once shipped: devices store choices by id).
pub const LIKES: &str = "likes";
pub const COMMENTS: &str = "comments";
pub const MENTIONS: &str = "mentions";
pub const FOLLOWS: &str = "follows";
pub const BOOSTS: &str = "boosts";
pub const OTHER: &str = "other";

/// The channels of `pixelfed`.
pub fn channels() -> Vec<NotificationChannel> {
    let app = crate::apps::PIXELFED.id;
    let c = |id: &str, title: &str, description: &str, on: bool, importance| NotificationChannel {
        app_id: app.into(),
        id: id.into(),
        title: title.into(),
        description: description.into(),
        default_enabled: on,
        importance,
    };
    vec![
        c(
            LIKES,
            "Likes",
            "When someone likes your post",
            true,
            Importance::Default,
        ),
        c(
            COMMENTS,
            "Comments",
            "When someone comments on your post or replies to you",
            true,
            Importance::High,
        ),
        c(
            MENTIONS,
            "Mentions",
            "When someone mentions you",
            true,
            Importance::High,
        ),
        c(
            FOLLOWS,
            "New followers",
            "When someone follows you",
            true,
            Importance::Default,
        ),
        c(
            BOOSTS,
            "Boosts",
            "When someone boosts your post",
            true,
            Importance::Default,
        ),
        c(
            OTHER,
            "Other",
            "Other Pixelfed activity",
            false,
            Importance::Low,
        ),
    ]
}

// ---------- upstream notifications ----------

/// One entry of `GET /api/v1/notifications`, parsed leniently.
#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamNotification {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, rename = "type", deserialize_with = "de_lenient_str")]
    pub kind: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_str")]
    pub created_at: Option<String>,
    /// Who did it.
    #[serde(default, deserialize_with = "de_lenient_obj")]
    pub account: Option<Account>,
    #[serde(default, deserialize_with = "de_lenient_obj")]
    pub status: Option<Status>,
    /// The status's `in_reply_to_account_id` (a mention that replies to us
    /// is a comment).
    #[serde(skip)]
    pub reply_to_account: Option<String>,
}

#[derive(Deserialize)]
struct ReplyTo {
    #[serde(default, deserialize_with = "de_opt_id")]
    in_reply_to_account_id: Option<String>,
}

/// Parse a notifications page; entries without an id are skipped (logged).
pub fn parse_notifications(raw: &[Value]) -> Vec<UpstreamNotification> {
    raw.iter()
        .filter_map(|v| match UpstreamNotification::deserialize(v) {
            Ok(mut n) => {
                n.reply_to_account = ReplyTo::deserialize(&v["status"])
                    .ok()
                    .and_then(|r| r.in_reply_to_account_id);
                Some(n)
            }
            Err(e) => {
                tracing::warn!(error = %e, id = %v["id"], "skipping unparseable Pixelfed notification");
                None
            }
        })
        .collect()
}

/// Upstream id order: numeric ids (snowflakes) numerically, others by length
/// then text.
pub fn id_cmp(a: &str, b: &str) -> Ordering {
    match (a.parse::<u128>(), b.parse::<u128>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
    }
}

/// The newest of `ids`.
pub fn newest_id<'a>(ids: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    ids.into_iter().max_by(|a, b| id_cmp(a, b))
}

fn kind(n: &UpstreamNotification) -> String {
    n.kind.as_deref().unwrap_or("").trim().to_ascii_lowercase()
}

/// The channel of a notification. `own_account_id`: the signed-in account
/// (a mention that replies to one of its statuses is a comment).
pub fn channel_of(n: &UpstreamNotification, own_account_id: &str) -> &'static str {
    match kind(n).as_str() {
        "favourite" | "favorite" | "like" => LIKES,
        "comment" => COMMENTS,
        "mention" if n.reply_to_account.as_deref() == Some(own_account_id) => COMMENTS,
        "mention" | "tagged" => MENTIONS,
        "follow" | "follow_request" => FOLLOWS,
        "reblog" | "share" => BOOSTS,
        _ => OTHER,
    }
}

/// `display name`, else `@handle`, else "Someone".
fn actor_name(a: Option<&Account>) -> String {
    let nonempty = |s: Option<&String>| {
        s.map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let Some(a) = a else {
        return "Someone".to_owned();
    };
    nonempty(a.display_name.as_ref())
        .or_else(|| actor_handle(Some(a)))
        .unwrap_or_else(|| "Someone".to_owned())
}

fn actor_handle(a: Option<&Account>) -> Option<String> {
    let a = a?;
    [a.acct.as_ref(), a.username.as_ref()]
        .into_iter()
        .flatten()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .map(|s| format!("@{s}"))
}

/// A status's text as plain text (HTML stripped, entities decoded).
pub fn status_text(s: &Status) -> String {
    match s.content.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(html) => html_to_text(html),
        None => s.content_text.clone().unwrap_or_default().trim().to_owned(),
    }
}

/// One line of at most `max` characters (whitespace collapsed, "…" when cut).
pub fn excerpt(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

fn path_safe(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The screen of a post (`pixelfed://posts/{id}`, the route of the `post`
/// screen in the pixelfed layouts; from 0008 it shows the comments too).
pub fn post_target(post_id: &str) -> String {
    format!("{}{post_id}", crate::POST_PREFIX)
}

/// One comment in the thread of its post (`pixelfed://posts/{id}/comments/{comment}`,
/// the route of the `comment_thread` screen in the pixelfed layouts from
/// 0007): the whole thread, scrolled to the comment and highlighted.
pub fn comment_target(post_id: &str, comment_id: &str) -> String {
    format!("{}{post_id}/comments/{comment_id}", crate::POST_PREFIX)
}

/// Turn one upstream notification into what the pod stores.
/// `instance`: the instance URL (part of the dedupe key); `now`: the time
/// used when the notification carries none; `root`: the post the
/// notification's status is under when it is a reply ([`find_roots`]), if
/// known.
pub fn to_new(
    n: &UpstreamNotification,
    instance: &str,
    own_account_id: &str,
    media: &mut MediaUrls,
    now: i64,
    quiet: bool,
    root: Option<&str>,
) -> NewNotification {
    let channel = channel_of(n, own_account_id);
    let kind = kind(n);
    let name = actor_name(n.account.as_ref());
    let status = n.status.as_ref();
    let mut comment_id = None;
    let (title, body, post_id) = match channel {
        LIKES | BOOSTS => {
            let verb = if channel == LIKES { "liked" } else { "boosted" };
            let target = status.map(Status::target);
            (
                format!("{name} {verb} your post"),
                target
                    .map(|s| excerpt(&status_text(s), EXCERPT_CHARS))
                    .unwrap_or_default(),
                target.map(|s| s.id.clone()),
            )
        }
        COMMENTS | MENTIONS => {
            let parent = status.and_then(|s| s.in_reply_to_id.clone());
            // A reply to a comment, not to the post itself.
            let to_comment = root.is_some() && parent.is_some() && root != parent.as_deref();
            let title = if channel == COMMENTS && to_comment {
                format!("{name} replied to your comment")
            } else if channel == COMMENTS {
                format!("{name} commented on your post")
            } else if kind == "tagged" {
                format!("{name} tagged you in a post")
            } else {
                format!("{name} mentioned you")
            };
            // A reply opens its post's thread at the reply: the post it is
            // under (else, not known, its parent). Not a reply: the post itself.
            if parent.is_some() {
                comment_id = status.map(|s| s.id.clone());
            }
            let post = match (root, parent) {
                (_, None) => status.map(|s| s.id.clone()),
                (Some(r), Some(_)) => Some(r.to_owned()),
                (None, Some(p)) => Some(p),
            };
            (title, status.map(status_text).unwrap_or_default(), post)
        }
        FOLLOWS => {
            let title = if kind == "follow_request" {
                format!("{name} asked to follow you")
            } else {
                format!("{name} followed you")
            };
            let handle = actor_handle(n.account.as_ref()).filter(|h| *h != name);
            (title, handle.unwrap_or_default(), None)
        }
        _ => {
            let title = match kind.as_str() {
                "direct" => format!("{name} sent you a message"),
                "story:react" => format!("{name} reacted to your story"),
                "story:comment" => format!("{name} replied to your story"),
                _ => format!("New activity from {name}"),
            };
            (
                title,
                status.map(status_text).unwrap_or_default(),
                status.map(|s| s.target().id.clone()),
            )
        }
    };
    let post_id = post_id.filter(|id| path_safe(id));
    let image_url = n.account.as_ref().and_then(|a| {
        normalize_account(a, media)
            .get_str("avatar_url")
            .map(str::to_owned)
    });
    let created_at = n
        .created_at
        .as_deref()
        .and_then(parse_rfc3339)
        .or_else(|| {
            status
                .and_then(|s| s.created_at.as_deref())
                .and_then(parse_rfc3339)
        })
        .unwrap_or(now);
    NewNotification {
        app_id: crate::apps::PIXELFED.id.to_owned(),
        channel: channel.to_owned(),
        dedupe_key: format!("{instance}:{}", n.id),
        title,
        body,
        image_url,
        target_uri: post_id
            .as_deref()
            .map(|p| match comment_id.filter(|c| path_safe(c)) {
                Some(c) => comment_target(p, &c),
                None => post_target(p),
            }),
        group_key: post_id.map(|id| format!("{id}:{channel}")),
        created_at,
        quiet,
    }
}

// ---------- polling ----------

/// Why one instance's poll failed.
#[derive(Debug)]
enum InstanceError {
    /// The upstream rejected the token and no refresh helped.
    Gone,
    Transient(String),
}

/// One instance's notifications since its cursor.
struct InstancePoll {
    new: Vec<NewNotification>,
    /// Stopped at [`MAX_PAGES`] with more to read.
    capped: bool,
}

/// [`pod_core::AppAdapter::poll_notifications`]: poll every stored account
/// whose instance this pod still accepts. No usable account (none stored,
/// or every token rejected for good): [`PollStatus::SignInGone`]. Only
/// failures (upstream down, 5xx, other errors): [`PollStatus::Transient`].
pub async fn poll(pf: &PixelfedAdapter, pod: &Pod, ctx: &PollCtx) -> PollOutcome {
    let transient = |e: String| PollOutcome {
        status: PollStatus::Transient(e),
        ..Default::default()
    };
    let rt = pf.runtime();
    let others = match others_allowed(pod).await {
        Ok(o) => o,
        Err(e) => return transient(format!("pod store: {e:?}")),
    };
    let accounts = match blocking(pod, |s| s.pixelfed_accounts()).await {
        Ok(a) => a,
        Err(e) => return transient(format!("pod store: {e:?}")),
    };
    let mut media = MediaUrls::new(
        &format!("{}{MEDIA_PATH}", ctx.public_prefix),
        rt.media_for(others),
    );
    let mut out = PollOutcome::default();
    let (mut ok, mut failure) = (0, None);
    for (url, account) in accounts {
        let Some(instance) = rt.resolve(&url, others) else {
            tracing::debug!(instance = %url, "not polling an instance this pod no longer accepts");
            continue;
        };
        match poll_instance(pod, &rt, &instance, account, &mut media).await {
            Ok(p) => {
                ok += 1;
                out.new.extend(p.new);
                if p.capped {
                    out.next_in = Some(CATCH_UP_IN);
                }
            }
            Err(InstanceError::Gone) => {}
            Err(InstanceError::Transient(e)) => {
                tracing::warn!(instance = %instance.url, error = %e, "Pixelfed notification poll failed");
                failure = Some(e);
            }
        }
    }
    if let Err(e) = save_media_keys(pod, media).await {
        tracing::warn!(error = ?e, "saving notification avatar keys failed");
    }
    out.status = match (ok, failure) {
        (0, Some(e)) => PollStatus::Transient(e),
        (0, None) => PollStatus::SignInGone(SIGN_IN.to_owned()),
        _ => PollStatus::Ok,
    };
    out
}

async fn poll_instance(
    pod: &Pod,
    rt: &Runtime,
    instance: &Instance,
    mut account: PixelfedAccount,
    media: &mut MediaUrls,
) -> Result<InstancePoll, InstanceError> {
    let store_err = |e: pod_core::ApiError| InstanceError::Transient(format!("pod store: {e:?}"));
    let url = instance.url.clone();
    let cursor = {
        let url = url.clone();
        blocking(pod, move |s| s.notify_cursor(&url))
            .await
            .map_err(store_err)?
    };
    let first = cursor.is_none();
    let mut min_id = cursor.flatten();
    let api = rt.api(instance);
    let mut raw: Vec<Value> = Vec::new();
    let mut capped = false;
    for page in 0..MAX_PAGES {
        let limit = if first { FIRST_POLL_LIMIT } else { PAGE_LIMIT };
        let (result, last_token) = with_token(pod, instance, account.clone(), |token| {
            let (api, min_id) = (&api, min_id.clone());
            async move { api.notifications(&token, min_id.as_deref(), limit).await }
        })
        .await;
        let items = match result {
            Ok(items) => items,
            Err(UpstreamError::Status(StatusCode::UNAUTHORIZED, _)) => {
                // As for the home feed's stale session: forget the token, but
                // only if it is still the stored one.
                let (u, t) = (url.clone(), last_token);
                let deleted = blocking(pod, move |s| {
                    let deleted = s.delete_pixelfed_account_if_token(&u, &t)?;
                    if deleted {
                        s.delete_notify_cursor(&u)?;
                    }
                    Ok(deleted)
                })
                .await
                .unwrap_or(false);
                tracing::warn!(deleted, instance = %url, "upstream rejected the stored token; notification polls of this instance stop");
                return Err(InstanceError::Gone);
            }
            Err(e) => return Err(InstanceError::Transient(e.to_string())),
        };
        account.access_token = last_token;
        let full = items.len() >= limit as usize;
        let ids: Vec<String> = items
            .iter()
            .filter_map(|v| match &v["id"] {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect();
        let newest = newest_id(ids.iter().map(String::as_str)).map(str::to_owned);
        raw.extend(items);
        // Only `min_id` pages can be followed to newer ones; the first poll
        // and a cursor without an id read one page.
        let follow = !first && min_id.is_some();
        if let Some(n) = newest
            && min_id
                .as_deref()
                .is_none_or(|m| id_cmp(&n, m) == Ordering::Greater)
        {
            min_id = Some(n);
        }
        if !follow || !full {
            break;
        }
        if page + 1 == MAX_PAGES {
            capped = true;
        }
    }
    let mut parsed = parse_notifications(&raw);
    parsed.sort_by(|a, b| id_cmp(&a.id, &b.id));
    parsed.dedup_by(|a, b| a.id == b.id);
    let roots = find_roots(&api, &account.access_token, &parsed, &account.account_id).await;
    let now = now_secs();
    let new: Vec<NewNotification> = parsed
        .iter()
        .map(|n| {
            let root = n
                .status
                .as_ref()
                .and_then(|s| roots.get(&s.id))
                .map(String::as_str);
            to_new(n, &url, &account.account_id, media, now, first, root)
        })
        .collect();
    let newest = min_id.clone();
    {
        let url = url.clone();
        blocking(pod, move |s| s.save_notify_cursor(&url, newest.as_deref()))
            .await
            .map_err(store_err)?;
    }
    tracing::info!(instance = %url, count = new.len(), first, capped, "Pixelfed notifications polled");
    Ok(InstancePoll { new, capped })
}

/// The post each comment or mention reply in `parsed` is under (status id ->
/// post id), found by walking up its parents ([`crate::thread_root`]). At most
/// [`MAX_ROOT_LOOKUPS`] lookups per poll; a reply whose walk fails or runs out
/// is left out (its notification then opens its parent's thread).
async fn find_roots(
    api: &crate::upstream::Upstream,
    token: &str,
    parsed: &[UpstreamNotification],
    own_account_id: &str,
) -> HashMap<String, String> {
    let budget = AtomicUsize::new(MAX_ROOT_LOOKUPS);
    // Parent id -> post id, shared by replies in the same thread.
    let mut known: HashMap<String, String> = HashMap::new();
    let mut out = HashMap::new();
    for n in parsed {
        if !matches!(channel_of(n, own_account_id), COMMENTS | MENTIONS) {
            continue;
        }
        let Some(s) = n.status.as_ref() else { continue };
        let Some(parent) = s.in_reply_to_id.clone().filter(|p| path_safe(p)) else {
            continue;
        };
        if let Some(root) = known.get(&parent) {
            out.insert(s.id.clone(), root.clone());
            continue;
        }
        let start = serde_json::json!({"id": s.id, "in_reply_to_id": parent});
        let (top, complete) = crate::thread_root(start, |id| {
            let budget = &budget;
            async move {
                // Lookups run one at a time (the walk awaits each).
                let left = budget.load(AtomicOrdering::Relaxed);
                if left == 0 || !path_safe(&id) {
                    return None;
                }
                budget.store(left - 1, AtomicOrdering::Relaxed);
                match api.status(token, &id).await {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::info!(status = %id, error = %e, "a reply's parent is unavailable");
                        None
                    }
                }
            }
        })
        .await;
        let top_id = crate::raw_status_id(&top).filter(|t| *t != s.id && complete);
        if let Some(root) = top_id {
            known.insert(parent, root.clone());
            out.insert(s.id.clone(), root);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ME: &str = "703641122334455667";
    const BASE: &str = "http://127.0.0.1:8081";

    fn media() -> MediaUrls {
        MediaUrls::new(
            &format!("/pods/p1{MEDIA_PATH}"),
            &crate::MediaAllowlist::new([BASE], []),
        )
    }

    fn bob() -> Value {
        json!({"id": "699112233445566778", "username": "bob", "acct": "bob",
               "display_name": "Bob 🚲", "avatar": format!("{BASE}/storage/avatar-bob.png")})
    }

    fn one(v: Value) -> UpstreamNotification {
        let mut list = parse_notifications(&[v]);
        assert_eq!(list.len(), 1);
        list.remove(0)
    }

    fn map(v: Value) -> NewNotification {
        to_new(&one(v), BASE, ME, &mut media(), 99, false, None)
    }

    fn our_post() -> Value {
        json!({"id": "781254316723458049", "account": {"id": ME, "username": "constructdemo"},
               "content": "<p>Sunrise over Millbrook, three ways &amp; one coffee later</p>",
               "created_at": "2026-10-31T18:42:10.000000Z"})
    }

    #[test]
    fn channels_are_valid_and_complete() {
        let list = channels();
        let ids: Vec<&str> = list.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "likes", "comments", "mentions", "follows", "boosts", "other"
            ]
        );
        for c in &list {
            assert_eq!(c.app_id, "pixelfed");
            assert!(pod_core::notify::valid_channel_id(&c.id), "{}", c.id);
            assert!(!c.title.is_empty() && !c.description.is_empty());
            assert_eq!(c.default_enabled, c.id != "other");
        }
        assert_eq!(list[0].title, "Likes");
        assert_eq!(list[0].description, "When someone likes your post");
    }

    #[test]
    fn a_like() {
        let n = map(
            json!({"id": 17, "type": "favourite", "created_at": "2026-11-01T10:00:00Z",
                           "account": bob(), "status": our_post()}),
        );
        assert_eq!(n.app_id, "pixelfed");
        assert_eq!(n.channel, "likes");
        assert_eq!(n.dedupe_key, format!("{BASE}:17"));
        assert_eq!(n.title, "Bob 🚲 liked your post");
        assert_eq!(
            n.body,
            "Sunrise over Millbrook, three ways & one coffee later"
        );
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/781254316723458049")
        );
        assert_eq!(n.group_key.as_deref(), Some("781254316723458049:likes"));
        assert_eq!(n.created_at, 1_793_527_200);
        assert!(!n.quiet);
        let img = n.image_url.unwrap();
        assert!(img.starts_with("/pods/p1/v1/pixelfed/media/"), "{img}");
        // Pixelfed's own name for it.
        assert_eq!(
            map(json!({"id": "1", "type": "like", "account": bob(), "status": our_post()})).channel,
            "likes"
        );
    }

    #[test]
    fn comments_and_mentions() {
        let reply = json!({"id": "900", "in_reply_to_id": "781254316723458049",
            "in_reply_to_account_id": ME, "account": bob(),
            "content": "<p>That second frame is <b>unreal</b></p>"});
        let n = map(json!({"id": "2", "type": "mention", "account": bob(), "status": reply}));
        assert_eq!(n.channel, "comments");
        assert_eq!(n.title, "Bob 🚲 commented on your post");
        assert_eq!(n.body, "That second frame is unreal");
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/781254316723458049/comments/900")
        );
        assert_eq!(n.group_key.as_deref(), Some("781254316723458049:comments"));
        // Pixelfed's "comment" type; a numeric reply-to account id.
        let n = map(json!({"id": "3", "type": "comment", "account": bob(),
            "status": {"id": 901, "in_reply_to_id": 5, "account": bob(), "content_text": "nice"}}));
        assert_eq!((n.channel.as_str(), n.body.as_str()), ("comments", "nice"));
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/5/comments/901")
        );
        // A mention in someone else's thread, or in a new post.
        let n = map(
            json!({"id": "4", "type": "mention", "account": {"id": "9", "acct": "carol@else.where"},
            "status": {"id": "902", "in_reply_to_account_id": "8", "in_reply_to_id": "77",
                       "account": {"id": "9"}, "content": "<p>@constructdemo look</p>"}}),
        );
        assert_eq!(n.channel, "mentions");
        assert_eq!(n.title, "@carol@else.where mentioned you");
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/77/comments/902")
        );
        let n = map(json!({"id": "5", "type": "mention", "account": bob(),
            "status": {"id": "903", "account": bob(), "content": "hi"}}));
        assert_eq!(n.target_uri.as_deref(), Some("pixelfed://posts/903"));
    }

    #[test]
    fn replies_open_the_whole_thread_at_the_reply() {
        // Bob replies to a comment two levels down; the poll found the post.
        let reply = json!({"id": "903", "in_reply_to_id": "902", "in_reply_to_account_id": ME,
            "account": bob(), "content": "<p>@constructdemo dedication</p>"});
        let raw = json!({"id": "20", "type": "comment", "account": bob(), "status": reply});
        let n = to_new(
            &one(raw.clone()),
            BASE,
            ME,
            &mut media(),
            99,
            false,
            Some("781254316723458049"),
        );
        assert_eq!(n.channel, "comments");
        assert_eq!(n.title, "Bob 🚲 replied to your comment");
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/781254316723458049/comments/903")
        );
        assert_eq!(n.group_key.as_deref(), Some("781254316723458049:comments"));
        // The post not found: the parent's thread, still at the reply (the
        // comments screen walks up to the post itself).
        let n = to_new(&one(raw), BASE, ME, &mut media(), 99, false, None);
        assert_eq!(n.title, "Bob 🚲 commented on your post");
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/902/comments/903")
        );
        // A comment straight on the post: its parent is the post.
        let reply = json!({"id": "904", "in_reply_to_id": "781254316723458049",
            "account": bob(), "content": "nice"});
        let n = to_new(
            &one(json!({"id": "21", "type": "comment", "account": bob(), "status": reply})),
            BASE,
            ME,
            &mut media(),
            99,
            false,
            Some("781254316723458049"),
        );
        assert_eq!(n.title, "Bob 🚲 commented on your post");
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/781254316723458049/comments/904")
        );
        assert_eq!(
            comment_target("1", "2"),
            "pixelfed://posts/1/comments/2",
            "the comment_thread screen route of the pixelfed layouts"
        );
    }

    #[test]
    fn follows_boosts_and_others() {
        let n = map(json!({"id": "6", "type": "follow", "account": bob()}));
        assert_eq!(n.channel, "follows");
        assert_eq!(n.title, "Bob 🚲 followed you");
        assert_eq!(n.body, "@bob");
        assert_eq!((n.target_uri, n.group_key), (None, None));

        // A boost, also as a reblog wrapper around our post.
        let n = map(json!({"id": "7", "type": "share", "account": bob(), "status": our_post()}));
        assert_eq!(n.channel, "boosts");
        assert_eq!(n.title, "Bob 🚲 boosted your post");
        let wrapper = json!({"id": "950", "account": bob(), "reblog": our_post()});
        let n = map(json!({"id": "8", "type": "reblog", "account": bob(), "status": wrapper}));
        assert_eq!(
            n.target_uri.as_deref(),
            Some("pixelfed://posts/781254316723458049")
        );

        let n = map(json!({"id": "9", "type": "story:react", "account": bob()}));
        assert_eq!(
            (n.channel.as_str(), n.title.as_str()),
            ("other", "Bob 🚲 reacted to your story")
        );
        let n = map(json!({"id": "10", "type": "something_new"}));
        assert_eq!(n.channel, "other");
        assert_eq!(n.title, "New activity from Someone");
        assert_eq!(n.image_url, None);
        assert_eq!(n.created_at, 99, "no time: now");
    }

    #[test]
    fn lenient_parsing() {
        let list = parse_notifications(&[
            json!({"id": null, "type": "favourite"}),
            json!({"id": 11, "type": 5, "account": "garbage", "status": [1],
                   "created_at": "not a date"}),
            json!({"id": "12", "type": "favourite", "account": bob(),
                   "status": {"id": "../../x", "account": bob()}}),
        ]);
        assert_eq!(list.len(), 2);
        let n = to_new(&list[0], BASE, ME, &mut media(), 5, true, None);
        assert_eq!(n.channel, "other");
        assert!(n.quiet);
        // A status id that is not path-safe gets no target.
        let n = to_new(&list[1], BASE, ME, &mut media(), 5, false, None);
        assert_eq!((n.target_uri, n.group_key), (None, None));
        // An avatar outside the media policy: no image.
        let n = map(json!({"id": "13", "type": "follow",
            "account": {"id": "1", "username": "x", "avatar": "https://evil.example/a.png"}}));
        assert_eq!(n.image_url, None);
    }

    #[test]
    fn ids_and_excerpts() {
        assert_eq!(id_cmp("9", "10"), Ordering::Less);
        assert_eq!(id_cmp("1022706489856569353", "999"), Ordering::Greater);
        assert_eq!(id_cmp("b", "ab"), Ordering::Less);
        assert_eq!(newest_id(["3", "12", "7"]), Some("12"));
        assert_eq!(newest_id([]), None);
        assert_eq!(excerpt("a\n\nb   c", 10), "a b c");
        assert_eq!(excerpt("abcdef ghij", 8), "abcdef…");
        assert_eq!(
            post_target("42"),
            "pixelfed://posts/42",
            "the post screen route of the pixelfed layouts"
        );
    }
}
