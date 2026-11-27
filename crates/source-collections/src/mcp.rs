//! The Collections MCP tools (decision `collections`): Claude declares
//! collections, changes them together with the apps that show them, and
//! edits rows (confirmed, undoable). This is the one place an MCP
//! credential touches app data, and only Collections' own tables.

use std::collections::HashMap;

use axum::http::StatusCode;
use pod_core::layouts::{self as pod_layouts, SourceUse};
use pod_core::mcp::{Access, untrusted};
use pod_core::{BoxFuture, Pod};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::changes::{self, Planned};
use crate::db::{self, Row};
use crate::model::{self, Definition, Write};
use crate::{Caps, CollectionsAdapter, KEEP_SECS, NAME, fail, refusable};

pub const INSTRUCTIONS: &str = "Data a layout keeps itself (a todo list, a reading log) lives in \
Collections: define_collection declares typed fields, and any layout can then list the collection \
as a feed ([\"collections\", \"<name>\"]) and add, change and delete rows with the usual mutation \
intents (describe_collection gives ready-made layout snippets). Change a collection with apply_change: \
nothing applies if any layout would break (change that layout with update_layout first). add_row adds one row the user just asked for (\"log that I had the oil changed\") at once. \
edit_rows changes rows directly (imports, backfills; all of them in one call, from inline edits or a file \
by url): it first answers a preview, which you must show the user (the count and the sample) before \
calling again with just its confirm token; every edit \
can be undone with undo_edit. delete_collection deletes a collection the user no longer wants once no layout \
shows it (restore_collection brings it back for 30 days). The pod's notifications are the system \
collection notifications (one row per notification: title, body, channel_layout, channel, layout, target_uri, \
subject, at, read, read_at, channel_enabled and channel_title from its channel, and subject_alert_matched: false once the row it is about no longer matches an alert of its collection, e.g. after the alert was narrowed; the newest 500 of 30 days; \
actions mark_all_read and clear_read) and its channels notification_channels (key \"<layout>.<channel>\", title, \
enabled: the pod-wide switch, remove_with_subject: whether a notification goes when the row it is about is \
deleted): bind, filter, edit or delete them like any collection (the inbox core layout shows notifications \
whose channel_enabled and subject_alert_matched are true, read ones older than a day in a closed section). Any field may be computed from the row's other fields \
whenever they change: compute: {kind: \"expr\", expr} evaluates a small expression at once (entity.<field>, \
entity.id (the row's id), literals, + joins text or adds numbers, -, *, !, < <= > >= (numbers, or texts in \
order: dates), c ? a : b, parentheses, money(x) for a price ($98, $28.50); an expression may read its own field's last value, e.g. a running lowest \
price: entity.now ? (entity.low ? (entity.now < entity.low ? entity.now : entity.low) : entity.now) : entity.low); \
compute: {kind: \"lookup\", from, \
collection, key, value, match: exact|contains|regex, learn?} takes the value of the best matching row of \
another collection, kept as editable rules (the longest key wins, then the oldest row); value: \"id\" \
answers that row's id instead (a text field, no learn), e.g. to delete the matching row with \
/v1/collections/<collection>/delete {id}; compute: {kind: \"count\", from, \
collection, key, where?: [{field, equals}]} (a number field) counts the rows of another collection whose key \
equals this row's from and that match every condition; and compute: {kind: \"model\", from: \
[fields], prompt} has a model answer the prompt about those fields in the background (a choice answers one \
of its options; each input is asked once, and a value set by hand is kept and remembered for that input). \
compute may be a list tried in order, e.g. [lookup with learn: true, model]: rules first, the model only \
when none matches, and its answers (and hand edits) are added as rules. Lookups and counts are live: \
when rows of the collection they read are added, changed, deleted or restored, the rows whose answer could \
change are computed again at once (and that cascades, so a count's where may name a lookup field of the \
collection it counts). apply_change {op: recompute_field} computes a field again for every row. \
order_by may list several fields ([\"section\", \"done\"]; a choice sorts by the order of its options). \
A collection can remind the user: apply_change {op: add_reminder, field: <a date field>, title: \"Due today: \
{title}\", unless: {field: \"done\", equals: true}, at?: \"09:00\"} sends a notification from the app showing \
the collection once a row's date is reached (at that time of day in the user's time zone), unless the row \
then matches unless. Set the zone first with set_time_zone (ask the user if you don't know it). \
A collection can keep itself current from the web with fillers (define_collection fillers, or apply_change \
{op: set_filler}; several per collection, e.g. one per website, each writing only its own rows): {kind: \
\"fetch\", url, extract: events|json|embedded-json|jsonld|rss|ics|selectors|llm, items?, types?, map: \
{field: path}} is a recipe the pod runs on a schedule; prefer it: try the page with collections_preview \
first (without map it shows sample items to write paths from) and pick the cheapest extractor that gives \
the fields: events (an ics feed or a page's schema.org events, normalized), an API's json, jsonld, rss or \
ics, embedded-json, CSS selectors, and only then llm (a model reads each changed page; fields and hint \
instead of map). {kind: \"agent\", prompt} has a model with web search answer the rows (each fill costs \
model calls; budgeted by fills_per_day): use it only when no page holds the data. Combine them for \
shopping: an agent finds the items once, a fetch filler with url_field keeps each row's price current. \
To research each row of a list people type (a shopping list's items, each with its own options), put an \
agent with for_each: {collection: <the list>, link: <a ref field here to it>, where?, from?, context?} on \
a collection of options: it answers each item on its own (one model call each, budgeted by fills_per_day), \
with the item's fields and context rows (the owner's preferences, a style deck's likes) in the request, and \
its rows here point back at the item; an item is researched again when its text or the context changes \
(refresh once), or also once every_minutes have passed since its research (refresh every, every_minutes 10080: weekly), soon after a \
row is written. keep: [{field, equals}] keeps rows someone marked (in_bag) when a refill leaves them out. \
For shopping start from the shopping kit (use_kit shopping, decision shopping-kit): needs explored through \
the taste service in rounds, picks with a daily price history and price alerts; add a need with add_row on \
shop_needs {name, category}, nothing else. \
With the web_search jack, links in its answer must be pages it saw and quotes must be on pages it read \
(others are left out), and web_fetch lists each page's main pictures, so an image field gets the store's \
own photo. \
For a list people type themselves (a shopping list: where to buy each item), a fetch filler with query_field \
(a field holding each row's search, e.g. its name), extract llm and fields (the store, price, url and why \
fields to fill) searches the web for each row with the pod's web_search jack and has a model pick from the \
results; it fills each row once (refresh once) and an empty or blocked search never erases a row's values \
(on_empty keep). It needs the web_search jack plugged (list_jacks; plug_jack web_search) and the model jack. \
For pictures (a product photo for each row, from sites that block fetches), a query_field filler with extract \
images and map {field: image | thumbnail | page | title | source} fills each row from its first image result, \
with no model (Brave or SearXNG; DuckDuckGo has no image search). To let people pick a row's picture, a \
screen with data /v1/collections/<c>/images?id=<row> lists its image results (query \
collection:<c>:images:<row>, entities {image, thumbnail, page, title, source, n, row_id}); a tap saves one \
with an ordinary update (payload {id: \"$item.row_id\", <field>: \"$item.image\"}) into a plain field the \
displayed picture prefers (an expr compute: entity.my_photo ? entity.my_photo : entity.found_photo). \
every_minutes schedules fills; on_open_minutes fills when the app opens and the data is older; upcoming \
(a date field) drops past items; zone places times without an offset and moves UTC (Z) times to its clock; details reads each new row's own \
page. A page whose content a script builds (the raw HTML has no items) or that answers a plain fetch with \
a bot check takes render: true (or {wait_for: <CSS selector>, timeout_seconds?: 5..=45}) on a url or url_field \
filler (and on collections_preview): the server's headless browser loads it, waits for the network to go quiet \
or the selector to appear, and the extractors read the rendered HTML. Rendering is slow and capped (one browser \
at a time, 100 pages a pod a day, every_minutes and on_open_minutes at least 360, no headers): use it only when \
a preview without it finds nothing. A site that still refuses is reported as blocked in collections_status and \
the pod backs off; it never solves a captcha. A datetime field (2026-11-06T19:30-07:00) is also served as <name>_date, <name>_time and \
<name>_text. Alerts (apply_change {op: add_alert, any?, all?, none?, filler?, days?, within_days?, repeat?}) notify \
once when a row matches, e.g. a show listing a team; with repeat: true a row notifies again each time it \
comes back into a match (a deal field an expr fills: a price that drops again). collections_status shows each filler's health: say so \
when one is failing or finds nothing. An API key goes in with collections_set_secret (with the host it is for) and is sent as a \
header value \"{secret:<name>}\", only to that host; ask the user for it and never repeat it. The Events app (app events) \
shows the collection events: to watch a website for events, add a filler there (extract events, map \
{source_id: id, title: title, start: start, end: end, venue: venue, url: url, tickets_url: tickets_url}, \
key source_id, upcoming start, every_minutes 360) and alerts for what the user wants to hear about. \
Rows can point at rows: a field {type: \"ref\", collection} holds the id of a row of that collection (write an id or \
{\"<unique field>\": value}; a layout reads through it with ${collections://<collection>/$item.<ref>#<field>}), and \
unique: true keeps a text or ref field unique. compute {kind: \"lookup\", via: <ref field>, value} reads a field of the \
row a ref points at; {kind: \"rollup\", collection, via, op: count|sum|min|max|any|list, field?, where?} aggregates \
the rows of another collection whose ref via points at this row (prefer it to count). A field {type: \"quantity\"} \
holds an amount and a unit parsed from text (\"1 1/2 cups\"), served also as <name>_text; a rollup sum adds \
quantities, converting units. An action (define_collection actions, apply_change set_action) is a named \
insert-from-query: for each row of a source collection matching where, it finds or creates a row of this collection \
by match and adds a child row to another collection (or, with delete: true, deletes the rows of this collection \
matching where, e.g. a grocery list's \"Clear bag\"; or, with only update, updates them, e.g. the \
notifications' \"Mark all read\"); run it with run_action, or from a layout with \
action.mutation.optimistic posting to /v1/collections/<collection>/actions/<action>; undo_edit reverts a call. \
A calendar app (Proton Calendar's Add calendar from link) can subscribe to rows: set_calendar_feed, where the server \
lists it, maps rows of collections to events (start, end or time fields, title templates); an event the user \
mentions that no filler fetches goes in a plain collection that is one of the feed's sources, with add_row.";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

