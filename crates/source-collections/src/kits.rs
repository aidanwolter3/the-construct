//! Kits: a layout and the collection it is over, ready to use (decision
//! `element-roles-and-reuse`).
//!
//! A kit's layout is a core layout that reads its collection through
//! inputs (`$in.collection`: the collection's name; `$in.title`: the
//! page's title), so one layout serves any collection of the kit's shape,
//! by reference: a deploy that improves it reaches every pod that uses it.
//! MCP `use_kit` makes the collection (or checks one that exists has the
//! kit's fields), adds its feed to home and answers the embed and the link
//! that show it, with the inputs bound.

use pod_core::layouts::LayoutSource;
use pod_core::{AuthPolicy, CoreLayoutInfo};
use serde_json::{Value, json};

include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// One kit.
pub struct Kit {
    /// Its layout, `@core/<id>` in every pod.
    pub layout: CoreLayoutInfo,
    /// One line: what it is for.
    pub summary: &'static str,
    /// The collection's name when the caller gives none.
    pub collection: &'static str,
    /// The collection it is over: `define_collection`'s `fields`.
    pub fields: fn() -> Value,
    /// A kit of several collections (decision `shopping-kit`): theirs, made
    /// under these names (its layouts name them), then the changes that
    /// need them all. `collection` and `fields` are its main one's.
    pub set: Option<fn() -> KitSet>,
    /// The other layouts its layout opens, `@core/<id>` too.
    pub more: &'static [CoreLayoutInfo],
}

/// The collections of a kit of several.
pub struct KitSet {
    /// `define_collection`'s arguments, in order (a ref names an earlier
    /// one).
    pub collections: Vec<Value>,
    /// Then `apply_change`'s changes per collection: what reads a later
    /// one (a rollup over the rows pointing at it).
    pub changes: Vec<(&'static str, Vec<Value>)>,
}

const fn layout(id: &'static str, title: &'static str) -> CoreLayoutInfo {
    CoreLayoutInfo {
        id,
        title,
        icon: "check_box",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: &[],
    }
}

/// The shopping kit's feeds: its layouts read its collections by name.
const SHOP_FEEDS: &[(&str, &str)] = &[
    ("collections", "shop_categories"),
    ("collections", "shop_needs"),
    ("collections", "shop_rounds"),
    ("collections", "shop_pieces"),
    ("collections", "shop_picks"),
    ("collections", "shop_prices"),
];

const fn shop_layout(id: &'static str, title: &'static str) -> CoreLayoutInfo {
    CoreLayoutInfo {
        id,
        title,
        icon: "shopping_cart",
        dev: false,
        auth: AuthPolicy::Public,
        feeds: SHOP_FEEDS,
    }
}

/// Every kit the server ships.
pub static KITS: &[Kit] = &[
    Kit {
        layout: layout("checklist", "Checklist"),
        summary: "Things to tick off: to-dos, a packing list, chores. Add at the top, tap to tick, \
                  swipe left to delete; done items move under Done.",
        collection: "checklist",
        fields: || {
            json!([
                { "name": "title", "type": "text" },
                { "name": "done", "type": "bool", "default": false },
                { "name": "notes", "type": "text", "optional": true },
            ])
        },
        set: None,
        more: &[],
    },
    Kit {
        layout: shop_layout("shopping", "Shopping"),
        summary: "Things to buy, from a vague need to a watched price (decision shopping-kit): \
                  a need (\"a full-length mirror\") is explored through the taste service, broad \
                  then narrow, its pieces liked, disliked and shortlisted; the one chosen is \
                  the need's pick, its price checked daily, charted, and notified when it drops. \
                  Categories without a taste catalog find options on the web instead. Its \
                  collections keep their names: shop_categories, shop_needs, shop_rounds, \
                  shop_pieces, shop_picks, shop_prices.",
        collection: "shop_needs",
        fields: shop_need_fields,
        set: Some(shopping_set),
        more: &[
            shop_layout("shopping_need", "Shopping need"),
            shop_layout("shopping_explore", "Explore"),
        ],
    },
];

pub fn kit(id: &str) -> Option<&'static Kit> {
    KITS.iter().find(|k| k.layout.id == id)
}

/// Every core layout of the kits.
pub fn layouts() -> Vec<CoreLayoutInfo> {
    KITS.iter()
        .flat_map(|k| std::iter::once(k.layout).chain(k.more.iter().copied()))
        .collect()
}

