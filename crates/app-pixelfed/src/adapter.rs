//! [`PixelfedAdapter`]: the `pixelfed` app, the `pixelfed` sign-in and
//! its notifications (D52, [`crate::notify`]).

use std::sync::{Arc, RwLock};

use axum::Router;
use pod_core::layouts::LayoutSource;
use pod_core::{
    AppAdapter, AppInfo, BoxFuture, EntityDoc, FeedSpec, Hydration, PathSpec, Pod, PollCtx,
    PollOutcome, RequestCtx, Session, SignInAccount, SignInProvider, SourceManifest,
};
use protocol::{NotificationChannel, WorkspaceSpec};

use crate::config::{Instance, PixelfedConfig};
use crate::upstream::{Clients, Upstream};
use crate::{MediaPolicy, apps};

/// The sign-in this adapter provides (recorded on sessions).
pub const SIGN_IN: &str = "pixelfed";

/// The adapter's name: its data source id, and the prefix of its tables
/// and routes.
pub const NAME: &str = "pixelfed";

/// The home timeline feed (`AppInfo::feeds` entry `("pixelfed", "home")`).
pub const HOME_FEED: &str = "home";

/// What layouts may use of Pixelfed (decision
/// `custom-apps-and-data-sources`). Every path a deployed layout uses stays
/// here.
pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Pixelfed",
    sign_in: Some(SIGN_IN),
    entity_schemes: &["pixelfed"],
    feeds: &[FeedSpec {
        name: HOME_FEED,
        query_keys: &["timeline:home"],
        doc: "The signed-in account's home timeline, newest first; posts on their way first.",
    }],
    data_paths: &[
        PathSpec {
            pattern: "/v1/pixelfed/screens/post?id={id}",
            query_keys: &["post:{id}"],
            doc: "One post, with its author and media.",
        },
        PathSpec {
            pattern: "/v1/pixelfed/screens/comments?post_id={post_id}",
            query_keys: &["comments:{post_id}"],
            doc: "A post's comments as a thread (each with a depth).",
        },
    ],
    actions: &[
        PathSpec {
            pattern: "/v1/pixelfed/posts/like",
            query_keys: &[],
            doc: "Like or unlike: {id, liked}.",
        },
        PathSpec {
            pattern: "/v1/pixelfed/posts/bookmark",
            query_keys: &[],
            doc: "Bookmark or unbookmark: {id, bookmarked}.",
        },
        PathSpec {
            pattern: "/v1/pixelfed/posts/comment",
            query_keys: &[],
            doc: "Comment on a post: {id, text, client_id}.",
        },
        PathSpec {
            pattern: "/v1/pixelfed/posts/create",
            query_keys: &[],
            doc: "Post photos: {caption, media (attachments), client_id}.",
        },
        PathSpec {
            pattern: "/v1/pixelfed/posts/pending/retry",
            query_keys: &[],
            doc: "Retry a post that failed on its way: {client_id}.",
        },
        PathSpec {
            pattern: "/v1/pixelfed/posts/pending/discard",
            query_keys: &[],
            doc: "Drop a post that failed on its way: {client_id}.",
        },
    ],
    entities: &[
        EntityDoc {
            uri: "pixelfed://posts/{id}",
            doc: "A post (a reblog is shown as the original, with reblogged_by_*).",
            fields: &[
                ("id", "the post id"),
                ("caption", "plain-text caption"),
                ("created_at", "RFC 3339 time"),
                ("author_name", "display name"),
                ("author_handle", "@user@instance"),
                ("author_avatar_url", "avatar image URL"),
                ("author_uri", "pixelfed://users/{id}"),
                ("media_uris", "pixelfed://media/{id} URIs, in order"),
                ("media_count", "how many media"),
                ("cover_url", "the first image's URL"),
                ("cover_blurhash", "the first image's blurhash"),
                ("like_count", "likes"),
                ("reply_count", "comments"),
                ("is_liked", "liked by the signed-in account"),
                ("is_bookmarked", "bookmarked by the signed-in account"),
                ("sensitive", "marked sensitive"),
                ("content_warning", "content warning text, if any"),
                ("content_warning_label", "what to show over sensitive media"),
                ("reblogged_by_name", "who reblogged it, if a reblog"),
                ("reblogged_by_uri", "pixelfed://users/{id} of the reblogger"),
                ("url", "the post's web page"),
            ],
        },
        EntityDoc {
            uri: "pixelfed://users/{id}",
            doc: "An account.",
            fields: &[
                ("id", "the account id"),
                ("username", "user name, without @"),
                ("display_name", "display name"),
                ("avatar_url", "avatar image URL"),
            ],
        },
        EntityDoc {
            uri: "pixelfed://media/{id}",
            doc: "A photo or video of a post.",
            fields: &[
                ("id", "the media id"),
                ("type", "image, video, ..."),
                ("url", "full-size URL (proxied by the pod)"),
                ("preview_url", "small URL"),
                ("alt_text", "description"),
                ("aspect_ratio", "width / height"),
                ("blurhash", "placeholder hash"),
            ],
        },
        EntityDoc {
            uri: "pixelfed://comments/{id}",
            doc: "A comment of a post's thread.",
            fields: &[
                ("id", "the comment id"),
                ("post_id", "the post it belongs to"),
                ("in_reply_to_id", "the comment or post it answers"),
                ("depth", "thread depth, 0 for a direct reply"),
                ("text", "plain text"),
                ("author_name", "display name"),
                ("author_handle", "@user@instance"),
                ("avatar_url", "avatar image URL"),
                ("created_at", "RFC 3339 time"),
                ("like_count", "likes"),
                ("is_liked", "liked by the signed-in account"),
                ("is_own", "written by the signed-in account"),
                ("meta", "one line: handle and time"),
            ],
        },
    ],
    dev: false,
};

