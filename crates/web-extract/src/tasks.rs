//! Web tasks (decision `web-and-agent-modules`): the prompts for model work
//! on web content live here, next to the fetching that gives them context.
//! Each builds a [`construct_agent::Task`] for the agent module to run, and
//! the answers are checked against what was fetched ([`check_rows`]) before
//! anyone stores them.
//!
//! - [`extract_rows`]: a page's text into rows of a table (`extract: llm`).
//! - [`pick_from_search`]: one row's fields chosen from search results
//!   (`query_field` fillers: where to buy a product).
//! - [`research`]: an open research request answered as rows, with web
//!   tools (agent fillers).

use construct_agent::{Task, Tier};
use serde_json::{Map, Value, json};

/// Most page text sent to the model.
pub const MAX_PAGE_CHARS: usize = 60_000;
/// Rows one answer may hold.
pub const MAX_ROWS: usize = 100;
/// Current rows shown to a research task.
pub const MAX_CURRENT: usize = 50;

/// The `feature` of reading a page (metrics, budgets).
pub const PAGE_FEATURE: &str = "fill_page";
/// The `feature` of picking from search results.
pub const PICK_FEATURE: &str = "web_pick";
/// The `feature` of a research task.
pub const RESEARCH_FEATURE: &str = "fill_agent";

/// One field a task answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowField {
    pub name: String,
    /// text, number, bool, date, datetime, choice, ref or quantity.
    pub kind: String,
    /// A choice's options.
    pub options: Vec<String>,
}

/// The JSON schema of one value of `f` (null: not found).
pub fn field_schema(f: &RowField) -> Value {
    let one = match f.kind.as_str() {
        "number" => json!({ "type": "number" }),
        "bool" => json!({ "type": "boolean" }),
        "date" => json!({ "type": "string", "format": "date" }),
        "datetime" => {
            json!({ "type": "string", "description": "YYYY-MM-DDTHH:MM with the UTC offset when the source gives one (e.g. 2026-11-06T19:30-07:00), or YYYY-MM-DD" })
        }
        "choice" => json!({ "type": "string", "enum": f.options }),
        "ref" => json!({ "type": "string", "description": "a row id" }),
        "quantity" => {
            json!({ "type": "string", "description": "an amount and a unit, e.g. 1 1/2 cups" })
        }
        _ => json!({ "type": "string" }),
    };
    json!({ "anyOf": [one, { "type": "null" }] })
}

/// The schema of `{rows: [...]}` for `fields` (strict: every field given).
pub fn rows_schema(fields: &[RowField]) -> Value {
    let props: Map<String, Value> = fields
        .iter()
        .map(|f| (f.name.clone(), field_schema(f)))
        .collect();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    json!({
        "type": "object",
        "properties": {
            "rows": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": props,
                    "required": names,
                    "additionalProperties": false,
                },
            },
        },
        "required": ["rows"],
        "additionalProperties": false,
    })
}

/// The `save_rows` tool for `fields` (the finish tool of a research task).
pub fn save_rows_tool(fields: &[RowField]) -> Value {
    json!({
        "name": "save_rows",
        "description": "Save the rows of the table. Call it once, with every row.",
        "strict": true,
        "input_schema": rows_schema(fields),
    })
}