pub fn tools() -> Vec<Value> {
    let collection = json!({ "type": "string", "description": "A collection's name" });
    let note = json!({ "type": "string", "description": "What changed, in the user's words (shown in the history)" });
    let kind = json!({ "type": "object",
        "properties": { "kind": { "type": "string", "enum": ["expr", "lookup", "count", "rollup", "model"] }, "expr": { "type": "string" },
            "via": { "type": "string", "description": "lookup: a ref field of this collection (read the row it points at); rollup: the ref field of the rolled-up collection that points at this collection" },
            "op": { "type": "string", "enum": model::RollupOp::ALL, "description": "rollup: what it computes" },
            "field": { "type": "string", "description": "rollup: the field of the rolled-up rows (sum, min, max, list)" },
            "from": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] },
            "prompt": { "type": "string" }, "collection": { "type": "string" }, "key": { "type": "string" },
            "value": { "type": "string" }, "match": { "type": "string", "enum": model::Match::ALL },
            "learn": { "type": "boolean" },
            "where": { "description": "count, rollup: conditions every counted row matches, {field, equals} or a list of them",
                "anyOf": [{ "type": "object" }, { "type": "array", "items": { "type": "object",
                    "properties": { "field": { "type": "string" }, "equals": { "type": ["string", "number", "boolean"] } },
                    "required": ["field", "equals"] } }] } }, "required": ["kind"] });
    let compute = json!({
        "description": "The pod computes the value from the row's other fields (not from its other computed fields; a lookup or count may read computed fields of the collection it reads), again whenever they change; a value written by hand stays until they change (for a lookup or count, also until the rows it reads change). One kind, or a list of kinds tried in order (the first with an answer wins; a model is last). kind \"lookup\": {kind, from: field, collection, key, value, match?: exact (the default; ignores case and spacing) | contains (the key's whole words, ignoring case) | regex (case-insensitive, at most 200 characters; an invalid one is skipped and listed by describe_collection), learn?}: the value field of the row of another collection whose key (a text field) matches this row's from field, the longest matching key winning, then the oldest row (deleted rows never match); value may be \"id\" (the only system name allowed), which answers the matched row's id as text (the field must be text; no learn), so a button can act on that row (e.g. delete it); evaluated in the write, and live: when a row of that collection is added, changed (its key or value), deleted or restored, the rows whose from matched its key before or after are computed again at once. learn: true adds a rule (key: the input, lowercase with spaces collapsed; for match regex, that text escaped and anchored) when a later kind (the model) answers, and a hand edit of the field adds or changes that rule; a collection a learning lookup names that does not exist yet is made (key: a text field, value: of this field's type and options). Example: [{kind: \"lookup\", from: \"name\", collection: \"section_rules\", key: \"pattern\", value: \"section\", match: \"contains\", learn: true}, {kind: \"model\", from: \"name\", prompt: \"...\"}]. A lookup through a ref: {kind: \"lookup\", via: <a ref field of this collection>, value: <a field of the collection it points at, or \"id\">} (no from, collection, key, match or learn): the value of the row the ref points at (empty while the ref is empty or that row is deleted; it comes back on restore); the field's type is the value's type (or text); live: computed again when the ref changes and when that row's value changes, is deleted or restored. Example: recipe_lines.favorite = {kind: \"lookup\", via: \"meal\", value: \"favorite\"}. kind \"rollup\": {kind, collection, via, op, field?, where?: [{field, equals}]}: aggregates the live rows of another collection whose ref field via points at this row and that match every condition (as count's where; conditions may name computed fields of that collection, e.g. a lookup through a ref): op count (a number field: how many), any (a bool field: whether there is one), sum (field: a number field into a number field, or a quantity field into a quantity field; quantities of one dimension convert and add, keeping the units written: one unit stays (4 tbsp + 2 tbsp = 6 tbsp), several make one amount of the largest when a plain fraction (1/2 cup + 2 tbsp = 5/8 cup), else stay apart (1 cup + 3 tbsp); other units stay separate parts, e.g. 2 cups + 3 cloves; text that didn't parse as a quantity is left out), min or max (field: a number field, into a number field), list (field: any field, into a text field: the distinct values as text in row order, joined by \", \"). With no rows: count 0, any false, a number sum 0, the others empty (the field's default). The field's only kind; live (computed again when those rows are added, deleted, restored or change their via, field or a where field). Prefer it to count for new fields. Examples: ingredients.favorite_count = {kind: \"rollup\", collection: \"recipe_lines\", via: \"ingredient\", op: \"count\", where: [{field: \"favorite\", equals: true}]}; groceries.total = {kind: \"rollup\", collection: \"grocery_entries\", via: \"grocery\", op: \"sum\", field: \"quantity\"}. kind \"count\": {kind, from: field, collection, key, where?: [{field, equals}]}: the number of live rows of another collection whose key equals this row's from field (ignoring case and spacing; a datetime key by the day it falls on, so a date counts the rows starting that day) and that match every condition (a field of that collection, computed ones too, whose value as text equals equals: \"true\", \"false\", a number or text; not blank); a number field's only kind; live like a lookup (computed again when those rows are added, deleted, restored or change their key or a where field, and when from changes). Example: {kind: \"count\", from: \"name\", collection: \"meal_ingredients\", key: \"item\", where: [{field: \"favorite\", equals: true}]}. kind \"subject\": {kind, from: a text field holding an entity URI (collections://<c>/<id>), value: a field or derived name that row serves, e.g. alert_matched}: what the row the URI names serves as value, whatever its collection (empty while it names no live row); live: computed again when that row or its collection's definition (an alert) changes, and hourly. The notifications' subject_alert_matched is one. A change cascades through lookups and counts (a few levels deep; a cycle stops). kind \"expr\": {kind, expr}, e.g. \"entity.quantity ? entity.name + \\\" (\\\" + entity.quantity + \\\")\\\" : entity.name\" (entity.<field>, entity.id: the row's own id, JSON literals, + joins text or adds numbers, -, !, c ? a : b, parentheses, money(x): a number as a price ($98, $28.50, $1,299.95); an empty value is \"\" or 0 and false in a condition), evaluated in the write. kind \"model\": {kind, from: field or [fields], prompt}, e.g. {from: \"name\", prompt: \"Which grocery store section is this item in?\"}: a model answers in the background (a choice: one of its options; text: trimmed), each input once, many inputs to a call; an answer that fails is asked again hourly, a few times (describe_collection counts computed_answers[].failed; recompute_field starts again); a value set by hand is remembered as the answer for that input. Needs optional or a default (what the row reads until computed). null: stop computing.",
        "anyOf": [kind, { "type": "array", "items": kind }, { "type": "null" }] });
    let field = json!({ "type": "object", "description": "{name, type: text|number|bool|date|datetime|choice|ref|quantity, options? (choice), collection? (ref), unique?, optional?, default?, compute?}. type \"ref\" with collection: <name>: holds the id of a live row of that collection; a write gives the id (a row's id field) or {\"<a unique field of that collection>\": value} (e.g. {\"name\": \"Chicken broth\"}, matched ignoring case and spacing), and an id that names no live row is refused; a ref has no default; served as the id; a layout reads the row's fields with ${collections://<collection>/$item.<ref>#<field>} (that collection must be one of the layout's feeds); when the row is deleted the ref keeps its id (reading through it shows nothing) and a restore heals it. unique: true (text or ref, not computed): no two live rows share a value (text ignoring case and spacing); a create, update or restore that would is refused, and making a field unique fails while rows share a value (the error lists them). type \"quantity\": an amount and a unit; write text (\"1 1/2 cups\", \"2 tbsp\", \"3\", \"1 (14 oz) can\", \"1/4 cup plus 2 tbsp\", \"a pinch\", \"2-3 cloves\" (the upper end), \"½ cup\"), a number (a count) or {amount, unit} (or a list of those); it is stored as {amount, unit} (unit canonical: tsp, tbsp, fl oz, cup, pint, quart, ml, l, g, kg, oz, lb, clove, can, slice, bunch, large, medium, small, piece, pinch, dash, \"\" for a plain count, or any other single word, singular), or kept as the text when it doesn't parse (\"to taste\": shown as is, left out of sums; describe_collection counts these); served also as <name>_text (\"1 1/2 cups\", \"2 Tbsp\", \"2 cups + 3 cloves\"; one spelling per unit, tablespoons always Tbsp): bind that to show it.",
        "properties": { "name": { "type": "string" }, "type": { "type": "string", "enum": model::Kind::ALL },
            "options": { "type": "array", "items": { "type": "string" } }, "optional": { "type": "boolean" }, "default": {},
            "collection": { "type": "string", "description": "ref: the collection it points at" },
            "unique": { "type": "boolean", "description": "text or ref: no two live rows share a value" },
            "compute": compute },
        "required": ["name", "type"] });
    let order = json!({ "description": "A field, or a list of fields (first key first), rows are listed by (else oldest first). A choice sorts by the order of its options; empty values last.",
        "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] });
    let filler = json!({ "type": "object",
        "description": "A filler: what keeps rows current from the web (decision collection-fillers); each writes only its own rows. kind \"fetch\" (a recipe, no model call per fill): {kind, title?, url (one page) | url_field (a text field holding each row's own page; that row is filled from it) | query_field (a text field holding each row's web search, e.g. its name: the row's item is searched with the pod's web_search jack and a model fills its fields from the results with extract llm, or with extract images (no model; Brave or SearXNG) its first image result fills map {field: image (the picture's URL) | thumbnail | page (where it is) | title | source (the site)}; people may add rows), extract: events (an ics feed or a page's schema.org events: items {id, title, start, end, venue, description, url, tickets_url}) | json | embedded-json (a page's <script id=script, default __NEXT_DATA__>) | jsonld (schema.org objects of types, e.g. [\"Product\"]) | rss (items {title, link, description, published, updated, id, author, category}) | ics (VEVENT properties lowercase: summary, dtstart, location, ...) | selectors (CSS) | llm (a model reads each changed page's text: fields? [names] and hint? instead of map), items? (json/embedded-json: a path to the list; selectors: a CSS selector per item), types?, script?, zone? (IANA, for times without an offset; UTC times move to its clock), map: {field: path} (paths: a.b, a[0], a[*]; a list key is looked into, so offers.price works; a path may end in filters: \"description | link\" the first link in it (Google redirects unwrapped), \"description | text\" its text without HTML; \"=<text>\" a constant every row gets, e.g. going: \"=true\"; selectors: a CSS selector inside the item, sel@attr for an attribute, @attr for the item's own), headers? {name: value, value may be \"Bearer {secret:<name>}\"}, details? {link: a mapped field with the row's page, extract, types?, map: {field: path} (fields not in map), text?: a field for the page's text when nothing is found; or extract \"llm\" with fields: [names] and hint?: a model reads the page's text for those fields}: read each new row's own page, once (a model call per new row with llm)}. kind \"agent\" (a model with web search and fetch answers the rows; costs model calls): {kind, title?, prompt, fields? (default every field that isn't computed or a bool with a default, the user's toggles: a listed field is locked on its rows), max_searches? (default 5, at most 20), max_fetches? (default 10, at most 30), fills_per_day? (default 4), for_each? {collection (another collection), link (a ref field here to it, set by the pod), where?: [{field, equals}] (its rows researched), from?: [its fields in the request; default every field but true/false ones and refs], context?: [{collection, where?, fields?, limit? (default 50, at most 100)}] (rows given with every request)}: research each row of that collection on its own, one model call per row (each counts toward fills_per_day), its rows here linked back; refresh? once (default: again when the row's text or the context changes) | every}. kind \"taste\" (the owner's taste service through the taste jack; no web, no model; decision shopping-kit): {kind, title?, for_each: {collection, link, where?}, request: {part: a field of that collection} (parts: query, like, unlike, exclude (piece keys, comma-separated), distance, spread (0..1), group (true: also cluster and label them), n, price_max; query or like required), map: {field: key | title | brand | image | thumb | price | price_text | url | host | description | category | distance | group | group_label | group_ids | group_size | group_prices}, copy?: {field here: field of that row} (e.g. the need a round is for), key? (key)}: explores once for each row of that collection, as soon as one is written, its pieces here linked back. kind \"range\" (no web, no model): one row per day, week or month of a window that moves with today in the pod's zone, e.g. a calendar's days: {kind, from: \"{today}\" | \"{today+N}\" | \"{today-N}\", to (likewise), step? day (default) | week (Monday to Sunday) | month, map: {field: date (the step's first day, YYYY-MM-DD) | end (its last day) | weekday (Mon) | weekday_long | day (16) | month (Nov) | month_long | year | week (ISO number) | index (0 for the first)}, key? (give the date field, so a day keeps its row), every_minutes? (default 60)}: at most 200 rows; days that leave the window are deleted; pair it with count or lookup fields over other collections keyed by date. Every kind: mode? replace (default: its rows are the latest answer; rows it no longer lists are deleted; they can't be added or deleted by hand) | upsert (adds and updates rows by key, keeps others; people may add rows), key? (a mapped field identifying a row; with replace, rows keep their ids and unfilled fields), upcoming? (a mapped date or datetime field: past items are left out), every_minutes? (fetch: 15..10080; agent: 60..10080), on_open_minutes? (1..1440: fill when an app showing it opens and the last fill is older), keep? [{field, equals}] (rows matching every condition stay when a refill leaves them out); url_field and query_field fillers also take refresh? (once: fill a row when its URL or query first appears or changes; every: on each fill, skipping pages whose text didn't change; default every for url_field, once for query_field) and on_empty? (keep, the default: a row whose page or search yields nothing, or is blocked, keeps its values; clear). Every request goes through the pod's host policy (spacing, a daily cap per site, a shared backoff after a site blocks the pod). Values are coerced to the fields' types (prices like \"$1,299\" and feed dates are read); a value that doesn't fit is skipped. A field a filler fills can't be set by hand on that filler's rows (a url_field or query_field filler's on every row)." });
    let alert = json!({ "type": "object",
        "description": "An alert: notify once when a row matches. {name?, any?: [phrases] (one must appear), all?: [phrases], none?: [phrases], fields?: [text fields searched; default all text fields], filler?: a filler id (only its rows), days?: [mon..sun], within_days?, date_field? (default the first date or datetime field; past rows stop matching), title?, body? (templates: {field}, {start_text}, {alert}, {filler}), label?, app?, repeat? (true: notify again each time a row comes back into a match)}" });
    let view = json!({ "type": "object",
        "description": "A view: a small derived list of the rows, served as its own query key collection:<collection>:view:<name> beside collection:<collection> (bind a core://List's query_key to it; $item.<field> works as for the whole list). {name ([a-z][a-z0-9_]*, unique in the collection), where?: {field, equals} or a list (all must match; compared as text, as a list's where: true, false, numbers), date?: {field (a date or datetime field), from_days?, to_days?}: the row's date (a datetime by its date in the pod's time zone, set_time_zone) from from_days to to_days days after today (0 today, -1 yesterday; either bound open; empty dates never match), order_by?: field or [fields] (else the collection's order), descending?, limit? (1..=200)}. At most 10 per collection. Examples: {name: \"upcoming\", date: {field: \"start\", from_days: 0}, order_by: \"start\", limit: 3}; {name: \"today\", date: {field: \"day\", from_days: 0, to_days: 0}}; {name: \"next_week\", date: {field: \"day\", from_days: 1, to_days: 7}}. Dated views move at the pod's midnight." });
    let action = json!({ "type": "object",
        "description": "An insert-from-query action of this collection (the parent): {name ([a-z][a-z0-9_]*), title?, params?: [{name, type: text|number|ref, collection? (ref), optional?}], source: {collection, where?: [{field, equals} | {field, empty: true|false}]}, scale?: {times, per}, match: {<field of this collection>: value}, create?: {<field>: value}, update?: {<field>: value}, reset_when?: [{field, equals} | {field, empty}], child?: {collection, values: {<field of the child collection>: value | {from: value, scaled: true}}}}. A value is a literal (a string starting with $ as {value: \"...\"}) or a binding: \"$param.<name>\" (a call's parameter: text; a number; a ref param is given as a row id or {\"<unique field>\": value} and binds the id), \"$row.<field>\" (the source row; through refs \"$row.<ref>.<field>\", up to 4 names; \"$row.id\", \"$row.<ref>.id\") and, only in child.values, \"$parent.id\" or \"$parent.<field>\". A call: for each live row of source.collection matching every where condition (equals compares as text ignoring case and spacing, a ref by its id; empty: false keeps rows where the field has a value), in that collection's order: the match values are computed (a row with an empty match value is skipped and reported); the live row of this collection whose match fields all equal them is found (the oldest, also one made earlier in the same call), else created with match and create values; a found row first, when it matches every reset_when condition, has its children deleted (the child rows whose $parent.id field points at it; once per call), then gets the update values (once per call). Then, with child, a row of child.collection is added with child.values; exactly one of them is a ref field to this collection set to \"$parent.id\". A value with scaled: true (a number or quantity field) is multiplied by scale.times / scale.per when both read as numbers above 0 (a number, a quantity's amount, or the first number in text such as \"Serves 4\"), else by 1; text read into a quantity field is parsed first, and text that doesn't parse is added unscaled and listed in the answer's unparsed. Fields required without a default must be given (match or create for a parent; child.values for a child). An update action {name, title?, params?, source: {collection: <this collection>, where?}, update: {field: value}} (no match, create, reset_when, scale or child) writes update to every live row of this collection matching where, as a row update does, e.g. {name: \"mark_all_read\", source: {collection: \"notifications\", where: [{field: \"read\", equals: false}]}, update: {read: true}}. A delete action {name, title?, params?, source: {collection: <this collection>, where?}, delete: true} (no match, create, update, reset_when, scale or child) instead deletes every live row of this collection matching where, as a row delete does (restorable; lookups, counts and rollups that read the collection follow), e.g. {name: \"clear_bag\", title: \"Clear the bag\", source: {collection: \"groceries\", where: [{field: \"done\", equals: true}]}, delete: true}. Every call is one edit across both collections (undo_edit reverts it: rows it made are deleted, rows it changed or deleted come back, rows changed since are left alone and listed; computed totals follow). Example (grocery list from recipe lines; on groceries): {name: \"add_recipe\", title: \"Add a recipe to groceries\", params: [{name: \"meal\", type: \"ref\", collection: \"meals\"}, {name: \"servings\", type: \"number\", optional: true}], source: {collection: \"recipe_lines\", where: [{field: \"meal\", equals: \"$param.meal\"}, {field: \"ingredient\", empty: false}]}, scale: {times: \"$param.servings\", per: \"$row.meal.servings\"}, match: {ingredient: \"$row.ingredient\"}, create: {name: \"$row.ingredient.name\", done: false}, update: {done: false}, reset_when: [{field: \"done\", equals: true}], child: {collection: \"grocery_entries\", values: {grocery: \"$parent.id\", quantity: {from: \"$row.amount\", scaled: true}, recipe: \"$row.meal\"}}}. Run it with run_action, or from a layout: {intent: \"action.mutation.optimistic\", entity_uri: \"$item.uri\", field: \"adding\", value_expr: \"true\", endpoint: \"/v1/collections/<collection>/actions/<name>\", payload: {<param>: \"$item.id\" or \"$form?.<field>\", client_id: \"$nonce\"}} (field is a view flag on the device only; the payload's keys must be params or client_id; a repeated client_id is applied once; the new rows arrive through the feed)." });
    vec![
        tool(
            "list_collections",
            "The pod's collections: fields, row counts, versions and the layouts that use each; deleted lists the deleted ones restore_collection can bring back.",
            json!({}),
            &[],
        ),
        tool(
            "use_kit",
            "Start from a kit: a core layout over a collection of a known shape (shopping: several collections under their own names), ready to use (find_elements lists them; category layout, scope core, their use_when says kit). It makes the collection (name it with collection; left out, the kit's own name) or, when one of that name exists, checks it has the kit's fields; adds its feed to home; and answers embed (a core://Layout to put in home, or in a tab's slot) and open (an action.navigation.open for a row or a button in home), each with the kit's inputs bound. The layout stays the server's: it follows deploys. Only home may hold the embed or the link (it binds the pod's data). Without kit it lists the kits.",
            json!({ "kit": { "type": "string" }, "collection": collection, "title": { "type": "string", "description": "The page's title (the kit's own when left out)" } }),
            &[],
        ),
        tool(
            "define_collection",
            "Declare a new collection (name [a-z][a-z0-9_]*, permanent) with typed fields. Custom apps then use it as a feed [\"collections\", name]. Define the collections a ref, lookup, rollup or action names first (a rollup's via is a ref of the other collection pointing at this one, so add the rollup with apply_change after both exist).",
            json!({ "name": collection, "title": { "type": "string" },
                    "fields": { "type": "array", "items": field },
                    "order_by": order,
                    "descending": { "type": "boolean" },
                    "fillers": { "type": "array", "items": filler.clone() },
                    "alerts": { "type": "array", "items": alert },
                    "actions": { "type": "array", "items": action.clone() },
                    "views": { "type": "array", "items": view },
                    "snapshots": { "type": "array", "description": "Daily copies of some fields as rows of another collection, one row per row and day, for a history to chart: [{into: collection, link: its ref field to this one, day: its date field, fields: {field there: field here}}]. Taken on every write of the row and once a day.", "items": { "type": "object" } },
                    "note": note }),
            &["name", "fields"],
        ),
        tool(
            "describe_collection",
            "A collection's definition (live and removed fields, aliases, actions with their endpoints, views with their query keys), version history, row count, values kept aside, quantity values that didn't parse (quantities: {field, unparsed, samples}), recent row edits and action calls, the apps using it, and layout snippets for listing, adding, ticking and deleting rows.",
            json!({ "collection": collection, "version": { "type": "integer", "description": "An older definition version" } }),
            &["collection"],
        ),
        tool(
            "apply_change",
            "Change a collection's definition and the layouts that show it in one step. changes: [{op: add_field, name, type, options?, collection?, unique?, optional?, default?, compute?} | {op: rename_field, field, to} | {op: remove_field, field} | {op: restore_field, field} | {op: update_field, field, type?, options?, collection?, unique?, optional?, default?, compute?} | {op: set_action, action} | {op: remove_action, action: name} | {op: set_view, view} | {op: remove_view, view: name} | {op: recompute_field, field} | {op: set_order, field?: name or [names], descending?} | {op: set_title, title} | {op: add_reminder, field, title, body?, at?, unless?, label?, app?} | {op: update_reminder, reminder: id, ...} | {op: remove_reminder, reminder: id} | {op: set_filler, filler} | {op: remove_filler, filler: id, keep_rows?} | {op: add_alert, ...} | {op: update_alert, alert: id, ...} | {op: remove_alert, alert: id}]. set_filler adds a filler (as define_collection's fillers), or changes the one filler.id names, and starts its fill; remove_filler stops one (its alerts go; a replace filler's rows are deleted, restorable, unless keep_rows: true hands them to people, no longer locked; an upsert or per-row filler's rows stay). To change how a site is read, set_filler with its id: removing it and adding a new one remakes its rows. A keyed filler leaves out an item whose key another filler's row already holds (the oldest row keeps it). add_alert notifies once per row when a row matches (as define_collection's alerts). describe_collection lists fillers and alerts with their ids. A reminder notifies the user once per row when its date field is reached: on that date at at (HH:MM, default 09:00) in the pod's time zone (set_time_zone), unless the row then has unless {field, equals} (e.g. {field: \"done\", equals: true}); dates before the day it was added never notify, and a reminder more than 7 days late is dropped. title and body are the notification's text with {field} replaced by the row's value; label names it in the user's notification settings (default \"<Title>: <field>\"); app (a layout id) is the layout it comes from and opens, default the oldest layout that shows the collection (with none, nothing is sent). describe_collection lists reminders with their ids. compute (one kind or a list tried in order: {kind: \"expr\", expr}, {kind: \"lookup\", from, collection, key, value, match?, learn?}, {kind: \"count\", from, collection, key, where?} or {kind: \"model\", from, prompt}, as in define_collection) is computed for existing rows too (a model only fills empty ones); null stops computing and keeps the values. Lookups and counts are live: a change to the rows of the collection they read recomputes the rows it affects at once, cascading. recompute_field computes the field again for every row (a row only the model could answer keeps its value), e.g. after changing a lookup's match. Fields and actions are as in define_collection. update_field {type: \"ref\", collection} turns a text field into a ref: each value becomes the id of the row of that collection whose id or unique text field equals it (ignoring case and spacing; make the target's name field unique first); values that match no row are kept aside and reported in conversions (set_aside, samples). unique: true fails while live rows share a value (the error lists them). set_action adds an action or replaces the one of that name; remove_action removes it. set_view adds a view (as define_collection's views) or replaces the one of that name; remove_view removes it; a layout listing a view that would be gone, or a field a view reads being removed, refuses the change. A collection can't be deleted while another collection's ref, lookup, count, rollup or action names it. Every layout using Collections is checked, as devices draw it, against the new definition; if any would break, nothing applies and the error names the layout and the binding. apps: [{app, layout | json_patch, base_version?}] edits layouts in the same step (app is a layout id; base_version is the version get_layout shows). Renamed fields keep their old name as an alias; removed fields are hidden and restorable for 30 days; type changes convert row by row and keep values that don't convert aside.",
            json!({ "collection": collection, "base_version": { "type": "integer", "description": "The definition version you read" },
                    "changes": { "type": "array", "items": { "type": "object" } }, "note": note }),
            &["collection", "changes"],
        ),
        tool(
            "restore_collection_version",
            "Make an older definition current again (appends a copy): removed fields come back with their data, fields added since are soft-removed, types convert back (values kept aside return). Every layout that shows it must still fit it, as for apply_change.",
            json!({ "collection": collection, "version": { "type": "integer" }, "note": note }),
            &["collection", "version"],
        ),
        tool(
            "delete_collection",
            "Delete a collection: its definition, rows, fillers, alerts and reminders. Only when the user asked for it. Refused while a layout binds it or another collection's lookup or count reads it (the error names them: change those first). Drops it from home's feeds. Restorable with restore_collection for 30 days, then purged.",
            json!({ "collection": collection, "base_version": { "type": "integer", "description": "The definition version you read" } }),
            &["collection"],
        ),
        tool(
            "restore_collection",
            "Bring back a collection delete_collection deleted in the last 30 days, with its rows, fillers, alerts and reminders (list_collections lists deleted ones). Its feed isn't added back to home: a layout that shows it adds it.",
            json!({ "collection": collection }),
            &["collection"],
        ),
        tool(
            "edit_rows",
            "Change rows directly, for imports, backfills and conversions: edits [{op: create, values: {field: value}} | {op: update, id, values} | {op: delete, id} | {op: restore, id}]. A ref value is a row id or {\"<unique field>\": value} of its collection; a quantity value is text (\"1 1/2 cups\") or {amount, unit}; a unique field refuses a value another live row has. Send the whole set in one call (up to the pod's row cap), not in batches. Data already in a file: give url (a https file the pod fetches: a JSON array of edits or of {field: value} rows, JSON lines, or CSV with a header of field names, optional op and id columns, and <ref field>.<unique field> columns naming a ref's row) instead of edits, or POST the file to /v1/collections/<collection>/import with the pod token. The first call answers a preview (count and sample) and a confirm token and changes nothing; every edit is checked first, and invalid ones are listed with where they are (/edits/<i>, or a file's line or item). Show the preview to the user, and only after they agree call again with just collection and confirm: the pod applies exactly the edits it previewed (kept for an hour). Undo with undo_edit.",
            json!({ "collection": collection, "edits": { "type": "array", "items": { "type": "object" } },
                    "url": { "type": "string", "description": "A file of edits or rows to fetch instead of edits" },
                    "format": { "type": "string", "enum": ["json", "jsonl", "csv"], "description": "The file's format (default: from its name or content)" },
                    "note": note, "confirm": { "type": "string", "description": "The token of the preview the user agreed to" } }),
            &["collection"],
        ),
        tool(
            "add_row",
            "Add one row the user just asked for in this conversation (\"log that I had the oil changed\", \"add milk\"): written at once, no preview, values as for a create in edit_rows (fields left out get their default; dates are YYYY-MM-DD). Answers the row id and an edit id for undo_edit. For several rows, imports or changes to existing rows, use edit_rows.",
            json!({ "collection": collection, "values": { "type": "object", "description": "{field: value}" }, "note": note }),
            &["collection", "values"],
        ),
        tool(
            "run_action",
            "Run an insert-from-query action of a collection now (describe_collection lists them; see define_collection actions): params {<param>: value} (a ref param: a row id or {\"<unique field>\": value}). Written at once, no preview: run it when the user asked for it (\"add the lasagna to my groceries for 6\"). Answers {edit (for undo_edit), rows (source rows read), created (parents made), updated (parents changed), reset (children deleted by reset_when), added (children made), deleted (rows a delete action deleted), parents (their ids), skipped [{row, reason}], unparsed [{row, field, value}] (quantity text that didn't parse, added unscaled)}.",
            json!({ "collection": collection, "action": { "type": "string", "description": "The action's name" },
                    "params": { "type": "object", "description": "{param: value}" }, "note": note }),
            &["collection", "action"],
        ),
        tool(
            "undo_edit",
            "Undo one edit_rows, add_row or run_action edit as one step (rows it made are deleted, rows it changed or deleted come back; rows changed since are left alone and reported; fields that follow other rows, such as rollups, don't count as changes).",
            json!({ "edit": { "type": "string", "description": "The edit id edit_rows answered" } }),
            &["edit"],
        ),
        tool(
            "collections_preview",
            "Try a fetch recipe now and store nothing: fetch the page, extract and map, and answer the rows (or, without map, sample items to write the paths from). Pass the recipe's {url, extract, items?, types?, script?, zone?, map?, headers?, render?}, or collection (and filler, an id; default its first) to try that filler (a url_field filler tries its first row's page). Use it before set_filler, and when collections_status says a filler finds nothing.",
            json!({ "collection": collection, "filler": { "type": "string" }, "url": { "type": "string" },
                    "extract": { "type": "string", "enum": web_extract::items::Kind::ALL }, "zone": { "type": "string" },
                    "items": { "type": "string" }, "types": { "type": "array", "items": { "type": "string" } },
                    "script": { "type": "string" }, "map": { "type": "object", "description": "{name: path}" },
                    "headers": { "type": "object" },
                    "render": { "description": "true or {wait_for?: CSS selector, timeout_seconds?}: load the page in the server's headless browser first" } }),
            &[],
        ),
        tool(
            "collections_status",
            "Each filler's health (of one collection, or all): state (pending, ok, failing, empty, off, unavailable), detail, last and next fill, items found, model fills today, and its alerts. Say so when one is failing or finds no items.",
            json!({ "collection": collection }),
            &[],
        ),
        tool(
            "collections_refresh",
            "Fill a collection's fillers now (or one: filler, an id), refetching even an unchanged page, and answer what changed. A model filler (agent or llm) runs in the background and counts toward its budget.",
            json!({ "collection": collection, "filler": { "type": "string" } }),
            &["collection"],
        ),
        tool(
            "collections_set_secret",
            "Store a value a fetch recipe sends in a header (an API key), sealed on the pod: headers name it as \"{secret:<name>}\". It is never shown back, and is sent only to https:// requests for host (the API's host name, e.g. api.example.com; ask the user which site the key is for). value null forgets it. Answers the stored names.",
            json!({ "name": { "type": "string", "description": "[a-z][a-z0-9_]*" },
                    "value": { "type": ["string", "null"] },
                    "host": { "type": "string", "description": "The host name the secret may be sent to (required with a value)" } }),
            &["name", "value"],
        ),
        tool(
            "export_collection",
            "Everything in a collection, deleted rows, removed fields' values and values kept aside included: the pod is the only copy of this data.",
            json!({ "collection": collection }),
            &["collection"],
        ),
    ]
}

/// The access each tool needs (decision `mcp-access-levels`). Reaching a
/// host the pod didn't reach before (a filler, a file to import, a recipe
/// to try) is outward, as are secrets and deleting a collection.
pub fn access(tool: &str, args: &Value) -> Option<Access> {
    let has = |k: &str| match args.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    Some(match tool {
        "collections_status" | "export_collection" => Access::Read,
        "define_collection" if has("fillers") => Access::Outward,
        "apply_change"
            if args["changes"]
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c["op"] == "set_filler")) =>
        {
            Access::Outward
        }
        "edit_rows" if has("url") => Access::Outward,
        "use_kit"
        | "define_collection"
        | "apply_change"
        | "restore_collection_version"
        | "restore_collection"
        | "edit_rows"
        | "add_row"
        | "run_action"
        | "undo_edit"
        | "collections_refresh" => Access::Edit,
        "collections_preview" | "collections_set_secret" | "delete_collection" => Access::Outward,
        _ => return None,
    })
}