/// The kits' layout files.
pub fn layout_files() -> &'static [LayoutSource] {
    static ALL: std::sync::LazyLock<Vec<LayoutSource>> = std::sync::LazyLock::new(|| {
        [
            CHECKLIST_FILES,
            SHOPPING_FILES,
            SHOPPING_NEED_FILES,
            SHOPPING_EXPLORE_FILES,
        ]
        .concat()
    });
    &ALL
}

/// What a need holds before the rollups over its rounds, pieces and
/// picks (added once those exist).
fn shop_need_fields() -> Value {
    json!([
        { "name": "name", "type": "text" },
        { "name": "category", "type": "text", "default": "Other" },
        { "name": "notes", "type": "text", "optional": true },
        { "name": "quantity", "type": "text", "optional": true },
        { "name": "remind_on", "type": "date", "optional": true },
        { "name": "bought", "type": "bool", "default": false },
        { "name": "bought_on", "type": "date", "optional": true },
        { "name": "find_web", "type": "bool", "default": false },
        { "name": "explorable", "type": "bool", "default": false,
          "compute": { "kind": "lookup", "from": "category", "collection": "shop_categories",
                       "key": "name", "value": "explore", "match": "exact" } },
        { "name": "preferences", "type": "text", "optional": true,
          "compute": { "kind": "lookup", "from": "category", "collection": "shop_categories",
                       "key": "name", "value": "preferences", "match": "exact" } },
        { "name": "avoid", "type": "text", "optional": true,
          "compute": { "kind": "lookup", "from": "category", "collection": "shop_categories",
                       "key": "name", "value": "explore_avoid", "match": "exact" } },
        { "name": "prefer", "type": "text", "optional": true,
          "compute": { "kind": "lookup", "from": "category", "collection": "shop_categories",
                       "key": "name", "value": "explore_prefer", "match": "exact" } },
    ])
}

