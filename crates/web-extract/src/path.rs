//! A small path language over JSON (a JSONPath subset): dot-separated keys,
//! each with optional `[n]` indexes or `[*]` (every element).
//!
//! | Path | Picks |
//! |---|---|
//! | `""` or `$` | the value itself |
//! | `name` | key `name` |
//! | `offers.price` | key `price` of key `offers` |
//! | `results[0]` | the first element of `results` |
//! | `results[*].votes` | `votes` of every element of `results` |
//! | `[*]` | every element of a top-level list |
//!
//! A key holding a list is looked into without `[*]` too: `offers.price` on
//! `{"offers": [{"price": 3}]}` picks `3` (schema.org data often has a list
//! where one value is expected).
//!
//! A map path may end in filters, each after a `|`, applied in order to the
//! value it picks (not to an `items` path):
//!
//! | Filter | Turns the value into |
//! |---|---|
//! | `link` | the first `http(s)` link in it (an `href`, else a bare URL), with a Google redirect (`google.com/url?q=…`) unwrapped; nothing if it has none |
//! | `text` | its text, HTML tags dropped and entities decoded |
//!
//! `description | link` reads the page an iCalendar event's description
//! only links to; `description | text` its words without the markup.

use serde_json::Value;

/// Longest path.
pub const MAX_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
    All,
}

/// A filter a map path ends in (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Link,
    Text,
}

impl Filter {
    /// The filter names.
    pub const ALL: &[&str] = &["link", "text"];

    fn parse(s: &str) -> Option<Filter> {
        match s.trim() {
            "link" => Some(Filter::Link),
            "text" => Some(Filter::Text),
            _ => None,
        }
    }

    fn apply(self, v: Value) -> Option<Value> {
        let s = match &v {
            Value::String(s) => s.as_str(),
            _ => return Some(v),
        };
        match self {
            Filter::Link => first_link(s).map(Value::String),
            Filter::Text => {
                let t = crate::html::to_text(s);
                (!t.trim().is_empty()).then_some(Value::String(t))
            }
        }
    }
}

/// A parsed path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Path {
    steps: Vec<Step>,
    filters: Vec<Filter>,
}

/// Parse `s` (see the module docs).
pub fn parse(s: &str) -> Result<Path, String> {
    let s = s.trim();
    if s.chars().count() > MAX_CHARS {
        return Err(format!("a path is at most {MAX_CHARS} characters"));
    }
    let mut parts = s.split('|');
    let s = parts.next().unwrap_or("").trim();
    let mut filters = Vec::new();
    for f in parts {
        filters.push(Filter::parse(f).ok_or_else(|| {
            format!(
                "'{}' is not a filter (one of: {})",
                f.trim(),
                Filter::ALL.join(", ")
            )
        })?);
    }
    let s = s.strip_prefix('$').unwrap_or(s);
    let s = s.strip_prefix('.').unwrap_or(s);
    let mut steps = Vec::new();
    if s.is_empty() {
        return Ok(Path { steps, filters });
    }
    for seg in s.split('.') {
        let (name, mut rest) = match seg.find('[') {
            Some(i) => (&seg[..i], &seg[i..]),
            None => (seg, ""),
        };
        if name.is_empty() && rest.is_empty() {
            return Err(format!("path '{s}' has an empty step (two dots in a row?)"));
        }
        if name.contains(']') {
            return Err(format!("path '{s}': a ']' without its '[' in '{seg}'"));
        }
        if !name.is_empty() {
            steps.push(Step::Key(name.to_owned()));
        }
        while !rest.is_empty() {
            let Some(inner) = rest.strip_prefix('[') else {
                return Err(format!("path '{s}': expected '[' in '{seg}'"));
            };
            let Some(end) = inner.find(']') else {
                return Err(format!("path '{s}': a '[' is not closed in '{seg}'"));
            };
            let idx = inner[..end].trim();
            steps.push(if idx == "*" {
                Step::All
            } else {
                Step::Index(
                    idx.parse().map_err(|_| {
                        format!("path '{s}': [{idx}] is not an index (a number or *)")
                    })?,
                )
            });
            rest = &inner[end + 1..];
        }
    }
    Ok(Path { steps, filters })
}

impl Path {
    /// Whether the path ends in filters (only map paths may).
    pub fn has_filters(&self) -> bool {
        !self.filters.is_empty()
    }

    /// Every value the path picks in `v`.
    pub fn select<'a>(&self, v: &'a Value) -> Vec<&'a Value> {
        let mut cur: Vec<&'a Value> = vec![v];
        for step in &self.steps {
            let mut next = Vec::new();
            for x in cur {
                match step {
                    Step::Key(k) => match x {
                        Value::Object(o) => next.extend(o.get(k)),
                        // Look into a list where one value was expected.
                        Value::Array(a) => next.extend(a.iter().filter_map(|e| e.get(k))),
                        _ => {}
                    },
                    Step::Index(i) => next.extend(x.as_array().and_then(|a| a.get(*i))),
                    Step::All => match x {
                        Value::Array(a) => next.extend(a.iter()),
                        Value::Object(o) => next.extend(o.values()),
                        _ => {}
                    },
                }
            }
            cur = next;
        }
        cur
    }

    /// The items the path picks: each picked list stands for its elements.
    pub fn items(&self, v: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        for x in self.select(v) {
            match x {
                Value::Array(a) => out.extend(a.iter().cloned()),
                other => out.push(other.clone()),
            }
        }
        out
    }

    /// The first value the path picks that reads as one value
    /// ([`scalar`]) and that its filters keep.
    pub fn value(&self, v: &Value) -> Option<Value> {
        self.select(v)
            .into_iter()
            .find_map(|x| self.filters.iter().try_fold(scalar(x)?, |v, f| f.apply(v)))
    }
}

