//! The Collections data source (decision `collections`, issue #54): data a
//! user's custom apps store in the pod itself, such as a todo list.
//!
//! Claude never writes a data source. It *declares* data over MCP: a
//! collection is a name and typed fields (`todos {title: text, done: bool,
//! due: date?}`), and this one built-in source stores and serves any
//! collection the same way. There is no stored code: no triggers or upstream
//! calls. A collection may have reminders ([`remind`], decision
//! `collection-reminders`): rules that notify when a row's date is reached.
//! A field may be computed from the row's other fields ([`compute`],
//! decision `collection-computed-fields`): by a small expression, by rules
//! kept in another collection ([`lookup`], decision `collection-lookups`),
//! or by a model's answer to a prompt about them, tried in order. A
//! collection may have a filler ([`filler`], [`fill`], decision
//! `collection-fillers`) that keeps its rows current from the web: a
//! declarative fetch recipe or an agent with web search.
//!
//! What layouts use (the same for every collection `<c>`):
//!
//! | | |
//! |---|---|
//! | feed `["collections", "<c>"]` | its rows under `collection:<c>` |
//! | `GET /v1/collections/<c>/rows` | the same, as a data path |
//! | `POST /v1/collections/<c>/create {<field>: value, client_id?}` | a new row (`action.mutation.create`; a repeated `client_id` answers the same row) |
//! | `POST /v1/collections/<c>/update {id, <field>: value, deleted?}` | change fields (`action.mutation.optimistic`) |
//! | `POST /v1/collections/<c>/delete {id}` | delete a row (soft: restorable for 30 days) |
//! | `GET /v1/collections/<c>/export` | everything, for keeping a copy |
//! | `POST /v1/collections/<c>/refresh {filler?}` | fill a filled collection now (in the background; one filler or every one) |
//! | `GET /v1/collections/<c>/fillers/<f>` | one filler's screen: `collection:<c>:filler:<f>` (itself), `…:rows`, `…:alerts` |
//! | `GET /v1/collections/<c>/row?id=<id>` | one row as a one-item list `collection:<c>:row:<id>` |
//!
//! With fillers, the feed also fills `collection:<c>:fillers` (each filler's
//! title, URL and health); with alerts, `collection:<c>:alerted` (the rows
//! an alert matches).
//!
//! Rows are entities `collections://<c>/<id>` with every field by name,
//! plus `id`, `deleted` (always `false` when served: a layout's list hides
//! a row at once with `where: {field: "deleted", equals: "false"}` while
//! the delete is on its way), `created_at` and `updated_at`.
//!
//! Writes are lenient across definition versions: a name resolves through
//! aliases (old names), a removed field's value is ignored, and only a
//! value of the wrong type is refused, so a todo typed offline before a
//! change still arrives. The MCP tools ([`mcp`]) define and change
//! collections (atomically with the apps that show them) and edit rows.

pub mod agent;
pub mod alerts;
pub mod bindings;
pub mod changes;
pub mod compute;
pub mod db;
pub mod expr;
pub mod fill;
pub mod filler;
pub mod lookup;
mod mcp;
pub mod model;
pub mod remind;

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pod_core::layouts::LayoutSource;
use pod_core::store::{Step, Store};
use pod_core::validate::LayoutError;
use pod_core::{
    AppAdapter, AppInfo, BoxFuture, EntityDoc, FeedSpec, Hydration, PathSpec, Pod, PollCtx,
    PollOutcome, RequestCtx, SourceManifest,
};
use protocol::NotificationChannel;
use serde_json::{Map, Value, json};

use crate::db::Row;
use crate::model::{Definition, Write};

/// The adapter name: the prefix of its tables and routes, and the source id.
pub const NAME: &str = "collections";

/// Removed fields, deleted rows, values kept aside and undo records are
/// kept this long, then purged.
pub const KEEP_SECS: i64 = 30 * 86_400;

