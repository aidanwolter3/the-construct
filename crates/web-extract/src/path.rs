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

use serde_json::Value;

/// Longest path.
pub const MAX_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
    All,
}

/// A parsed path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Path {
    steps: Vec<Step>,
}

/// Parse `s` (see the module docs).
pub fn parse(s: &str) -> Result<Path, String> {
    let s = s.trim();
    if s.chars().count() > MAX_CHARS {
        return Err(format!("a path is at most {MAX_CHARS} characters"));
    }
    let s = s.strip_prefix('$').unwrap_or(s);
    let s = s.strip_prefix('.').unwrap_or(s);
    let mut steps = Vec::new();
    if s.is_empty() {
        return Ok(Path { steps });
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
    Ok(Path { steps })
}

impl Path {
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
    /// ([`scalar`]).
    pub fn value(&self, v: &Value) -> Option<Value> {
        self.select(v).into_iter().find_map(scalar)
    }
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
        assert_eq!(text(&json!(4.5)).as_deref(), Some("4.5"));
    }
}
