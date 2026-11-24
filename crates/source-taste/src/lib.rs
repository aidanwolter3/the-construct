//! The `taste` data source (decision `taste-data-source`): a feed of clothes
//! from the user's own **taste service** (a separate HTTP service that
//! crawls stores, embeds every product photo and ranks a feed that learns
//! what they like without narrowing to it), shown and reacted to from the
//! pod. The service owns the catalog, the ranking and the reactions; the pod
//! keeps the feed it was served ([`db`]) and passes likes and skips on.
//!
//! The pod signs in with the service's URL and a token (`POST
//! /v1/taste/sign-in`, from the Taste layout's form), stored sealed.
//!
//! - Feed `feed` (`taste:status`, `taste:feed`): the kept feed's pieces not
//!   reacted to yet, a deck; the first page is fetched when it is empty, the
//!   next whenever a reaction leaves fewer than [`LOW`] (and on More,
//!   `/v1/taste/more`), a fresh one on Refresh (`/v1/taste/refresh`).
//! - Data paths: an item with its similar and opposite pieces, "this, but
//!   …" (steer), search, the likes, the taste page, and the taste map: the
//!   catalog's map cut into tiles to zoom into, and the pieces of an area.
//! - Actions: sign in and out, react (like, skip, undo), more, refresh, and
//!   write the taste's summary.
//! - The `taste` core layout: `layouts/taste/`.

// Route helpers hand back the refusal as an axum `Response`.
#![allow(clippy::result_large_err)]

pub mod api;
pub mod bundle;
pub mod db;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::layouts::LayoutSource;
use pod_core::net::OutboundClient;
use pod_core::session::Caller;
use pod_core::store::{Step, Store};
use pod_core::{
    AuthPolicy, BoxFuture, CoreLayoutInfo, EntityDoc, FeedSpec, Hydration, PathSpec, Pod,
    RequestCtx, Source, SourceManifest,
};
use protocol::DataBundle;
use serde_json::{Value, json};

use crate::api::{ApiError, Service};

/// The adapter name and the entity scheme.
pub const NAME: &str = "taste";
/// The core layout.
pub const APP_ID: &str = "taste";
/// The taste service jack's id.
pub const JACK: &str = "taste";

pub const FEED: &str = "feed";

pub const SIGN_IN_PATH: &str = "/v1/taste/sign-in";
pub const SIGN_OUT_PATH: &str = "/v1/taste/sign-out";
pub const REACT_PATH: &str = "/v1/taste/react";
pub const MORE_PATH: &str = "/v1/taste/more";
pub const REFRESH_PATH: &str = "/v1/taste/refresh";
pub const SUMMARY_PATH: &str = "/v1/taste/summary";
pub const ITEM_PATH: &str = "/v1/taste/screens/item";
pub const STEER_PATH: &str = "/v1/taste/screens/steer";
pub const SEARCH_PATH: &str = "/v1/taste/screens/search";
pub const LIKES_PATH: &str = "/v1/taste/screens/likes";
pub const TASTE_PATH: &str = "/v1/taste/screens/taste";
pub const MAP_PATH: &str = "/v1/taste/screens/map";
pub const AREA_PATH: &str = "/v1/taste/screens/area";
pub const SCATTER_PATH: &str = "/v1/taste/screens/scatter";
pub const POINT_PATH: &str = "/v1/taste/screens/point";
/// How long the pod keeps the whole map before asking for it again.
pub const MAP_KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// After a reaction (the model's chances moved), the kept map is asked for
/// again when shown, but not more often than this.
pub const MAP_REFRESH: std::time::Duration = std::time::Duration::from_secs(15);

/// Items asked for per page of the feed, and per list of an item screen.
pub const PAGE: usize = 24;
pub const RELATED: usize = 12;
pub const RESULTS: usize = 36;
/// The deck is topped up when a reaction leaves fewer pieces than this.
pub const LOW: usize = 6;
/// Tiles per side of a map screen.
pub const GRID: usize = 4;

const ITEM_FIELDS: &[(&str, &str)] = &[
    (
        "id",
        "the service's id of the product (e.g. shop.example:123)",
    ),
    (
        "key",
        "the id as a URI segment (shop_example__123): screens route on it",
    ),
    ("uri", "taste://items/<key>"),
    ("brand", "the brand"),
    ("title", "the product's name"),
    ("category", "e.g. jacket, trousers, boots (from the photo)"),
    ("price_text", "e.g. $148, £95, or empty"),
    (
        "buy_url",
        "the product's page in its store (http(s), or empty)",
    ),
    ("host", "the store's host, e.g. shop.example"),
    ("image", "the first photo 720 px wide (the store's CDN)"),
    ("image_large", "the same 1600 px wide"),
    ("thumb", "the same 360 px wide"),
    ("liked", "true once liked"),
    ("skipped", "true once disliked (a left swipe; the field keeps this name)"),
    (
        "why_text",
        "feed items: why it is there, Near your taste · 2, Adjacent or Exploring",
    ),
    (
        "why_tone",
        "a pill tone for why_text: accent, success or muted",
    ),
    (
        "description",
        "the store's description, plain text of at most 300 characters (may be empty)",
    ),
    ("worn", "true when the first photo shows someone wearing it"),
    (
        "chance_text",
        "how likely the service thinks it's liked, e.g. 41% your taste (empty until it knows)",
    ),
    (
        "image_0 .. image_5",
        "photo k 720 px wide (someone wearing it first), or empty past photo_count",
    ),
    ("photo_count", "how many photos (0 to 6)"),
    ("photos_many", "true with more than one photo"),
    (
        "next_0 .. next_5",
        "the photo after photo k, wrapping to 0: a tap sets the device-only field photo to \
         entity.next_<k> with action.local.set (photo is never served: 0 or missing is the first)",
    ),
];

pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Taste",
    sign_in: None,
    entity_schemes: &[NAME],
    feeds: &[FeedSpec {
        name: FEED,
        query_keys: &[bundle::STATUS_QUERY, bundle::FEED_QUERY],
        doc: "The taste feed as the pod keeps it (taste:feed): the pieces not liked or disliked \
              yet, in order, each with why (On the edge of the user's taste, Exploring, Probably \
              yours), a deck: a reaction takes a piece out, and the pod fetches the next page \
              itself when few are left; More appends one, Refresh starts again. taste:status: \
              whether the pod is signed in to the taste service, its host, the like and skip \
              counts (counts_text) and the last error.",
    }],
    data_paths: &[
        PathSpec {
            pattern: "/v1/taste/screens/item?key={key}",
            query_keys: &["taste:similar:{key}", "taste:contrast:{key}"],
            doc: "One item (taste://items/<key>), its similar pieces and its opposites (the same \
                  kind of garment, as different as the catalog has).",
        },
        PathSpec {
            pattern: "/v1/taste/screens/steer?key={key}&q={q}",
            query_keys: &["taste:steer:{key}"],
            doc: "\"This, but <q>\": the item moved in the direction the words point (more \
                  relaxed, in brown suede); load it with action.data.load.",
        },
        PathSpec {
            pattern: "/v1/taste/screens/search?q={q}",
            query_keys: &[bundle::SEARCH_QUERY],
            doc: "Pieces that look like the words (matched by image, not product text), and \
                  taste://search/pod {q, count_text}; load it with action.data.load.",
        },
        PathSpec {
            pattern: "/v1/taste/screens/map?area={area}",
            query_keys: &["taste:tiles:{area}"],
            doc: "The taste map over an area (taste://areas/<area>: title, count_text, \
                  reactions_text): the catalog laid out by how its photos look, the area cut into \
                  4 × 4 tiles (taste:tiles:<area>, row by row), each tinted by how likely the user \
                  likes what's there (color), with a photo from it (image, empty for an empty \
                  tile), counts and opens: the address a tap opens (a finer map of the tile, \
                  taste://map/<tile's area>, or its pieces when it holds only a few, \
                  taste://area/<tile's area>; empty for an empty tile). The whole map is area \
                  0-0-1000000-1000000 (x0-y0-x1-y1 in millionths).",
        },
        PathSpec {
            pattern: "/v1/taste/screens/area?area={area}",
            query_keys: &["taste:area:{area}"],
            doc: "A sample of the pieces inside an area of the taste map (taste:area:<area>).",
        },
        PathSpec {
            pattern: SCATTER_PATH,
            query_keys: &[],
            doc: "The whole taste map for a core://Scatter: taste://maps/catalog {points (every \
                  piece, packed: likes highlighted, skips muted, coloured by how likely the user \
                  likes it), count_text, reactions_text, modelled}. A point's index (on_tap's \
                  $point.index) opens taste://points/<index>; a box selected ($select.box) opens \
                  taste://area/<box>.",
        },
        PathSpec {
            pattern: "/v1/taste/screens/point?n={n}",
            query_keys: &["taste:point-similar:{n}", "taste:point-contrast:{n}"],
            doc: "The piece at point <n> of the taste map (taste://points/<n>, with the item's \
                  fields and its own key, so taste://items/<key> opens its full screen), its \
                  similar pieces (taste:point-similar:<n>) and its opposites \
                  (taste:point-contrast:<n>).",
        },
        PathSpec {
            pattern: LIKES_PATH,
            query_keys: &[bundle::LIKES_QUERY],
            doc: "Everything liked, newest first.",
        },
        PathSpec {
            pattern: TASTE_PATH,
            query_keys: &[bundle::TASTES_QUERY, bundle::TASTE_ITEMS_QUERY],
            doc: "The user's tastes (clusters of their likes, taste:tastes), each with its \
                  describing words and brands; their pieces (taste:taste_items, each with the \
                  taste n it belongs to: a list in a taste's template filters where taste \
                  equals $item.n); and the written summary (taste://summary/pod).",
        },
    ],
    actions: &[
        PathSpec {
            pattern: SIGN_IN_PATH,
            query_keys: &[],
            doc: "Sign the pod in to the taste service: {url, token} (url https://, the \
                  service's TASTE_TOKEN). Checked with the service first; stored sealed.",
        },
        PathSpec {
            pattern: SIGN_OUT_PATH,
            query_keys: &[],
            doc: "Forget the service and the kept feed: {}. Wrap it in action.confirm.",
        },
        PathSpec {
            pattern: REACT_PATH,
            query_keys: &[],
            doc: "React to an item: {id, liked} (true likes, false undoes), {id, skipped} or \
                  {id, kind: like|skip|clear}; the service learns from it.",
        },
        PathSpec {
            pattern: MORE_PATH,
            query_keys: &[],
            doc: "Append the next page to the feed: {}.",
        },
        PathSpec {
            pattern: REFRESH_PATH,
            query_keys: &[],
            doc: "Replace the feed with a fresh page: {}.",
        },
        PathSpec {
            pattern: SUMMARY_PATH,
            query_keys: &[],
            doc: "Have the service write the taste's summary in words: {} (it asks a model, so \
                  it takes a while; the taste page reloads with it).",
        },
    ],
    mcp_actions: &[],
    entities: &[
        EntityDoc {
            uri: "taste://items/{key}",
            doc: "A product: a real piece from a real store, with its photo and where to buy it.",
            fields: ITEM_FIELDS,
        },
        EntityDoc {
            uri: bundle::STATUS_URI,
            doc: "The connection to the taste service (taste:status).",
            fields: &[
                ("signed_in", "true once the pod is signed in to the service"),
                ("host", "the service's host"),
                ("status_text", "e.g. Connected to taste.example"),
                ("likes", "how many pieces are liked"),
                ("skips", "how many are disliked"),
                ("counts_text", "e.g. 12 liked · 30 disliked"),
                ("error", "the last call's failure in words, or empty"),
                (
                    "loading",
                    "false as served; a layout sets it while More or Refresh runs",
                ),
            ],
        },
        EntityDoc {
            uri: "taste://tastes/{n}",
            doc: "One taste: a cluster of the user's likes (taste:tastes).",
            fields: &[
                (
                    "n",
                    "its place, from \"0\" (its pieces: taste:taste_items where taste equals it)",
                ),
                ("title", "Taste 1, Taste 2, ..."),
                ("size_text", "e.g. 8 likes"),
                (
                    "attributes_text",
                    "words an image model puts to it, e.g. minimal · wool · earth tones",
                ),
                ("brands_text", "its brands, most liked first"),
                ("categories_text", "its kinds of garment"),
            ],
        },
        EntityDoc {
            uri: bundle::SUMMARY_URI,
            doc: "The taste in words (the taste page).",
            fields: &[
                ("text", "the description, or empty until written"),
                ("written", "true once written"),
                ("written_text", "e.g. Written at 12 likes"),
                ("likes", "how many likes there are now"),
                (
                    "writing",
                    "false as served; a layout sets it while one is written",
                ),
            ],
        },
        EntityDoc {
            uri: "taste://areas/{area}",
            doc: "An area of the taste map (the map screen's).",
            fields: &[
                ("key", "the area, x0-y0-x1-y1 in millionths"),
                ("title", "Your taste map, or Zoomed in ×16"),
                ("whole", "true for the whole map"),
                ("count_text", "e.g. 1,204 pieces"),
                ("reactions_text", "e.g. 12 liked · 30 disliked here"),
            ],
        },
        EntityDoc {
            uri: "taste://tiles/{area}/{j}_{i}",
            doc: "A tile of a map screen (taste:tiles:<area>).",
            fields: &[
                ("area", "the tile's own area"),
                (
                    "color",
                    "#RRGGBB: warm where the user likes what's there, cool where not",
                ),
                (
                    "image",
                    "a photo of a piece there, 360 px wide (empty for an empty tile)",
                ),
                ("count", "how many pieces"),
                ("count_text", "e.g. 40 pieces"),
                ("reactions_text", "e.g. ♥ 2 · ✕ 1, or empty"),
                ("empty", "true with nothing there"),
                (
                    "opens",
                    "what a tap opens: a finer map or the pieces (empty for an empty tile)",
                ),
                (
                    "description",
                    "for screen readers, e.g. 40 pieces, like Waxed Chore Coat",
                ),
            ],
        },
        EntityDoc {
            uri: bundle::SEARCH_URI,
            doc: "The last search (with taste:search).",
            fields: &[
                ("q", "what was searched for"),
                ("count_text", "e.g. 36 pieces, or Nothing found"),
            ],
        },
    ],
    dev: false,
};