/// Limits per pod (temporary pods get the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Collections.
    pub collections: usize,
    /// Live fields per collection.
    pub fields: usize,
    /// Live rows across the pod's collections.
    pub rows: usize,
    /// Bytes of the live rows' values across the pod.
    pub bytes: usize,
    /// Rows one `edit_rows` may touch.
    pub edit_rows: usize,
    /// Characters of one text value.
    pub text_chars: usize,
    /// Rows one fill may write.
    pub fill_items: usize,
    /// Pages one `url_field` fill fetches.
    pub fill_urls: usize,
    /// Agent fills per day across the pod (the operator's ceiling; one
    /// filler's `fills_per_day` is at most this).
    pub agent_fills_per_day: u32,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            collections: 20,
            fields: 40,
            rows: 5_000,
            bytes: 4 * 1024 * 1024,
            edit_rows: 500,
            text_chars: 4_000,
            fill_items: 200,
            fill_urls: 20,
            agent_fills_per_day: 20,
        }
    }
}

/// What layouts may use of Collections. The routes and query keys name the
/// collection; the capability check covers every collection, and the
/// adapter's own check ([`AppAdapter::check_custom_layout`]) that the
/// collection exists and has the fields a layout binds.
pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Collections",
    sign_in: None,
    entity_schemes: &["collections"],
    feeds: &[FeedSpec {
        name: "{collection}",
        query_keys: &[
            "collection:{collection}",
            "collection:{collection}:fillers",
            "collection:{collection}:alerted",
        ],
        doc: "The rows of one collection, in its order (list the collection's name as the feed: [\"collections\", \"todos\"]).",
    }],
    data_paths: &[
        PathSpec {
            pattern: "/v1/collections/{collection}/rows",
            query_keys: &[
                "collection:{collection}",
                "collection:{collection}:fillers",
                "collection:{collection}:alerted",
            ],
            doc: "The rows of one collection (for a screen's data).",
        },
        PathSpec {
            pattern: "/v1/collections/{collection}/fillers/{filler}",
            query_keys: &[
                "collection:{collection}:filler:{filler}",
                "collection:{collection}:filler:{filler}:rows",
                "collection:{collection}:filler:{filler}:alerts",
            ],
            doc: "One filler (a site) as a one-item list with its health, its rows and its alerts.",
        },
        PathSpec {
            pattern: "/v1/collections/{collection}/row?id={id}",
            query_keys: &["collection:{collection}:row:{id}"],
            doc: "One row as a one-item list (a row's own screen).",
        },
    ],
    actions: &[
        PathSpec {
            pattern: "/v1/collections/{collection}/create",
            query_keys: &[],
            doc: "Add a row: payload {<field>: value, client_id: \"$nonce\"} (action.mutation.create).",
        },
        PathSpec {
            pattern: "/v1/collections/{collection}/update",
            query_keys: &[],
            doc: "Change a row's fields: payload {id: \"$item.id\", <field>: \"$value\"} (action.mutation.optimistic).",
        },
        PathSpec {
            pattern: "/v1/collections/{collection}/delete",
            query_keys: &[],
            doc: "Delete a row: payload {id: \"$item.id\"}, as a mutation of field \"deleted\" to true (hide it with the List's where: {field: \"deleted\", equals: \"false\"}).",
        },
        PathSpec {
            pattern: "/v1/collections/{collection}/refresh",
            query_keys: &[],
            doc: "Fill a collection's fillers now (a \"Refresh\" button): payload {} for every filler or {filler: id} for one; the rows update when the fill ends.",
        },
    ],
    entities: &[EntityDoc {
        uri: "collections://{collection}/{id}",
        doc: "A row of a collection: every field by name (see describe_collection), and the system fields; a datetime field also as <name>_date (YYYY-MM-DD), <name>_time (7:30 PM) and <name>_text (Fri Nov 6 · 7:30 PM); with fillers filler_id and filler_title; with alerts alert_matched and alert_names. A filler is collections://{collection}/fillers/{id} (title, kind, url, state, detail, failing, alerts_text), an alert collections://{collection}/alerts/{id} (name, summary).",
        fields: &[
            (
                "id",
                "the row id (a pending create's is \"pending-<nonce>\")",
            ),
            (
                "deleted",
                "always false when served; set to true by a delete",
            ),
            ("created_at", "RFC 3339"),
            ("updated_at", "RFC 3339"),
            (
                "<field>",
                "each field of the collection, by its name (and old names)",
            ),
        ],
    }],
    dev: false,
};