/// The first `http(s)` link in `s`: an `href`, else a bare URL; a Google
/// redirect (`https://www.google.com/url?q=<link>&…`) unwrapped.
pub fn first_link(s: &str) -> Option<String> {
    let s = crate::html::decode_entities(s);
    let href = s.find("href=").and_then(|i| {
        let rest = &s[i + 5..];
        let q = rest.chars().next()?;
        let rest = rest.strip_prefix(['"', '\''])?;
        let end = rest.find(q).unwrap_or(rest.len());
        Some(rest[..end].trim().to_owned())
    });
    let found = href
        .filter(|h| h.starts_with("http://") || h.starts_with("https://"))
        .or_else(|| {
            let i = s.find("https://").or_else(|| s.find("http://"))?;
            let rest = &s[i..];
            let end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')'))
                .unwrap_or(rest.len());
            Some(rest[..end].trim_end_matches(['.', ',', ';']).to_owned())
        })?;
    let url = reqwest::Url::parse(&found).ok()?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if (host == "google.com" || host == "www.google.com")
        && url.path() == "/url"
        && let Some((_, q)) = url.query_pairs().find(|(k, _)| k == "q")
        && (q.starts_with("http://") || q.starts_with("https://"))
    {
        return Some(q.into_owned());
    }
    matches!(url.scheme(), "http" | "https").then_some(found)
}

/// One value from `v`: text, a number or a bool as they are; a list's first
/// such value; an object's `name`, `@value`, `value`, `text` or `#text`
/// (how JSON-LD, feeds and some APIs wrap a value). `None`: null or nothing
/// readable.
pub fn scalar(v: &Value) -> Option<Value> {
    match v {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Some(v.clone()),
        Value::Array(a) => a.iter().find_map(scalar),
        Value::Object(o) => ["name", "@value", "value", "text", "#text", "@id"]
            .iter()
            .find_map(|k| o.get(*k).and_then(scalar)),
    }
}

/// A value as display text (`None`: empty).
pub fn text(v: &Value) -> Option<String> {
    match scalar(v)? {
        Value::String(s) => Some(s.trim().to_owned()),
        other => Some(other.to_string()),
    }
}

/// `{"a": 1}` as a one-element list, a list as it is: the shape items
/// paths expect at the top.
pub fn as_list(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_pick_values() {
        let doc = json!({
            "races": [
                {"name": "Mayor", "candidates": [{"name": "A", "votes": 10}, {"name": "B", "votes": 7}]},
                {"name": "Clerk", "candidates": [{"name": "C", "votes": 3}]}
            ],
            "offers": [{"price": "19.99", "priceCurrency": "USD"}],
            "brand": {"@type": "Brand", "name": "Acme"}
        });
        let p = |s: &str| parse(s).unwrap();
        assert_eq!(p("races[*].candidates[*]").items(&doc).len(), 3);
        assert_eq!(
            p("races").items(&doc).len(),
            2,
            "a list stands for its elements"
        );
        assert_eq!(p("races[1].name").value(&doc), Some(json!("Clerk")));
        assert_eq!(p("offers.price").value(&doc), Some(json!("19.99")));
        assert_eq!(p("brand").value(&doc), Some(json!("Acme")));
        assert_eq!(p("$").items(&json!([1, 2])), vec![json!(1), json!(2)]);
        assert_eq!(
            p("$.races[0].candidates[1].votes").value(&doc),
            Some(json!(7))
        );
        assert_eq!(p("nope.x").value(&doc), None);
        for bad in ["a..b", "a[", "a[x]", "a]b"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(text(&json!({"@value": " 3 "})).as_deref(), Some("3"));
    }

    #[test]
    fn filters_read_links_and_text() {
        let p = |s: &str| parse(s).unwrap();
        let ev = json!({
            "a": "<a href=\"https://www.google.com/url?q=https://town.example/do/market&amp;sa=D&amp;usg=x\" target=\"_blank\">https://town.example/do/market</a>",
            "b": "Details at https://town.example/e/7. See you!",
            "c": "<p>Live <b>jazz</b> &amp; food</p>",
            "d": "no link here",
            "e": "<a href=\"/relative\">x</a> and https://town.example/abs"
        });
        assert_eq!(
            p("a | link").value(&ev),
            Some(json!("https://town.example/do/market"))
        );
        assert_eq!(
            p("b|link").value(&ev),
            Some(json!("https://town.example/e/7"))
        );
        assert_eq!(p("c | text").value(&ev), Some(json!("Live jazz & food")));
        assert_eq!(p("d | link").value(&ev), None);
        assert_eq!(
            p("e | link").value(&ev),
            Some(json!("https://town.example/abs"))
        );
        assert_eq!(
            p("a | text | link").value(&ev),
            Some(json!("https://town.example/do/market"))
        );
        assert!(p("a | link").has_filters());
        assert!(!p("a").has_filters());
        assert!(parse("a | nope").is_err());
        assert_eq!(text(&json!(4.5)).as_deref(), Some("4.5"));
    }
}