pub const APP: CoreLayoutInfo = CoreLayoutInfo {
    id: APP_ID,
    title: "Taste",
    icon: "favorite",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[(NAME, FEED)],
};

// `FILES`: every `layouts/*.json` of this crate, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The env var that lets the service live on a private address (local
/// development and tests only).
pub const PRIVATE_HOSTS_VAR: &str = "CONSTRUCT_TASTE_PRIVATE_HOSTS";

#[derive(Debug, Clone)]
pub struct TasteAdapter {
    /// The service on any address over http or https (tests, local
    /// development); otherwise https on public hosts only.
    pub allow_private_hosts: bool,
    pub client: OutboundClient,
}

impl TasteAdapter {
    pub fn new(allow_private_hosts: bool) -> Self {
        Self {
            allow_private_hosts,
            client: api::client(allow_private_hosts),
        }
    }

    pub fn from_env() -> Self {
        Self::new(std::env::var(PRIVATE_HOSTS_VAR).as_deref() == Ok("1"))
    }
}

impl Default for TasteAdapter {
    fn default() -> Self {
        Self::new(false)
    }
}

/// One fetch of the feed at a time per pod, so two devices opening an
/// empty feed don't each take a page.
#[derive(Default)]
struct FeedLock(tokio::sync::Mutex<()>);

