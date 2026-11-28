//! What answers `taste` fillers (decision `shopping-kit`): the owner's
//! taste service, through the `taste` source, behind the [`Explorer`] trait
//! so Collections needn't know that source (it depends on Collections) and
//! tests use a fake.
//!
//! A request is a JSON object of the parts [`crate::filler::TASTE_REQUEST`]
//! names: `query` (text), `like`, `unlike` and `exclude` (piece keys), `avoid` and `prefer`
//! (words or short phrases, from text split on commas, semicolons and lines),
//! `distance` and `spread` (0..1), `group` (true or false), `n` and
//! `price_max` (numbers). The answer is the pieces, each an object of the
//! parts [`crate::filler::TASTE_PARTS`] names (absent parts are empty).

use pod_core::{BoxFuture, Pod};
use serde_json::{Map, Value};

/// The most pieces one explore answers.
pub const MAX_PIECES: usize = 48;

pub trait Explorer: Send + Sync + std::fmt::Debug {
    /// The pieces for `request`, or why there are none (not signed in, the
    /// service unreachable).
    fn explore<'a>(
        &'a self,
        pod: &'a Pod,
        request: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Vec<Map<String, Value>>, String>>;
}

/// A row's text as a list of piece keys: split on commas and spaces (a
/// rollup lists values joined by `, `), at most [`MAX_KEYS`].
pub fn keys_in(v: &Value) -> Vec<String> {
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(","),
        _ => return Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for k in text.split([',', ' ', '\n']).map(str::trim) {
        if !k.is_empty() && k.len() <= 300 && !out.iter().any(|o| o == k) {
            out.push(k.to_owned());
            if out.len() == MAX_KEYS {
                break;
            }
        }
    }
    out
}

/// A row's text as words or short phrases (`avoid`, `prefer`): split on commas, semicolons
/// and lines, each trimmed, at most 60 characters, at most [`MAX_PHRASES`].
pub fn phrases_in(v: &Value) -> Vec<String> {
    let Value::String(text) = v else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for p in text.split([',', ';', '\n']).map(str::trim) {
        let p: String = p.chars().take(60).collect();
        if !p.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&p)) {
            out.push(p);
            if out.len() == MAX_PHRASES {
                break;
            }
        }
    }
    out
}

/// The most phrases an `avoid` or `prefer` part carries (the service takes 40).
pub const MAX_PHRASES: usize = 40;

/// The most keys a list part carries.
pub const MAX_KEYS: usize = 200;

/// The request part `part` from the row's value `v`, or `None` when the
/// value is empty (the part is left out).
pub fn part(part: &str, v: &Value) -> Option<Value> {
    let num = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    match part {
        "like" | "unlike" | "exclude" => {
            let k = keys_in(v);
            (!k.is_empty()).then(|| Value::from(k))
        }
        "avoid" | "prefer" => {
            let p = phrases_in(v);
            (!p.is_empty()).then(|| Value::from(p))
        }
        "distance" | "spread" => num(v)
            .filter(|x| x.is_finite())
            .map(|x| Value::from(x.clamp(0.0, 1.0))),
        "n" => num(v)
            .filter(|x| x.is_finite() && *x >= 1.0)
            .map(|x| Value::from((x as u64).min(MAX_PIECES as u64))),
        "price_max" => num(v)
            .filter(|x| x.is_finite() && *x > 0.0)
            .map(Value::from),
        "group" => Some(Value::Bool(match v {
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
            Value::String(s) => matches!(s.trim(), "true" | "1" | "yes"),
            _ => false,
        })),
        "query" => match v {
            Value::String(s) if !s.trim().is_empty() => {
                Some(Value::from(s.trim().chars().take(300).collect::<String>()))
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_split_rollup_lists() {
        assert_eq!(
            keys_in(&json!("a__1, b__2,a__1 ")),
            vec!["a__1".to_owned(), "b__2".to_owned()]
        );
        assert!(keys_in(&json!(null)).is_empty());
    }

    #[test]
    fn parts_are_clamped_and_empty_ones_left_out() {
        assert_eq!(part("distance", &json!("1.7")), Some(json!(1.0)));
        assert_eq!(part("n", &json!(500)), Some(json!(48)));
        assert_eq!(part("query", &json!("  ")), None);
        assert_eq!(part("group", &json!(true)), Some(json!(true)));
        assert_eq!(part("like", &json!("")), None);
        assert_eq!(
            part("avoid", &json!("logos; graphic prints,\n polyester")),
            Some(json!(["logos", "graphic prints", "polyester"]))
        );
    }
}
