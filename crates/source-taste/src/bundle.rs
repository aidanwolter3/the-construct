//! The feeds' data: the service's items as entities with what a layout
//! draws worked out (image URLs at a few widths, "$148", "Near your taste ·
//! 2"), so layouts bind fields and need no logic. Everything the service
//! sends is untrusted: text is cleaned and cut, links must be `http(s)`.

use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};

pub const STATUS_URI: &str = "taste://status/pod";
pub const SUMMARY_URI: &str = "taste://summary/pod";
pub const SEARCH_URI: &str = "taste://search/pod";

pub const FEED_QUERY: &str = "taste:feed";
pub const STATUS_QUERY: &str = "taste:status";
pub const LIKES_QUERY: &str = "taste:likes";
pub const SEARCH_QUERY: &str = "taste:search";
pub const TASTES_QUERY: &str = "taste:tastes";

/// Text from the service: control characters gone, at most `max` characters.
pub fn clean(s: &str, max: usize) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn text(v: &Value, k: &str, max: usize) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .map(|s| clean(s, max))
        .unwrap_or_default()
}

/// An `http(s)` URL, else empty.
pub fn link(s: &str) -> String {
    match reqwest::Url::parse(s.trim()) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && s.len() <= 2048 => u.to_string(),
        _ => String::new(),
    }
}

/// `url` at `width` pixels (the stores' CDNs take a `width` parameter).
pub fn sized(url: &str, width: u32) -> String {
    let url = link(url);
    if url.is_empty() {
        return url;
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}width={width}")
}

/// The service's id as a URI segment: ids hold `:` (`shop.example:123`),
/// which the pod's query keys use between their parts, so it becomes `~`
/// (never in a host name or a number). [`id_of`] turns it back.
pub fn key_of(id: &str) -> String {
    id.replace(':', "~")
}

pub fn id_of(key: &str) -> String {
    key.replace('~', ":")
}

/// A price in its currency: `$148`, `£95`, `€210.50`, `12,000 JPY`.
pub fn price_text(price: Option<f64>, currency: &str) -> String {
    let Some(p) = price.filter(|p| p.is_finite() && *p >= 0.0) else {
        return String::new();
    };
    let whole = (p - p.round()).abs() < 0.005;
    let digits = |p: f64| {
        let n = if whole {
            format!("{}", p.round() as i64)
        } else {
            format!("{p:.2}")
        };
        let (int, frac) = n.split_once('.').map_or((n.as_str(), ""), |(a, b)| (a, b));
        let mut grouped = String::new();
        for (i, c) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(c);
        }
        if frac.is_empty() {
            grouped
        } else {
            format!("{grouped}.{frac}")
        }
    };
    let symbol = match currency.trim().to_ascii_uppercase().as_str() {
        "USD" | "" => "$",
        "GBP" => "£",
        "EUR" => "€",
        "CAD" => "CA$",
        "AUD" => "A$",
        "NZD" => "NZ$",
        other => return format!("{} {}", digits(p), clean(other, 8)),
    };
    format!("{symbol}{}", digits(p))
}

/// Why the feed showed an item, in words, and a pill tone for it.
pub fn why_text(why: &str) -> (String, &'static str) {
    if let Some(n) = why.strip_prefix("near:") {
        let n = n.parse::<u32>().map(|n| n + 1).unwrap_or(1);
        return (format!("Near your taste · {n}"), "accent");
    }
    match why {
        "adjacent" => ("Adjacent".into(), "success"),
        "explore" => ("Exploring".into(), "muted"),
        _ => (String::new(), "muted"),
    }
}

/// An item as its entity, `taste://items/<key>`; `None` without an id.
pub fn item(v: &Value) -> Option<(String, EntityRecord)> {
    let id = v.get("id").and_then(Value::as_str)?;
    let id = clean(id, 300);
    if id.is_empty() {
        return None;
    }
    let key = key_of(&id);
    let uri = format!("taste://items/{key}");
    let image = v
        .get("images")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find_map(Value::as_str))
        .unwrap_or("");
    let buy = link(v.get("url").and_then(Value::as_str).unwrap_or(""));
    let (why, tone) = why_text(&text(v, "why", 40));
    let currency = text(v, "currency", 8);
    let record = json!({
        "id": id,
        "key": key,
        "uri": uri,
        "brand": text(v, "brand", 80),
        "title": text(v, "title", 200),
        "category": text(v, "category", 60),
        "price_text": price_text(v.get("price").and_then(Value::as_f64), &currency),
        "buy_url": buy,
        "host": crate::api::host_of(&buy).trim_start_matches("www.").to_owned(),
        "image": sized(image, 720),
        "image_large": sized(image, 1600),
        "thumb": sized(image, 360),
        "liked": v.get("liked").and_then(Value::as_bool).unwrap_or(false),
        "skipped": v.get("skipped").and_then(Value::as_bool).unwrap_or(false),
        "why_text": why,
        "why_tone": tone,
    });
    match record {
        Value::Object(m) => Some((uri, EntityRecord(m))),
        _ => None,
    }
}

/// Items into `b` as query `key`, in order, each once.
pub fn put_items(b: &mut DataBundle, key: &str, items: &[Value]) {
    let mut order = Vec::new();
    for v in items {
        if let Some((uri, rec)) = item(v)
            && !order.contains(&uri)
        {
            b.entities.insert(uri.clone(), rec);
            order.push(uri);
        }
    }
    b.queries.insert(key.to_owned(), order);
}

