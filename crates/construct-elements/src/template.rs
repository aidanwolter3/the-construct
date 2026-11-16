//! Substituting a component's arguments into its template, with a budget on
//! the work an expansion may do. Moved from `pod_core::parts` (decision
//! `content-addressed-construct`): the pod's expansion and the engine's use
//! the same rules.

use serde_json::{Map, Value};

/// Largest expansion served.
pub const MAX_EXPANDED_BYTES: usize = 1024 * 1024;
/// The most an expansion may build along the way, in approximate JSON
/// bytes: every value it visits or copies (a component's children, an
/// argument) counts. Checked as it goes, so a component that repeats its
/// children or arguments can't multiply a use into gigabytes before
/// [`MAX_EXPANDED_BYTES`] is checked at the end.
pub const MAX_EXPANSION_WORK: usize = 4 * MAX_EXPANDED_BYTES;

/// Why an expansion failed: where (a JSON pointer) and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub pointer: String,
    pub message: String,
}

impl Error {
    pub fn at(pointer: &str, message: impl Into<String>) -> Error {
        Error {
            pointer: pointer.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.pointer.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.pointer, self.message)
        }
    }
}

/// Whether a template value is "on" (`core://IfArg` without `equals`).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty() && s != "0" && s != "false",
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// A value as the text it puts into a string.
pub fn as_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The approximate JSON size of `v`, without serializing it.
pub fn weight(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len() + 2,
        Value::Array(a) => a.iter().map(weight).sum::<usize>() + 2,
        Value::Object(m) => {
            m.iter()
                .map(|(k, c)| k.len() + 3 + weight(c))
                .sum::<usize>()
                + 2
        }
        _ => 4,
    }
}

/// The work an expansion has done so far ([`MAX_EXPANSION_WORK`]).
#[derive(Default)]
pub struct Work(pub usize);

impl Work {
    pub fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.0 = self.0.saturating_add(n);
        if self.0 > MAX_EXPANSION_WORK {
            return Err(Error::at(
                "",
                format!(
                    "the expansion is too large (over {MAX_EXPANSION_WORK} bytes of work; \
                     a component repeats its children or arguments too often)"
                ),
            ));
        }
        Ok(())
    }
}

/// The argument prefixes a template reads: `$arg.` and its other name
/// `$in.` (an element's inputs, decision `content-addressed-construct`).
/// `$in.` is read only where `with_in` is set (the construct's elements), so
/// the old system's templates mean what they meant.
fn prefix_at(s: &str, with_in: bool) -> Option<(usize, usize)> {
    let a = s.find("$arg.").map(|i| (i, 5));
    let b = if with_in {
        s.find("$in.").map(|i| (i, 4))
    } else {
        None
    };
    match (a, b) {
        (Some(x), Some(y)) => Some(if x.0 <= y.0 { x } else { y }),
        (x, y) => x.or(y),
    }
}

fn is_arg_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `s` with each `$arg.<name>` replaced; a string that is only `$arg.<name>`
/// becomes the argument itself (its own JSON type).
pub fn subst(s: &str, args: &Map<String, Value>, work: &mut Work) -> Result<Value, Error> {
    subst_in(s, args, work, false)
}