pub fn call<'a>(
    adapter: &'a CollectionsAdapter,
    pod: &'a Pod,
    tool: &'a str,
    args: &'a Value,
) -> Option<BoxFuture<'a, Result<Value, Value>>> {
    let caps = adapter.caps;
    // A scoped credential's grant (decision `mcp-grants`): the tools that
    // answer rows answer only those of the collections it may read (a write's
    // preview shows rows too; the write itself is held to `rows` as well).
    if matches!(
        tool,
        "describe_collection"
            | "export_collection"
            | "collections_status"
            | "edit_rows"
            | "add_row"
            | "run_action"
    ) && let Some(g) = pod_core::grant::current()
        && g.reads.is_some()
    {
        let named = args["collection"].as_str().map(str::trim).unwrap_or("");
        if named.is_empty() || !g.allows_read(named) {
            let what = if named.is_empty() {
                format!("{tool} without a collection it may read")
            } else {
                format!("reading the collection {named}")
            };
            return Some(Box::pin(async move {
                let mut body = pod_core::grant::refusal(&what);
                body["status"] = json!(403);
                Err(body)
            }));
        }
    }
    Some(match tool {
        "list_collections" => Box::pin(list(pod)),
        "define_collection" => Box::pin(define(pod, adapter, args)),
        "use_kit" => Box::pin(use_kit(pod, adapter, args)),
        "describe_collection" => Box::pin(describe(pod, args)),
        "apply_change" => Box::pin(apply_change(pod, adapter, args)),
        "collections_preview" => Box::pin(preview(pod, adapter, args)),
        "collections_status" => Box::pin(status(pod, args)),
        "collections_refresh" => Box::pin(refresh(pod, args)),
        "collections_set_secret" => Box::pin(set_secret(pod, args)),
        "restore_collection_version" => Box::pin(restore_version(pod, caps, args)),
        "delete_collection" => Box::pin(delete(pod, caps, args)),
        "restore_collection" => Box::pin(restore(pod, caps, args)),
        "edit_rows" => Box::pin(edit_rows(pod, adapter, args)),
        "add_row" => Box::pin(add_row(pod, caps, args)),
        "undo_edit" => Box::pin(undo_edit(pod, args)),
        "run_action" => Box::pin(run_action(pod, args)),
        "export_collection" => Box::pin(export(pod, args)),
        _ => return None,
    })
}

fn bad(detail: impl Into<String>) -> Value {
    fail(StatusCode::BAD_REQUEST, "invalid_arguments", detail)
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Result<&'a str, Value> {
    args[k]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("\"{k}\" is required")))
}

