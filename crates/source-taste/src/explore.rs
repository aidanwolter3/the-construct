//! The taste source's answer to Collections' `taste` fillers (decision
//! `shopping-kit`): an explore request to the owner's taste service
//! (`POST /api/explore`), its pieces as rows' values.
//!
//! A service from before `/api/explore` (404 or 405) is asked what it can
//! answer instead: the pieces similar to the first liked one, else a search
//! for the query, without groups or distances.

use pod_core::{BoxFuture, Pod};
use serde_json::{Map, Value, json};
use source_collections::explore::{Explorer, MAX_PIECES};

use crate::api::{self, ApiError};
use crate::{TasteAdapter, bundle, db};

/// The taste source as Collections' explorer.
#[derive(Debug, Default)]
pub struct TasteExplorer;

/// The explorer the server gives Collections.
pub fn explorer() -> std::sync::Arc<dyn Explorer> {
    std::sync::Arc::new(TasteExplorer)
}

impl Explorer for TasteExplorer {
    fn explore<'a>(
        &'a self,
        pod: &'a Pod,
        request: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>> {
        Box::pin(async move {
            let Some(a) = pod.adapter::<TasteAdapter>() else {
                return Err("no taste source".into());
            };
            let service = match pod.blocking(db::service).await {
                Ok(Some(s)) => s,
                Ok(None) => return Err("the pod is not signed in to a taste service".into()),
                Err(_) => return Err("the taste sign-in could not be read".into()),
            };
            let service = crate::with_history(pod, a, service).await;
            let body = to_service(&request);
            let n = body["n"].as_u64().unwrap_or(12) as usize;
            let answer = match api::explore(&a.client, &service, &body).await {
                Ok(v) => v,
                Err(ApiError::Status(404 | 405, detail)) if !detail.contains("domain_unknown") => {
                    let like = body["like"]
                        .as_array()
                        .and_then(|l| l.first())
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let items = match (like, body["query"].as_str()) {
                        (Some(id), _) => api::related(&a.client, &service, &id, "similar", n).await,
                        (None, Some(q)) => api::search(&a.client, &service, q, n).await,
                        (None, None) => Ok(Vec::new()),
                    }
                    .map_err(|e| e.to_string())?;
                    json!({ "items": items, "groups": [] })
                }
                Err(e) => return Err(e.to_string()),
            };
            Ok(pieces(&answer, &body))
        })
    }
}

/// The request as the service takes it: piece keys as its ids, the price
/// cap under `filters`.
fn to_service(request: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (k, v) in request {
        match k.as_str() {
            "like" | "unlike" | "exclude" => {
                let ids: Vec<Value> = v
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|k| Value::from(bundle::id_of(k)))
                    .collect();
                out.insert(k.clone(), Value::from(ids));
            }
            "price_max" => {
                out.insert("filters".into(), json!({ "price_max": v }));
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out.entry("n").or_insert(json!(12));
    Value::Object(out)
}

/// The service's answer as pieces ([`source_collections::filler::TASTE_PARTS`]),
/// never one it was told to leave out.
fn pieces(answer: &Value, request: &Value) -> Vec<Map<String, Value>> {
    let skip: Vec<&str> = ["like", "unlike", "exclude"]
        .iter()
        .flat_map(|k| request[*k].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    let groups: Vec<&Value> = answer["groups"].as_array().into_iter().flatten().collect();
    let group_of = |g: i64| groups.iter().find(|x| x["group"].as_i64() == Some(g));
    answer["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| {
            v.get("id")
                .and_then(Value::as_str)
                .is_none_or(|id| !skip.contains(&id))
        })
        .filter_map(|v| {
            let (_, rec) = bundle::item(v)?;
            let r = &rec.0;
            let s = |k: &str| r.get(k).cloned().unwrap_or(json!(""));
            let mut p = Map::new();
            for (part, from) in [
                ("key", "key"),
                ("title", "title"),
                ("brand", "brand"),
                ("image", "image"),
                ("thumb", "thumb"),
                ("price_text", "price_text"),
                ("url", "buy_url"),
                ("host", "host"),
                ("description", "description"),
                ("category", "category"),
            ] {
                p.insert(part.into(), s(from));
            }
            if let Some(price) = v
                .get("price")
                .and_then(Value::as_f64)
                .filter(|x| x.is_finite())
            {
                p.insert("price".into(), json!(price));
            }
            if let Some(d) = v
                .get("distance")
                .and_then(Value::as_f64)
                .filter(|x| x.is_finite())
            {
                p.insert(
                    "distance".into(),
                    json!((d.clamp(0.0, 1.0) * 100.0).round() / 100.0),
                );
            }
            if let Some(g) = v.get("group").and_then(Value::as_i64) {
                p.insert("group".into(), json!(g));
                if let Some(x) = group_of(g) {
                    let currency =
                        bundle::clean(v.get("currency").and_then(Value::as_str).unwrap_or(""), 8);
                    let label = bundle::clean(x["label"].as_str().unwrap_or(""), 80);
                    let ids: Vec<String> = x["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(|id| bundle::key_of(&bundle::clean(id, 300)))
                        .collect();
                    let (lo, hi) = (x["price_low"].as_f64(), x["price_high"].as_f64());
                    let prices = match (lo, hi) {
                        (Some(l), Some(h)) if (h - l).abs() >= 0.5 => format!(
                            "{}–{}",
                            bundle::price_text(Some(l), &currency),
                            bundle::price_text(Some(h), &currency)
                        ),
                        (Some(l), _) => bundle::price_text(Some(l), &currency),
                        _ => String::new(),
                    };
                    p.insert("group_label".into(), json!(label));
                    p.insert("group_ids".into(), json!(ids.join(", ")));
                    p.insert("group_size".into(), json!(x["size"].as_i64().unwrap_or(0)));
                    p.insert("group_prices".into(), json!(prices));
                }
            }
            Some(p)
        })
        .take(MAX_PIECES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_go_to_the_service_as_ids_and_the_cap_as_a_filter() {
        let mut r = Map::new();
        r.insert("like".into(), json!(["shop_example__1"]));
        r.insert("price_max".into(), json!(300));
        let v = to_service(&r);
        assert_eq!(v["like"], json!(["shop.example:1"]));
        assert_eq!(v["filters"]["price_max"], json!(300));
        assert_eq!(v["n"], json!(12));
    }

    #[test]
    fn pieces_carry_their_group_and_never_an_anchor() {
        let answer = json!({
            "items": [
                {"id": "shop.example:1", "title": "Arched", "price": 140, "group": 0, "distance": 0.3,
                 "url": "https://shop.example/p/1", "images": ["https://cdn.shop.example/1.jpg"]},
                {"id": "shop.example:2", "title": "Liked one", "group": 0}
            ],
            "groups": [{"group": 0, "label": "Arched", "size": 24, "price_low": 140, "price_high": 560,
                        "items": ["shop.example:1", "shop.example:2"]}]
        });
        let p = pieces(&answer, &json!({"like": ["shop.example:2"]}));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0]["key"], json!("shop_example__1"));
        assert_eq!(p[0]["group_label"], json!("Arched"));
        assert_eq!(p[0]["group_ids"], json!("shop_example__1, shop_example__2"));
        assert_eq!(p[0]["group_prices"], json!("$140–$560"));
        assert_eq!(p[0]["distance"], json!(0.3));
        assert_eq!(p[0]["url"], json!("https://shop.example/p/1"));
    }
}
