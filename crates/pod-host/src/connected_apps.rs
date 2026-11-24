//! Settings' Connected apps (decision `connected-apps`): the pod's MCP
//! credentials, which live in `host.db`, so the host answers these two
//! paths itself for the pod's own sessions (pod code, a browser it opened,
//! or the open pod) before anything goes to the pod:
//!
//! - `GET /v1/connected-apps/screen[?id=]`: every working MCP credential as
//!   an entity `connected_apps://<id>` and the `connected_apps` query (with
//!   `id`, that one only), as Settings' screens bind them.
//! - `POST /v1/connected-apps/revoke {id}`: revoke that one credential; the
//!   app stops working at once.
//! - `POST /v1/connected-apps/access {id, access}`: change what it may do,
//!   `read`, `edit` or `outward` (decision `mcp-access-levels`); its next
//!   call uses the new level.
//!
//! An MCP credential never gets here: the host refuses it on every path but
//! `/v1/mcp`.

use std::sync::Arc;

use axum::{
    Json,
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use pod_core::json_error;
use pod_core::util::now_secs;
use serde_json::{Map, Value, json};

use crate::Host;
use crate::db::{ConnectedApp, MCP_IDLE_SECS};
use crate::oauth::{access_summary, access_title};
use pod_core::mcp::Access;

/// The data path of Settings' Connected apps screens.
pub const SCREEN_PATH: &str = "/v1/connected-apps/screen";
/// The action that revokes one credential.
pub const REVOKE_PATH: &str = "/v1/connected-apps/revoke";
/// The action that sets one credential's access.
pub const ACCESS_PATH: &str = "/v1/connected-apps/access";
/// The query key of the list.
pub const SCREEN_QUERY: &str = "connected_apps";

/// The entity URI of connected app `id`.
pub fn entity_uri(id: &str) -> String {
    format!("connected_apps://{id}")
}

/// "today", "yesterday", "N days ago" for a time `secs` before now.
fn ago(secs: i64) -> String {
    match secs.max(0) / 86_400 {
        0 => "today".into(),
        1 => "yesterday".into(),
        d => format!("{d} days ago"),
    }
}

/// One credential as Settings draws it.
pub fn entity(a: &ConnectedApp, now: i64) -> Value {
    let to = a
        .redirect_host
        .clone()
        .unwrap_or_else(|| "made on the server (server pods mcp)".into());
    let left = (a.last_used_at + MCP_IDLE_SECS - now).max(0) / 86_400;
    json!({
        "id": a.id,
        "title": if a.label.is_empty() { "An MCP client" } else { a.label.as_str() },
        "host": to,
        "detail": format!(
            "{to} · connected {} · last used {}",
            ago(now - a.created_at),
            ago(now - a.last_used_at)
        ),
        "expiry": format!("Ends by itself if unused for {left} more days."),
        "connected": true,
        "access": a.access.id(),
        "access_label": access_label(a.access),
    })
}

/// A level as Settings shows it: its name and what it allows.
pub fn access_label(a: Access) -> String {
    format!("{}. {}", access_title(a), access_summary(a))
}

/// The screen data: entities and (without `id`) the query.
pub fn screen(apps: &[ConnectedApp], id: Option<&str>, now: i64) -> Value {
    let mut entities = Map::new();
    let mut uris = Vec::new();
    for a in apps.iter().filter(|a| id.is_none_or(|id| a.id == id)) {
        let uri = entity_uri(&a.id);
        entities.insert(uri.clone(), entity(a, now));
        uris.push(json!(uri));
    }
    let mut queries = Map::new();
    if id.is_none() {
        queries.insert(SCREEN_QUERY.into(), Value::Array(uris));
    }
    json!({ "entities": entities, "queries": queries })
}

fn store_error() -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "error": "host_error" }),
    )
}

/// Whether the host answers `path` itself ([`route`]).
pub(crate) fn is_ours(path: &str) -> bool {
    path == SCREEN_PATH || path == REVOKE_PATH || path == ACCESS_PATH
}