fn note_arg(args: &Value, default: &str) -> String {
    args["note"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(default)
        .chars()
        .take(pod_layouts::MAX_NOTE_CHARS)
        .collect()
}

fn db_err(e: (StatusCode, axum::Json<Value>)) -> Value {
    let (status, axum::Json(mut body)) = e;
    body["status"] = json!(status.as_u16());
    body
}

fn missing(name: &str) -> Value {
    json!({ "error": "collection_not_found", "collection": name, "status": 404,
            "detail": "list_collections lists them; define_collection makes one" })
}

/// The layouts that use collection `name`: `(layout id, title, version)` of
/// each one whose element binds it (its query key, a row URI or a data
/// path).
fn users(apps: &[SourceUse], name: &str) -> Vec<Value> {
    let key = db::query_key(name);
    let scheme = format!("collections://{name}/");
    let route = format!("/v1/collections/{name}/");
    // Not by feeds: every layout reports home's (decision `layouts-not-apps`).
    // A layout bound two ways is listed once.
    let mut seen = std::collections::BTreeSet::new();
    apps.iter()
        .filter(|u| {
            let text = u.node.to_string();
            text.contains(&format!("\"{key}\"")) || text.contains(&scheme) || text.contains(&route)
        })
        .filter(|u| seen.insert(u.layout.id.clone()))
        .map(|u| json!({ "layout": u.layout.id, "title": u.layout.title, "version": u.version }))
        .collect()
}

async fn list(pod: &Pod) -> Result<Value, Value> {
    let (out, deleted) = pod
        .blocking(move |s| {
            let apps = pod_layouts::layouts_with_source(s, NAME)?;
            s.with_conn(|c| {
                let others = db::defs_by_name(c)?;
                let live = db::all_defs(c)?
                    .into_iter()
                    .map(|(version, def)| {
                        let rows = db::all_rows(c, &def.name, false)?.len();
                        Ok(json!({
                            "name": def.name, "title": def.title, "version": version, "rows": rows,
                            "fields": def.live_fields().map(|f| def.describe_field(f, &others)).collect::<Vec<_>>(),
                            "used_by": users(&apps, &def.name),
                        }))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let deleted = db::deleted_defs(c)?
                    .into_iter()
                    .map(|(at, version, def)| {
                        let rows = db::all_rows(c, &def.name, false)?.len();
                        Ok(json!({
                            "name": def.name, "title": def.title, "version": version, "rows": rows,
                            "deleted_at": at, "purged_after": at + KEEP_SECS,
                        }))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((live, deleted))
            })
        })
        .await
        .map_err(db_err)?;
    let mut body = json!({ "collections": out });
    if !deleted.is_empty() {
        body["deleted"] = json!(deleted);
    }
    Ok(body)
}

/// MCP `use_kit {kit?, collection?, title?}` (decision
/// `element-roles-and-reuse`): a kit's collection made or checked, its feed
/// bound by home, and the references that show the kit's layout over it.
async fn use_kit(pod: &Pod, adapter: &CollectionsAdapter, args: &Value) -> Result<Value, Value> {
    let list = || -> Vec<Value> {
        crate::kits::KITS
            .iter()
            .map(|k| {
                json!({ "kit": k.layout.id, "title": k.layout.title, "summary": k.summary,
                "collection": k.collection, "fields": (k.fields)() })
            })
            .collect()
    };
    let Some(id) = args["kit"].as_str().filter(|k| !k.is_empty()) else {
        return Ok(json!({ "kits": list() }));
    };
    let Some(kit) = crate::kits::kit(id) else {
        return Err(json!({ "error": "unknown_kit", "kits": list(), "status": 404 }));
    };
    if let Some(set) = kit.set {
        return use_kit_set(pod, adapter, kit, set(), args).await;
    }
    let name = args["collection"]
        .as_str()
        .filter(|n| !n.is_empty())
        .unwrap_or(kit.collection)
        .to_owned();
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(kit.layout.title)
        .to_owned();
    let fields = (kit.fields)();
    let existing = pod
        .blocking(|s| s.with_conn(db::defs_by_name))
        .await
        .map_err(db_err)?
        .remove(&name);
    let created = match existing {
        // One of that name exists: it must have the kit's fields, by name.
        Some(def) => {
            let missing: Vec<&str> = fields
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["name"].as_str())
                .filter(|n| !def.live_fields().any(|f| f.name == *n))
                .collect();
            if !missing.is_empty() {
                return Err(
                    json!({ "error": "collection_mismatch", "collection": name, "missing": missing,
                    "detail": "the collection lacks fields the kit reads; add them (apply_change add_field, or rename a field to that name: its old name keeps working), or give the kit a collection of its own", "status": 409 }),
                );
            }
            false
        }
        None => {
            define(
                pod,
                adapter,
                &json!({ "name": name, "title": title, "fields": fields }),
            )
            .await?;
            true
        }
    };
    pod_core::construct::write::add_home_feed(pod, (NAME.to_owned(), name.clone())).await?;
    let reference = json!({ "name": format!("@core/{}", kit.layout.id), "as": format!("{}-{name}", kit.layout.id) });
    let bind = json!({ "collection": name, "title": title });
    Ok(json!({
        "kit": kit.layout.id, "collection": name, "created": created,
        "embed": { "type": "core://Layout", "ref": reference, "bind": bind },
        "open": { "intent": "action.navigation.open", "ref": reference, "presentation": "Push", "bind": bind },
        "next": "Put embed in home (as its content, or in a tab's slot of @core/tab_shell), or open on a row or button of home, with update_layout on home. Only home may hold them: it binds the pod's data.",
    }))
}

/// `use_kit` for a kit of several collections (decision `shopping-kit`):
/// each made under its own name, or checked to have the kit's fields when
/// it exists; then the changes that need them all, leaving out what is
/// there already (so a second use changes nothing); categories seeded when
/// there are none; every feed bound by home.
async fn use_kit_set(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    kit: &crate::kits::Kit,
    set: crate::kits::KitSet,
    args: &Value,
) -> Result<Value, Value> {
    if args["collection"]
        .as_str()
        .is_some_and(|c| !c.is_empty() && c != kit.collection)
    {
        return Err(json!({ "error": "invalid_request", "status": 400,
            "detail": "this kit's collections keep their own names (its layouts read them)" }));
    }
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(kit.layout.title)
        .to_owned();
    let mut made = Vec::new();
    let mut names = Vec::new();
    for spec in &set.collections {
        let name = spec["name"].as_str().unwrap_or_default().to_owned();
        let existing = pod
            .blocking(|s| s.with_conn(db::defs_by_name))
            .await
            .map_err(db_err)?
            .remove(&name);
        match existing {
            Some(def) => {
                let missing: Vec<&str> = spec["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f["name"].as_str())
                    .filter(|n| !def.live_fields().any(|f| f.name == *n))
                    .collect();
                if !missing.is_empty() {
                    return Err(
                        json!({ "error": "collection_mismatch", "collection": name, "missing": missing,
                        "detail": "a collection of this name lacks fields the kit reads; add them (apply_change add_field) or rename that collection", "status": 409 }),
                    );
                }
            }
            None => {
                define(pod, adapter, spec).await?;
                made.push(name.clone());
            }
        }
        names.push(name);
    }
    for (name, changes) in set.changes {
        let def = current(pod, name).await?.1;
        let todo: Vec<Value> = changes
            .into_iter()
            .filter(|c| {
                let n = c["name"].as_str().unwrap_or("");
                match c["op"].as_str().unwrap_or("") {
                    "add_field" => def.field(n).is_none(),
                    "add_alert" => !def.alerts.iter().any(|a| a.name == n),
                    "set_filler" => {
                        let t = c["filler"]["title"].as_str().unwrap_or("");
                        !def.fillers.iter().any(|f| f.title == t)
                    }
                    _ => true,
                }
            })
            .collect();
        if !todo.is_empty() {
            apply_change(
                pod,
                adapter,
                &json!({ "collection": name, "changes": todo, "note": "The shopping kit" }),
            )
            .await?;
        }
    }
    // Categories to start from, when there are none.
    let count = pod
        .blocking(|s| s.with_conn(|c| db::all_rows(c, "shop_categories", false)))
        .await
        .map_err(db_err)?
        .len();
    if count == 0 {
        for (i, (n, explore, prefs)) in [
            ("Clothes", true, ""),
            ("Home", false, ""),
            ("Other", false, ""),
        ]
        .into_iter()
        .enumerate()
        {
            let mut values = json!({ "name": n, "explore": explore, "position": i });
            if !prefs.is_empty() {
                values["preferences"] = json!(prefs);
            }
            add_row(
                pod,
                adapter.caps,
                &json!({ "collection": "shop_categories", "values": values, "note": "The shopping kit's categories" }),
            )
            .await?;
        }
    }
    for n in &names {
        pod_core::construct::write::add_home_feed(pod, (NAME.to_owned(), n.clone())).await?;
    }
    let reference = json!({ "name": format!("@core/{}", kit.layout.id), "as": kit.layout.id });
    let bind = json!({ "title": title });
    Ok(json!({
        "kit": kit.layout.id, "collections": names, "created": made,
        "embed": { "type": "core://Layout", "ref": reference, "bind": bind },
        "open": { "intent": "action.navigation.open", "ref": reference, "presentation": "Push", "bind": bind },
        "next": "Put embed in home (as its content, or in a tab's slot of @core/tab_shell), or open on a row or button of home, with update_layout on home. Add needs with add_row on shop_needs (name, category: one of shop_categories' names); a category with explore true is explored through the taste service, others find options on the web.",
    }))
}

async fn define(pod: &Pod, adapter: &CollectionsAdapter, args: &Value) -> Result<Value, Value> {
    let caps = adapter.caps;
    let name = str_arg(args, "name")?.to_owned();
    let Some(specs) = args["fields"].as_array() else {
        return Err(bad("\"fields\" is a list of {name, type, ...}"));
    };
    let mut fields = Vec::new();
    let invalid = |i: usize, e: String| json!({ "error": "invalid_field", "at": format!("/fields/{i}"), "detail": e, "status": 400 });
    for (i, spec) in specs.iter().enumerate() {
        let f = model::parse_field(spec, format!("f{}", i + 1), caps.text_chars)
            .map_err(|e| invalid(i, e))?;
        fields.push(f);
    }
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map_or_else(|| title_case(&name), str::to_owned);
    let mut def = Definition {
        name: name.clone(),
        title,
        next_field: fields.len() as u32 + 1,
        fields,
        reminders: Vec::new(),
        fillers: Vec::new(),
        alerts: Vec::new(),
        system: None,
        actions: Vec::new(),
        views: Vec::new(),
        order_by: Vec::new(),
        descending: args["descending"].as_bool().unwrap_or(false),
        keep: None,
        snapshots: Vec::new(),
    };
    def.snapshots = crate::snapshots::parse(&def, args.get("snapshots")).map_err(bad)?;
    // After every field exists: a compute may read a later one.
    let mut others = pod
        .blocking(|s| s.with_conn(db::defs_by_name))
        .await
        .map_err(db_err)?;
    let mut created = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        model::set_compute_creating(
            &mut def,
            &format!("f{}", i + 1),
            spec.get("compute"),
            &mut others,
            &mut created,
            caps.text_chars,
        )
        .map_err(|e| invalid(i, e))?;
    }
    let order = model::order_names(args.get("order_by")).map_err(bad)?;
    def.order_by = model::resolve_order(&def, &order).map_err(bad)?;
    // One filler (`filler`) or several (`fillers`); alerts after them.
    let specs: Vec<(String, Value)> = match (args.get("fillers"), args.get("filler")) {
        (Some(Value::Array(a)), _) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("/fillers/{i}"), v.clone()))
            .collect(),
        (_, Some(v)) if v.is_object() => vec![("/filler".to_owned(), v.clone())],
        _ => Vec::new(),
    };
    for (at, spec) in specs {
        let f = crate::filler::parse_in(
            &def,
            &others,
            &spec,
            db::new_id("s"),
            adapter.allow_private_hosts(),
        )
        .map_err(|e| json!({ "error": "invalid_filler", "at": at, "detail": e, "status": 400 }))?;
        def.fillers.push(f);
    }
    if let Some(Value::Array(a)) = args.get("alerts") {
        for (i, spec) in a.iter().enumerate() {
            crate::alerts::add(&mut def, spec, pod_core::util::now_secs()).map_err(
                |e| json!({ "error": "invalid_alert", "at": format!("/alerts/{i}"), "detail": e, "status": 400 }),
            )?;
        }
    }
    if let Some(Value::Array(a)) = args.get("actions") {
        let mut with_self = others.clone();
        with_self.insert(def.name.clone(), def.clone());
        for (i, spec) in a.iter().enumerate() {
            let action = crate::actions::parse(&def, spec, &with_self).map_err(
                |e| json!({ "error": "invalid_action", "at": format!("/actions/{i}"), "detail": e, "status": 400 }),
            )?;
            def.actions.push(action);
        }
    }
    if let Some(Value::Array(a)) = args.get("views") {
        for (i, spec) in a.iter().enumerate() {
            crate::views::parse(&def, spec)
                .and_then(|v| {
                    if crate::views::find(&def, &v.name).is_some() {
                        return Err(format!("there are two views named '{}'", v.name));
                    }
                    crate::views::set(&mut def, v)
                })
                .map_err(
                    |e| json!({ "error": "invalid_view", "at": format!("/views/{i}"), "detail": e, "status": 400 }),
                )?;
        }
    }
    model::check_definition(&def, caps.fields)
        .map_err(|e| json!({ "error": "invalid_definition", "detail": e, "status": 400 }))?;
    crate::filler::check(&def, caps.agent_fills_per_day)
        .map_err(|e| json!({ "error": "invalid_filler", "detail": e, "status": 400 }))?;
    let mut with_new = others.clone();
    with_new.insert(def.name.clone(), def.clone());
    model::check_lookups(&def, &with_new)
        .map_err(|e| json!({ "error": "invalid_definition", "detail": e, "status": 400 }))?;
    let note = note_arg(args, "Created");
    let stored = def.clone();
    let rules = created.clone();
    let version = refusable(pod, move |tx| {
        if db::deleted_at(tx, &stored.name)?.is_some() {
            return Ok(Err(
                json!({ "error": "collection_deleted", "collection": stored.name, "status": 409,
                "detail": "a deleted collection has this name: restore_collection brings it back, or pick another name" }),
            ));
        }
        if db::latest_def(tx, &stored.name)?.is_some() {
            return Ok(Err(
                json!({ "error": "collection_exists", "collection": stored.name, "status": 409,
                "detail": "change it with apply_change (describe_collection shows it)" }),
            ));
        }
        if db::all_defs(tx)?.iter().filter(|(_, d)| !crate::inbox::is_inbox(&d.name)).count() >= caps.collections {
            return Ok(Err(fail(
                StatusCode::CONFLICT,
                "too_many_collections",
                format!("a pod has at most {} collections", caps.collections),
            )));
        }
        let now = pod_core::util::now_secs();
        if let Err(e) = create_rules(tx, &rules, caps, now)? {
            return Ok(Err(e));
        }
        Ok(Ok(db::append_def(
            tx,
            &stored,
            &note,
            pod_layouts::AUTHOR_CLAUDE,
            now,
        )?))
    })
    .await?;
    let mut out = json!({
        "ok": true, "collection": name, "version": version,
        "definition": def.describe(pod_core::util::now_secs(), KEEP_SECS, &with_new),
        "next": format!("Make an app with create_app and feeds [[\"collections\", \"{name}\"]]; describe_collection has layout snippets."),
    });
    if !created.is_empty() {
        out["created_rules"] = json!(created.iter().map(|d| &d.name).collect::<Vec<_>>());
    }
    if !def.fillers.is_empty() {
        crate::fill::spawn(pod, &name, None, true);
        out["filling"] = json!("a first fill started; collections_status shows how it went");
    }
    let warnings = crate::filler::toggle_warnings(&def);
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(out)
}

fn title_case(name: &str) -> String {
    let t = name.replace('_', " ");
    let mut c = t.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// Layout snippets for `def`'s collection.
fn hints(def: &Definition) -> Value {
    let n = &def.name;
    let first_text = def
        .live_fields()
        .find(|f| f.kind == model::Kind::Text)
        .map_or("title", |f| f.name.as_str());
    let first_bool = def.live_fields().find(|f| f.kind == model::Kind::Bool);
    let mut entity = json!({ "id": "pending-$nonce", "deleted": false });
    entity[first_text] = json!(format!("$form.{first_text}"));
    let mut out = json!({
        "feed": ["collections", n],
        "list": { "type": "core://List", "query_key": db::query_key(n),
            "where": { "field": "deleted", "equals": "false" },
            "empty_text": format!("No {} yet", def.title.to_lowercase()),
            "item_template": { "type": "core://Text", "style": "Body", "value_expr": format!("$item.{first_text}") } },
        "add": { "type": "core://TextField", "field": first_text, "placeholder": "Add…",
            "on_submit": { "intent": "action.mutation.create", "query_key": db::query_key(n),
                "entity_uri": format!("collections://{n}/pending-$nonce"), "entity": entity,
                "endpoint": format!("/v1/collections/{n}/create"),
                "payload": { first_text: format!("$form.{first_text}"), "client_id": "$nonce" },
                "refresh_workspace": true } },
        "delete": { "type": "core://IconButton", "icon": "delete", "content_description": "Delete",
            "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                "field": "deleted", "value_expr": "true",
                "endpoint": format!("/v1/collections/{n}/delete"), "payload": { "id": "$item.id" } } },
        "notes": "Give every create's entity \"deleted\": false and an \"id\" of \"pending-$nonce\" (so the List's where shows it and a tick before the pod answers still reaches it). Fields left out of a create get their default. Dates are YYYY-MM-DD text.",
    });
    if let Some(b) = first_bool {
        out["toggle"] = json!({ "type": "core://IconButton", "icon": "check_box_outline_blank",
            "active": format!("$item.{}", b.name), "active_icon": "check_box",
            "content_description": b.name,
            "on_click": { "intent": "action.mutation.optimistic", "entity_uri": "$item.uri",
                "field": b.name, "value_expr": format!("!entity.{}", b.name),
                "endpoint": format!("/v1/collections/{n}/update"),
                "payload": { "id": "$item.id", b.name.as_str(): "$value" } } });
    }
    // A whole row: the tick, the text, then the delete at the end. Inside a
    // Row, a core://Column takes the full width (pushing the delete off
    // screen); a core://Container takes what it needs.
    let mut children = Vec::new();
    if out.get("toggle").is_some() {
        children.push(out["toggle"].clone());
    }
    children.push(
        json!({ "type": "core://Container", "orientation": "Vertical", "padding_dp": 0,
        "children": [out["list"]["item_template"].clone()] }),
    );
    children.push(json!({ "type": "core://Spacer" }));
    children.push(out["delete"].clone());
    out["row"] = json!({ "type": "core://Row", "spacing_dp": 12, "padding_h_dp": 16, "padding_v_dp": 6,
        "align": "Center", "children": children });
    // Display formatting is the client's (decision `text-dates-and-counts`).
    if let Some(d) = def.live_fields().find(|f| f.kind == model::Kind::Date) {
        let mut due = json!({ "type": "core://Text", "style": "Secondary",
            "value_expr": format!("Due $item.{}", d.name),
            "format": { "date": "Relative", "past": "Overdue" } });
        if d.optional {
            due = json!({ "type": "core://Conditional", "value": format!("$item.{}", d.name), "content": due });
        }
        out["date"] = due;
    }
    let mut open = vec![json!({ "field": "deleted", "equals": "false" })];
    if let Some(b) = first_bool {
        open.push(json!({ "field": b.name, "equals": "false" }));
    }
    out["count"] = json!({ "type": "core://Text", "style": "Secondary",
        "value_expr": if first_bool.is_some() { "$count left" } else { "$count" },
        "count": { "query_key": db::query_key(n), "where": open } });
    out["notes"] = json!(format!(
        "{} Dates and counts are drawn by the device: \"date\" shows a date field as \"Due tomorrow\" or \"Overdue\" (format), \"count\" counts rows (count; add \"zero_text\" for an empty list). A List's \"where\" may also be a list of conditions that all match, as in \"count\" (e.g. [{{\"field\": \"deleted\", \"equals\": \"false\"}}, {{\"field\": \"category\", \"equals\": \"$form.tab\"}}]: a blank value matches everything). Use \"row\" as the list's item_template. Inside a core://Row, use core://Container (not core://Column, which takes the whole width) and a core://Spacer to push the delete to the end.",
        out["notes"].as_str().unwrap_or("")
    ));
    out
}

async fn describe(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let version = args["version"].as_i64();
    let lookup = name.clone();
    let found = pod
        .blocking(move |s| {
            let apps = pod_layouts::layouts_with_source(s, NAME)?;
            s.with_conn(|c| {
                let Some((latest, def)) = db::latest_def(c, &lookup)? else {
                    return Ok(None);
                };
                let shown = match version {
                    Some(v) => db::def_version(c, &lookup, v)?.map(|d| (v, d)),
                    None => Some((latest, def.clone())),
                };
                let rows = db::all_rows(c, &lookup, false)?.len();
                let aside: Vec<Value> = db::aside_counts(c, &lookup)?
                    .into_iter()
                    .map(|(f, n)| json!({ "field": def.by_id(&f).map_or(f.clone(), |x| x.name.clone()), "values": n }))
                    .collect();
                let failures = db::failure_counts(c, &lookup)?;
                let mut computed: Vec<Value> = db::computed_counts(c, &lookup)?
                    .into_iter()
                    .filter_map(|(f, n)| def.by_id(&f).filter(|x| x.computed()).map(|x| json!({ "field": x.name, "answers": n })))
                    .collect();
                // Rows whose model answer failed: retried, or given up on.
                for (f, retrying, gave_up) in failures {
                    let Some(x) = def.by_id(&f).filter(|x| x.computed()) else { continue };
                    let entry = match computed.iter_mut().find(|e| e["field"] == x.name.as_str()) {
                        Some(e) => e,
                        None => {
                            computed.push(json!({ "field": x.name, "answers": 0 }));
                            computed.last_mut().expect("pushed")
                        }
                    };
                    entry["failed"] = json!({ "retrying": retrying, "gave_up": gave_up });
                    if gave_up > 0 {
                        entry["fix"] = json!("apply_change {op: recompute_field} asks the model again for every row still empty");
                    }
                }
                let lookups = crate::lookup::report(c, &def)?;
                let quantities = quantity_report(c, &def)?;
                let lookups = (lookups, quantities);
                let others = db::defs_by_name(c)?;
                let fill_state = crate::fill::states(c, &def)?;
                Ok(Some((
                    latest,
                    shown,
                    rows,
                    (aside, computed, lookups, others, (def.clone(), fill_state)),
                    db::def_history(c, &lookup, 20)?,
                    db::recent_edits(c, &lookup)?,
                    users(&apps, &lookup),
                )))
            })
        })
        .await
        .map_err(db_err)?;
    let Some((
        latest,
        shown,
        rows,
        (aside, computed, (lookups, quantities), others, (live, fill_state)),
        history,
        edits,
        used_by,
    )) = found
    else {
        return Err(missing(&name));
    };
    let Some((v, def)) = shown else {
        return Err(json!({ "error": "version_not_found", "collection": name, "status": 404 }));
    };
    Ok(json!({
        "collection": name,
        "version": v,
        "latest_version": latest,
        "definition": def.describe(pod_core::util::now_secs(), KEEP_SECS, &others),
        "rows": rows,
        "kept_aside": aside,
        "computed_answers": computed,
        "lookups": lookups,
        "quantities": quantities,
        "history": history,
        "recent_edits": edits,
        "used_by": used_by,
        "fill": fill_health(pod, &live, &fill_state).await,
        "layout_hints": hints(&def),
    }))
}

/// For each quantity field of `def`: how many live rows hold text that
/// didn't parse (left out of sums), with a few samples.
fn quantity_report(c: &rusqlite::Connection, def: &Definition) -> rusqlite::Result<Vec<Value>> {
    let fields: Vec<&model::Field> = def
        .live_fields()
        .filter(|f| f.kind == model::Kind::Quantity)
        .collect();
    if fields.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db::all_rows(c, &def.name, false)?;
    Ok(fields
        .iter()
        .map(|f| {
            let bad: Vec<Value> = rows
                .iter()
                .filter_map(|r| {
                    let v = r.values.get(&f.id)?;
                    crate::quantity::is_unparsed(v).then(|| json!({ "id": r.id, "value": v }))
                })
                .collect();
            json!({ "field": f.name, "unparsed": bad.len(),
                    "samples": bad.into_iter().take(5).collect::<Vec<_>>() })
        })
        .collect())
}

/// Every filler's health of `def` ([`crate::fill::health`]).
async fn fill_health(
    pod: &Pod,
    def: &Definition,
    states: &HashMap<String, crate::fill::State>,
) -> Value {
    if def.fillers.is_empty() {
        return Value::Null;
    }
    let off = crate::fill::off(pod).await;
    let agent = pod
        .adapter::<CollectionsAdapter>()
        .is_some_and(|a| a.agent().is_some());
    let now = pod_core::util::now_secs();
    Value::Array(
        def.fillers
            .iter()
            .map(|f| {
                let st = states.get(&f.id).cloned().unwrap_or_default();
                crate::fill::health(def, f, &st, now, off, agent)
            })
            .collect(),
    )
}

async fn status(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let only = args["collection"].as_str().map(str::to_owned);
    let rows = pod
        .blocking(|s| {
            s.with_conn(|c| {
                let mut out = Vec::new();
                for (_, def) in db::all_defs(c)? {
                    if !def.fillers.is_empty() {
                        let st = crate::fill::states(c, &def)?;
                        out.push((def, st));
                    }
                }
                Ok((out, crate::fill::secret_names(c)?))
            })
        })
        .await
        .map_err(db_err)?;
    let (rows, secrets) = rows;
    let mut out = Vec::new();
    for (def, st) in rows {
        if only.as_ref().is_some_and(|o| *o != def.name) {
            continue;
        }
        if let Value::Array(h) = fill_health(pod, &def, &st).await {
            out.extend(h);
        }
    }
    if let Some(o) = &only
        && out.is_empty()
    {
        return Err(
            json!({ "error": "no_filler", "collection": o, "status": 404,
            "detail": "that collection has no filler (or doesn't exist)" }),
        );
    }
    Ok(json!({ "fillers": out, "secrets": secrets }))
}

async fn refresh(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let (_, def) = current(pod, &name).await?;
    if def.fillers.is_empty() {
        return Err(
            json!({ "error": "no_filler", "collection": name, "status": 409,
            "detail": "set one with apply_change {op: set_filler}" }),
        );
    }
    let ids: Vec<String> = match args["filler"].as_str() {
        Some(id) => match crate::filler::find(&def, id) {
            Some(f) => vec![f.id.clone()],
            None => return Err(bad(format!("no filler '{id}' in '{name}'"))),
        },
        None => def.fillers.iter().map(|f| f.id.clone()).collect(),
    };
    let mut out = Vec::new();
    for id in ids {
        let f = crate::filler::find(&def, &id).cloned();
        if f.as_ref().is_some_and(crate::filler::Filler::uses_model) {
            crate::fill::spawn(pod, &name, Some(&id), true);
            out.push(json!({ "filler": id, "ok": true, "started": true,
                "detail": "a model is filling it in the background (it can take minutes); collections_status shows how it went" }));
            continue;
        }
        let mut v = crate::fill::fill(pod, &name, &id, true).await.to_json();
        v["filler"] = json!(id);
        v["title"] = json!(f.map(|f| f.title).unwrap_or_default());
        out.push(v);
    }
    Ok(json!({ "collection": name, "fills": out }))
}

async fn set_secret(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "name")?.to_owned();
    if !crate::filler::valid_secret_name(&name) {
        return Err(bad(
            "a secret's name is [a-z][a-z0-9_]*, at most 40 characters",
        ));
    }
    let value = match &args["value"] {
        Value::Null => None,
        Value::String(v) if !v.trim().is_empty() && v.len() <= 4_000 => Some(v.trim().to_owned()),
        _ => {
            return Err(bad(
                "\"value\" is the secret (1..=4000 characters) or null to forget it",
            ));
        }
    };
    let host = match (&value, args["host"].as_str()) {
        (None, _) => None,
        (Some(_), Some(h)) => Some(crate::fill::secret_host(h).ok_or_else(|| {
            bad("\"host\" is the host name the secret is sent to, e.g. api.example.com")
        })?),
        (Some(_), None) => {
            return Err(bad(
                "\"host\" is required: the host name the secret may be sent to (e.g. api.example.com)",
            ));
        }
    };
    let now = pod_core::util::now_secs();
    let n = name.clone();
    let shown = host.clone();
    let names = pod
        .blocking(move |s| {
            let v = value.as_deref().zip(host.as_deref());
            crate::fill::set_secret(s, &n, v, now)?;
            s.with_conn(crate::fill::secret_names)
        })
        .await
        .map_err(db_err)?;
    Ok(
        json!({ "ok": true, "secret": name, "secrets": names, "host": shown,
        "detail": "stored sealed; a filler's header sends it as {secret:<name>}, only to this host" }),
    )
}

/// Most rows or sample items a preview shows.
const PREVIEW_ROWS: usize = 10;

async fn preview(pod: &Pod, adapter: &CollectionsAdapter, args: &Value) -> Result<Value, Value> {
    // An `mcp` filler calls its tool (the mcp jack, not web).
    if let Some(name) = args["collection"].as_str() {
        let (_, def) = current(pod, name).await?;
        let f = match args["filler"].as_str() {
            Some(id) => crate::filler::find(&def, id).cloned(),
            None => def.fillers.first().cloned(),
        };
        if let Some(f) = f.as_ref().filter(|f| f.is_range()) {
            return preview_range(pod, adapter, &def, f);
        }
        if let Some(f) = f.filter(|f| f.is_mcp()) {
            return preview_mcp(pod, adapter, &def, &f).await;
        }
    }
    if !crate::fill::allowed(pod).await {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "fill_off",
            crate::fill::FILL_OFF,
        ));
    }
    // The recipe: a collection's filler, or the arguments.
    let (url, spec, map, headers, def, render) = if let Some(name) = args["collection"].as_str() {
        let (_, def) = current(pod, name).await?;
        let f = match args["filler"].as_str() {
            Some(id) => crate::filler::find(&def, id).cloned(),
            None => def.fillers.first().cloned(),
        };
        let Some(f) = f else {
            return Err(bad(format!(
                "'{name}' has no such filler; pass filler (an id from describe_collection) or a recipe"
            )));
        };
        if f.is_llm() {
            return Err(bad(
                "an llm filler can't be previewed (it asks a model; collections_refresh runs it)",
            ));
        }
        if f.is_images() {
            return Err(bad(
                "an images filler can't be previewed (it searches each row; collections_refresh runs it)",
            ));
        }
        let crate::filler::Source::Fetch {
            url,
            url_field,
            headers,
            map,
            ..
        } = &f.source
        else {
            return Err(bad(
                "an agent filler can't be previewed (collections_refresh runs it)",
            ));
        };
        let url = match (url, url_field) {
            (Some(u), _) => u.clone(),
            (None, Some(field)) => {
                let (n, field) = (name.to_owned(), field.clone());
                pod.blocking(move |s| s.with_conn(|c| db::all_rows(c, &n, false)))
                    .await
                    .map_err(db_err)?
                    .into_iter()
                    .find_map(|r| {
                        r.values
                            .get(&field)
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .ok_or_else(|| bad("no row has a URL yet"))?
            }
            _ => return Err(bad("the filler has no URL")),
        };
        let map: Vec<(String, String)> = map
            .iter()
            .map(|(id, p)| (def.name_of(id), p.clone()))
            .collect();
        let render = f.render().cloned();
        (
            url,
            f.extract_spec(),
            map,
            headers.clone(),
            Some(def),
            render,
        )
    } else {
        let url = str_arg(args, "url")?.to_owned();
        let kind = web_extract::items::Kind::parse(args["extract"].as_str().unwrap_or(""))
            .ok_or_else(|| {
                bad(format!(
                    "\"extract\" is one of {}",
                    web_extract::items::Kind::ALL.join(", ")
                ))
            })?;
        let types = match &args["types"] {
            Value::String(t) => vec![t.clone()],
            Value::Array(a) => a
                .iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        let spec = web_extract::items::Spec {
            kind,
            items: args["items"].as_str().unwrap_or("").to_owned(),
            types,
            script: args["script"].as_str().unwrap_or("").to_owned(),
            zone: args["zone"].as_str().unwrap_or("").to_owned(),
        };
        let map: Vec<(String, String)> = args["map"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let headers: std::collections::BTreeMap<String, String> = args["headers"]
            .as_object()
            .map(|h| {
                h.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let render = crate::filler::Render::parse(args.get("render"), "render").map_err(bad)?;
        if render.is_some() && !headers.is_empty() {
            return Err(bad(
                "a rendered page is fetched without headers: the browser would send them to every site the page loads from",
            ));
        }
        (url, Some(spec), map, headers, None, render)
    };
    let Some(spec) = spec else {
        return Err(bad("unknown extractor"));
    };
    let url = web_extract::fetch::check_url(&url, adapter.allow_private_hosts()).map_err(bad)?;
    if spec.kind == web_extract::items::Kind::Selectors && map.is_empty() {
        return Err(bad(
            "the selectors extractor needs map: {name: CSS selector}",
        ));
    }
    web_extract::items::check(&spec, &map).map_err(bad)?;
    let mut sent = std::collections::BTreeMap::new();
    for (k, v) in headers {
        sent.insert(k, v);
    }
    let mut sensitive: Vec<String> = Vec::new();
    let fetched = if let Some(r) = &render {
        crate::fill::render_page(pod, adapter, &url, r).await
    } else {
        let resolved = crate::fill::resolve_headers_for(pod, adapter, &sent, &url).map_err(bad)?;
        sensitive = resolved.sensitive.clone();
        let client = if resolved.has_secret {
            adapter.secret_fetch_client()
        } else {
            adapter.fetch_client()
        };
        web_extract::hosts::fetch(
            &pod.local::<web_extract::hosts::Hosts>(),
            client,
            &url,
            None,
            None,
            &resolved.headers,
        )
        .await
    }
    .map_err(|e| {
        let mut v = fail(StatusCode::BAD_GATEWAY, "fetch_failed", e.message);
        if e.blocked {
            v["blocked"] = json!(true);
        }
        v
    })?;
    let web_extract::fetch::Fetched::Body { bytes, .. } = fetched else {
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "fetch_failed",
            "the site answered 304",
        ));
    };
    // A host that echoes the request's headers would hand the secret to
    // Claude: scrub it before anything is extracted (issue #460).
    let body = if sensitive.is_empty() {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        crate::fill::redact(&String::from_utf8_lossy(&bytes), &sensitive)
    };
    let body = body.as_str();
    // What the page holds is its site's text (issue #439).
    let from = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.clone());
    let mut out = json!({ "url": url, "extract": spec.kind.name(), "bytes": bytes.len() });
    if render.is_some() {
        out["rendered"] = json!(true);
    }
    if map.is_empty() {
        // Sample items to write the paths from.
        let items = crate::fill::sample_items(&spec, body).map_err(bad)?;
        out["items"] = json!(items.len());
        out["sample_items"] =
            untrusted(&from, json!(items.into_iter().take(3).collect::<Vec<_>>()));
        out["next"] = json!("write map: {field: path into an item} and preview again");
        return Ok(out);
    }
    let rows = web_extract::items::extract(&spec, body, &url, &map).map_err(bad)?;
    out["items"] = json!(rows.len());
    if let Some(def) = &def {
        // As the collection would store them.
        let by_id: Vec<Map<String, Value>> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .filter_map(|(n, v)| def.field(n).map(|f| (f.id.clone(), v.clone())))
                    .collect()
            })
            .collect();
        let (vals, skipped) =
            crate::fill::coerce_preview(def, by_id, adapter.caps.text_chars, spec.zone().as_ref());
        out["rows"] = untrusted(
            &from,
            json!(
                vals.iter()
                    .take(PREVIEW_ROWS)
                    .map(|v| model::by_name(def, v))
                    .collect::<Vec<_>>()
            ),
        );
        out["values_skipped"] = json!(skipped);
    } else {
        out["rows"] = untrusted(
            &from,
            json!(rows.into_iter().take(PREVIEW_ROWS).collect::<Vec<_>>()),
        );
    }
    if out["items"] == 0 {
        out["detail"] =
            json!("no items: check items/types (preview without map shows what the page has)");
    }
    Ok(out)
}