/// What the adapter's handlers read per request: the config and what is
/// built from it.
pub struct Runtime {
    pub config: PixelfedConfig,
    /// [`PixelfedConfig::media_allowlist`], parsed once, plus any public
    /// HTTPS media when users may sign in to any instance.
    pub media: MediaPolicy,
    pub clients: Clients,
    /// Comments just posted, listed until the upstream lists them.
    pub recent_comments: crate::recent::RecentComments,
}

impl Runtime {
    fn new(config: PixelfedConfig) -> Self {
        let media = MediaPolicy {
            allow: config.media_allowlist(),
            any_public_https: config.any_instance,
        };
        Self {
            clients: Clients::new(media.clone()),
            media,
            config,
            recent_comments: Default::default(),
        }
    }

    /// The API client for `instance`.
    pub fn api(&self, instance: &Instance) -> Upstream {
        self.clients.api(instance)
    }

    /// [`PixelfedConfig::resolve`] in a pod whose settings may forbid other
    /// instances than the default (`others` false: temporary pods, D46).
    pub fn resolve(&self, typed: &str, others: bool) -> Option<Instance> {
        let instance = self.config.resolve(typed)?;
        (others || instance.url == self.config.instance_url).then_some(instance)
    }

    /// The media policy of a pod: without other instances, only the
    /// configured origins (no public HTTPS media from anywhere).
    pub fn media_for(&self, others: bool) -> MediaPolicy {
        MediaPolicy {
            allow: self.media.allow.clone(),
            any_public_https: others && self.media.any_public_https,
        }
    }
}

/// The Pixelfed adapter (one owner per instance; each session uses the
/// instance it signed in to, D45): sign-in with PKCE (the pod keeps the
/// upstream token, D17), the home feed, likes, bookmarks, comments and the
/// media proxy.
pub struct PixelfedAdapter {
    runtime: RwLock<Arc<Runtime>>,
}

impl PixelfedAdapter {
    pub fn new(config: PixelfedConfig) -> Self {
        Self {
            runtime: RwLock::new(Arc::new(Runtime::new(config))),
        }
    }

    /// The current runtime (cheap: one `Arc` clone).
    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn config(&self) -> PixelfedConfig {
        self.runtime().config.clone()
    }

    /// Point the adapter at another instance (tests that start the fake
    /// upstream after the pod's state). Requests already running keep the
    /// old runtime.
    pub fn configure(&self, config: PixelfedConfig) {
        *self.runtime.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(Runtime::new(config));
    }
}

impl AppAdapter for PixelfedAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn apps(&self) -> Vec<AppInfo> {
        vec![apps::PIXELFED]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        apps::PIXELFED_FILES
    }

    fn component_files(&self) -> &'static [pod_core::components::ComponentSource] {
        apps::COMPONENT_FILES
    }

    fn router(&self) -> Router<Pod> {
        crate::routes::router()
    }

    fn migrations(&self) -> &'static [pod_core::store::Step] {
        crate::db::MIGRATIONS
    }

    fn on_open(&self, store: &pod_core::store::Store) -> Result<(), String> {
        let sealed =
            crate::db::reseal(store).map_err(|e| format!("sealing stored credentials: {e}"))?;
        if sealed > 0 {
            tracing::info!(
                sealed,
                "sealed stored Pixelfed credentials under the current key"
            );
        }
        Ok(())
    }

    fn public_paths(&self) -> &'static [&'static str] {
        // Images load without the bearer; keys are opaque hashes of public
        // upstream media (D17).
        &["/v1/pixelfed/media/"]
    }

    fn sign_ins(&self) -> Vec<SignInProvider> {
        vec![SignInProvider {
            id: SIGN_IN,
            title: "Pixelfed",
        }]
    }

    fn sign_in_start<'a>(
        &'a self,
        pod: &'a Pod,
        _sign_in: &'a str,
    ) -> BoxFuture<'a, Option<pod_core::SignInStart>> {
        Box::pin(async move {
            let rt = self.runtime();
            let others = crate::routes::others_allowed(pod).await.unwrap_or(false);
            Some(crate::layout::sign_in_start(&rt.config, others))
        })
    }

    fn signed_out(&self, _sign_in: &str, app: &AppInfo, _req: &RequestCtx) -> WorkspaceSpec {
        crate::layout::signed_out(&self.runtime().config, app.id, app.title)
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            match feed {
                HOME_FEED => crate::routes::home_feed(self, pod, req).await,
                _ => pod_core::unknown_feed(NAME, feed),
            }
        })
    }

    fn notification_channels(&self) -> Vec<NotificationChannel> {
        crate::notify::channels()
    }

    fn poll_interval(&self) -> Option<std::time::Duration> {
        Some(crate::notify::POLL_INTERVAL)
    }

    fn poll_notifications<'a>(
        &'a self,
        pod: &'a Pod,
        ctx: &'a PollCtx,
    ) -> BoxFuture<'a, PollOutcome> {
        Box::pin(crate::notify::poll(self, pod, ctx))
    }

    fn sign_in_account<'a>(
        &'a self,
        pod: &'a Pod,
        _sign_in: &'a str,
        session: &'a Session,
    ) -> BoxFuture<'a, Option<SignInAccount>> {
        Box::pin(async move {
            match crate::routes::session_account(pod, session).await {
                Ok(Some((instance, account))) => Some(SignInAccount {
                    server: Some(instance.url),
                    username: Some(account.username).filter(|u| !u.is_empty()),
                }),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(status = %e.0, "sign-in account lookup failed");
                    None
                }
            }
        })
    }

    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        Box::pin(crate::posting::resume(pod))
    }
}