/// The Collections adapter: no apps of its own; custom apps use it.
#[derive(Debug)]
pub struct CollectionsAdapter {
    caps: Caps,
    /// What answers `model`-computed fields (none: they keep their
    /// default).
    model: Option<Arc<dyn compute::FieldModel>>,
    /// What answers `agent` fillers (none: they report themselves
    /// unavailable).
    agent: Option<Arc<dyn agent::FillAgent>>,
    /// Fetch fillers may read private hosts (tests; the server sets it from
    /// `CONSTRUCT_FILL_PRIVATE_HOSTS=1`).
    allow_private_hosts: bool,
    fetch_client: reqwest::Client,
    /// For fetches that carry a secret: redirects stay on the same host,
    /// since a custom header (unlike `Authorization`) would follow them.
    secret_fetch_client: reqwest::Client,
}

impl Default for CollectionsAdapter {
    fn default() -> Self {
        Self {
            caps: Caps::default(),
            model: None,
            agent: None,
            allow_private_hosts: false,
            fetch_client: fetch_client(false, false),
            secret_fetch_client: fetch_client(false, true),
        }
    }
}

/// The HTTP client of fetch fillers: public hosts only unless allowed.
/// `same_host`: follow redirects only on the first request's host (for
/// requests that carry a secret header).
fn fetch_client(allow_private: bool, same_host: bool) -> reqwest::Client {
    let b = if allow_private {
        pod_core::net::builder(web_extract::fetch::FETCH_TIMEOUT)
    } else {
        pod_core::net::public_builder(web_extract::fetch::FETCH_TIMEOUT)
    };
    let b = if same_host {
        b.redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let first = attempt.previous().first().and_then(|u| u.host_str());
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if first != attempt.url().host_str() {
                tracing::warn!(
                    "a redirect to another host was not followed: the request carries a secret"
                );
                attempt.stop()
            } else if allow_private || pod_core::net::is_public_https(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
    } else {
        b
    };
    b.build().expect("an HTTP client")
}

impl CollectionsAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// With `agent` answering `agent` fillers
    /// ([`agent::MessagesAgent::from_env`] in the server; a fake in tests).
    pub fn with_agent(mut self, agent: Option<Arc<dyn agent::FillAgent>>) -> Self {
        self.agent = agent;
        self
    }

    /// Let fetch fillers read private hosts (tests, or the operator's
    /// `CONSTRUCT_FILL_PRIVATE_HOSTS=1`).
    pub fn with_private_hosts(mut self, allow: bool) -> Self {
        self.allow_private_hosts = allow;
        self.fetch_client = fetch_client(allow, false);
        self.secret_fetch_client = fetch_client(allow, true);
        self
    }

    pub fn allow_private_hosts(&self) -> bool {
        self.allow_private_hosts
    }

    pub fn fetch_client(&self) -> &reqwest::Client {
        &self.fetch_client
    }

    /// The client for a request that carries a secret header: redirects
    /// stay on the host the secret is bound to.
    pub fn secret_fetch_client(&self) -> &reqwest::Client {
        &self.secret_fetch_client
    }

    pub fn agent(&self) -> Option<&Arc<dyn agent::FillAgent>> {
        self.agent.as_ref()
    }

    /// With other limits (tests).
    pub fn with_caps(caps: Caps) -> Self {
        Self {
            caps,
            ..Self::default()
        }
    }

    /// With `model` answering `model`-computed fields
    /// ([`compute::MessagesModel::from_env`] in the server; a fake in tests).
    pub fn with_model(mut self, model: Option<Arc<dyn compute::FieldModel>>) -> Self {
        self.model = model;
        self
    }

    pub fn caps(&self) -> Caps {
        self.caps
    }
}