/// The shopping kit (decision `shopping-kit`).
fn shopping_set() -> KitSet {
    let price_hint = "price_now is today's price in USD as a number for this exact product at \
        this store (the sale price when one is shown). stock_now is In stock, Low stock or Sold \
        out when a result says so. Leave a field null when no result shows it; never guess.";
    let research = "This product is already chosen; from the results about it: why (two or \
        three sentences on what it is, its materials and how it's made, where results say), \
        pros (2-4 short lines, each starting '- '), cons (1-3 honest short lines, each starting \
        '- ', including anything the results don't confirm), reviews (up to three short quotes \
        copied word for word from the results, each in double quotes then ' (site)', one per \
        line) and sources (the results used, one per line, as 'Site · title'). Leave a field \
        null when no result says it; never guess.";
    let options = "Find up to five options to buy for this need, following its notes and \
        preferences: well-made pieces from good makers over the cheapest. Save each with title \
        (the product), brand, price (today's USD price, a number), price_text (as the store \
        writes it), url (its product page, exactly as seen), image (one of that page's Images of \
        the product), host (the store's site) and description (one or two sentences: why it \
        fits).";
    KitSet {
        collections: vec![
            json!({
                "name": "shop_categories", "title": "Shopping categories",
                "fields": [
                    { "name": "name", "type": "text", "unique": true },
                    { "name": "explore", "type": "bool", "default": false },
                    { "name": "preferences", "type": "text", "optional": true },
                    { "name": "position", "type": "number", "default": 0 },
                    { "name": "explore_avoid", "type": "text", "optional": true },
                    { "name": "explore_prefer", "type": "text", "optional": true },
                ],
                "order_by": "position",
            }),
            json!({
                "name": "shop_needs", "title": "Shopping",
                "fields": shop_need_fields(),
                "order_by": ["bought", "name"],
            }),
            json!({
                "name": "shop_rounds", "title": "Explore rounds",
                "fields": [
                    { "name": "need", "type": "ref", "collection": "shop_needs" },
                    { "name": "n", "type": "number", "default": 0 },
                    { "name": "query", "type": "text", "optional": true },
                    { "name": "label", "type": "text", "optional": true },
                    { "name": "like", "type": "text", "optional": true },
                    { "name": "unlike", "type": "text", "optional": true },
                    { "name": "exclude", "type": "text", "optional": true },
                    { "name": "how_far", "type": "number", "optional": true },
                    { "name": "avoid", "type": "text", "optional": true },
                    { "name": "prefer", "type": "text", "optional": true },
                    { "name": "distance", "type": "number", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.how_far ? entity.how_far : entity.n < 1 ? 0.5 : entity.n < 2 ? 0.3 : entity.n < 3 ? 0.18 : 0.11" } },
                    { "name": "spread", "type": "number", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.n < 1 ? 1 : entity.n < 2 ? 0.6 : entity.n < 3 ? 0.4 : 0.25" } },
                    { "name": "group", "type": "bool", "default": false,
                      "compute": { "kind": "expr", "expr": "entity.n < 1" } },
                    { "name": "title_text", "type": "text", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.n < 1 ? \"The kinds\" : \"Round \" + entity.n" } },
                ],
                "order_by": "n",
            }),
            json!({
                "name": "shop_pieces", "title": "Explore pieces",
                "fields": [
                    { "name": "round", "type": "ref", "collection": "shop_rounds", "optional": true },
                    { "name": "need", "type": "ref", "collection": "shop_needs", "optional": true },
                    { "name": "taste_key", "type": "text", "optional": true },
                    { "name": "title", "type": "text", "optional": true },
                    { "name": "brand", "type": "text", "optional": true },
                    { "name": "image", "type": "text", "optional": true },
                    { "name": "price", "type": "number", "optional": true },
                    { "name": "price_text", "type": "text", "optional": true },
                    { "name": "url", "type": "text", "optional": true },
                    { "name": "host", "type": "text", "optional": true },
                    { "name": "description", "type": "text", "optional": true },
                    { "name": "distance", "type": "number", "optional": true },
                    { "name": "group", "type": "number", "optional": true },
                    { "name": "group_label", "type": "text", "optional": true },
                    { "name": "group_ids", "type": "text", "optional": true },
                    { "name": "group_size", "type": "number", "optional": true },
                    { "name": "group_prices", "type": "text", "optional": true },
                    { "name": "reaction", "type": "choice", "options": ["more", "less"], "optional": true },
                    { "name": "shortlisted", "type": "bool", "default": false },
                    { "name": "avoid", "type": "text", "optional": true },
                    { "name": "prefer", "type": "text", "optional": true },
                    { "name": "web", "type": "bool", "default": false,
                      "compute": { "kind": "expr", "expr": "!entity.taste_key" } },
                ],
                "fillers": [{
                    "kind": "taste", "title": "Explore",
                    "for_each": { "collection": "shop_rounds", "link": "round" },
                    "request": { "query": "query", "like": "like", "unlike": "unlike",
                                 "exclude": "exclude", "distance": "distance",
                                 "spread": "spread", "group": "group",
                                 "avoid": "avoid", "prefer": "prefer" },
                    "map": { "taste_key": "key", "title": "title", "brand": "brand",
                             "image": "image", "price": "price", "price_text": "price_text",
                             "url": "url", "host": "host", "description": "description",
                             "distance": "distance", "group": "group",
                             "group_label": "group_label", "group_ids": "group_ids",
                             "group_size": "group_size", "group_prices": "group_prices" },
                    "copy": { "need": "need", "avoid": "avoid", "prefer": "prefer" },
                    "key": "taste_key",
                    "keep": [{ "field": "shortlisted", "equals": true }],
                }, {
                    "kind": "agent", "title": "Find options", "prompt": options,
                    "fields": ["title", "brand", "price", "price_text", "url", "image", "host", "description"],
                    "for_each": { "collection": "shop_needs", "link": "need",
                                  "where": [{ "field": "find_web", "equals": true }],
                                  "from": ["name", "notes", "quantity", "category", "preferences"] },
                    "key": "url",
                    "keep": [{ "field": "shortlisted", "equals": true }],
                    "max_searches": 6, "max_fetches": 8, "fills_per_day": 6,
                }],
            }),
            json!({
                "name": "shop_picks", "title": "Shopping picks",
                "fields": [
                    { "name": "need", "type": "ref", "collection": "shop_needs" },
                    { "name": "product", "type": "text" },
                    { "name": "brand", "type": "text", "optional": true },
                    { "name": "store", "type": "text", "optional": true },
                    { "name": "url", "type": "text", "optional": true },
                    { "name": "image", "type": "text", "optional": true },
                    { "name": "taste_key", "type": "text", "optional": true },
                    { "name": "chosen_price", "type": "number", "optional": true },
                    { "name": "chosen_on", "type": "date", "optional": true },
                    { "name": "watch", "type": "bool", "default": true },
                    { "name": "target", "type": "number", "optional": true },
                    { "name": "price_now", "type": "number", "optional": true },
                    { "name": "stock_now", "type": "text", "optional": true },
                    { "name": "why", "type": "text", "optional": true },
                    { "name": "pros", "type": "text", "optional": true },
                    { "name": "cons", "type": "text", "optional": true },
                    { "name": "reviews", "type": "text", "optional": true },
                    { "name": "sources", "type": "text", "optional": true },
                    { "name": "price", "type": "number", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.price_now ? entity.price_now : entity.chosen_price" } },
                    { "name": "price_text", "type": "text", "optional": true,
                      "compute": { "kind": "expr", "expr": "money(entity.price)" } },
                    { "name": "chosen_text", "type": "text", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.price_now ? entity.chosen_price ? \"was \" + money(entity.chosen_price) : \"\" : \"\"" } },
                    { "name": "price_query", "type": "text", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.watch ? entity.product + (entity.store ? \" \" + entity.store : \"\") : \"\"" } },
                    { "name": "target_text", "type": "text", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.target ? money(entity.target) : \"Not set\"" } },
                    { "name": "review_query", "type": "text", "optional": true,
                      "compute": { "kind": "expr", "expr": "entity.product + \" review\"" } },
                ],
                "fillers": [{
                    "kind": "fetch", "title": "Daily price check", "query_field": "price_query",
                    "extract": "llm", "fields": ["price_now", "stock_now"], "hint": price_hint,
                    "refresh": "every", "every_minutes": 1440, "on_empty": "keep",
                }, {
                    "kind": "fetch", "title": "About this pick", "query_field": "review_query",
                    "extract": "llm", "fields": ["why", "pros", "cons", "reviews", "sources"],
                    "hint": research, "refresh": "once",
                }],
                "snapshots": [{ "into": "shop_prices", "link": "pick", "day": "day",
                                "fields": { "price": "price", "in_stock": "stock_now" } }],
            }),
            json!({
                "name": "shop_prices", "title": "Shopping prices",
                "fields": [
                    { "name": "pick", "type": "ref", "collection": "shop_picks" },
                    { "name": "day", "type": "date" },
                    { "name": "price", "type": "number", "optional": true },
                    { "name": "in_stock", "type": "text", "optional": true },
                ],
                "order_by": "day",
            }),
        ],
        changes: vec![
            (
                "shop_picks",
                vec![
                    json!({ "op": "add_field", "name": "low", "type": "number", "optional": true,
                        "compute": { "kind": "rollup", "collection": "shop_prices", "via": "pick", "op": "min", "field": "price" } }),
                    json!({ "op": "add_field", "name": "high", "type": "number", "optional": true,
                        "compute": { "kind": "rollup", "collection": "shop_prices", "via": "pick", "op": "max", "field": "price" } }),
                    json!({ "op": "add_field", "name": "points", "type": "number", "default": 0,
                        "compute": { "kind": "rollup", "collection": "shop_prices", "via": "pick", "op": "count" } }),
                    json!({ "op": "add_field", "name": "was_out", "type": "number", "default": 0,
                        "compute": { "kind": "rollup", "collection": "shop_prices", "via": "pick", "op": "count",
                                     "where": [{ "field": "in_stock", "equals": "Sold out" }] } }),
                    json!({ "op": "add_field", "name": "range_text", "type": "text", "optional": true,
                        "compute": { "kind": "expr", "expr": "entity.points > 1 ? \"Low \" + money(entity.low) + \" · high \" + money(entity.high) : \"\"" } }),
                    json!({ "op": "add_field", "name": "under_target", "type": "bool", "default": false,
                        "compute": { "kind": "expr", "expr": "entity.target ? entity.price <= entity.target : false" } }),
                    json!({ "op": "add_field", "name": "dropped", "type": "bool", "default": false,
                        "compute": { "kind": "expr", "expr": "entity.chosen_price ? entity.price * 10 <= entity.chosen_price * 9 : false" } }),
                    json!({ "op": "add_field", "name": "new_low", "type": "bool", "default": false,
                        "compute": { "kind": "expr", "expr": "entity.points > 1 ? (entity.price <= entity.low ? entity.price < entity.high : false) : false" } }),
                    json!({ "op": "add_field", "name": "deal", "type": "text", "optional": true,
                        "compute": { "kind": "expr", "expr": "entity.watch ? entity.under_target ? \"Under your target\" : entity.dropped ? \"Down from \" + money(entity.chosen_price) : entity.new_low ? \"Lowest since you chose it\" : \"\" : \"\"" } }),
                    json!({ "op": "add_field", "name": "back", "type": "text", "optional": true,
                        "compute": { "kind": "expr", "expr": "entity.watch ? entity.was_out ? entity.stock_now : \"\" : \"\"" } }),
                    json!({ "op": "add_alert", "name": "Price drops", "label": "Price drops",
                        "any": ["under your target", "down from", "lowest since"], "fields": ["deal"],
                        "title": "{product}: {price_text}", "body": "{deal} at {store}.", "repeat": true }),
                    json!({ "op": "add_alert", "name": "Back in stock", "label": "Back in stock",
                        "all": ["in stock"], "fields": ["back"],
                        "title": "{product} is back in stock", "body": "At {store}, {price_text}.", "repeat": true }),
                ],
            ),
            (
                "shop_needs",
                vec![
                    json!({ "op": "add_field", "name": "pick_count", "type": "number", "default": 0,
                        "compute": { "kind": "rollup", "collection": "shop_picks", "via": "need", "op": "count" } }),
                    json!({ "op": "add_field", "name": "watching", "type": "bool", "default": false,
                        "compute": { "kind": "rollup", "collection": "shop_picks", "via": "need", "op": "any",
                                     "where": [{ "field": "watch", "equals": "true" }] } }),
                    json!({ "op": "add_field", "name": "pick_line", "type": "text", "optional": true,
                        "compute": { "kind": "rollup", "collection": "shop_picks", "via": "need", "op": "list", "field": "product" } }),
                    json!({ "op": "add_field", "name": "pick_deal", "type": "text", "optional": true,
                        "compute": { "kind": "rollup", "collection": "shop_picks", "via": "need", "op": "list", "field": "deal" } }),
                    json!({ "op": "add_field", "name": "round_count", "type": "number", "default": 0,
                        "compute": { "kind": "rollup", "collection": "shop_rounds", "via": "need", "op": "count" } }),
                    json!({ "op": "add_field", "name": "shortlist_count", "type": "number", "default": 0,
                        "compute": { "kind": "rollup", "collection": "shop_pieces", "via": "need", "op": "count",
                                     "where": [{ "field": "shortlisted", "equals": "true" }] } }),
                    json!({ "op": "add_field", "name": "like_ids", "type": "text", "optional": true,
                        "compute": { "kind": "rollup", "collection": "shop_pieces", "via": "need", "op": "list", "field": "taste_key",
                                     "where": [{ "field": "reaction", "equals": "more" }] } }),
                    json!({ "op": "add_field", "name": "unlike_ids", "type": "text", "optional": true,
                        "compute": { "kind": "rollup", "collection": "shop_pieces", "via": "need", "op": "list", "field": "taste_key",
                                     "where": [{ "field": "reaction", "equals": "less" }] } }),
                    json!({ "op": "add_field", "name": "shown_ids", "type": "text", "optional": true,
                        "compute": { "kind": "rollup", "collection": "shop_pieces", "via": "need", "op": "list", "field": "taste_key" } }),
                    json!({ "op": "add_field", "name": "state", "type": "text", "optional": true,
                        "compute": { "kind": "expr", "expr": "entity.bought ? \"bought\" : entity.pick_count ? entity.watching ? \"watching\" : \"ready\" : \"choosing\"" } }),
                    json!({ "op": "add_field", "name": "status", "type": "text", "optional": true,
                        "compute": { "kind": "expr", "expr": "entity.bought ? \"Bought\" : entity.pick_count ? entity.pick_line : entity.round_count > 1 ? \"Exploring · round \" + (entity.round_count - 1) + (entity.shortlist_count ? \" · \" + entity.shortlist_count + \" shortlisted\" : \"\") : entity.round_count ? \"Exploring · the kinds\" : \"Nothing chosen yet\"" } }),
                ],
            ),
            (
                "shop_rounds",
                vec![
                    json!({ "op": "add_field", "name": "need_rounds", "type": "number", "optional": true,
                        "compute": { "kind": "lookup", "via": "need", "value": "round_count" } }),
                    json!({ "op": "add_field", "name": "current", "type": "bool", "default": false,
                        "compute": { "kind": "expr", "expr": "entity.n + 1 >= entity.need_rounds" } }),
                ],
            ),
            (
                "shop_pieces",
                vec![
                    json!({ "op": "add_field", "name": "current", "type": "bool", "default": false,
                        "compute": { "kind": "lookup", "via": "round", "value": "current" } }),
                ],
            ),
        ],
    }
}