const INSTRUCTIONS: &str = "The taste source shows the user's taste feed: clothes from their own \
taste service, which learns what they like from likes and dislikes (a dislike is sent as \
skipped) and keeps showing them things they haven't seen, looking for the edges of their taste. \
Feed feed: taste:feed (a deck of the pieces not reacted to yet, each with why_text: On the edge, \
Exploring or Probably yours, its photos image_0..image_5 and description) and taste:status \
(signed_in, counts_text). Data paths: an item with its similar pieces (the core layout shows it \
as a sheet: a swipe up on a card), search, likes, the taste page and the taste map. Actions: \
/v1/taste/react {id, liked} or {id, skipped}, /v1/taste/more, /v1/taste/refresh, \
/v1/taste/summary. The core layout taste is the whole thing: use it whole ({type: core://Use, \
ref: @core/taste}) as a layout and link it from home, where taste:status's counts_text makes a \
one-line summary. The pod signs in to the service from that layout's form (its URL and token) \
and signs out from Settings (the taste jack): never ask for the token in the conversation.";

impl Source for TasteAdapter {
    fn element_catalog(&self) -> &'static str {
        include_str!("../catalog.json")
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn core_layouts(&self) -> Vec<CoreLayoutInfo> {
        vec![APP]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }

    fn migrations(&self) -> &'static [Step] {
        db::MIGRATIONS
    }

    fn on_open(&self, store: &Store) -> Result<(), String> {
        db::reseal(store).map_err(|e| e.to_string())
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn router(&self) -> Router<Pod> {
        Router::new()
            .route(SIGN_IN_PATH, post(sign_in_route))
            .route(SIGN_OUT_PATH, post(sign_out_route))
            .route(REACT_PATH, post(react_route))
            .route(MORE_PATH, post(more_route))
            .route(REFRESH_PATH, post(refresh_route))
            .route(SUMMARY_PATH, post(summary_route))
            .route(ITEM_PATH, get(item_route))
            .route(STEER_PATH, get(steer_route))
            .route(SEARCH_PATH, get(search_route))
            .route(LIKES_PATH, get(likes_route))
            .route(TASTE_PATH, get(taste_route))
            .route(MAP_PATH, get(map_route))
            .route(AREA_PATH, get(area_route))
            .route(SCATTER_PATH, get(scatter_route))
            .route(POINT_PATH, get(point_route))
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            if MANIFEST.feed(feed).is_none() {
                return pod_core::unknown_feed(NAME, feed);
            }
            let account = match pod.blocking(db::account).await {
                Ok(a) => a,
                Err(e) => return Hydration::Error(e.into_response()),
            };
            let mut b = DataBundle::default();
            let Some(account) = account else {
                bundle::status(&mut b, &bundle::Status::default());
                b.queries.insert(bundle::FEED_QUERY.into(), Vec::new());
                return Hydration::Data(b);
            };
            let mut error = account.last_error.clone();
            let mut items = match pod.blocking(db::waiting).await {
                Ok(i) => i,
                Err(e) => return Hydration::Error(e.into_response()),
            };
            if items.is_empty() {
                match next_page(pod, self, &account.service, Fetch::First).await {
                    Ok(page) => {
                        items = page;
                        error = None;
                    }
                    Err(e) => error = Some(e.to_string()),
                }
            }
            bundle::status(
                &mut b,
                &bundle::Status {
                    signed_in: true,
                    host: api::host_of(&account.service.url),
                    likes: account.likes,
                    skips: account.skips,
                    error,
                },
            );
            bundle::put_items(&mut b, bundle::FEED_QUERY, &items);
            Hydration::Data(b)
        })
    }

    /// The taste service jack (decision `jacks`). Plugged by the Taste
    /// layout's sign-in form (its URL and token); unplugging signs out.
    fn jacks(&self) -> Vec<pod_core::jacks::JackSpec> {
        vec![pod_core::jacks::JackSpec {
            id: JACK,
            title: "Taste service",
            summary: "Your taste service: a feed of clothes that learns what you like from \
                      what you like and dislike, with similar pieces, search and your taste in \
                      words (the taste core layout and source).",
            kind: pod_core::jacks::JackKind::Account {
                how: "sign in from the Taste layout's form (the service's https:// URL and its \
                      token; never ask for the token in the conversation)",
            },
        }]
    }

    fn jack_status<'a>(
        &'a self,
        pod: &'a Pod,
        jack: &'a str,
    ) -> BoxFuture<'a, Option<pod_core::jacks::JackStatus>> {
        Box::pin(async move {
            if jack != JACK {
                return None;
            }
            let service = pod.blocking(db::service).await.ok().flatten();
            Some(match service {
                Some(s) => pod_core::jacks::JackStatus {
                    plugged: true,
                    detail: Some(s.url),
                },
                None => pod_core::jacks::JackStatus::default(),
            })
        })
    }

    fn unplug_jack<'a>(
        &'a self,
        pod: &'a Pod,
        jack: &'a str,
    ) -> Option<BoxFuture<'a, Result<(), String>>> {
        (jack == JACK).then(|| {
            Box::pin(async move {
                pod.blocking(db::sign_out)
                    .await
                    .map_err(|_| "signing out of the taste service failed".to_owned())?;
                pod.publish_data_all(NAME);
                Ok(())
            }) as BoxFuture<'a, Result<(), String>>
        })
    }

    fn mcp_summary(&self) -> Option<&'static str> {
        Some("the taste feed of clothes")
    }

    fn mcp_instructions(&self) -> Option<&'static str> {
        Some(INSTRUCTIONS)
    }
}