/// The adapter of `pod` (it is always there when its routes run).
fn adapter(pod: &Pod) -> &CollectionsAdapter {
    pod.adapter::<CollectionsAdapter>()
        .expect("the pod has the Collections adapter")
}

impl AppAdapter for CollectionsAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn apps(&self) -> Vec<AppInfo> {
        Vec::new()
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        &[]
    }

    fn router(&self) -> Router<Pod> {
        Router::new()
            .route("/v1/collections/{collection}/rows", get(rows_route))
            .route("/v1/collections/{collection}/create", post(create_route))
            .route("/v1/collections/{collection}/update", post(update_route))
            .route("/v1/collections/{collection}/delete", post(delete_route))
            .route("/v1/collections/{collection}/export", get(export_route))
            .route("/v1/collections/{collection}/refresh", post(refresh_route))
            .route(
                "/v1/collections/{collection}/fillers/{filler}",
                get(filler_route),
            )
            .route("/v1/collections/{collection}/row", get(row_screen_route))
    }

    fn migrations(&self) -> &'static [Step] {
        db::MIGRATIONS
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            let name = feed.to_owned();
            let now = pod_core::util::now_secs();
            let found = pod
                .blocking(move |s| {
                    s.with_conn(|c| match db::latest_def(c, &name)? {
                        Some((_, def)) => {
                            // Opening an app fills a stale collection.
                            let due = fill::due_on_open(&def, &fill::states(c, &def)?, now);
                            Ok(Some((db::bundle(c, &def)?, due)))
                        }
                        None => Ok(None),
                    })
                })
                .await;
            match found {
                Ok(Some((bundle, due))) => {
                    for f in due {
                        fill::spawn(pod, feed, Some(&f), false);
                    }
                    Hydration::Data(bundle)
                }
                Ok(None) => Hydration::Error(not_found(feed)),
                Err(e) => Hydration::Error(e.into_response()),
            }
        })
    }

    fn resume<'a>(&'a self, pod: &'a Pod) -> BoxFuture<'a, ()> {
        // The hourly purge, holding the pod only while it runs (a host
        // drops temporary pods).
        let weak = Arc::downgrade(pod);
        tokio::spawn(async move {
            loop {
                let Some(pod) = weak.upgrade() else { return };
                let purged = pod
                    .blocking(|s| {
                        s.transaction(|tx| db::purge(tx, pod_core::util::now_secs(), KEEP_SECS))
                    })
                    .await;
                if let Ok(p) = purged
                    && p != db::Purged::default()
                {
                    tracing::info!(?p, "old collection data purged");
                }
                drop(pod);
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
        Box::pin(async {})
    }

    fn notifies_custom_apps(&self) -> bool {
        true
    }

    fn custom_notification_channels(&self, store: &Store) -> Vec<NotificationChannel> {
        let mut out = remind::channels(store);
        out.extend(alerts::channels(store));
        out
    }

    fn poll_interval(&self) -> Option<std::time::Duration> {
        Some(remind::MAX_WAIT)
    }

    fn poll_notifications<'a>(
        &'a self,
        pod: &'a Pod,
        _ctx: &'a PollCtx,
    ) -> BoxFuture<'a, PollOutcome> {
        Box::pin(async move {
            let next = remind::run(pod, pod_core::util::now_secs()).await;
            // Scheduled fills run in the background (an agent can take
            // longer than the poll may).
            let p = pod.clone();
            tokio::spawn(async move { fill::run_due(&p).await });
            let scheduled = pod
                .blocking(|s| s.with_conn(fill::any_scheduled))
                .await
                .unwrap_or(false);
            let next = if scheduled {
                next.min(remind::MAX_WAIT)
            } else {
                next
            };
            PollOutcome {
                next_in: Some(next),
                ..Default::default()
            }
        })
    }

    fn check_custom_layout(
        &self,
        pod: &Pod,
        feeds: &[String],
        layout: &Value,
    ) -> Result<(), LayoutError> {
        let defs = pod
            .store()
            .with_conn(db::all_defs)
            .map_err(|e| LayoutError {
                pointer: String::new(),
                message: format!("collections could not be read: {e}"),
                allowed: Vec::new(),
            })?;
        let defs: HashMap<String, Definition> =
            defs.into_iter().map(|(_, d)| (d.name.clone(), d)).collect();
        bindings::check(layout, feeds, &defs)
    }

    fn mcp_tools(&self) -> Vec<Value> {
        mcp::tools()
    }

    fn mcp_instructions(&self) -> Option<&'static str> {
        Some(mcp::INSTRUCTIONS)
    }

    fn mcp_call<'a>(
        &'a self,
        pod: &'a Pod,
        tool: &'a str,
        args: &'a Value,
    ) -> Option<BoxFuture<'a, Result<Value, Value>>> {
        mcp::call(self, pod, tool, args)
    }
}

