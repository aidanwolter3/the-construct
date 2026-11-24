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
pub const TASTE_ITEMS_QUERY: &str = "taste:taste_items";

/// The most photos an item carries (`image_0` to `image_5`).
pub const PHOTOS: usize = 6;
/// The width every big photo is asked for at, on a card and on the item's
/// screen alike, so one download serves both (and the image caches hit).
pub const PHOTO_WIDTH: u32 = 1080;
/// The entity of the whole taste map's points (`core://Scatter`).
pub const SCATTER_URI: &str = "taste://maps/catalog";

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

/// The service's id as a URI segment. Ids are `<store host>:<product id>`
/// (`shop.example:123`), and a screen's route captures only letters, digits,
/// `_` and `-` (at most 64), so the host's dots become `_` and the colon
/// `__` (`shop_example__123`): host names hold no `_`, so [`id_of`] turns it
/// back. An id of another shape gets a key that opens nothing.
pub fn key_of(id: &str) -> String {
    let plain = |s: &str, extra: char| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == extra)
    };
    match id.split_once(':') {
        Some((host, rest)) if plain(host, '.') && plain(rest, '-') => {
            format!("{}__{rest}", host.replace('.', "_"))
        }
        _ => id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect(),
    }
}

pub fn id_of(key: &str) -> String {
    match key.split_once("__") {
        Some((host, rest)) => format!("{}:{rest}", host.replace('_', ".")),
        None => key.to_owned(),
    }
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

/// Why the feed showed an item, in words, and a pill tone for it. The
/// service looks for the edges of the user's taste: `boundary` (it can't
/// tell whether they'd like it), `explore` (somewhere they haven't been) and
/// `likely` (probably a like); the older `near:<n>` and `adjacent` still read.
pub fn why_text(why: &str) -> (String, &'static str) {
    if let Some(n) = why.strip_prefix("near:") {
        let n = n.parse::<u32>().map(|n| n + 1).unwrap_or(1);
        return (format!("Near your taste · {n}"), "accent");
    }
    match why {
        "boundary" => ("On the edge".into(), "accent"),
        "likely" => ("Probably yours".into(), "success"),
        "adjacent" => ("Adjacent".into(), "success"),
        "explore" => ("Exploring".into(), "muted"),
        _ => (String::new(), "muted"),
    }
}

/// The item's photos (at most [`PHOTOS`], `http(s)` only), in the
/// service's order: someone wearing it first, when it has such a photo.
fn photos(v: &Value) -> Vec<String> {
    v.get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|u| !link(u).is_empty())
        .take(PHOTOS)
        .map(str::to_owned)
        .collect()
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
    let photos = photos(v);
    let image = photos.first().map(String::as_str).unwrap_or("");
    let buy = link(v.get("url").and_then(Value::as_str).unwrap_or(""));
    let (why, tone) = why_text(&text(v, "why", 40));
    let currency = text(v, "currency", 8);
    let p = v
        .get("p")
        .and_then(Value::as_f64)
        .filter(|p| p.is_finite())
        .map(|p| p.clamp(0.0, 1.0));
    let mut record = json!({
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
        "description": text(v, "description", 300),
        "worn": v.get("worn").and_then(Value::as_bool).unwrap_or(false),
        "p": p,
        "chance_text": p.map(|p| format!("{}% your taste", (p * 100.0).round() as i64)).unwrap_or_default(),
        "photo_count": photos.len(),
        "photos_many": photos.len() > 1,
        // Each photo as an entity of its own, for a carousel to page through.
        "photos": (0..photos.len())
            .map(|k| format!("taste://photos/{key}-{k}"))
            .collect::<Vec<_>>(),
        "photo_url": sized(image, PHOTO_WIDTH),
    });
    // A card shows one photo at a time: `image_<k>` is photo k, and a tap on
    // photo k sets the device-only field `photo` to `next_<k>` (wrapping), so
    // the layout needs no arithmetic. `photo` is never served: a reload shows
    // the first photo again.
    for k in 0..PHOTOS {
        let url = photos.get(k).map(|u| sized(u, 720)).unwrap_or_default();
        record[format!("image_{k}")] = json!(url);
        let next = if photos.is_empty() {
            0
        } else {
            (k + 1) % photos.len()
        };
        record[format!("next_{k}")] = json!(next);
    }
    match record {
        Value::Object(m) => Some((uri, EntityRecord(m))),
        _ => None,
    }
}