/// Preview an `mcp` filler: call its tool now and show the rows it would
/// store.
async fn preview_mcp(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    f: &crate::filler::Filler,
) -> Result<Value, Value> {
    let now = pod_core::util::now_secs();
    let (rows, _) = crate::fill::mcp_rows(pod, f, now)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, "mcp_failed", e))?;
    let n = rows.len();
    let (vals, skipped) =
        crate::fill::coerce_preview(def, rows, adapter.caps.text_chars, f.zone().as_ref());
    // The tool's answer is its server's text (issue #439).
    let from = match &f.source {
        crate::filler::Source::Mcp { server, .. } => format!("mcp:{server}"),
        _ => format!("filler:{}", f.id),
    };
    let rows: Vec<Map<String, Value>> = vals
        .iter()
        .take(PREVIEW_ROWS)
        .map(|v| model::by_name(def, v))
        .collect();
    let mut out = json!({
        "filler": f.id, "kind": "mcp", "items": n,
        "rows": untrusted(&from, json!(rows)),
        "values_skipped": skipped,
    });
    if n == 0 {
        out["detail"] = json!("no items: check items (mcp_call_tool shows what the tool answers)");
    }
    Ok(out)
}

/// Preview a `range` filler: the rows it makes today (no jack needed).
fn preview_range(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    def: &Definition,
    f: &crate::filler::Filler,
) -> Result<Value, Value> {
    let (rows, _) = crate::fill::range_rows(pod, f, pod_core::util::now_secs())
        .map_err(|e| fail(StatusCode::UNPROCESSABLE_ENTITY, "range_failed", e))?;
    let n = rows.len();
    let (vals, skipped) = crate::fill::coerce_preview(def, rows, adapter.caps.text_chars, None);
    Ok(json!({
        "filler": f.id, "kind": "range", "items": n,
        "rows": vals.iter().take(PREVIEW_ROWS).map(|v| model::by_name(def, v)).collect::<Vec<_>>(),
        "values_skipped": skipped,
    }))
}