// ---------- errors ----------

fn not_found(collection: &str) -> Response {
    pod_core::json_error(
        StatusCode::NOT_FOUND,
        json!({ "error": "collection_not_found", "collection": collection }),
    )
}

/// A refusal as an HTTP answer (`{"error": ...}`; status inside or 400).
fn refusal(body: Value) -> Response {
    let status = body["status"]
        .as_u64()
        .and_then(|s| StatusCode::from_u16(s as u16).ok())
        .unwrap_or(StatusCode::BAD_REQUEST);
    let mut body = body;
    if let Some(o) = body.as_object_mut() {
        o.remove("status");
    }
    pod_core::json_error(status, body)
}

pub(crate) fn fail(status: StatusCode, error: &str, detail: impl Into<String>) -> Value {
    json!({ "error": error, "detail": detail.into(), "status": status.as_u16() })
}

/// Run `f` in one transaction; `Ok(Err(body))` rolls it back and answers
/// the refusal.
pub(crate) async fn refusable<T: Send + 'static>(
    pod: &Pod,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<Result<T, Value>> + Send + 'static,
) -> Result<T, Value> {
    let slot: Arc<std::sync::Mutex<Option<Value>>> = Default::default();
    let refused = slot.clone();
    let r = pod
        .blocking(move |s| {
            s.transaction(|tx| match f(tx)? {
                Ok(v) => Ok(v),
                Err(body) => {
                    *refused.lock().unwrap_or_else(|e| e.into_inner()) = Some(body);
                    Err(rusqlite::Error::QueryReturnedNoRows)
                }
            })
        })
        .await;
    if let Some(body) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
        return Err(body);
    }
    r.map_err(|e| {
        let (status, Json(mut body)) = e;
        body["status"] = json!(status.as_u16());
        body
    })
}

// ---------- rows ----------

/// Check the caps on live rows and bytes for adding `rows` rows and
/// `bytes` bytes.
pub(crate) fn check_usage(
    tx: &rusqlite::Connection,
    caps: Caps,
    rows: i64,
    bytes: i64,
) -> rusqlite::Result<Result<(), Value>> {
    let (n, b) = db::usage(tx)?;
    if rows > 0 && n + rows > caps.rows as i64 {
        return Ok(Err(fail(
            StatusCode::CONFLICT,
            "too_many_rows",
            format!(
                "a pod holds at most {} rows across its collections",
                caps.rows
            ),
        )));
    }
    if bytes > 0 && b + bytes > caps.bytes as i64 {
        return Ok(Err(fail(
            StatusCode::CONFLICT,
            "collections_full",
            format!("a pod's collections hold at most {} bytes", caps.bytes),
        )));
    }
    Ok(Ok(()))
}