/// Which page a fetch takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fetch {
    /// The first page of an empty deck (nothing when another fetch filled it
    /// meanwhile).
    First,
    /// The next page when the deck is running low (nothing when it isn't,
    /// by the time this fetch's turn comes).
    TopUp,
    /// The next page, after the feed.
    More,
    /// A fresh page instead of the feed.
    Refresh,
}

/// Fetch a page and keep it; the deck after it (the pieces not reacted to
/// yet). One at a time per pod.
async fn next_page(
    pod: &Pod,
    a: &TasteAdapter,
    service: &Service,
    fetch: Fetch,
) -> Result<Vec<Value>, ApiError> {
    let lock = pod.local::<FeedLock>();
    let _held = lock.0.lock().await;
    let enough = match fetch {
        Fetch::First => 1,
        Fetch::TopUp => LOW,
        Fetch::More | Fetch::Refresh => usize::MAX,
    };
    if let Ok(waiting) = pod.blocking(db::waiting).await
        && waiting.len() >= enough
    {
        return Ok(waiting);
    }
    let page = match api::feed(&a.client, service, PAGE).await {
        Ok(p) => p,
        Err(e) => {
            let text = e.to_string();
            let _ = pod.blocking(move |s| db::set_error(s, Some(&text))).await;
            return Err(e);
        }
    };
    let replace = fetch == Fetch::Refresh;
    pod.blocking(move |s| {
        db::append(s, &page, replace)?;
        db::set_error(s, None)?;
        db::waiting(s)
    })
    .await
    .map_err(|_| ApiError::Unreachable("the pod couldn't keep the page".into()))
}

// ---------- routes ----------

fn refuse(status: StatusCode, error: &str, detail: impl Into<String>) -> Response {
    pod_core::json_error(status, json!({"error": error, "detail": detail.into()}))
}

/// The caller's session, or the refusal: everything here reaches a service
/// outside the pod, so it needs a valid session.
async fn check_caller(pod: &Pod, headers: &HeaderMap) -> Result<(), Response> {
    match pod.caller(headers).await {
        Ok(Caller::Signed(_)) => Ok(()),
        Ok(Caller::Stale) => Err(refuse(
            StatusCode::UNAUTHORIZED,
            "invalid_session",
            "the session is not valid",
        )),
        Ok(Caller::Anonymous) => Err(refuse(
            StatusCode::FORBIDDEN,
            "session_required",
            "open the pod first",
        )),
        Err(e) => Err(e.into_response()),
    }
}

/// The JSON object body, or the refusal.
async fn body_of(pod: &Pod, headers: &HeaderMap, body: &Bytes) -> Result<Value, Response> {
    check_caller(pod, headers).await?;
    let v = pod_core::parse_json_body(headers, body).map_err(IntoResponse::into_response)?;
    match v {
        Value::Object(_) => Ok(v),
        Value::Null => Ok(json!({})),
        _ => Err(refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the body is a JSON object",
        )),
    }
}

/// A text field of `body`: trimmed, non-empty, at most `max` characters.
fn field(body: &Value, k: &str, max: usize) -> Result<String, Response> {
    body.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= max)
        .map(str::to_owned)
        .ok_or_else(|| {
            refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("\"{k}\" is text of 1 to {max} characters"),
            )
        })
}

/// A service failure as the pod's answer. A refused token is 502, never
/// 401, which would tell the client its own session is stale.
fn service_error(e: ApiError) -> Response {
    match e {
        ApiError::Status(s @ (400 | 404 | 409), d) => refuse(
            StatusCode::from_u16(s).unwrap_or(StatusCode::BAD_REQUEST),
            "taste_refused",
            if d.is_empty() {
                "the taste service refused the request".to_owned()
            } else {
                d
            },
        ),
        ApiError::Unauthorized => refuse(
            StatusCode::BAD_GATEWAY,
            "taste_refused_token",
            "the taste service refused the pod's token; sign in to it again",
        ),
        other => refuse(
            StatusCode::BAD_GATEWAY,
            "taste_unreachable",
            other.to_string(),
        ),
    }
}