// ---------- change sets ----------

/// The layout edits of a change set, as construct
/// `update_layout` arguments (`base_version` is the name's history seq,
/// as `get_layout` shows it): checked and written in the change set's
/// transaction.
fn construct_edits(args: &Value, note: &str) -> Result<Vec<Value>, Value> {
    let Some(items) = args.get("apps").filter(|a| !a.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(items) = items.as_array() else {
        return Err(bad(
            "\"apps\" is a list of {app, layout | json_patch, base_version?}",
        ));
    };
    let mut out: Vec<Value> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let Some(id) = item["app"].as_str().filter(|a| !a.is_empty()) else {
            return Err(bad(format!("apps[{i}]: \"app\" is a layout id")));
        };
        if out.iter().any(|a| a["app"] == id) {
            return Err(bad(format!("app '{id}' is listed twice")));
        }
        if item.get("json_patch").is_none_or(Value::is_null) && !item["layout"].is_object() {
            return Err(bad(format!("apps[{i}]: pass \"layout\" or \"json_patch\"")));
        }
        let mut a = item.clone();
        if a.get("note").is_none_or(Value::is_null) {
            a["note"] = json!(note);
        }
        out.push(a);
    }
    Ok(out)
}

/// A layout that a change of a collection would break.
fn breaks_layout(
    id: &str,
    title: &str,
    in_change_set: bool,
    e: &pod_core::validate::LayoutError,
) -> Value {
    json!({
        "error": "breaks_layout", "status": 422, "layout": id, "title": title,
        "in_change_set": in_change_set,
        "at": e.pointer, "allowed": e.allowed,
        "detail": format!("layout '{title}' ({id}) would break: {}{}",
            if e.pointer.is_empty() { String::new() } else { format!("{}: ", e.pointer) }, e.message),
        "fix": "change that layout in the same apply_change (apps: [{app, layout | json_patch}]), or change the collection differently",
    })
}

/// Inside the change set's transaction: write `edits`
/// to the construct (as `update_layout` does), then check every layout as
/// clients draw it against `defs`. `Ok(Err(_))`: refused; roll back.
#[allow(clippy::type_complexity)]
fn construct_commit(
    tx: &rusqlite::Transaction<'_>,
    pod: &Pod,
    edits: &[Value],
    defs: &HashMap<String, Definition>,
) -> rusqlite::Result<Result<(Vec<(String, i64)>, Vec<(String, i64)>), Value>> {
    use pod_core::construct::write as cwrite;
    let mut versions = Vec::new();
    let mut moved = Vec::new();
    for a in edits {
        let id = a["app"].as_str().unwrap_or("").to_owned();
        match cwrite::update_layout(pod, tx, a) {
            Ok((answer, m)) => {
                versions.push((id, answer["version"].as_i64().unwrap_or(0)));
                moved.extend(m);
            }
            Err(mut b) => {
                let status = match b["error"].as_str() {
                    Some("stale_base") => 409,
                    Some("layout_not_found" | "not_found") => 404,
                    Some("invalid_layout") => 422,
                    _ => 400,
                };
                b["app"] = json!(id);
                b["status"] = json!(status);
                return Ok(Err(b));
            }
        }
    }
    let feeds: Vec<String> = cwrite::home_feeds(tx)
        .into_iter()
        .filter(|(s, _)| s == NAME)
        .map(|(_, f)| f)
        .collect();
    if feeds.is_empty() {
        return Ok(Ok((versions, moved)));
    }
    for l in cwrite::live_layouts(tx)? {
        if let Err(e) = crate::bindings::check(&l.node, &feeds, defs) {
            let edited = versions.iter().any(|(a, _)| *a == l.name);
            return Ok(Err(breaks_layout(&l.name, &l.title, edited, &e)));
        }
    }
    Ok(Ok((versions, moved)))
}

/// Apply `planned` (a new definition of `name`, from `base`) with the layout
/// edits of `args["apps"]`, atomically: every layout using Collections must
/// fit the new definition as devices draw it, else nothing applies.
async fn commit(
    pod: &Pod,
    caps: crate::Caps,
    name: &str,
    base: i64,
    planned: Planned,
    args: &Value,
    note: String,
) -> Result<Value, Value> {
    // The layout edits, and the check of every layout as devices draw it,
    // happen in the change set's transaction below.
    let edits = construct_edits(args, &note)?;
    let catalog_apps = {
        pod.blocking(move |s| s.with_conn(db::all_defs))
            .await
            .map_err(db_err)?
    };
    let defs = catalog_apps;
    let mut new_defs: HashMap<String, Definition> =
        defs.into_iter().map(|(_, d)| (d.name.clone(), d)).collect();
    new_defs.insert(name.to_owned(), planned.def.clone());
    for d in &planned.created {
        new_defs.insert(d.name.clone(), d.clone());
    }
    // Every lookup (into this collection too) against the new definitions.
    for d in new_defs.values() {
        model::check_lookups(d, &new_defs)
            .map_err(|e| json!({ "error": "invalid_change", "detail": e, "status": 400 }))?;
    }
    let pod2 = pod.clone();
    let def = planned.def.clone();
    let convert = planned.convert.clone();
    let to_compute = planned.compute.clone();
    let to_recompute = planned.recompute.clone();
    let coll = name.to_owned();
    let others = new_defs.clone();
    let created = planned.created.clone();
    let (version, conversions, versions, moved, jobs) = refusable(pod, move |tx| {
        let current = db::latest_def(tx, &coll)?.map(|(v, _)| v);
        if current != Some(base) {
            return Ok(Err(json!({ "error": "stale_base", "status": 409, "latest_version": current,
                "detail": "the collection changed since that version; describe_collection and reapply" })));
        }
        let now = pod_core::util::now_secs();
        let version = db::append_def(tx, &def, &note, pod_layouts::AUTHOR_CLAUDE, now)?;
        let mut conversions = Vec::new();
        for id in &convert {
            if let Some(f) = def.by_id(id) {
                conversions.push(db::convert_field(tx, &coll, f, version, now, caps.text_chars)?);
            }
        }
        let dups = crate::refs::duplicates(tx, &def)?;
        if !dups.is_empty() {
            return Ok(Err(json!({ "error": "not_unique", "status": 409, "duplicates": dups,
                "detail": "a unique field would have rows sharing a value (listed in duplicates): change or delete those rows first" })));
        }
        // The lookups were checked against the other collections as read
        // above; one changed meanwhile: refuse.
        if db::defs_by_name(tx)?
            .into_iter()
            .any(|(n, d)| n != coll && others.get(&n) != Some(&d))
        {
            return Ok(Err(json!({ "error": "stale_base", "status": 409,
                "detail": "another collection changed meanwhile; apply the change again" })));
        }
        if let Err(e) = create_rules(tx, &created, caps, now)? {
            return Ok(Err(e));
        }
        let mut jobs = Vec::new();
        // A new compute, or one asked again, starts its attempts afresh.
        for id in to_compute.iter().chain(&to_recompute) {
            db::clear_field_failures(tx, &coll, id)?;
        }
        for id in &to_compute {
            jobs.extend(crate::compute::backfill(tx, &def, id, false, caps.rows, now, caps)?);
        }
        for id in &to_recompute {
            jobs.extend(crate::compute::backfill(tx, &def, id, true, caps.rows, now, caps)?);
        }
        let (versions, moved) = match construct_commit(tx, &pod2, &edits, &others)? {
            Ok(vm) => vm,
            Err(b) => return Ok(Err(b)),
        };
        Ok(Ok((version, conversions, versions, moved, jobs)))
    })
    .await?;
    pod_core::construct::write::publish(pod, &moved);
    pod.publish_data_all(NAME);
    let computing = jobs.len();
    crate::compute::spawn(pod, name, jobs);
    Ok(json!({
        "ok": true, "collection": name, "version": version,
        "conversions": conversions.iter().map(|c| c.to_json(&planned.def.title)).collect::<Vec<_>>(),
        "apps": versions.iter().map(|(a, v)| json!({ "app": a, "version": v })).collect::<Vec<_>>(),
        "definition": planned.def.describe(pod_core::util::now_secs(), KEEP_SECS, &new_defs),
        "computing_rows": computing,
        "created_rules": planned.created.iter().map(|d| &d.name).collect::<Vec<_>>(),
    }))
}

/// Store the rules collections `created` that a learning lookup named
/// (each new, unless an identical one exists already), inside `tx`.
/// `Err`: the refusal body.
fn create_rules(
    tx: &rusqlite::Transaction<'_>,
    created: &[Definition],
    caps: crate::Caps,
    now: i64,
) -> rusqlite::Result<Result<(), Value>> {
    for d in created {
        if db::deleted_at(tx, &d.name)?.is_some() {
            return Ok(Err(
                json!({ "error": "collection_deleted", "collection": d.name, "status": 409,
                "detail": format!("a lookup learns into '{}', a deleted collection: restore_collection brings it back, or name another", d.name) }),
            ));
        }
        match db::latest_def(tx, &d.name)? {
            Some((_, existing)) if existing == *d => continue,
            Some(_) => {
                return Ok(Err(json!({ "error": "stale_base", "status": 409,
                    "detail": format!("collection '{}' was made meanwhile; apply the change again", d.name) })));
            }
            None => {}
        }
        if db::all_defs(tx)?
            .iter()
            .filter(|(_, d)| !crate::inbox::is_inbox(&d.name))
            .count()
            >= caps.collections
        {
            return Ok(Err(fail(
                StatusCode::CONFLICT,
                "too_many_collections",
                format!(
                    "a pod has at most {} collections (the rules collection '{}' would be one more)",
                    caps.collections, d.name
                ),
            )));
        }
        db::append_def(
            tx,
            d,
            "Rules learned for a model field",
            pod_layouts::AUTHOR_CLAUDE,
            now,
        )?;
    }
    Ok(Ok(()))
}

async fn current(pod: &Pod, name: &str) -> Result<(i64, Definition), Value> {
    let lookup = name.to_owned();
    pod.blocking(move |s| s.with_conn(|c| db::latest_def(c, &lookup)))
        .await
        .map_err(db_err)?
        .ok_or_else(|| missing(name))
}