fn columns(fields: &[RowField]) -> String {
    fields
        .iter()
        .map(|f| match f.kind.as_str() {
            "choice" => format!("- {} (one of: {})", f.name, f.options.join(", ")),
            k => format!("- {} ({k})", f.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const PAGE_SYSTEM: &str = "You read the text of a web page and list the items it holds as rows of \
a table in a personal app. Give every field of each row: the page's own words for text; null for \
anything the page doesn't say; never guess a number, a price or a date. Dates are YYYY-MM-DD; a date \
and time is YYYY-MM-DDTHH:MM, with the UTC offset only when the page states one. Skip navigation, ads \
and anything the notes exclude. Answer {\"rows\": []} when there are none. Field values are plain \
values: never put JSON, quotes or other fields' names inside a value.";

/// What [`extract_rows`] reads.
#[derive(Debug, Clone)]
pub struct PageInput<'a> {
    pub title: &'a str,
    pub fields: &'a [RowField],
    pub page_text: &'a str,
    pub url: &'a str,
    pub hint: &'a str,
    pub today: &'a str,
    /// The caller leaves out past items itself: the model lists them too,
    /// so a page of only past ones doesn't read as an empty page.
    pub with_past: bool,
}

/// A page's text into rows (`extract: llm`).
pub fn extract_rows(p: &PageInput<'_>) -> Task {
    let mut prompt = format!(
        "Today is {}. The table is \"{}\", with columns:\n{}\n\nThe page is {}.\n\n<page>\n{}\n</page>",
        p.today,
        p.title,
        columns(p.fields),
        p.url,
        crate::html::truncate(p.page_text, MAX_PAGE_CHARS)
    );
    if !p.hint.trim().is_empty() {
        prompt.push_str(&format!("\n\nNotes about this page: {}", p.hint.trim()));
    }
    if p.with_past {
        prompt.push_str(
            "\n\nList past items too, with their dates, even when the notes ask for upcoming \
             ones: the app leaves out past items itself.",
        );
    }
    Task::new(PAGE_FEATURE, Tier::Capable, prompt)
        .system(PAGE_SYSTEM)
        .max_tokens(16_000)
        .effort("low")
        .schema(rows_schema(p.fields))
}

const PICK_SYSTEM: &str = "You help someone decide where to buy or find something on their list \
in a personal app. You get web search results for one item and fill one row of a table from them. \
Use only what the results say: a URL must be one of the results' own URLs, copied exactly; a price \
only when a result shows it; null for anything they don't say. Prefer the maker's own store and \
reputable retailers' product pages over marketplaces, category pages, listicles and ads. Field \
values are plain values: never put JSON, quotes or other fields' names inside a value.";

/// What [`pick_from_search`] reads.
#[derive(Debug, Clone)]
pub struct PickInput<'a> {
    pub title: &'a str,
    pub fields: &'a [RowField],
    /// The item, as the row says it (e.g. its name and notes).
    pub item: &'a str,
    pub results: &'a [crate::search::SearchResult],
    pub hint: &'a str,
    pub today: &'a str,
}

/// One row's fields chosen from search results (`query_field` fillers).
pub fn pick_from_search(p: &PickInput<'_>) -> Task {
    let mut prompt = format!(
        "Today is {}. The table is \"{}\". Fill these columns for the item below:\n{}\n\nItem: {}\n\nSearch results:\n{}",
        p.today,
        p.title,
        columns(p.fields),
        p.item,
        crate::search::results_text(p.results),
    );
    if !p.hint.trim().is_empty() {
        prompt.push_str(&format!("\n\nNotes: {}", p.hint.trim()));
    }
    prompt.push_str(
        "\n\nAnswer exactly one row, or {\"rows\": []} when no result is about this item.",
    );
    Task::new(PICK_FEATURE, Tier::Capable, prompt)
        .system(PICK_SYSTEM)
        .max_tokens(4_000)
        .effort("low")
        .schema(rows_schema(p.fields))
}

const RESEARCH_SYSTEM: &str = "You keep a table in a personal app up to date. Research the request \
on the web with the tools you have, then call save_rows exactly once with every row the request asks \
for. Each row gives every field: use null for anything you could not find; never guess a number, a \
price or a date. Dates are YYYY-MM-DD. Prefer primary sources (the store's own page, the official \
results page) and current information. Keep a current row when it still holds, with the same key.";

/// What [`research`] reads.
#[derive(Debug, Clone)]
pub struct ResearchInput<'a> {
    pub title: &'a str,
    pub prompt: &'a str,
    pub fields: &'a [RowField],
    pub today: &'a str,
    /// The key field's name (rows with the same key are the same row).
    pub key: Option<&'a str>,
    /// The rows it filled last time (by field name).
    pub current: &'a [Map<String, Value>],
}

/// The first message of a research task.
pub fn research_prompt(r: &ResearchInput<'_>) -> String {
    let mut p = format!(
        "Today is {}. The table is \"{}\". Its columns:\n{}\n\nRequest: {}",
        r.today,
        r.title,
        columns(r.fields),
        r.prompt
    );
    if let Some(k) = r.key {
        p.push_str(&format!(
            "\n\nA row is identified by its {k}: give the same {k} for the same thing."
        ));
    }
    if !r.current.is_empty() {
        let rows: Vec<String> = r
            .current
            .iter()
            .take(MAX_CURRENT)
            .map(|x| Value::Object(x.clone()).to_string())
            .collect();
        p.push_str(&format!(
            "\n\nThe table's current rows (check them; they may be out of date):\n{}",
            rows.join("\n")
        ));
    }
    p
}

/// A research request answered as rows; the caller adds its tools (the
/// pod's own search and fetch, or the API's server-side web tools).
pub fn research(r: &ResearchInput<'_>, max_turns: usize) -> Task {
    let mut t = Task::new(RESEARCH_FEATURE, Tier::Capable, research_prompt(r))
        .system(RESEARCH_SYSTEM)
        .max_tokens(16_000)
        .effort("medium");
    t.finish_tool = Some(save_rows_tool(r.fields));
    t.max_turns = max_turns;
    t
}

/// The rows of a structured answer (`{"rows": [...]}` as text).
pub fn rows_of(output: &construct_agent::Output) -> Result<Vec<Map<String, Value>>, String> {
    let parsed: Value = serde_json::from_str(output.text().trim())
        .map_err(|e| format!("the model's answer is not JSON: {e}"))?;
    Ok(rows_in(&parsed))
}

/// The rows of `{"rows": [...]}`.
pub fn rows_in(v: &Value) -> Vec<Map<String, Value>> {
    v["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_object().cloned())
        .take(MAX_ROWS)
        .collect()
}

/// What [`check_rows`] took out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked {
    /// URLs that were not in the source (set to null).
    pub urls_dropped: usize,
    /// Text values cut where stray JSON began.
    pub values_trimmed: usize,
}

fn looks_like_url(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("http://") || s.starts_with("https://")
}

/// A URL reduced for comparison: no scheme, no `www.`, no trailing slash,
/// lowercase.
fn url_key(u: &str) -> String {
    let u = u.trim().to_lowercase();
    let u = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .unwrap_or(&u);
    let u = u.strip_prefix("www.").unwrap_or(u);
    u.trim_end_matches('/').to_owned()
}

/// Where stray JSON starts in a text value (`...pickup.  ','opt1_price':null}]}`).
fn stray_json_at(s: &str) -> Option<usize> {
    const MARKERS: &[&str] = &[
        "','", "\",\"", "':null", "\":null", "'}", "\"}", "}]", "{'", "{\"",
    ];
    MARKERS.iter().filter_map(|m| s.find(m)).min()
}

/// Check rows against the text they came from: a URL that doesn't appear in
/// `source` (the page, or the search results) is set to null, and text cut
/// where stray JSON begins.
pub fn check_rows(rows: &mut [Map<String, Value>], source: &str) -> Checked {
    let mut out = Checked::default();
    let haystack: String = {
        // Every URL in the source, reduced, plus the raw text.
        let mut h = source.to_lowercase();
        for word in source
            .split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
        {
            if looks_like_url(word) {
                h.push('\n');
                h.push_str(&url_key(word));
            }
        }
        h
    };
    for row in rows.iter_mut() {
        for v in row.values_mut() {
            let Some(s) = v.as_str().map(str::to_owned) else {
                continue;
            };
            let s = s.as_str();
            if looks_like_url(s) {
                if !haystack.contains(&url_key(s)) {
                    *v = Value::Null;
                    out.urls_dropped += 1;
                }
                continue;
            }
            if let Some(at) = stray_json_at(s) {
                let cut = s[..at]
                    .trim_end()
                    .trim_end_matches(['\'', '"', ','])
                    .trim_end()
                    .to_owned();
                out.values_trimmed += 1;
                *v = if cut.is_empty() {
                    Value::Null
                } else {
                    json!(cut)
                };
            } else if s.ends_with(['\'', '"']) && !s.starts_with(['\'', '"']) {
                let t = s.trim_end_matches(['\'', '"']).trim_end();
                *v = json!(t);
                out.values_trimmed += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<RowField> {
        vec![
            RowField {
                name: "store".into(),
                kind: "text".into(),
                options: vec![],
            },
            RowField {
                name: "url".into(),
                kind: "text".into(),
                options: vec![],
            },
            RowField {
                name: "price".into(),
                kind: "number".into(),
                options: vec![],
            },
        ]
    }

    #[test]
    fn a_page_task_asks_for_past_items_only_when_told() {
        let f = fields();
        let t = |with_past| {
            extract_rows(&PageInput {
                title: "Shows",
                fields: &f,
                page_text: "May 29: a talk",
                url: "https://club.example/shows",
                hint: "upcoming shows",
                today: "2026-11-05",
                with_past,
            })
        };
        let body = t(true).body("m");
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("List past items too")
        );
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert!(
            !t(false).body("m")["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("past items")
        );
    }

    #[test]
    fn urls_must_come_from_the_source_and_stray_json_is_cut() {
        let source =
            "1. AeroPress filters\n   https://aeropress.com/products/micro-filters\n   official";
        let mut rows = vec![
            json!({
                "store": "AeroPress",
                "url": "https://www.aeropress.com/products/micro-filters/",
                "price": null,
            })
            .as_object()
            .unwrap()
            .clone(),
            json!({
                "store": "Buy from AeroPress directly, or Target for same day.  ','opt1_price':null}]}",
                "url": "https://made-up.example/filters",
                "price": 5.95,
            })
            .as_object()
            .unwrap()
            .clone(),
        ];
        let c = check_rows(&mut rows, source);
        assert_eq!(c.urls_dropped, 1);
        assert_eq!(
            rows[0]["url"],
            "https://www.aeropress.com/products/micro-filters/"
        );
        assert_eq!(rows[1]["url"], Value::Null);
        assert_eq!(
            rows[1]["store"],
            "Buy from AeroPress directly, or Target for same day."
        );
        let mut q = vec![
            json!({"store": "Target for pickup.'"})
                .as_object()
                .unwrap()
                .clone(),
        ];
        check_rows(&mut q, "");
        assert_eq!(q[0]["store"], "Target for pickup.");
    }

    #[test]
    fn research_ends_with_save_rows() {
        let f = fields();
        let t = research(
            &ResearchInput {
                title: "Monitors",
                prompt: "five best",
                fields: &f,
                today: "2026-11-05",
                key: Some("store"),
                current: &[],
            },
            6,
        );
        let b = t.body("m");
        assert_eq!(b["tools"][0]["name"], "save_rows");
        assert!(
            b["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("identified by its store")
        );
    }
}