/// The adapter and the service the pod is signed in to, or the refusal.
async fn signed_in(pod: &Pod) -> Result<(&TasteAdapter, Service), Response> {
    let Some(a) = pod.adapter::<TasteAdapter>() else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            "not_found",
            "no taste source",
        ));
    };
    match pod.blocking(db::service).await {
        Ok(Some(s)) => Ok((a, s)),
        Ok(None) => Err(refuse(
            StatusCode::CONFLICT,
            "not_signed_in",
            "the pod is not signed in to a taste service",
        )),
        Err(e) => Err(e.into_response()),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn sign_in_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Some(a) = pod.adapter::<TasteAdapter>() else {
        return refuse(StatusCode::NOT_FOUND, "not_found", "no taste source");
    };
    // A temporary pod (anyone can make one) has the jack locked: it would
    // make the server call a host of the visitor's choosing.
    let locked = pod
        .blocking(|s| s.with_conn(|c| pod_core::jacks::locked_on(c, JACK)))
        .await;
    if !matches!(locked, Ok(false)) {
        return refuse(
            StatusCode::FORBIDDEN,
            "jack_locked",
            "this server keeps the taste jack unplugged on this pod",
        );
    }
    let url = match field(&body, "url", 500) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let url = match api::check_url(&url, a.allow_private_hosts) {
        Ok(u) => u,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, "invalid_url", e),
    };
    let token = match field(&body, "token", 4096) {
        Ok(t) if t.chars().all(|c| c.is_ascii_graphic()) => t,
        Ok(_) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "the token is one word of printable characters",
            );
        }
        Err(r) => return r,
    };
    let service = Service { url, token };
    let taste = match api::taste(&a.client, &service).await {
        Ok(t) => t,
        Err(ApiError::Unauthorized) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "taste_refused_token",
                "the taste service refused that token",
            );
        }
        Err(e) => return service_error(e),
    };
    let n = |k: &str| taste.get(k).and_then(Value::as_i64).unwrap_or(0);
    let (likes, skips) = (n("likes"), n("skips"));
    let host = api::host_of(&service.url);
    let now = now_ms();
    if let Err(e) = pod
        .blocking(move |s| db::sign_in(s, &service, likes, skips, now))
        .await
    {
        return e.into_response();
    }
    pod.publish_data_all(NAME);
    tracing::info!(%host, "signed in to a taste service");
    Json(json!({"ok": true, "host": host})).into_response()
}

async fn sign_out_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = body_of(&pod, &headers, &body).await {
        return r;
    }
    if let Err(e) = pod.blocking(db::sign_out).await {
        return e.into_response();
    }
    pod.local::<MapCache>().forget();
    pod.publish_data_all(NAME);
    Json(json!({"ok": true})).into_response()
}

/// What a reaction body asks: the service's kind, and the item's `liked`
/// and `skipped` after it.
fn reaction(body: &Value) -> Option<(&'static str, bool, bool)> {
    if let Some(kind) = body.get("kind").and_then(Value::as_str) {
        return match kind {
            "like" => Some(("like", true, false)),
            "skip" => Some(("skip", false, true)),
            "clear" => Some(("clear", false, false)),
            _ => None,
        };
    }
    let flag = |k: &str| match body.get(k) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) if s == "true" || s == "false" => Some(s == "true"),
        _ => None,
    };
    match (flag("liked"), flag("skipped")) {
        (Some(true), _) => Some(("like", true, false)),
        (_, Some(true)) => Some(("skip", false, true)),
        (Some(false), _) | (_, Some(false)) => Some(("clear", false, false)),
        _ => None,
    }
}

async fn react_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match body_of(&pod, &headers, &body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let id = match field(&body, "id", 300) {
        Ok(id) => bundle::id_of(&id),
        Err(r) => return r,
    };
    let Some((kind, liked, skipped)) = reaction(&body) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "send {id, liked}, {id, skipped} or {id, kind: like|skip|clear}",
        );
    };
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let counts = match api::event(&a.client, &service, &id, kind).await {
        Ok(c) => Some(c),
        // The service no longer has the piece (its catalog changed under
        // the kept deck): it leaves the deck all the same, rather than
        // coming back as the device undoes a refused swipe.
        Err(ApiError::Status(404 | 410, _)) => None,
        Err(e) => return service_error(e),
    };
    let kept = id.clone();
    let (liked, skipped) = if counts.is_some() {
        (liked, skipped)
    } else {
        (false, true)
    };
    if let Err(e) = pod
        .blocking(move |s| {
            db::react(s, &kept, liked, skipped)?;
            if let Some((likes, skips)) = counts {
                db::set_counts(s, likes, skips)?;
            }
            Ok(())
        })
        .await
    {
        return e.into_response();
    }
    pod.local::<MapCache>().reacted(&id, liked, skipped);
    pod.publish_data_all(NAME);
    top_up(&pod);
    match counts {
        Some((likes, skips)) => {
            Json(json!({"ok": true, "likes": likes, "skips": skips})).into_response()
        }
        None => Json(json!({"ok": true, "gone": true})).into_response(),
    }
}

/// Keep the deck stocked: when fewer than [`LOW`] pieces are left, fetch
/// the next page in the background and tell the devices once it is kept.
fn top_up(pod: &Pod) {
    let pod = pod.clone();
    tokio::spawn(async move {
        let low = matches!(pod.blocking(db::waiting).await, Ok(w) if w.len() < LOW);
        if !low {
            return;
        }
        let Some(a) = pod.adapter::<TasteAdapter>() else {
            return;
        };
        let Ok(Some(service)) = pod.blocking(db::service).await else {
            return;
        };
        match next_page(&pod, a, &service, Fetch::TopUp).await {
            Ok(_) => pod.publish_data_all(NAME),
            Err(e) => tracing::warn!(error = %e, "topping up the taste deck failed"),
        }
    });
}

async fn page_route(pod: Pod, headers: HeaderMap, body: Bytes, fetch: Fetch) -> Response {
    if let Err(r) = body_of(&pod, &headers, &body).await {
        return r;
    }
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match next_page(&pod, a, &service, fetch).await {
        Ok(items) => {
            pod.publish_data_all(NAME);
            Json(json!({"ok": true, "items": items.len()})).into_response()
        }
        Err(e) => {
            pod.publish_data_all(NAME);
            service_error(e)
        }
    }
}