async fn apply_change(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    args: &Value,
) -> Result<Value, Value> {
    let caps = adapter.caps;
    let name = str_arg(args, "collection")?.to_owned();
    let Some(ops) = args["changes"].as_array() else {
        return Err(bad("\"changes\" is a list of ops"));
    };
    let (version, def) = current(pod, &name).await?;
    let others = pod
        .blocking(|s| s.with_conn(db::defs_by_name))
        .await
        .map_err(db_err)?;
    if let Some(b) = args["base_version"].as_i64()
        && b != version
    {
        return Err(
            json!({ "error": "stale_base", "status": 409, "latest_version": version,
            "detail": "the collection changed since that version; describe_collection and reapply" }),
        );
    }
    let planned = changes::plan(&def, ops, &others, pod_core::util::now_secs(), caps.text_chars, caps.fields, adapter.allow_private_hosts())
        .map_err(|(i, e)| json!({ "error": "invalid_change", "at": format!("/changes/{i}"), "detail": e, "status": 400 }))?;
    crate::filler::check(&planned.def, caps.agent_fills_per_day)
        .map_err(|e| json!({ "error": "invalid_change", "detail": e, "status": 400 }))?;
    let changed_fillers = planned.fillers_changed(&def);
    let warnings = crate::filler::toggle_warnings(&planned.def);
    let keep: Vec<String> = planned.def.fillers.iter().map(|f| f.id.clone()).collect();
    // Removed fillers whose rows go with them (`remove_filler` without
    // `keep_rows`).
    let kept_rows: Vec<&str> = ops
        .iter()
        .filter(|o| o["op"] == "remove_filler" && o["keep_rows"] == true)
        .filter_map(|o| o["filler"].as_str())
        .collect();
    let dropped: Vec<crate::filler::Filler> = def
        .fillers
        .iter()
        .filter(|f| !keep.contains(&f.id) && !kept_rows.contains(&f.id.as_str()))
        .cloned()
        .collect();
    for r in &planned.def.reminders {
        if let Some(app) = r.app.clone() {
            let exists = pod
                .blocking(move |s| s.with_conn(|c| pod_layouts::pod_layout(c, &app)))
                .await
                .map_err(db_err)?
                .is_some();
            if !exists {
                return Err(json!({ "error": "invalid_change", "status": 400,
                    "detail": format!("reminder '{}': no layout '{}' (list_layouts lists them)", r.id, r.app.as_deref().unwrap_or("")) }));
            }
        }
    }
    let note = note_arg(args, "Changed");
    let mut out = commit(pod, caps, &name, version, planned, args, note).await?;
    let n = name.clone();
    let now = pod_core::util::now_secs();
    let removed_rows = pod
        .blocking(move |s| {
            s.transaction(|tx| {
                let mut gone = 0;
                for f in &dropped {
                    gone += crate::fill::drop_rows_of(tx, &def, f, now)?;
                }
                crate::fill::forget_removed(tx, &n, &keep)?;
                Ok(gone)
            })
        })
        .await
        .unwrap_or(0);
    if removed_rows > 0 {
        out["removed_rows"] = json!({ "rows": removed_rows,
            "detail": "the removed filler's rows were deleted (restorable with edit_rows restore for 30 days)" });
    }
    for f in &changed_fillers {
        crate::fill::spawn(pod, &name, Some(f), true);
    }
    if !changed_fillers.is_empty() {
        out["filling"] = json!({ "fillers": changed_fillers,
            "detail": "a fill started for each new or changed filler; collections_status shows how it went" });
        if !warnings.is_empty() {
            out["warnings"] = json!(warnings);
        }
    }
    Ok(out)
}

async fn restore_version(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let Some(n) = args["version"].as_i64() else {
        return Err(bad("\"version\" is required"));
    };
    let (version, def) = current(pod, &name).await?;
    let lookup = name.clone();
    let old = pod
        .blocking(move |s| s.with_conn(|c| db::def_version(c, &lookup, n)))
        .await
        .map_err(db_err)?
        .ok_or_else(|| json!({ "error": "version_not_found", "collection": name, "version": n, "status": 404 }))?;
    let planned = changes::restore(&def, &old, pod_core::util::now_secs(), caps.fields)
        .map_err(|e| json!({ "error": "invalid_change", "detail": e, "status": 400 }))?;
    let note = note_arg(args, &format!("Restored version {n}"));
    commit(pod, caps, &name, version, planned, args, note).await
}

// ---------- deleting a collection ----------

/// Delete collection `{collection}` (issue #107): refused while a layout
/// binds it or another collection's lookup or count reads it; its feed leaves
/// home's; the data stays, hidden, for [`KEEP_SECS`].
async fn delete(pod: &Pod, caps: Caps, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let (version, def) = current(pod, &name).await?;
    if let Some(b) = args["base_version"].as_i64()
        && b != version
    {
        return Err(
            json!({ "error": "stale_version", "collection": name, "latest_version": version,
            "status": 409, "detail": "the collection changed since you read it: describe_collection" }),
        );
    }
    if let Some(sys) = &def.system {
        return Err(fail(
            StatusCode::CONFLICT,
            "system_collection",
            format!(
                "the {} app's collection can't be deleted (change its fillers and alerts instead)",
                sys.app
            ),
        ));
    }
    let (apps, others) = pod
        .blocking(move |s| {
            let apps = pod_layouts::layouts_with_source(s, NAME)?;
            let others = s.with_conn(db::defs_by_name)?;
            Ok((apps, others))
        })
        .await
        .map_err(db_err)?;
    let used_by = users(&apps, &name);
    if !used_by.is_empty() {
        let titles: Vec<String> = used_by
            .iter()
            .map(|u| {
                format!(
                    "'{}' ({})",
                    u["title"].as_str().unwrap_or(""),
                    u["layout"].as_str().unwrap_or("")
                )
            })
            .collect();
        return Err(
            json!({ "error": "collection_in_use", "collection": name, "used_by": used_by,
            "status": 409,
            "detail": format!("layouts still show it: {}; remove it from them with update_layout first", titles.join(", ")) }),
        );
    }
    let mut lookups: Vec<Value> = others
        .values()
        .filter(|d| d.name != name)
        .flat_map(|d| {
            d.live_fields()
                .filter(|f| {
                    f.compute.iter().any(|c| c.reads() == Some(name.as_str()))
                        || f.collection.as_deref() == Some(name.as_str())
                })
                .map(|f| json!({ "collection": d.name, "field": f.name }))
                .collect::<Vec<_>>()
        })
        .collect();
    lookups.extend(crate::actions::users_of(&others, &name));
    if !lookups.is_empty() {
        return Err(
            json!({ "error": "collection_in_use", "collection": name, "lookups": lookups,
            "status": 409,
            "detail": "another collection's field points at it (a ref), computes from it (a lookup, count or rollup) or an action reads or adds its rows: change those with apply_change first" }),
        );
    }
    // Home's feed goes first: a layout naming a collection that doesn't
    // exist fails validation, so a stale feed would block home's saves. It
    // is dropped from home's feeds as they are when written, so deletes at
    // once each drop their own (issue #358).
    let dropped_feed =
        pod_core::construct::write::drop_home_feed(pod, (NAME.to_owned(), name.clone())).await?;
    let target = name.clone();
    let now = pod_core::util::now_secs();
    refusable(pod, move |tx| {
        if db::latest_def(tx, &target)?.map(|(v, _)| v) != Some(version) {
            return Ok(Err(
                json!({ "error": "stale_version", "collection": target, "status": 409,
                "detail": "the collection changed meanwhile: try again" }),
            ));
        }
        db::mark_deleted(tx, &target, now)?;
        db::trim_retained(tx, crate::retained_cap(caps))?;
        Ok(Ok(()))
    })
    .await?;
    pod.publish_data_all(NAME);
    Ok(
        json!({ "ok": true, "collection": name, "deleted_at": now, "purged_after": now + KEEP_SECS,
        "dropped_from_home_feeds": dropped_feed,
        "detail": "restore_collection brings it back with its rows for 30 days" }),
    )
}

/// Bring back deleted collection `{collection}`.
async fn restore(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let target = name.clone();
    // Rows Claude brings back below outward access are Claude's (decision
    // `mcp-access-levels`).
    let writer = db::current_writer();
    let (version, fillers) = refusable(pod, move |tx| {
        let Some((_, version, def)) = db::deleted_def(tx, &target)? else {
            return Ok(Err(json!({ "error": "collection_not_found", "collection": target, "status": 404,
                "detail": "no deleted collection has this name (list_collections lists deleted ones)" })));
        };
        if db::all_defs(tx)?.iter().filter(|(_, d)| !crate::inbox::is_inbox(&d.name)).count() >= caps.collections {
            return Ok(Err(fail(
                StatusCode::CONFLICT,
                "too_many_collections",
                format!("a pod has at most {} collections", caps.collections),
            )));
        }
        let rows = db::all_rows(tx, &target, false)?;
        let bytes: i64 = rows
            .iter()
            .map(|r| serde_json::to_string(&r.values).map_or(0, |s| s.len() as i64))
            .sum();
        if let Err(e) = crate::check_usage(tx, caps, rows.len() as i64, bytes)? {
            return Ok(Err(e));
        }
        db::undelete(tx, &target)?;
        if writer.is_some() {
            let now = pod_core::util::now_secs();
            for r in rows.iter().filter(|r| r.deleted_at.is_none()) {
                db::note_writes(tx, &target, None, r, writer, now)?;
            }
        }
        Ok(Ok((version, !def.fillers.is_empty())))
    })
    .await?;
    if fillers {
        crate::fill::spawn(pod, &name, None, true);
    }
    pod.publish_data_all(NAME);
    Ok(json!({ "ok": true, "collection": name, "version": version,
        "next": format!("To show it, add [\"collections\", \"{name}\"] to a layout's feeds (create_layout, or rename_layout with feeds).") }))
}

// ---------- row edits ----------

/// One planned row change: `(op, row id, before, after)`.
type Change = (&'static str, String, Option<Row>, Row);

/// One planned edit: its change and the write it makes.
type Planned1 = (Change, Map<String, Value>);

/// What writing an edit made: the model jobs and each row's
/// `(id, state before, state after)` for its undo record.
type Written = (
    Vec<crate::compute::Job>,
    Vec<(String, Option<Value>, Value)>,
);

/// Most invalid edits a refusal lists (it counts them all).
const MAX_ERRORS: usize = 50;

async fn edit_rows(pod: &Pod, adapter: &CollectionsAdapter, args: &Value) -> Result<Value, Value> {
    let caps = adapter.caps;
    let name = str_arg(args, "collection")?.to_owned();
    let token = args["confirm"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let url = args["url"]
        .as_str()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let mut note = note_arg(args, "Edited rows");
    let edits: Vec<crate::import::Located> = if let Some(list) = args["edits"].as_array() {
        if url.is_some() {
            return Err(bad("give edits or url, not both"));
        }
        list.iter()
            .enumerate()
            .map(|(i, e)| (format!("/edits/{i}"), e.clone()))
            .collect()
    } else if let Some(url) = url {
        let format = match args["format"].as_str() {
            Some(f) => Some(
                crate::import::Format::parse(f)
                    .ok_or_else(|| bad("format is json, jsonl or csv"))?,
            ),
            None => None,
        };
        fetch_edits(pod, adapter, url, format).await?
    } else if let Some(token) = &token {
        let (lookup, t) = (name.clone(), token.clone());
        let now = pod_core::util::now_secs();
        let staged = pod
            .blocking(move |s| s.with_conn(|c| db::staged(c, &t, &lookup, now)))
            .await
            .map_err(db_err)?;
        let Some((stored, stored_note)) = staged else {
            return Err(fail(
                StatusCode::CONFLICT,
                "preview_expired",
                format!(
                    "no preview of '{name}' has that confirm token now (previews are kept for an hour, and only the newest {}): call edit_rows with the edits or url again for a new preview",
                    db::MAX_STAGED
                ),
            ));
        };
        if args["note"].as_str().is_none_or(|n| n.trim().is_empty()) {
            note = stored_note;
        }
        stored
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| (p[0].as_str().unwrap_or("").to_owned(), p[1].clone()))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        return Err(bad(
            "give edits (a list of {op, id?, values?}), url (a file of them), or confirm (the token of a preview)",
        ));
    };
    edit(pod, caps, name, edits, note, Confirm::Token(token)).await
}

/// `POST /v1/collections/<c>/import`: preview the edits of a posted file
/// and keep them under the preview's confirm token. Nothing changes until
/// edit_rows {collection, confirm} (the user's agreement, over MCP).
pub(crate) async fn import_file(
    pod: &Pod,
    collection: String,
    bytes: &[u8],
    format: crate::import::Format,
    note: Option<String>,
) -> Result<Value, Value> {
    let caps = crate::adapter(pod).caps;
    let edits = crate::import::parse(bytes, format, caps.edit_rows)
        .map_err(|d| fail(StatusCode::UNPROCESSABLE_ENTITY, "invalid_file", d))?;
    let note = note_arg(&json!({ "note": note }), "Imported rows");
    edit(pod, caps, collection, edits, note, Confirm::Token(None)).await
}

/// The edits of the file at `url` ([`crate::import`]), fetched through the
/// pod's web module.
async fn fetch_edits(
    pod: &Pod,
    adapter: &CollectionsAdapter,
    url: &str,
    format: Option<crate::import::Format>,
) -> Result<Vec<crate::import::Located>, Value> {
    if !crate::fill::allowed(pod).await {
        return Err(fail(
            StatusCode::CONFLICT,
            "web_unplugged",
            "the pod's web jack is unplugged, so it can't fetch the file: plug_jack web, or POST the file to /v1/collections/<collection>/import",
        ));
    }
    let url = web_extract::fetch::check_url(url, adapter.allow_private_hosts()).map_err(bad)?;
    let headers = [(
        "Accept".to_owned(),
        "application/json, application/x-ndjson, text/csv, text/plain;q=0.9, */*;q=0.5".to_owned(),
    )];
    let fetched = web_extract::hosts::fetch(
        &pod.local::<web_extract::hosts::Hosts>(),
        adapter.fetch_client(),
        &url,
        None,
        None,
        &headers,
    )
    .await
    .map_err(|e| fail(StatusCode::BAD_GATEWAY, "fetch_failed", e.message))?;
    let web_extract::fetch::Fetched::Body { bytes, .. } = fetched else {
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "fetch_failed",
            "the site answered 304",
        ));
    };
    let format = format
        .or_else(|| crate::import::Format::of_url(&url))
        .unwrap_or_else(|| crate::import::Format::sniff(&String::from_utf8_lossy(&bytes)));
    crate::import::parse(&bytes, format, adapter.caps().edit_rows)
        .map_err(|d| fail(StatusCode::UNPROCESSABLE_ENTITY, "invalid_file", d))
}

/// One row the user just asked for in the conversation ("log that I had
/// the oil changed"): written at once, without edit_rows' preview, since
/// the request itself is the confirmation. Still recorded as an edit, so
/// undo_edit reverts it.
async fn add_row(pod: &Pod, caps: crate::Caps, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let Some(values) = args["values"].as_object().cloned() else {
        return Err(bad("\"values\" is an object {field: value}"));
    };
    let edits = vec![(
        "/values".to_owned(),
        json!({ "op": "create", "values": values }),
    )];
    let mut out = edit(
        pod,
        caps,
        name,
        edits,
        note_arg(args, "Added a row"),
        Confirm::Skip,
    )
    .await?;
    if let Some(id) = out["created"][0].as_str() {
        out["id"] = json!(id);
    }
    Ok(out)
}

/// Whether an edit waits for the user's agreement.
pub(crate) enum Confirm {
    /// edit_rows: applies only with the token of the preview the user saw.
    Token(Option<String>),
    /// add_row: the user's own request is the agreement.
    Skip,
}