/// The answer to one of the two paths ([`is_ours`]) for pod `pod_id`.
pub(crate) async fn route(host: &Arc<Host>, pod_id: &str, req: Request) -> Response {
    let path = req.uri().path();
    if path == SCREEN_PATH && req.method() == Method::GET {
        let id = req.uri().query().and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("id="))
                .map(str::to_owned)
        });
        let pod = pod_id.to_owned();
        return match host.blocking(move |h| h.store.connected_apps(&pod)).await {
            Ok(Ok(apps)) => Json(screen(&apps, id.as_deref(), now_secs())).into_response(),
            _ => store_error(),
        };
    }
    if path == REVOKE_PATH && req.method() == Method::POST {
        let body = match axum::body::to_bytes(req.into_body(), 4096).await {
            Ok(b) => b,
            Err(_) => return bad_body(),
        };
        let Some(id) = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_owned))
        else {
            return bad_body();
        };
        let pod = pod_id.to_owned();
        let lookup = id.clone();
        return match host
            .blocking(move |h| h.store.revoke_connected_app(&pod, &lookup))
            .await
        {
            Ok(Ok(0)) => json_error(
                StatusCode::NOT_FOUND,
                json!({ "error": "unknown_connected_app",
                            "detail": "no connected app with that id (already disconnected?)" }),
            ),
            Ok(Ok(_)) => {
                tracing::info!("connected app revoked from Settings");
                Json(json!({ "ok": true, "id": id })).into_response()
            }
            _ => store_error(),
        };
    }
    if path == ACCESS_PATH && req.method() == Method::POST {
        let body = match axum::body::to_bytes(req.into_body(), 4096).await {
            Ok(b) => b,
            Err(_) => return bad_access_body(),
        };
        let v = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
        let (Some(id), Some(access)) = (
            v["id"].as_str().map(str::to_owned),
            v["access"].as_str().and_then(Access::from_id),
        ) else {
            return bad_access_body();
        };
        let pod = pod_id.to_owned();
        let lookup = id.clone();
        return match host
            .blocking(move |h| h.store.set_connected_app_access(&pod, &lookup, access))
            .await
        {
            Ok(Ok(0)) => json_error(
                StatusCode::NOT_FOUND,
                json!({ "error": "unknown_connected_app",
                        "detail": "no connected app with that id (disconnected?)" }),
            ),
            Ok(Ok(_)) => {
                tracing::info!(
                    access = access.id(),
                    "connected app's access changed from Settings"
                );
                Json(json!({ "ok": true, "id": id, "access": access.id(),
                             "access_label": access_label(access) }))
                .into_response()
            }
            _ => store_error(),
        };
    }
    json_error(
        StatusCode::METHOD_NOT_ALLOWED,
        json!({ "error": "method_not_allowed" }),
    )
}

fn bad_body() -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_json", "detail": "the body is {id}" }),
    )
}

fn bad_access_body() -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_json",
                "detail": "the body is {id, access}; access is read, edit or outward" }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_say_where_and_when() {
        let now = 100 * 86_400;
        let a = ConnectedApp {
            id: "0123456789abcdef".into(),
            label: "Claude".into(),
            redirect_host: Some("claude.ai".into()),
            created_at: now - 3 * 86_400,
            last_used_at: now - 10,
            access: Access::Edit,
        };
        let s = screen(std::slice::from_ref(&a), None, now);
        let e = &s["entities"]["connected_apps://0123456789abcdef"];
        assert_eq!(
            e["detail"],
            "claude.ai · connected 3 days ago · last used today"
        );
        assert_eq!(e["expiry"], "Ends by itself if unused for 89 more days.");
        assert_eq!(e["access"], "edit");
        assert!(e["access_label"].as_str().unwrap().starts_with("Edit. "));
        assert_eq!(
            s["queries"]["connected_apps"],
            json!(["connected_apps://0123456789abcdef"])
        );
        let one = screen(&[a], Some("nope"), now);
        assert_eq!(one["entities"], json!({}));
        assert!(one["queries"].get("connected_apps").is_none());
    }
}