async fn more_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    page_route(pod, headers, body, Fetch::More).await
}

async fn refresh_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    page_route(pod, headers, body, Fetch::Refresh).await
}

async fn summary_route(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = body_of(&pod, &headers, &body).await {
        return r;
    }
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match api::summary(&a.client, &service).await {
        Ok(_) => {
            pod.publish_data_all(NAME);
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => service_error(e),
    }
}

// ---------- data paths ----------

#[derive(serde::Deserialize)]
struct KeyQuery {
    #[serde(default)]
    key: String,
    #[serde(default)]
    q: String,
}

/// `items` with the kept feed's reasons filled in ([`bundle::with_why`]).
async fn with_why(pod: &Pod, items: Vec<Value>) -> Vec<Value> {
    match pod.blocking(db::items).await {
        Ok(kept) => bundle::with_why(&items, &kept),
        Err(_) => items,
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn bad_key() -> Response {
    refuse(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "\"key\" names an item",
    )
}

fn words(q: &str) -> Result<String, Response> {
    let q = bundle::clean(q, 200);
    if q.is_empty() {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "\"q\" is the words to search for",
        ));
    }
    Ok(q)
}

async fn item_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<KeyQuery>,
) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    if !valid_key(&q.key) {
        return bad_key();
    }
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let id = bundle::id_of(&q.key);
    let (item, similar, contrast) = tokio::join!(
        api::item(&a.client, &service, &id),
        api::related(&a.client, &service, &id, "similar", RELATED),
        api::related(&a.client, &service, &id, "contrast", RELATED),
    );
    let item = match item {
        Ok(i) => i,
        Err(e) => return service_error(e),
    };
    let similar = with_why(&pod, similar.unwrap_or_default()).await;
    let contrast = with_why(&pod, contrast.unwrap_or_default()).await;
    let item = with_why(&pod, vec![item]).await.remove(0);
    let mut b = DataBundle::default();
    bundle::put_items(&mut b, &format!("taste:similar:{}", q.key), &similar);
    bundle::put_items(&mut b, &format!("taste:contrast:{}", q.key), &contrast);
    // The item itself last, so its own fields win over a list's copy.
    bundle::put_item(&mut b, &item);
    Json(b).into_response()
}

/// The whole map, as the pod keeps it ([`MAP_KEEP`]): asked for again once
/// it is old, or a while after a reaction moved the model's chances; the
/// likes and skips are kept current in between.
#[derive(Default)]
struct MapCache(std::sync::Mutex<Option<KeptMap>>);

struct KeptMap {
    map: Arc<api::MapData>,
    at: std::time::Instant,
    /// A reaction since: the chances are out of date.
    stale: bool,
}

impl MapCache {
    /// The kept map if it will do.
    fn fresh(&self) -> Option<Arc<api::MapData>> {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let k = g.as_ref()?;
        let age = k.at.elapsed();
        (age < MAP_KEEP && (!k.stale || age < MAP_REFRESH)).then(|| k.map.clone())
    }

    /// The kept map, however old.
    fn any(&self) -> Option<Arc<api::MapData>> {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().map(|k| k.map.clone())
    }

    fn keep(&self, map: Arc<api::MapData>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(KeptMap {
            map,
            at: std::time::Instant::now(),
            stale: false,
        });
    }

    /// A reaction: its point is marked at once, and the chances are asked
    /// for again next time.
    fn reacted(&self, id: &str, liked: bool, skipped: bool) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(k) = g.as_mut() else {
            return;
        };
        if let Some(i) = k.map.index_of(id) {
            let m = Arc::make_mut(&mut k.map);
            if liked {
                m.liked.insert(i);
            } else {
                m.liked.remove(&i);
            }
            if skipped {
                m.skipped.insert(i);
            } else {
                m.skipped.remove(&i);
            }
        }
        k.stale = true;
    }

    fn forget(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The map: kept, or asked for (falling back on an old one when the
/// service doesn't answer).
async fn the_map(
    pod: &Pod,
    a: &TasteAdapter,
    service: &Service,
) -> Result<Arc<api::MapData>, ApiError> {
    let cache = pod.local::<MapCache>();
    if let Some(m) = cache.fresh() {
        return Ok(m);
    }
    match api::map(&a.client, service).await {
        Ok(m) => {
            let m = Arc::new(m);
            cache.keep(m.clone());
            Ok(m)
        }
        Err(e) => cache.any().ok_or(e),
    }
}

async fn scatter_route(State(pod): State<Pod>, headers: HeaderMap) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let map = match the_map(&pod, a, &service).await {
        Ok(m) => m,
        Err(e) => return service_error(e),
    };
    let mut b = DataBundle::default();
    let (uri, rec) = bundle::scatter(&map);
    b.entities.insert(uri, rec);
    Json(b).into_response()
}

#[derive(serde::Deserialize)]
struct PointQuery {
    #[serde(default)]
    n: String,
}

async fn point_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<PointQuery>,
) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let Ok(n) = q.n.parse::<usize>() else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "\"n\" is a point of the map",
        );
    };
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let map = match the_map(&pod, a, &service).await {
        Ok(m) => m,
        Err(e) => return service_error(e),
    };
    let Some(id) = map.ids.get(n).cloned() else {
        return refuse(StatusCode::NOT_FOUND, "not_found", "no such point");
    };
    let (item, similar, contrast) = tokio::join!(
        api::item(&a.client, &service, &id),
        api::related(&a.client, &service, &id, "similar", RELATED),
        api::related(&a.client, &service, &id, "contrast", RELATED),
    );
    let item = match item {
        Ok(i) => i,
        Err(e) => return service_error(e),
    };
    let similar = with_why(&pod, similar.unwrap_or_default()).await;
    let contrast = with_why(&pod, contrast.unwrap_or_default()).await;
    let item = with_why(&pod, vec![item]).await.remove(0);
    let mut b = DataBundle::default();
    bundle::put_items(&mut b, &format!("taste:point-similar:{n}"), &similar);
    bundle::put_items(&mut b, &format!("taste:point-contrast:{n}"), &contrast);
    bundle::put_item(&mut b, &item);
    // The point's own copy of the piece: what its screen shows (a reaction
    // there marks this copy, and the piece's own entity follows on reload).
    if let Some((_, mut rec)) = bundle::item(&item) {
        let point = format!("taste://points/{n}");
        rec.0.insert("uri".into(), json!(point));
        b.entities.insert(point, rec);
    }
    Json(b).into_response()
}