/// Plan one edit: the change and the write it makes, or why it is invalid.
#[allow(clippy::too_many_arguments)]
fn plan_one(
    tx: &rusqlite::Transaction<'_>,
    def: &Definition,
    name: &str,
    e: &Value,
    now: i64,
    caps: crate::Caps,
    seen: &mut std::collections::HashSet<String>,
    hasher: &mut Sha256,
) -> rusqlite::Result<Result<Planned1, String>> {
    let op = e["op"].as_str().unwrap_or("");
    let values = e["values"].as_object().cloned().unwrap_or_default();
    if op == "create" {
        if let Err(d) = crate::filler::check_write(def, &values, op, None) {
            return Ok(Err(d));
        }
        let mut row = Row {
            id: db::new_id("r"),
            client_id: None,
            values: Map::new(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            filler: None,
        };
        let values = match crate::refs::prepare_write(tx, def, &values)? {
            Ok(v) => v,
            Err(d) => return Ok(Err(d)),
        };
        if let Err(d) = model::apply_write(
            def,
            &mut row.values,
            &values,
            Write::Create,
            caps.text_chars,
        ) {
            return Ok(Err(d));
        }
        return Ok(Ok((("create", row.id.clone(), None, row), values)));
    }
    let op = match op {
        "update" => "update",
        "delete" => "delete",
        "restore" => "restore",
        other => {
            return Ok(Err(format!(
                "unknown op '{other}' (create, update, delete, restore)"
            )));
        }
    };
    let id = e["id"].as_str().unwrap_or("").trim();
    let Some(before) = db::row(tx, name, id)? else {
        return Ok(Err(format!("no row '{id}'")));
    };
    if let Err(d) = crate::filler::check_write(def, &values, op, before.filler.as_deref()) {
        return Ok(Err(d));
    }
    if !seen.insert(id.to_owned()) {
        return Ok(Err(format!("row '{id}' is edited twice")));
    }
    hasher.update(before.state().to_string());
    let mut after = before.clone();
    after.updated_at = now;
    let mut write = Map::new();
    match op {
        "update" => {
            if before.deleted_at.is_some() {
                return Ok(Err(format!("row '{id}' is deleted (restore it first)")));
            }
            write = match crate::refs::prepare_write(tx, def, &values)? {
                Ok(v) => v,
                Err(d) => return Ok(Err(d)),
            };
            if let Err(d) = model::apply_write(
                def,
                &mut after.values,
                &write,
                Write::Update,
                caps.text_chars,
            ) {
                return Ok(Err(d));
            }
        }
        "delete" => after.deleted_at = Some(now),
        _ => after.deleted_at = None,
    }
    Ok(Ok(((op, id.to_owned(), Some(before), after), write)))
}

/// A refusal listing invalid edits (`(at, detail)`), the first one as
/// `at` and `detail`.
fn invalid_edits(error: &str, status: u16, errors: &[(String, String)], total: usize) -> Value {
    let (at, first) = &errors[0];
    json!({
        "error": error, "status": status, "at": at,
        "detail": if errors.len() == 1 { first.clone() } else {
            format!("{} of {total} edits are invalid and nothing changed; the first: {first}", errors.len())
        },
        "invalid": errors.len(),
        "errors": errors.iter().take(MAX_ERRORS)
            .map(|(at, d)| json!({ "at": at, "detail": d })).collect::<Vec<_>>(),
    })
}

/// Write the planned changes (checking the caps and unique fields), or
/// refuse with every row that breaks a unique field.
#[allow(clippy::too_many_arguments)]
fn write_all(
    tx: &rusqlite::Transaction<'_>,
    def: &Definition,
    name: &str,
    planned: &mut [Change],
    writes: &[Map<String, Value>],
    ats: &[String],
    caps: crate::Caps,
    now: i64,
    writer: Option<&str>,
) -> rusqlite::Result<Result<Written, Value>> {
    let count = |op: &str| planned.iter().filter(|(o, ..)| *o == op).count() as i64;
    let restored = planned
        .iter()
        .filter(|(o, _, b, _)| {
            *o == "restore" && b.as_ref().is_some_and(|b| b.deleted_at.is_some())
        })
        .count() as i64;
    let added_rows = count("create") + restored - count("delete");
    let bytes: i64 = planned
        .iter()
        .map(|(_, _, b, a)| {
            let live = |r: &Row| {
                if r.deleted_at.is_none() {
                    crate::bytes_of(&r.values)
                } else {
                    0
                }
            };
            live(a) - b.as_ref().map_or(0, live)
        })
        .sum();
    if let Err(e) = crate::check_usage(tx, caps, added_rows, bytes)? {
        return Ok(Err(e));
    }
    let mut records = Vec::new();
    let mut jobs = Vec::new();
    let mut not_unique = Vec::new();
    for (((op, id, before, after), write), at) in planned.iter_mut().zip(writes).zip(ats) {
        if *op == "create" || *op == "update" {
            jobs.extend(crate::compute::after_write(
                tx,
                def,
                before.as_ref().map(|b| &b.values),
                after,
                write,
                now,
                caps,
            )?);
        }
        let after = &*after;
        if after.deleted_at.is_none()
            && let Err(d) = crate::refs::check_unique(tx, def, after)?
        {
            not_unique.push((at.clone(), d));
            continue;
        }
        if *op == "create" {
            db::insert_row(tx, name, after)?;
            if writer.is_some() {
                db::note_writes(tx, name, None, after, writer, now)?;
            }
        } else {
            db::save_row(tx, name, after)?;
            // A row Claude brings back is Claude's whole, though its values
            // are as they were: else a row the owner put away comes back
            // counting as theirs (decision `mcp-access-levels`).
            let revived = writer.is_some()
                && before.as_ref().is_some_and(|b| b.deleted_at.is_some())
                && after.deleted_at.is_none();
            db::note_writes(
                tx,
                name,
                before.as_ref().filter(|_| !revived).map(|b| &b.values),
                after,
                writer,
                now,
            )?;
        }
        records.push((id.clone(), before.as_ref().map(Row::state), after.state()));
    }
    if !not_unique.is_empty() {
        return Ok(Err(invalid_edits(
            "not_unique",
            409,
            &not_unique,
            planned.len(),
        )));
    }
    Ok(Ok((jobs, records)))
}

async fn edit(
    pod: &Pod,
    caps: crate::Caps,
    name: String,
    edits: Vec<crate::import::Located>,
    note: String,
    confirm: Confirm,
) -> Result<Value, Value> {
    if edits.is_empty() || edits.len() > caps.edit_rows {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "too_many_edits",
            format!(
                "an edit changes 1..={} rows (this one: {})",
                caps.edit_rows,
                edits.len()
            ),
        ));
    }
    let collection = name.clone();
    // Below outward access, what Claude writes is recorded as Claude's
    // (decision `mcp-access-levels`).
    let writer = db::current_writer();
    let out = refusable(pod, move |tx| {
        let Some((version, def)) = db::latest_def(tx, &name)? else {
            return Ok(Err(missing(&name)));
        };
        let now = pod_core::util::now_secs();
        let mut planned: Vec<Change> = Vec::new();
        let mut writes: Vec<Map<String, Value>> = Vec::new();
        let mut ats: Vec<String> = Vec::new();
        let mut errors: Vec<(String, String)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut hasher = Sha256::new();
        hasher.update(format!("{name}\n{version}\n"));
        for (at, e) in &edits {
            hasher.update(e.to_string());
            match plan_one(tx, &def, &name, e, now, caps, &mut seen, &mut hasher)? {
                Ok((change, write)) => {
                    planned.push(change);
                    writes.push(write);
                    ats.push(at.clone());
                }
                Err(d) => errors.push((at.clone(), d)),
            }
        }
        if !errors.is_empty() {
            return Ok(Err(invalid_edits("invalid_edit", 422, &errors, edits.len())));
        }
        let token = hasher
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let count = |planned: &[Change], op: &str| planned.iter().filter(|(o, ..)| *o == op).count();
        let confirm = match &confirm {
            Confirm::Skip => None,
            Confirm::Token(t) => Some(t.as_deref()),
        };
        if let Some(confirm) = confirm
            && confirm != Some(token.as_str())
        {
            let sample: Vec<Value> = planned
                .iter()
                .take(5)
                .map(|(op, id, before, after)| {
                    let mut v = json!({ "op": op, "after": model::by_name(&def, &after.values) });
                    if let Some(b) = before {
                        v["id"] = json!(id);
                        v["before"] = json!(model::by_name(&def, &b.values));
                    }
                    v
                })
                .collect();
            // Try the whole set (caps, unique fields), then put everything back.
            tx.execute_batch("SAVEPOINT edit_preview")?;
            let tried = write_all(tx, &def, &name, &mut planned.clone(), &writes, &ats, caps, now, writer);
            tx.execute_batch("ROLLBACK TO edit_preview; RELEASE edit_preview")?;
            if let Err(e) = tried? {
                return Ok(Err(e));
            }
            let kept = Value::Array(
                edits
                    .iter()
                    .map(|(at, e)| json!([at, e]))
                    .collect(),
            );
            db::stage(tx, &token, &name, &kept, &note, now)?;
            let mut preview = json!({
                "needs_confirmation": true, "confirm": token, "count": planned.len(),
                "creates": count(&planned, "create"), "updates": count(&planned, "update"),
                "deletes": count(&planned, "delete"), "restores": count(&planned, "restore"), "sample": sample,
                "detail": "Nothing changed yet. Show the user this preview (the count and the sample); if they agree, call edit_rows again with just {collection, confirm: this token}: the pod keeps these edits for an hour.",
            });
            if confirm.is_some() {
                preview["error"] = json!("stale_preview");
                preview["status"] = json!(409);
                preview["detail"] = json!("the rows or the collection changed since that preview (or the edits differ); show the user this new preview");
                // Keep the new preview: a refusal rolls back.
                return Ok(Ok((Vec::new(), preview)));
            }
            return Ok(Ok((Vec::new(), preview)));
        }
        let (jobs, records) = match write_all(tx, &def, &name, &mut planned, &writes, &ats, caps, now, writer)? {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        };
        db::unstage(tx, &token)?;
        let edit = db::record_edit(tx, &name, &note, &records, now)?;
        Ok(Ok((jobs, json!({
            "ok": true, "edit": edit, "count": planned.len(),
            "creates": count(&planned, "create"), "updates": count(&planned, "update"),
            "deletes": count(&planned, "delete"), "restores": count(&planned, "restore"),
            "created": planned.iter().filter(|(o, ..)| *o == "create").map(|(_, id, ..)| id).collect::<Vec<_>>(),
            "undo": "undo_edit with this edit id (for 30 days)",
        }))))
    })
    .await?;
    let (jobs, out) = out;
    if out["error"] == "stale_preview" {
        return Err(out);
    }
    if out["ok"] == true {
        pod.publish_data_all(NAME);
        crate::compute::spawn(pod, &collection, jobs);
    }
    Ok(out)
}

/// Run an insert-from-query action (decision `collection-primitives`):
/// written at once, one edit that undo_edit reverts.
async fn run_action(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let collection = str_arg(args, "collection")?.to_owned();
    let action = str_arg(args, "action")?.to_owned();
    let params = match &args["params"] {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => return Err(bad("\"params\" is an object {param: value}")),
    };
    let note = args["note"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    let mut out = crate::run_action(pod, &collection, &action, params, note).await?;
    if !out["edit"].is_null() {
        out["undo"] = json!("undo_edit with this edit id reverts the whole call (for 30 days)");
    }
    Ok(out)
}

async fn undo_edit(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let id = str_arg(args, "edit")?.to_owned();
    // An undo Claude makes below outward access is Claude's write, though
    // it puts back values someone else wrote (decision `mcp-access-levels`).
    let writer = db::current_writer();
    let out = refusable(pod, move |tx| {
        let Some((_, undone, rows)) = db::edit(tx, &id)? else {
            return Ok(Err(
                json!({ "error": "edit_not_found", "edit": id, "status": 404,
                "detail": "edits can be undone for 30 days" }),
            ));
        };
        if undone.is_some() {
            return Ok(Err(
                json!({ "error": "already_undone", "edit": id, "status": 409 }),
            ));
        }
        let now = pod_core::util::now_secs();
        let defs = db::defs_by_name(tx)?;
        let (mut restored, mut skipped) = (0, Vec::new());
        // Children first (the reverse order), so a parent is undone last.
        for r in rows.into_iter().rev() {
            let collection = r.collection.clone();
            let Some(mut row) = db::row(tx, &collection, &r.id)? else {
                skipped.push(json!(r.id));
                continue;
            };
            let def = defs.get(&collection);
            if own_state(def, &row.state()) != own_state(def, &r.after) {
                skipped.push(json!(r.id));
                continue;
            }
            match &r.before {
                Some(b) => {
                    row.values = b["values"].as_object().cloned().unwrap_or_default();
                    row.deleted_at = b["deleted_at"].as_i64();
                }
                // It was made by the edit.
                None => row.deleted_at = Some(now),
            }
            if row.deleted_at.is_none()
                && let Some(def) = def
                && let Err(e) = crate::refs::check_unique(tx, def, &row)?
            {
                skipped.push(json!({ "id": r.id, "reason": e }));
                continue;
            }
            row.updated_at = now;
            db::save_row(tx, &collection, &row)?;
            if writer.is_some() && row.deleted_at.is_none() {
                db::note_writes(tx, &collection, None, &row, writer, now)?;
            }
            restored += 1;
        }
        tx.execute(
            "UPDATE collections_edits SET undone_at = ?1 WHERE id = ?2",
            rusqlite::params![now, id],
        )?;
        Ok(Ok(
            json!({ "ok": true, "edit": id, "restored": restored, "skipped": skipped,
            "detail": if skipped.is_empty() { "every row is back as it was".to_owned() }
                else { format!("{} rows changed since and were left alone", skipped.len()) } }),
        ))
    })
    .await?;
    pod.publish_data_all(NAME);
    Ok(out)
}

/// A row's recorded state without the fields that follow other rows
/// (lookups, counts, rollups): those move on their own after an edit, and
/// don't make a row "changed since".
fn own_state(def: Option<&Definition>, state: &Value) -> Value {
    let mut s = state.clone();
    if let (Some(def), Some(values)) = (def, s["values"].as_object_mut()) {
        for f in def
            .live_fields()
            .filter(|f| f.compute.iter().any(|c| c.reads().is_some()))
        {
            values.remove(&f.id);
        }
    }
    s
}

async fn export(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let name = str_arg(args, "collection")?.to_owned();
    let lookup = name.clone();
    let (out, def, writers) = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                let Some(out) = crate::export(c, &lookup, pod_core::util::now_secs())? else {
                    return Ok(None);
                };
                let def = db::latest_def(c, &lookup)?.map(|(_, d)| d);
                Ok(Some((out, def, db::field_writers(c, &lookup)?)))
            })
        })
        .await
        .map_err(db_err)?
        .ok_or_else(|| missing(&name))?;
    let mut out = out;
    if let Some(def) = def {
        mark_written(&mut out, &def, &writers);
    }
    Ok(out)
}

/// Each row value of an export that a filler or a model wrote last, in an
/// untrusted envelope naming the writer (`filler:<id>`, `model`, `derived`;
/// issue #439): the store records who wrote each field
/// ([`db::note_writes`]), so a person's values stay plain.
fn mark_written(
    out: &mut Value,
    def: &Definition,
    writers: &std::collections::HashMap<String, std::collections::BTreeMap<String, String>>,
) {
    let Some(rows) = out["rows"].as_array_mut() else {
        return;
    };
    for row in rows {
        let Some(w) = row["id"].as_str().and_then(|id| writers.get(id)) else {
            continue;
        };
        let w = w.clone();
        let Some(values) = row["values"].as_object_mut() else {
            continue;
        };
        for (field_id, writer) in &w {
            let Some(f) = def.by_id(field_id) else {
                continue;
            };
            if let Some(v) = values.get_mut(&f.name) {
                *v = untrusted(writer, v.take());
            }
        }
    }
}