/// An item's photos as entities, `taste://photos/<key>-<k>`: the photo at
/// [`PHOTO_WIDTH`] (`url`), its number and the item's title (to describe it).
pub fn photo_entities(v: &Value) -> Vec<(String, EntityRecord)> {
    let Some(id) = v.get("id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let key = key_of(&clean(id, 300));
    let title = text(v, "title", 200);
    photos(v)
        .iter()
        .enumerate()
        .filter_map(|(k, u)| {
            let rec = json!({
                "url": sized(u, PHOTO_WIDTH),
                "n": k + 1,
                "title": title,
            });
            match rec {
                Value::Object(m) => Some((format!("taste://photos/{key}-{k}"), EntityRecord(m))),
                _ => None,
            }
        })
        .collect()
}

/// An item and its photos into a bundle.
pub fn put_item(b: &mut DataBundle, v: &Value) -> Option<String> {
    let (uri, rec) = item(v)?;
    for (u, r) in photo_entities(v) {
        b.entities.insert(u, r);
    }
    b.entities.insert(uri.clone(), rec);
    Some(uri)
}

/// The query of the similar pieces of the piece a sheet shows (the
/// `taste_item` core layout): one fixed name, since a layout's list can't
/// name a query by its route.
pub const SHEET_SIMILAR: &str = "taste:sheet:similar";

/// The query of the piece a sheet shows, for its photo carousel: one fixed
/// name (a layout's carousel can't name a piece by its route), so the
/// sheet lists it where key equals $route.key and pages `$item.photos`.
pub const SHEET_PIECE: &str = "taste:sheet:piece";

/// The piece `item` as [`SHEET_PIECE`] (its own entity, put by the caller).
pub fn put_sheet_piece(b: &mut DataBundle, item: &Value) {
    if let Some((uri, _)) = self::item(item) {
        b.queries.insert(SHEET_PIECE.to_owned(), vec![uri]);
    }
}

/// `items` (the pieces like the one keyed `key`) as [`SHEET_SIMILAR`]:
/// copies at `taste://sheet-similar/<key>/<n>`, each with `of` set to
/// `key`, so a sheet shows only its own piece's (`where of equals
/// $route.key`), never another piece's while its own loads.
pub fn put_sheet_similar(b: &mut DataBundle, key: &str, items: &[Value]) {
    let mut uris = Vec::new();
    for (n, v) in items.iter().enumerate() {
        let Some((_, mut rec)) = item(v) else {
            continue;
        };
        let uri = format!("taste://sheet-similar/{key}/{n}");
        rec.0.insert("uri".into(), Value::from(uri.clone()));
        rec.0.insert("of".into(), Value::from(key));
        b.entities.insert(uri.clone(), rec);
        uris.push(uri);
    }
    b.queries.insert(SHEET_SIMILAR.into(), uris);
}

/// The whole map as one entity for a `core://Scatter` ([`SCATTER_URI`]):
/// every item a point (likes highlighted, skips muted, coloured by the
/// model's like chance), its counts in words.
pub fn scatter(map: &crate::api::MapData) -> (String, EntityRecord) {
    use protocol::scatter::{Mark, Point, encode};
    let points: Vec<Point> = map
        .xy
        .iter()
        .enumerate()
        .map(|(i, [x, y])| Point {
            x: *x,
            y: *y,
            value: map
                .p
                .as_ref()
                .and_then(|p| p.get(i))
                .copied()
                .filter(|v| v.is_finite()),
            mark: if map.liked.contains(&i) {
                Mark::Highlight
            } else if map.skipped.contains(&i) {
                Mark::Muted
            } else {
                Mark::Plain
            },
        })
        .collect();
    let n = map.ids.len();
    let rec = json!({
        "points": encode(&points),
        "count": n,
        "count_text": format!("{} pieces", grouped(n as i64)),
        "reactions_text": format!("{} liked · {} disliked", map.liked.len(), map.skipped.len()),
        "modelled": map.p.is_some(),
    });
    let rec = match rec {
        Value::Object(m) => EntityRecord(m),
        _ => EntityRecord(Default::default()),
    };
    (SCATTER_URI.to_owned(), rec)
}

/// Items with the feed's reason filled in where the service left it out
/// (only feed pages carry `why`): the device keeps one copy of each entity,
/// so a list or screen without it would take the badge off the feed's card.
pub fn with_why(items: &[Value], kept: &[Value]) -> Vec<Value> {
    let why: std::collections::HashMap<&str, &Value> = kept
        .iter()
        .filter_map(|k| Some((k.get("id")?.as_str()?, k.get("why")?)))
        .collect();
    items
        .iter()
        .map(|v| {
            let mut v = v.clone();
            if v.get("why").is_none()
                && let Some(id) = v.get("id").and_then(Value::as_str)
                && let Some(w) = why.get(id)
            {
                v["why"] = (*w).clone();
            }
            v
        })
        .collect()
}

/// Items into `b` as query `key`, in order, each once.
pub fn put_items(b: &mut DataBundle, key: &str, items: &[Value]) {
    let mut order = Vec::new();
    for v in items {
        if let Some(uri) = put_item(b, v)
            && !order.contains(&uri)
        {
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
    let counts = format!("{} liked · {} disliked", s.likes, s.skips);
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

/// The taste page: each taste (`taste:tastes`), their pieces
/// (`taste:taste_items`, each with the `taste` it belongs to) and the summary.
pub fn tastes(b: &mut DataBundle, v: &Value) {
    let mut order = Vec::new();
    let mut pieces = Vec::new();
    let list = v
        .get("tastes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (n, t) in list.iter().take(12).enumerate() {
        let uri = format!("taste://tastes/{n}");
        b.entities.insert(uri.clone(), taste_record(n, t));
        // Each taste's pieces as entities of their own (`taste` says whose),
        // all in one query a list inside a taste's template filters.
        for v in t
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(12)
        {
            if let Some((_, mut rec)) = item(v) {
                let key = rec.0["key"].as_str().unwrap_or_default().to_owned();
                let piece = format!("taste://taste_items/{n}/{key}");
                rec.0.insert("taste".into(), json!(n.to_string()));
                rec.0.insert("uri".into(), json!(piece));
                b.entities.insert(piece.clone(), rec);
                pieces.push(piece);
            }
        }
        order.push(uri);
    }
    b.queries.insert(TASTES_QUERY.into(), order);
    b.queries.insert(TASTE_ITEMS_QUERY.into(), pieces);
    let likes = v.get("likes").and_then(Value::as_i64).unwrap_or(0);
    summary(b, v.get("summary").filter(|s| s.is_object()), likes);
}

/// A taste as an entity (`taste://tastes/<n>`): its title, size and words.
fn taste_record(n: usize, t: &Value) -> EntityRecord {
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
    let mut m = serde_json::Map::new();
    m.insert("n".into(), json!(n.to_string()));
    m.insert("title".into(), json!(format!("Taste {}", n + 1)));
    m.insert(
        "size_text".into(),
        json!(if size == 1 {
            "1 like".to_owned()
        } else {
            format!("{size} likes")
        }),
    );
    m.insert("attributes_text".into(), json!(words("attributes", 6)));
    m.insert("brands_text".into(), json!(words("brands", 5)));
    m.insert("categories_text".into(), json!(words("categories", 5)));
    EntityRecord(m)
}

/// The query of one taste's pieces to browse (taste `n`'s likes, then the
/// nearest pieces not yet judged).
pub fn one_taste_query(n: usize) -> String {
    format!("taste:one-taste:{n}")
}

/// One taste to browse, from `GET /api/taste/<n>`: the taste's entity (with
/// `count_text`, how many pieces the grid holds) and its pieces in order,
/// likes first; a like's `liked` is true, so a grid can mark it.
pub fn one_taste(b: &mut DataBundle, n: usize, v: &Value) {
    let mut rec = taste_record(n, v);
    let likes: Vec<Value> = v
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let more: Vec<Value> = v
        .get("more")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut all = Vec::with_capacity(likes.len() + more.len());
    for mut l in likes {
        // The service lists a taste's likes as liked; say so whatever it sent.
        if let Value::Object(m) = &mut l {
            m.insert("liked".into(), Value::Bool(true));
        }
        all.push(l);
    }
    all.extend(more);
    let total = all.len();
    rec.0.insert(
        "count_text".into(),
        json!(match total {
            1 => "1 piece".to_owned(),
            t => format!("{t} pieces"),
        }),
    );
    b.entities.insert(format!("taste://tastes/{n}"), rec);
    put_items(b, &one_taste_query(n), &all);
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

/// The whole map, in millionths: areas are named by their box,
/// `x0-y0-x1-y1` (`0-0-1000000-1000000`), so a screen's route can carry one
/// (routes take only letters, digits, `_` and `-`).
pub const SCALE: f64 = 1_000_000.0;
pub const WHOLE_MAP: &str = "0-0-1000000-1000000";

/// An area's key from its box (`0..1` each way).
pub fn area_key(b: crate::api::Bounds) -> String {
    let m = |f: f64| (f.clamp(0.0, 1.0) * SCALE).round() as i64;
    format!("{}-{}-{}-{}", m(b.x0), m(b.y0), m(b.x1), m(b.y1))
}

/// The box an area's key names: four whole numbers `0..=1000000`, each
/// side longer than nothing. `None` for anything else.
pub fn area_of(key: &str) -> Option<crate::api::Bounds> {
    let n: Vec<i64> = key
        .split('-')
        .map(|p| {
            (!p.is_empty() && p.len() <= 7 && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        })
        .collect::<Option<_>>()?;
    let [x0, y0, x1, y1] = n[..] else { return None };
    let ok = |a: i64, b: i64| (0..=1_000_000).contains(&a) && (0..=1_000_000).contains(&b) && a < b;
    if !ok(x0, x1) || !ok(y0, y1) {
        return None;
    }
    let f = |v: i64| v as f64 / SCALE;
    Some(crate::api::Bounds {
        x0: f(x0),
        y0: f(y0),
        x1: f(x1),
        y1: f(y1),
    })
}

/// `#RRGGBB`, else the map's colour for "not known yet".
fn colour(v: &Value) -> String {
    let c = v.get("color").and_then(Value::as_str).unwrap_or("");
    if c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        c.to_owned()
    } else {
        "#D9D4CC".to_owned()
    }
}

/// `1,204`.
fn grouped(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn pieces(n: i64) -> String {
    match n {
        0 => "Nothing here".to_owned(),
        1 => "1 piece".to_owned(),
        n => format!("{} pieces", grouped(n)),
    }
}

/// How far in an area is, from its width: 1 for the whole map.
fn zoom(b: crate::api::Bounds) -> u32 {
    let w = (b.x1 - b.x0).max(1e-6);
    (1.0 / w).log(4.0).round().max(0.0) as u32
}

pub fn tiles_query(area: &str) -> String {
    format!("taste:tiles:{area}")
}

pub fn area_query(area: &str) -> String {
    format!("taste:area:{area}")
}

/// A tile opens a finer map of itself, or its pieces when it holds only a
/// few (a finer map would be mostly empty).
pub const FEW: i64 = 6;

/// The map screen's data: the area (`taste://areas/<key>`) and its tiles
/// (`taste:tiles:<key>`, row by row), each `taste://tiles/<key>/<j>_<i>`
/// with its tint, photo, counts and what a tap opens.
pub fn map(b: &mut DataBundle, area: &str, v: &Value) {
    let mut order = Vec::new();
    let mut total = 0;
    let (mut likes, mut skips) = (0, 0);
    let list = v
        .get("tiles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for t in list.iter().take(64) {
        let num = |k: &str| t.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let int = |k: &str| t.get(k).and_then(Value::as_i64).unwrap_or(0).max(0);
        let (i, j) = (int("i"), int("j"));
        let bounds = crate::api::Bounds {
            x0: num("x0"),
            y0: num("y0"),
            x1: num("x1"),
            y1: num("y1"),
        };
        let child = area_key(bounds);
        if area_of(&child).is_none() {
            continue;
        }
        let count = int("count");
        let (l, s) = (int("likes"), int("skips"));
        total += count;
        likes += l;
        skips += s;
        let photo = t
            .get("item")
            .and_then(|item| photos(item).into_iter().next())
            .map(|u| sized(&u, 360))
            .unwrap_or_default();
        let opens = if count == 0 {
            String::new()
        } else if count <= FEW {
            format!("taste://area/{child}")
        } else {
            format!("taste://map/{child}")
        };
        let reactions = match (l, s) {
            (0, 0) => String::new(),
            (l, 0) => format!("♥ {l}"),
            (0, s) => format!("✕ {s}"),
            (l, s) => format!("♥ {l} · ✕ {s}"),
        };
        let uri = format!("taste://tiles/{area}/{j}_{i}");
        let rec = json!({
            "area": child,
            "color": colour(t),
            "image": photo,
            "count": count,
            "count_text": pieces(count),
            "reactions_text": reactions,
            "empty": count == 0,
            "opens": opens,
            "description": match (count, t.get("item").and_then(|i| i.get("title")).and_then(Value::as_str)) {
                (0, _) => "An empty part of the map".to_owned(),
                (_, Some(title)) => format!("{}, like {}", pieces(count), clean(title, 80)),
                _ => pieces(count),
            },
        });
        if let Value::Object(m) = rec {
            b.entities.insert(uri.clone(), EntityRecord(m));
        }
        order.push(uri);
    }
    b.queries.insert(tiles_query(area), order);
    let bounds = area_of(area);
    let level = bounds.map(zoom).unwrap_or(0);
    let rec = json!({
        "key": area,
        "title": if area == WHOLE_MAP { "Your taste map".to_owned() } else { format!("Zoomed in ×{}", 4u32.pow(level)) },
        "whole": area == WHOLE_MAP,
        "count_text": pieces(total),
        "reactions_text": format!("{} liked · {} disliked here", grouped(likes), grouped(skips)),
    });
    if let Value::Object(m) = rec {
        b.entities
            .insert(format!("taste://areas/{area}"), EntityRecord(m));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photos_cycle_and_wrap() {
        let (_, rec) = item(&json!({
            "id": "shop.example:7", "images": ["https://cdn.example/worn.jpg", "javascript:x",
            "https://cdn.example/flat.jpg", "https://cdn.example/back.jpg"],
            "description": "A heavy\ncanvas jacket.", "worn": true, "p": 0.4123
        }))
        .unwrap();
        assert_eq!(rec.0["photo_count"], 3);
        assert_eq!(rec.0["image_0"], "https://cdn.example/worn.jpg?width=720");
        assert_eq!(rec.0["image_2"], "https://cdn.example/back.jpg?width=720");
        assert_eq!(
            [&rec.0["next_0"], &rec.0["next_1"], &rec.0["next_2"]],
            [&json!(1), &json!(2), &json!(0)]
        );
        assert_eq!(rec.0["description"], "A heavy canvas jacket.");
        assert_eq!(rec.0["chance_text"], "41% your taste");
        assert_eq!(rec.0["photos_many"], true);
    }

    #[test]
    fn areas_round_trip_and_refuse_nonsense() {
        let b = area_of(WHOLE_MAP).unwrap();
        assert_eq!((b.x0, b.x1), (0.0, 1.0));
        assert_eq!(area_key(b), WHOLE_MAP);
        let q = crate::api::Bounds {
            x0: 0.25,
            y0: 0.5,
            x1: 0.5,
            y1: 0.75,
        };
        assert_eq!(area_key(q), "250000-500000-500000-750000");
        assert_eq!(area_of(&area_key(q)), Some(q));
        for bad in [
            "",
            "1-2-3",
            "5-0-5-1",
            "0-0-2000000-1",
            "a-b-c-d",
            "0-0-1-1-1",
            "-1-0-1-1",
        ] {
            assert!(area_of(bad).is_none(), "{bad}");
        }
        assert_eq!(zoom(b), 0);
        assert_eq!(zoom(q), 1);
    }

    #[test]
    fn a_map_has_tiles_that_zoom_or_open_their_pieces() {
        let mut b = DataBundle::default();
        let tile = |i: i64, j: i64, count: i64| {
            json!({
                "i": i, "j": j, "x0": i as f64 * 0.5, "y0": j as f64 * 0.5,
                "x1": i as f64 * 0.5 + 0.5, "y1": j as f64 * 0.5 + 0.5,
                "count": count, "likes": 1, "skips": 0, "color": "#aabbcc",
                "item": if count > 0 { json!({"id": "s.example:1", "title": "Coat",
                    "images": ["https://cdn.example/c.jpg"]}) } else { Value::Null }
            })
        };
        map(
            &mut b,
            WHOLE_MAP,
            &json!({"tiles": [tile(0, 0, 40), tile(1, 0, 3), tile(0, 1, 0),
                              {"i": 1, "j": 1, "x0": 0.5, "y0": 0.5, "x1": 0.5, "y1": 1.0}]}),
        );
        let q = &b.queries[&tiles_query(WHOLE_MAP)];
        assert_eq!(q.len(), 3, "a tile with no width is dropped");
        let t = |n: usize| &b.entities[&q[n]].0;
        assert_eq!(t(0)["opens"], "taste://map/0-0-500000-500000");
        assert_eq!(t(1)["opens"], "taste://area/500000-0-1000000-500000");
        assert_eq!(t(2)["opens"], "");
        assert_eq!(t(2)["empty"], true);
        assert_eq!(t(0)["image"], "https://cdn.example/c.jpg?width=360");
        assert_eq!(t(0)["color"], "#aabbcc");
        let area = &b.entities[&format!("taste://areas/{WHOLE_MAP}")].0;
        assert_eq!(area["count_text"], "43 pieces");
        assert_eq!(pieces(10533), "10,533 pieces");
        assert_eq!(area["title"], "Your taste map");
    }

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
        assert_eq!(why_text("boundary").0, "On the edge");
        assert_eq!(why_text("likely").0, "Probably yours");
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
        assert_eq!(uri, "taste://items/shop_example__42");
        assert_eq!(rec.0["brand"], "Example Co");
        assert_eq!(rec.0["buy_url"], "");
        assert_eq!(rec.0["image"], "https://cdn.example/a.jpg?v=1&width=720");
        assert_eq!(rec.0["price_text"], "$98");
        assert_eq!(rec.0["liked"], true);
        assert_eq!(rec.0["photo_count"], 1);
        assert_eq!(rec.0["next_0"], 0);
        assert_eq!(rec.0["image_1"], "");
        assert_eq!(id_of("shop_example__42"), "shop.example:42");
        assert_eq!(
            key_of("www.shop-two.co.uk:7454613995597"),
            "www_shop-two_co_uk__7454613995597"
        );
        assert_eq!(
            id_of(&key_of("www.shop-two.co.uk:7454613995597")),
            "www.shop-two.co.uk:7454613995597"
        );
    }
}