async fn steer_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<KeyQuery>,
) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    if !valid_key(&q.key) {
        return bad_key();
    }
    let words = match words(&q.q) {
        Ok(w) => w,
        Err(r) => return r,
    };
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let id = bundle::id_of(&q.key);
    match api::steer(&a.client, &service, &id, &words, RELATED).await {
        Ok(items) => {
            let items = with_why(&pod, items).await;
            let mut b = DataBundle::default();
            bundle::put_items(&mut b, &format!("taste:steer:{}", q.key), &items);
            Json(b).into_response()
        }
        Err(e) => service_error(e),
    }
}

async fn search_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<KeyQuery>,
) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let words = match words(&q.q) {
        Ok(w) => w,
        Err(r) => return r,
    };
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match api::search(&a.client, &service, &words, RESULTS).await {
        Ok(items) => {
            let items = with_why(&pod, items).await;
            let mut b = DataBundle::default();
            bundle::search(&mut b, &words, &items);
            Json(b).into_response()
        }
        Err(e) => service_error(e),
    }
}

async fn likes_route(State(pod): State<Pod>, headers: HeaderMap) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match api::likes(&a.client, &service).await {
        Ok(items) => {
            let items = with_why(&pod, items).await;
            let mut b = DataBundle::default();
            bundle::put_items(&mut b, bundle::LIKES_QUERY, &items);
            Json(b).into_response()
        }
        Err(e) => service_error(e),
    }
}

async fn taste_route(State(pod): State<Pod>, headers: HeaderMap) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match api::taste(&a.client, &service).await {
        Ok(v) => {
            let mut b = DataBundle::default();
            bundle::tastes(&mut b, &v);
            Json(b).into_response()
        }
        Err(e) => service_error(e),
    }
}

#[derive(serde::Deserialize)]
struct AreaQuery {
    #[serde(default)]
    area: String,
}

fn bad_area() -> Response {
    refuse(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "\"area\" is x0-y0-x1-y1 in millionths of the map, e.g. 0-0-1000000-1000000",
    )
}

async fn map_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<AreaQuery>,
) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let Some(bounds) = bundle::area_of(&q.area) else {
        return bad_area();
    };
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match api::map_tiles(&a.client, &service, bounds, GRID).await {
        Ok(v) => {
            let mut b = DataBundle::default();
            bundle::map(&mut b, &q.area, &v);
            Json(b).into_response()
        }
        Err(e) => service_error(e),
    }
}

async fn area_route(
    State(pod): State<Pod>,
    headers: HeaderMap,
    Query(q): Query<AreaQuery>,
) -> Response {
    if let Err(r) = check_caller(&pod, &headers).await {
        return r;
    }
    let Some(bounds) = bundle::area_of(&q.area) else {
        return bad_area();
    };
    let (a, service) = match signed_in(&pod).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match api::map_items(&a.client, &service, bounds, RESULTS).await {
        Ok(items) => {
            let items = with_why(&pod, items).await;
            let mut b = DataBundle::default();
            bundle::put_items(&mut b, &bundle::area_query(&q.area), &items);
            Json(b).into_response()
        }
        Err(e) => service_error(e),
    }
}

/// The adapter as the server builds it.
pub fn adapter() -> Arc<dyn Source> {
    Arc::new(TasteAdapter::from_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_read_every_shape() {
        assert_eq!(
            reaction(&json!({"liked": true})),
            Some(("like", true, false))
        );
        assert_eq!(
            reaction(&json!({"liked": false})),
            Some(("clear", false, false))
        );
        assert_eq!(
            reaction(&json!({"skipped": true})),
            Some(("skip", false, true))
        );
        assert_eq!(
            reaction(&json!({"skipped": "true"})),
            Some(("skip", false, true))
        );
        assert_eq!(
            reaction(&json!({"kind": "clear"})),
            Some(("clear", false, false))
        );
        assert_eq!(reaction(&json!({"kind": "flag"})), None);
        assert_eq!(reaction(&json!({})), None);
    }

    #[test]
    fn keys_are_checked() {
        assert!(valid_key("shop_example__123"));
        assert!(!valid_key("../x"));
        assert!(!valid_key(""));
    }
}