fn bytes_of(values: &Map<String, Value>) -> i64 {
    Value::Object(values.clone()).to_string().len() as i64
}

/// The row `id` names: a row id, or a pending create's
/// `pending-<client id>` (an update sent right after its create).
pub(crate) fn find_row(
    tx: &rusqlite::Connection,
    collection: &str,
    id: &str,
) -> rusqlite::Result<Option<Row>> {
    if let Some(r) = db::row(tx, collection, id)? {
        return Ok(Some(r));
    }
    match id.strip_prefix("pending-") {
        Some(client) => db::row_by_client(tx, collection, client),
        None => Ok(None),
    }
}

/// What a write route does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowOp {
    Create,
    Update,
    Delete,
}

/// Create, update or delete a row from a layout's write `body`.
async fn write_row(pod: &Pod, collection: &str, op: RowOp, body: Value) -> Result<Value, Value> {
    let Some(body) = body.as_object().cloned() else {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the body is a JSON object",
        ));
    };
    let caps = adapter(pod).caps;
    let name = collection.to_owned();
    let out = refusable(pod, move |tx| {
        let Some((_, def)) = db::latest_def(tx, &name)? else {
            return Ok(Err(
                json!({ "error": "collection_not_found", "collection": name, "status": 404 }),
            ));
        };
        let now = pod_core::util::now_secs();
        let str_of = |k: &str| {
            body.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        if op == RowOp::Create
            && let Err(e) = filler::check_write(&def, &body, "create", None)
        {
            return Ok(Err(fail(StatusCode::CONFLICT, "filled_by_filler", e)));
        }
        if op == RowOp::Create {
            let client_id = str_of("client_id").map(str::to_owned);
            if let Some(c) = &client_id {
                if c.len() > 128 {
                    return Ok(Err(fail(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "\"client_id\" is at most 128 characters",
                    )));
                }
                if let Some(r) = db::row_by_client(tx, &name, c)? {
                    // The outbox retried: the same row.
                    return Ok(Ok((
                        json!({ "ok": true, "id": r.id, "uri": db::row_uri(&name, &r.id) }),
                        Vec::new(),
                    )));
                }
            }
            let mut values = Map::new();
            if let Err(e) =
                model::apply_write(&def, &mut values, &body, Write::Create, caps.text_chars)
            {
                return Ok(Err(fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_value",
                    e,
                )));
            }
            let mut row = Row {
                id: db::new_id("r"),
                client_id,
                values,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                filler: None,
            };
            let jobs = compute::after_write(tx, &def, None, &mut row, &body, now, caps)?;
            if let Err(e) = check_usage(tx, caps, 1, bytes_of(&row.values))? {
                return Ok(Err(e));
            }
            db::insert_row(tx, &name, &row)?;
            return Ok(Ok((
                json!({ "ok": true, "id": row.id, "uri": db::row_uri(&name, &row.id) }),
                jobs,
            )));
        }
        let Some(id) = str_of("id") else {
            return Ok(Err(fail(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "\"id\" names the row",
            )));
        };
        let Some(mut row) = find_row(tx, &name, id)? else {
            return Ok(Err(
                json!({ "error": "row_not_found", "collection": name, "id": id, "status": 404 }),
            ));
        };
        let op_name = match op {
            RowOp::Update if body.get("deleted") == Some(&json!(false)) => "restore",
            RowOp::Update if body.get("deleted") == Some(&json!(true)) => "delete",
            RowOp::Update => "update",
            _ => "delete",
        };
        if let Err(e) = filler::check_write(&def, &body, op_name, row.filler.as_deref()) {
            return Ok(Err(fail(StatusCode::CONFLICT, "filled_by_filler", e)));
        }
        if op == RowOp::Delete {
            if row.deleted_at.is_none() {
                row.deleted_at = Some(now);
                row.updated_at = now;
                db::save_row(tx, &name, &row)?;
            }
            return Ok(Ok((json!({ "ok": true, "id": row.id }), Vec::new())));
        }
        if row.deleted_at.is_some() && body.get("deleted") != Some(&json!(false)) {
            return Ok(Err(
                json!({ "error": "row_deleted", "collection": name, "id": row.id, "status": 404 }),
            ));
        }
        let before = bytes_of(&row.values);
        let before_values = row.values.clone();
        if let Err(e) =
            model::apply_write(&def, &mut row.values, &body, Write::Update, caps.text_chars)
        {
            return Ok(Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_value",
                e,
            )));
        }
        let jobs =
            compute::after_write(tx, &def, Some(&before_values), &mut row, &body, now, caps)?;
        match body.get("deleted") {
            Some(Value::Bool(true)) => row.deleted_at = row.deleted_at.or(Some(now)),
            Some(Value::Bool(false)) => {
                if row.deleted_at.is_some()
                    && let Err(e) = check_usage(tx, caps, 1, bytes_of(&row.values))?
                {
                    return Ok(Err(e));
                }
                row.deleted_at = None;
            }
            _ => {}
        }
        if let Err(e) = check_usage(tx, caps, 0, bytes_of(&row.values) - before)? {
            return Ok(Err(e));
        }
        row.updated_at = now;
        db::save_row(tx, &name, &row)?;
        Ok(Ok((json!({ "ok": true, "id": row.id }), jobs)))
    })
    .await?;
    pod.publish_data_all(NAME);
    let (out, jobs) = out;
    compute::spawn(pod, collection, jobs);
    Ok(out)
}

