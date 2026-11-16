//! The construct's HTTP surface (decision `content-addressed-construct`).
//!
//! | Call | Answer |
//! |---|---|
//! | `GET /v1/construct` | `{root, generation}`: home's element for the client's features; `409 not_switched` on a pod that isn't switched |
//! | `POST /v1/objects {have, want}` | `{objects: {id: object}}`: what `want` reaches that `have` doesn't list, `want`'s order first |
//! | `GET /v1/objects/{id}` | one object |
//! | `GET /v1/construct/data/{id}?bind=` | `{entities, queries}`: the home feeds element `id` needs, with a use site's bindings (a JSON object) |
//! | `GET /v1/construct/names/{name}` | `{name, el}`: a layout's element for the client's features, by name or key (a notification's layout the root doesn't reach) |
//!
//! They answer as `GET /v1/workspaces/<id>` does: a request that reaches the
//! pod is the pod's (a hosted pod is reached only with its token), and a
//! bearer the pod no longer knows is told so.

use std::collections::{BTreeSet, VecDeque};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Map, Value, json};

use super::store as cstore;
use crate::session::{Caller, unauthorized};
use crate::{Pod, json_error, parse_json_body};

/// Most objects one answer carries.
pub const MAX_OBJECTS: usize = 2000;
/// Most bytes of objects one answer carries (the rest: ask again).
pub const MAX_OBJECT_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn router() -> Router<Pod> {
    Router::new()
        .route("/v1/construct", get(get_construct))
        .route("/v1/objects", post(post_objects))
        .route("/v1/objects/{id}", get(get_object))
        .route("/v1/construct/data/{id}", get(crate::get_element_data))
        .route("/v1/construct/names/{name}", get(get_name))
}

/// `None` when the caller may read the construct, else the answer.
pub(crate) async fn guard(pod: &Pod, headers: &HeaderMap) -> Option<Response> {
    match pod.caller(headers).await {
        Ok(Caller::Stale) => Some(unauthorized()),
        Ok(_) => {
            let p = pod.clone();
            let on = tokio::task::spawn_blocking(move || super::is_switched(&p))
                .await
                .unwrap_or(false);
            (!on).then(|| {
                json_error(
                    StatusCode::CONFLICT,
                    json!({ "error": "not_switched",
                            "detail": "this pod is on the layout system: use /v1/workspaces" }),
                )
            })
        }
        Err(e) => Some(e.into_response()),
    }
}

async fn get_construct(State(pod): State<Pod>, headers: HeaderMap) -> Response {
    if let Some(r) = guard(&pod, &headers).await {
        return r;
    }
    let client = crate::layouts::client_caps(&headers);
    let p = pod.clone();
    let r = tokio::task::spawn_blocking(move || {
        let root = super::seal::root_for(&p, &client)?;
        let g = p.store().with_conn(cstore::generation)?;
        Ok::<_, rusqlite::Error>((root, g))
    })
    .await;
    match r {
        Ok(Ok((Some(root), g))) => Json(json!({ "root": root, "generation": g })).into_response(),
        Ok(Ok((None, _))) => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "no_home", "detail": "the construct has no home" }),
        ),
        _ => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "store_error" }),
        ),
    }
}

/// `GET /v1/construct/names/{name}`: the element a layout's name (or a
/// blueprint's key, `events` for `@core/events`) points to, sealed for the
/// client. Only layouts and blueprints: a component is reached through what
/// uses it.
async fn get_name(
    State(pod): State<Pod>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = guard(&pod, &headers).await {
        return r;
    }
    let client = crate::layouts::client_caps(&headers);
    let p = pod.clone();
    let found = tokio::task::spawn_blocking(move || {
        let names = if super::is_layout_name(&name) {
            vec![name.clone()]
        } else if p.catalog().is_app(&name) {
            vec![super::blueprint_name(&name)]
        } else {
            Vec::new()
        };
        for n in names {
            let el = p
                .store()
                .with_conn(|c| super::seal::seal_name(c, &n, &client, &mut Default::default()))?;
            if let Some(el) = el {
                return Ok::<_, rusqlite::Error>(Some((name, el)));
            }
        }
        Ok(None)
    })
    .await;
    match found {
        Ok(Ok(Some((name, el)))) => Json(json!({ "name": name, "el": el })).into_response(),
        Ok(Ok(None)) => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "unknown_name", "detail": "no layout has that name" }),
        ),
        _ => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "store_error" }),
        ),
    }
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| construct_elements::is_object_id(s))
        .map(str::to_owned)
        .take(10_000)
        .collect()
}