/// [`subst`], reading `$in.<name>` too when `with_in` is set.
pub fn subst_in(
    s: &str,
    args: &Map<String, Value>,
    work: &mut Work,
    with_in: bool,
) -> Result<Value, Error> {
    let whole = s
        .strip_prefix("$arg.")
        .or_else(|| with_in.then(|| s.strip_prefix("$in.")).flatten());
    if let Some(name) = whole
        && is_arg_name(name)
    {
        let arg = args.get(name);
        work.charge(arg.map_or(4, weight))?;
        return Ok(arg.cloned().unwrap_or(Value::Null));
    }
    work.charge(s.len() + 2)?;
    if prefix_at(s, with_in).is_none() {
        return Ok(Value::String(s.to_owned()));
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some((i, len)) = prefix_at(rest, with_in) {
        out.push_str(&rest[..i]);
        let after = &rest[i + len..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let piece = as_text(args.get(&after[..end]).unwrap_or(&Value::Null));
        work.charge(piece.len())?;
        out.push_str(&piece);
        rest = &after[end..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

/// [`subst`] through a whole value (keys too).
pub fn subst_value(v: &Value, args: &Map<String, Value>, work: &mut Work) -> Result<Value, Error> {
    subst_value_in(v, args, work, false)
}

/// [`subst_value`], reading `$in.<name>` too when `with_in` is set.
pub fn subst_value_in(
    v: &Value,
    args: &Map<String, Value>,
    work: &mut Work,
    with_in: bool,
) -> Result<Value, Error> {
    Ok(match v {
        Value::String(s) => subst_in(s, args, work, with_in)?,
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|c| subst_value_in(c, args, work, with_in))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(m) => {
            work.charge(2)?;
            let mut out = Map::new();
            for (k, c) in m {
                let key = as_text(&subst_in(k, args, work, with_in)?);
                out.insert(key, subst_value_in(c, args, work, with_in)?);
            }
            Value::Object(out)
        }
        other => {
            work.charge(4)?;
            other.clone()
        }
    })
}

/// An element's tree with the inputs in `inputs` bound (decision
/// `content-addressed-construct`): each `$in.<name>` that `inputs` names is
/// replaced, in strings and keys; any other `$in.` or `$arg.` stays as
/// written (a component's own arguments are bound when it is used).
pub fn bind_inputs(
    v: &Value,
    inputs: &Map<String, Value>,
    work: &mut Work,
) -> Result<Value, Error> {
    fn text(s: &str, inputs: &Map<String, Value>, work: &mut Work) -> Result<Value, Error> {
        if let Some(name) = s.strip_prefix("$in.")
            && is_arg_name(name)
        {
            return Ok(match inputs.get(name) {
                Some(x) => {
                    work.charge(weight(x))?;
                    x.clone()
                }
                None => Value::String(s.to_owned()),
            });
        }
        work.charge(s.len() + 2)?;
        if !s.contains("$in.") {
            return Ok(Value::String(s.to_owned()));
        }
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find("$in.") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 4..];
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            match inputs.get(&after[..end]).filter(|_| end > 0) {
                Some(x) => {
                    let piece = as_text(x);
                    work.charge(piece.len())?;
                    out.push_str(&piece);
                }
                None => out.push_str(&rest[i..i + 4 + end]),
            }
            rest = &after[end..];
        }
        out.push_str(rest);
        Ok(Value::String(out))
    }
    Ok(match v {
        Value::String(s) => text(s, inputs, work)?,
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|c| bind_inputs(c, inputs, work))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(m) => {
            work.charge(2)?;
            let mut out = Map::new();
            for (k, c) in m {
                let key = if k.contains("$in.") {
                    as_text(&text(k, inputs, work)?)
                } else {
                    k.clone()
                };
                out.insert(key, bind_inputs(c, inputs, work)?);
            }
            Value::Object(out)
        }
        other => other.clone(),
    })
}

/// The inputs element `el` reads at its own level (`$in.<name>` in its tree),
/// but not its declared parameters (a component's, bound by `args`): what an
/// edge to it binds.
pub fn inputs_of(el: &Value) -> std::collections::BTreeSet<String> {
    let declared: std::collections::BTreeSet<&str> = el["params"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str())
        .collect();
    let text = el["node"].to_string();
    let mut out = std::collections::BTreeSet::new();
    for (i, _) in text.match_indices("$in.") {
        let rest = &text[i + 4..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if end > 0 && !declared.contains(&rest[..end]) {
            out.insert(rest[..end].to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binds_only_the_inputs_given() {
        let inputs = json!({"items": "collections:books", "n": 2})
            .as_object()
            .unwrap()
            .clone();
        let mut w = Work::default();
        let v = json!({"query_key": "$in.items", "label": "$in.n of $in.other $arg.x",
            "count": "$in.n", "bind": {"items": "$in.items"}});
        assert_eq!(
            bind_inputs(&v, &inputs, &mut w).unwrap(),
            json!({"query_key": "collections:books", "label": "2 of $in.other $arg.x",
                "count": 2, "bind": {"items": "collections:books"}})
        );
        let el = json!({"params": [{"name": "title"}],
            "node": {"a": "$in.items", "b": "$in.title", "c": "$arg.x"}});
        assert_eq!(
            inputs_of(&el).into_iter().collect::<Vec<_>>(),
            vec!["items".to_owned()]
        );
    }

    #[test]
    fn substitutes_whole_and_inline_arguments() {
        let args = json!({"n": 3, "t": "hi"}).as_object().unwrap().clone();
        let mut w = Work::default();
        assert_eq!(subst("$arg.n", &args, &mut w).unwrap(), json!(3));
        assert_eq!(subst("x $arg.t y", &args, &mut w).unwrap(), json!("x hi y"));
        // `$in.` only where asked.
        assert_eq!(subst("$in.n", &args, &mut w).unwrap(), json!("$in.n"));
        assert_eq!(subst_in("$in.n", &args, &mut w, true).unwrap(), json!(3));
        assert_eq!(
            subst_in("$in.t and $arg.t", &args, &mut w, true).unwrap(),
            json!("hi and hi")
        );
    }
}