async fn row_route(
    pod: Pod,
    collection: String,
    op: RowOp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = match pod_core::parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    match write_row(&pod, &collection, op, body).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => refusal(e),
    }
}

async fn create_route(
    State(pod): State<Pod>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    row_route(pod, collection, RowOp::Create, headers, body).await
}

async fn update_route(
    State(pod): State<Pod>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    row_route(pod, collection, RowOp::Update, headers, body).await
}

async fn delete_route(
    State(pod): State<Pod>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    row_route(pod, collection, RowOp::Delete, headers, body).await
}

async fn refresh_route(
    State(pod): State<Pod>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = if body.is_empty() {
        json!({})
    } else {
        match pod_core::parse_json_body(&headers, &body) {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        }
    };
    let only = body["filler"].as_str().map(str::to_owned);
    let name = collection.clone();
    let found = pod
        .blocking(move |s| s.with_conn(|c| db::latest_def(c, &name)))
        .await;
    match found {
        Ok(Some((_, def))) if !def.fillers.is_empty() => {
            if let Some(f) = &only
                && filler::find(&def, f).is_none()
            {
                return refusal(fail(
                    StatusCode::NOT_FOUND,
                    "filler_not_found",
                    format!("collection '{collection}' has no filler '{f}'"),
                ));
            }
            fill::spawn(&pod, &collection, only.as_deref(), true);
            Json(json!({ "ok": true, "refreshing": true })).into_response()
        }
        Ok(Some(_)) => refusal(fail(
            StatusCode::CONFLICT,
            "no_filler",
            format!("collection '{collection}' has no filler to refresh"),
        )),
        Ok(None) => not_found(&collection),
        Err(e) => e.into_response(),
    }
}

async fn filler_route(
    State(pod): State<Pod>,
    Path((collection, filler)): Path<(String, String)>,
) -> Response {
    let (name, f) = (collection.clone(), filler.clone());
    let found = pod
        .blocking(move |s| {
            s.with_conn(|c| match db::latest_def(c, &name)? {
                Some((_, def)) => db::filler_bundle(c, &def, &f).map(Some),
                None => Ok(None),
            })
        })
        .await;
    match found {
        Ok(Some(Some(b))) => Json(b).into_response(),
        Ok(Some(None)) => pod_core::json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "filler_not_found", "collection": collection, "filler": filler }),
        ),
        Ok(None) => not_found(&collection),
        Err(e) => e.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct IdQuery {
    id: String,
}