async fn post_objects(State(pod): State<Pod>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(r) = guard(&pod, &headers).await {
        return r;
    }
    let body = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let have: BTreeSet<String> = ids(&body["have"]).into_iter().collect();
    let want = ids(&body["want"]);
    let p = pod.clone();
    let r = tokio::task::spawn_blocking(move || {
        let objects = super::seal::objects(&p);
        let mut out = Map::new();
        let mut bytes = 0usize;
        let mut more = false;
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<String> = want.into_iter().collect();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some(v) = construct_elements::Objects::object(&objects, &id) else {
                continue;
            };
            construct_elements::refs::visit(&v["node"], &mut |r, _| {
                if let Some(e) = &r.el
                    && !seen.contains(e)
                {
                    queue.push_back(e.clone());
                }
            });
            if have.contains(&id) {
                continue;
            }
            let size = construct_elements::canonical(&v).len();
            if out.len() >= MAX_OBJECTS || bytes + size > MAX_OBJECT_BYTES {
                more = true;
                break;
            }
            bytes += size;
            out.insert(id, (*v).clone());
        }
        json!({ "objects": out, "more": more })
    })
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "internal" }),
        ),
    }
}

async fn get_object(
    State(pod): State<Pod>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = guard(&pod, &headers).await {
        return r;
    }
    if !construct_elements::is_object_id(&id) {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_object_id" }),
        );
    }
    let p = pod.clone();
    let found = tokio::task::spawn_blocking(move || {
        construct_elements::Objects::object(&super::seal::objects(&p), &id)
    })
    .await
    .ok()
    .flatten();
    match found {
        Some(v) => Json((*v).clone()).into_response(),
        None => json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "object_not_found" }),
        ),
    }
}

/// What the old endpoint `GET /v1/workspaces/<id>` serves for one of a
/// switched pod's layouts: its history's `seq` as the version, the element
/// sealed for the client and materialized, and the home feeds it needs.
pub struct Served {
    pub version: i64,
    pub layout: protocol::UiNode,
    pub feeds: Vec<(String, String)>,
}

/// Layout `id` of a switched pod as an installed client gets it. Blocking.
pub fn serve_layout(
    pod: &Pod,
    id: &str,
    client: &protocol::ClientCaps,
) -> rusqlite::Result<Option<Served>> {
    let store = pod.store();
    let Some(el) =
        store.with_conn(|c| super::seal::seal_name(c, id, client, &mut Default::default()))?
    else {
        return Ok(None);
    };
    let version = store.with_conn(|c| cstore::last_seq(c, id))?;
    // A layout that reads inputs draws what the first use site reaching it
    // from home binds, as the engine draws it.
    let objects = super::seal::objects(pod);
    let reads = construct_elements::Objects::object(&objects, &el)
        .is_some_and(|v| !construct_elements::template::inputs_of(&v).is_empty());
    let root = if reads {
        super::seal::root_for(pod, client)?
    } else {
        None
    };
    let inputs = root
        .and_then(|root| {
            construct_elements::keyed(&objects, crate::HOME_APP, &root)
                .remove(id)
                .map(|k| k.inputs)
        })
        .unwrap_or_default();
    let m = if inputs.is_empty() {
        super::seal::materialize(pod, &el)
    } else {
        construct_elements::materialize_with(&objects, &el, &inputs).ok()
    };
    let Some(m) = m else {
        return Ok(None);
    };
    let Ok(layout) = serde_json::from_value::<protocol::UiNode>(m.node.clone()) else {
        return Ok(None);
    };
    let feeds = bound_element_feeds(pod, &el, &inputs);
    Ok(Some(Served {
        version,
        layout,
        feeds,
    }))
}