/// What the status entity says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub signed_in: bool,
    pub host: String,
    pub likes: i64,
    pub skips: i64,
    pub error: Option<String>,
}

pub fn status(b: &mut DataBundle, s: &Status) {
    let counts = format!("{} liked · {} skipped", s.likes, s.skips);
    let rec = json!({
        "signed_in": s.signed_in,
        "host": s.host,
        "likes": s.likes,
        "skips": s.skips,
        "counts_text": counts,
        "status_text": if s.signed_in { format!("Connected to {}", s.host) } else { String::new() },
        "error": s.error.clone().unwrap_or_default(),
        "loading": false,
    });
    if let Value::Object(m) = rec {
        b.entities.insert(STATUS_URI.into(), EntityRecord(m));
    }
    b.queries
        .insert(STATUS_QUERY.into(), vec![STATUS_URI.to_owned()]);
}

/// The taste page: each taste (`taste:tastes`, its items as
/// `taste:taste:<n>`) and the summary.
pub fn tastes(b: &mut DataBundle, v: &Value) {
    let mut order = Vec::new();
    let list = v.get("tastes").and_then(Value::as_array).cloned().unwrap_or_default();
    for (n, t) in list.iter().take(12).enumerate() {
        let uri = format!("taste://tastes/{n}");
        let words = |k: &str, max: usize| {
            t.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .take(max)
                        .map(|s| clean(s, 40))
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .unwrap_or_default()
        };
        let size = t.get("size").and_then(Value::as_i64).unwrap_or(0);
        let rec = json!({
            "n": n,
            "title": format!("Taste {}", n + 1),
            "size_text": if size == 1 { "1 like".to_owned() } else { format!("{size} likes") },
            "attributes_text": words("attributes", 6),
            "brands_text": words("brands", 5),
            "categories_text": words("categories", 5),
        });
        if let Value::Object(m) = rec {
            b.entities.insert(uri.clone(), EntityRecord(m));
        }
        let items = t.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        put_items(b, &format!("taste:taste:{n}"), &items);
        order.push(uri);
    }
    b.queries.insert(TASTES_QUERY.into(), order);
    let likes = v.get("likes").and_then(Value::as_i64).unwrap_or(0);
    summary(b, v.get("summary").filter(|s| s.is_object()), likes);
}

/// The summary entity from `{text, likes}` (or none written yet).
pub fn summary(b: &mut DataBundle, s: Option<&Value>, likes: i64) {
    let text = s.map(|s| text(s, "text", 4000)).unwrap_or_default();
    let at = s.and_then(|s| s.get("likes")).and_then(Value::as_i64);
    let rec = json!({
        "text": text,
        "written": !text.is_empty(),
        "likes": likes,
        "written_text": match at {
            Some(n) if !text.is_empty() => format!("Written at {n} likes"),
            _ => String::new(),
        },
        "writing": false,
    });
    if let Value::Object(m) = rec {
        b.entities.insert(SUMMARY_URI.into(), EntityRecord(m));
    }
}

/// The search entity: what was searched for, and how many came back.
pub fn search(b: &mut DataBundle, q: &str, items: &[Value]) {
    let rec = json!({
        "q": clean(q, 200),
        "count_text": match items.len() {
            0 => "Nothing found".to_owned(),
            1 => "1 piece".to_owned(),
            n => format!("{n} pieces"),
        },
    });
    if let Value::Object(m) = rec {
        b.entities.insert(SEARCH_URI.into(), EntityRecord(m));
    }
    put_items(b, SEARCH_QUERY, items);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_read_naturally() {
        assert_eq!(price_text(Some(148.0), "USD"), "$148");
        assert_eq!(price_text(Some(28.5), "GBP"), "£28.50");
        assert_eq!(price_text(Some(1250.0), "EUR"), "€1,250");
        assert_eq!(price_text(Some(12000.0), "JPY"), "12,000 JPY");
        assert_eq!(price_text(None, "USD"), "");
    }

    #[test]
    fn why_has_words() {
        assert_eq!(why_text("near:1").0, "Near your taste · 2");
        assert_eq!(why_text("adjacent").0, "Adjacent");
        assert_eq!(why_text("explore").0, "Exploring");
        assert_eq!(why_text("").0, "");
    }

    #[test]
    fn an_item_is_ready_to_draw_and_safe() {
        let (uri, rec) = item(&json!({
            "id": "shop.example:42", "brand": "Example Co\u{7}", "title": "Chore Coat",
            "url": "javascript:alert(1)", "price": 98, "currency": "USD",
            "images": ["https://cdn.example/a.jpg?v=1"], "liked": true, "why": "explore"
        }))
        .unwrap();
        assert_eq!(uri, "taste://items/shop.example~42");
        assert_eq!(rec.0["brand"], "Example Co");
        assert_eq!(rec.0["buy_url"], "");
        assert_eq!(rec.0["image"], "https://cdn.example/a.jpg?v=1&width=720");
        assert_eq!(rec.0["price_text"], "$98");
        assert_eq!(rec.0["liked"], true);
        assert_eq!(id_of("shop.example~42"), "shop.example:42");
    }
}