async fn row_screen_route(
    State(pod): State<Pod>,
    Path(collection): Path<String>,
    axum::extract::Query(q): axum::extract::Query<IdQuery>,
) -> Response {
    let (name, id) = (collection.clone(), q.id.clone());
    let found = pod
        .blocking(move |s| {
            s.with_conn(|c| match db::latest_def(c, &name)? {
                Some((_, def)) => db::row_bundle(c, &def, &id).map(Some),
                None => Ok(None),
            })
        })
        .await;
    match found {
        Ok(Some(Some(b))) => Json(b).into_response(),
        Ok(Some(None)) => pod_core::json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "row_not_found", "collection": collection, "id": q.id }),
        ),
        Ok(None) => not_found(&collection),
        Err(e) => e.into_response(),
    }
}

async fn rows_route(State(pod): State<Pod>, Path(collection): Path<String>) -> Response {
    let name = collection.clone();
    let found = pod
        .blocking(move |s| {
            s.with_conn(|c| match db::latest_def(c, &name)? {
                Some((_, def)) => db::bundle(c, &def).map(Some),
                None => Ok(None),
            })
        })
        .await;
    match found {
        Ok(Some(b)) => Json(b).into_response(),
        Ok(None) => not_found(&collection),
        Err(e) => e.into_response(),
    }
}

/// A collection's whole data: its definition, every row (deleted ones and
/// the values of removed fields too) and the values kept aside.
pub fn export(
    conn: &rusqlite::Connection,
    name: &str,
    now: i64,
) -> rusqlite::Result<Option<Value>> {
    let Some((version, def)) = db::latest_def(conn, name)? else {
        return Ok(None);
    };
    let removed: Vec<&model::Field> = def
        .fields
        .iter()
        .filter(|f| !f.live() && !f.purged)
        .collect();
    let rows: Vec<Value> = db::all_rows(conn, name, true)?
        .into_iter()
        .map(|r| {
            let mut v = json!({
                "id": r.id,
                "values": model::by_name(&def, &r.values),
                "created_at": pod_core::layouts::rfc3339(r.created_at),
                "updated_at": pod_core::layouts::rfc3339(r.updated_at),
            });
            if let Some(d) = r.deleted_at {
                v["deleted_at"] = json!(pod_core::layouts::rfc3339(d));
            }
            let gone: Map<String, Value> = removed
                .iter()
                .filter_map(|f| r.values.get(&f.id).map(|x| (f.name.clone(), x.clone())))
                .collect();
            if !gone.is_empty() {
                v["removed_fields"] = Value::Object(gone);
            }
            v
        })
        .collect();
    let mut stmt = conn.prepare_cached(
        "SELECT row_id, field_id, value_json FROM collections_aside WHERE collection = ?1",
    )?;
    let aside: Vec<Value> = stmt
        .query_map([name], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|(row, field, v)| {
            json!({ "id": row,
                    "field": def.by_id(&field).map_or(field.clone(), |f| f.name.clone()),
                    "value": serde_json::from_str::<Value>(&v).unwrap_or(Value::Null) })
        })
        .collect();
    Ok(Some(json!({
        "collection": name,
        "version": version,
        "exported_at": pod_core::layouts::rfc3339(now),
        "definition": def.describe(now, KEEP_SECS, &db::defs_by_name(conn)?),
        "rows": rows,
        "kept_aside": aside,
    })))
}

async fn export_route(State(pod): State<Pod>, Path(collection): Path<String>) -> Response {
    let name = collection.clone();
    match pod
        .blocking(move |s| s.with_conn(|c| export(c, &name, pod_core::util::now_secs())))
        .await
    {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => not_found(&collection),
        Err(e) => e.into_response(),
    }
}