/// The home feeds element `id` needs (its `needs.feeds`).
pub fn element_feeds(pod: &Pod, id: &str) -> Vec<(String, String)> {
    construct_elements::Objects::object(&super::seal::objects(pod), id)
        .and_then(|v| serde_json::from_value(v["needs"]["feeds"].clone()).ok())
        .unwrap_or_default()
}

/// The home feeds element `id` needs drawn with `inputs` bound (a use
/// site's `bind`): its own `needs.feeds` and those its bound tree binds.
/// Only home's feeds are ever loaded: a binding reaches no other data.
pub fn bound_element_feeds(
    pod: &Pod,
    id: &str,
    inputs: &Map<String, Value>,
) -> Vec<(String, String)> {
    let mut feeds = element_feeds(pod, id);
    if inputs.is_empty() {
        return feeds;
    }
    let objects = super::seal::objects(pod);
    let Ok(m) = construct_elements::materialize_with(&objects, id, inputs) else {
        return feeds;
    };
    let home = pod
        .store()
        .with_conn(|c| Ok::<_, rusqlite::Error>(super::write::home_feeds(c)))
        .unwrap_or_default();
    for f in crate::custom::bound_feeds(pod.catalog(), &home, &m.node) {
        if !feeds.contains(&f) {
            feeds.push(f);
        }
    }
    feeds
}

/// The inputs a request binds (`?bind=<JSON object>`); none when absent or
/// not an object.
pub fn inputs_param(raw: Option<&str>) -> Map<String, Value> {
    raw.and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The `/v1/apps` rows of a switched pod: its layouts, by name.
pub fn app_rows(pod: &Pod) -> rusqlite::Result<Vec<Value>> {
    pod.store().with_conn(|c| {
        let mut rows = Vec::new();
        for (n, h) in cstore::names(c)? {
            if !super::is_layout_name(&n) {
                continue;
            }
            let el = cstore::object(c, &h)?.map(|(v, _)| v);
            let hist = cstore::history(c, &n)?;
            rows.push(json!({
                "id": n,
                "title": el.as_ref().map(|e| e["title"].clone()),
                "icon": el.as_ref().map(|e| e["icon"].clone()),
                "kind": "custom",
                "hidden": false,
                "latest_version": hist.first().map(|e| e.seq),
                "latest_compatible_version": hist.first().map(|e| e.seq),
                "updated_at": hist.first().map(|e| crate::layouts::rfc3339(e.at)),
            }));
        }
        rows.sort_by_key(|r| r["id"] != json!(crate::HOME_APP));
        Ok(rows)
    })
}

/// `GET /v1/apps/:id/versions` of a switched pod's layout: its history.
pub fn versions(pod: &Pod, id: &str) -> rusqlite::Result<Option<Value>> {
    pod.store().with_conn(|c| {
        let hist = cstore::history(c, id)?;
        if hist.is_empty() {
            return Ok(None);
        }
        let latest = hist.first().map(|e| e.seq);
        let rows: Vec<Value> = hist
            .iter()
            .filter(|e| !e.hash.is_empty())
            .take(crate::layouts::MAX_VERSIONS_LIMIT as usize)
            .map(|e| {
                let mut v = json!({ "version": e.seq, "created_at": crate::layouts::rfc3339(e.at),
                    "source": "construct", "author": e.author, "compatible": true });
                if let Some(n) = &e.note {
                    v["note"] = json!(n);
                }
                v
            })
            .collect();
        Ok(Some(json!({ "app_id": id, "latest_version": latest,
            "latest_compatible_version": latest, "versions": rows })))
    })
}

/// One history entry of a switched pod's layout, materialized.
pub fn version(pod: &Pod, id: &str, n: i64) -> rusqlite::Result<Option<Value>> {
    let entry = pod.store().with_conn(|c| {
        Ok::<_, rusqlite::Error>(
            cstore::history(c, id)?
                .into_iter()
                .find(|e| e.seq == n && !e.hash.is_empty()),
        )
    })?;
    let Some(e) = entry else { return Ok(None) };
    let layout = super::seal::materialize(pod, &e.hash).map_or(Value::Null, |m| m.node);
    Ok(Some(
        json!({ "app_id": id, "version": e.seq, "created_at": crate::layouts::rfc3339(e.at),
        "source": "construct", "author": e.author, "note": e.note, "compatible": true, "layout": layout }),
    ))
}
