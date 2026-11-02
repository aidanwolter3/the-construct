//! Minimal mutation-expression evaluator (docs/DECISIONS.md D4).
//!
//! Stands in for a real CEL evaluator. Supported forms, evaluated against the
//! target entity's current payload:
//!
//! - a JSON literal: `0`, `true`, `"x"`, `null`, `[1]`, `{"a":1}`
//! - `entity.<field>`
//! - `!entity.<field>` (boolean negation; field must be a bool)
//! - `entity.<field> + N` / `entity.<field> - N` (integer arithmetic; a
//!   missing field counts as 0)
//! - `<cond> ? <expr> : <expr>` where `<cond>` is `entity.<field>` or
//!   `!entity.<field>` (the field must be a bool, like `!entity.<field>`) and
//!   each branch is one of the forms above (no nested ternary)

use std::fmt;

use protocol::EntityRecord;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprError {
    /// The expression is not in the supported subset.
    Unsupported(String),
    /// The referenced field is missing.
    MissingField(String),
    /// The field has the wrong type for the operation.
    TypeMismatch {
        field: String,
        expected: &'static str,
    },
    /// Integer overflow in `+`/`-`.
    Overflow,
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(e) => write!(f, "unsupported expression: {e}"),
            Self::MissingField(name) => write!(f, "missing field: {name}"),
            Self::TypeMismatch { field, expected } => {
                write!(f, "field {field} is not a {expected}")
            }
            Self::Overflow => write!(f, "integer overflow"),
        }
    }
}

impl std::error::Error for ExprError {}

/// Evaluate `expr` against `entity`, returning the new field value.
pub fn eval(expr: &str, entity: &EntityRecord) -> Result<Value, ExprError> {
    let expr = expr.trim();
    if let Some((cond, rest)) = expr.split_once('?') {
        let cond = cond.trim();
        let (negate, cond_ref) = match cond.strip_prefix('!') {
            Some(r) => (true, r.trim()),
            None => (false, cond),
        };
        if let Some(field) = parse_field_ref(cond_ref) {
            let unsupported = || ExprError::Unsupported(expr.to_string());
            let (then_branch, else_branch) = split_top_level_colon(rest).ok_or_else(unsupported)?;
            let truthy = match entity.get(field) {
                Some(Value::Bool(b)) => *b != negate,
                Some(_) => {
                    return Err(ExprError::TypeMismatch {
                        field: field.to_string(),
                        expected: "bool",
                    });
                }
                None => return Err(ExprError::MissingField(field.to_string())),
            };
            let branch = if truthy { then_branch } else { else_branch };
            // Branches are non-ternary forms only.
            return match eval_simple(branch.trim(), entity) {
                Err(ExprError::Unsupported(_)) => Err(unsupported()),
                other => other,
            };
        }
    }
    eval_simple(expr, entity)
}

/// The non-ternary forms.
fn eval_simple(expr: &str, entity: &EntityRecord) -> Result<Value, ExprError> {
    let unsupported = || ExprError::Unsupported(expr.to_string());

    if let Some(rest) = expr.strip_prefix('!') {
        let field = parse_field_ref(rest.trim()).ok_or_else(unsupported)?;
        return match entity.get(field) {
            Some(Value::Bool(b)) => Ok(Value::Bool(!b)),
            Some(_) => Err(ExprError::TypeMismatch {
                field: field.to_string(),
                expected: "bool",
            }),
            None => Err(ExprError::MissingField(field.to_string())),
        };
    }

    if expr.starts_with("entity.") {
        if let Some(field) = parse_field_ref(expr) {
            return entity
                .get(field)
                .cloned()
                .ok_or_else(|| ExprError::MissingField(field.to_string()));
        }
        let (lhs, op, rhs) = split_binary(expr).ok_or_else(unsupported)?;
        let field = parse_field_ref(lhs).ok_or_else(unsupported)?;
        let n: i64 = rhs.parse().map_err(|_| unsupported())?;
        let current = match entity.get(field) {
            None | Some(Value::Null) => 0,
            Some(v) => v.as_i64().ok_or_else(|| ExprError::TypeMismatch {
                field: field.to_string(),
                expected: "integer",
            })?,
        };
        let out = match op {
            '+' => current.checked_add(n),
            _ => current.checked_sub(n),
        }
        .ok_or(ExprError::Overflow)?;
        return Ok(Value::from(out));
    }

    serde_json::from_str::<Value>(expr).map_err(|_| unsupported())
}

/// `entity.<ident>` -> `<ident>`, where ident is `[A-Za-z0-9_]+`.
fn parse_field_ref(s: &str) -> Option<&str> {
    let field = s.strip_prefix("entity.")?;
    let valid = !field.is_empty() && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some(field)
}

/// Split `a : b` on the first `:` outside JSON strings, arrays and objects.
/// `None` if there is none, or if either side is empty or contains a
/// top-level `?` (nested ternaries are not supported).
fn split_top_level_colon(s: &str) -> Option<(&str, &str)> {
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    let mut split = None;
    let mut has_question = false;
    for (i, c) in s.char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ':' if depth == 0 && split.is_none() => split = Some(i),
            '?' if depth == 0 => has_question = true,
            _ => {}
        }
    }
    let i = split?;
    let (a, b) = (s[..i].trim(), s[i + 1..].trim());
    (!has_question && !a.is_empty() && !b.is_empty()).then_some((a, b))
}

/// Split `a + b` / `a - b` on the single operator.
fn split_binary(s: &str) -> Option<(&str, char, &str)> {
    let idx = s.find(['+', '-'])?;
    let op = s[idx..].chars().next()?;
    let (lhs, rhs) = (s[..idx].trim(), s[idx + 1..].trim());
    if rhs.is_empty() || rhs.starts_with(['+', '-']) {
        return None;
    }
    Some((lhs, op, rhs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entity(v: Value) -> EntityRecord {
        EntityRecord::try_from(v).unwrap()
    }

    #[test]
    fn literals() {
        let e = EntityRecord::new();
        assert_eq!(eval("0", &e), Ok(json!(0)));
        assert_eq!(eval(" 42 ", &e), Ok(json!(42)));
        assert_eq!(eval("true", &e), Ok(json!(true)));
        assert_eq!(eval("\"x\"", &e), Ok(json!("x")));
        assert_eq!(eval("null", &e), Ok(json!(null)));
        assert_eq!(eval("[1,2]", &e), Ok(json!([1, 2])));
    }

    #[test]
    fn field_ref() {
        let e = entity(json!({"count": 3, "name": "a"}));
        assert_eq!(eval("entity.count", &e), Ok(json!(3)));
        assert_eq!(eval("entity.name", &e), Ok(json!("a")));
        assert_eq!(
            eval("entity.nope", &e),
            Err(ExprError::MissingField("nope".into()))
        );
    }

    #[test]
    fn negation() {
        let e = entity(json!({"is_liked": false, "count": 1}));
        assert_eq!(eval("!entity.is_liked", &e), Ok(json!(true)));
        assert_eq!(eval("! entity.is_liked", &e), Ok(json!(true)));
        assert!(matches!(
            eval("!entity.count", &e),
            Err(ExprError::TypeMismatch { .. })
        ));
        assert!(matches!(
            eval("!entity.missing", &e),
            Err(ExprError::MissingField(_))
        ));
    }

    #[test]
    fn arithmetic() {
        let e = entity(json!({"count": 5}));
        assert_eq!(eval("entity.count + 1", &e), Ok(json!(6)));
        assert_eq!(eval("entity.count+10", &e), Ok(json!(15)));
        assert_eq!(eval("entity.count - 7", &e), Ok(json!(-2)));
        assert_eq!(eval("entity.missing + 2", &e), Ok(json!(2)));
    }

    #[test]
    fn arithmetic_errors() {
        let e = entity(json!({"s": "x", "big": i64::MAX, "f": 1.5}));
        assert!(matches!(
            eval("entity.s + 1", &e),
            Err(ExprError::TypeMismatch { .. })
        ));
        assert!(matches!(
            eval("entity.f + 1", &e),
            Err(ExprError::TypeMismatch { .. })
        ));
        assert_eq!(eval("entity.big + 1", &e), Err(ExprError::Overflow));
    }

    #[test]
    fn ternary() {
        let liked = entity(json!({"is_liked": true, "like_count": 5}));
        let unliked = entity(json!({"is_liked": false, "like_count": 5}));
        let expr = "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1";
        assert_eq!(eval(expr, &liked), Ok(json!(4)));
        assert_eq!(eval(expr, &unliked), Ok(json!(6)));
        let neg = "!entity.is_liked ? \"like\" : \"unlike\"";
        assert_eq!(eval(neg, &liked), Ok(json!("unlike")));
        assert_eq!(eval(neg, &unliked), Ok(json!("like")));
        // Branches: literals (including ones holding `:` / `?`), refs, negation.
        assert_eq!(
            eval("entity.is_liked?{\"a\":1}:\"x:y?\"", &liked),
            Ok(json!({"a": 1}))
        );
        assert_eq!(
            eval("entity.is_liked ? {\"a\":1} : \"x:y?\"", &unliked),
            Ok(json!("x:y?"))
        );
        assert_eq!(
            eval(
                "entity.is_liked ? !entity.is_liked : entity.like_count",
                &liked
            ),
            Ok(json!(false))
        );
        assert_eq!(
            eval("entity.is_liked ? 0 : entity.like_count", &unliked),
            Ok(json!(5))
        );
        // A plain string literal containing `?` is still a literal.
        assert_eq!(eval("\"a?b:c\"", &liked), Ok(json!("a?b:c")));
    }

    #[test]
    fn ternary_errors() {
        let e = entity(json!({"flag": true, "n": 1, "s": "x"}));
        assert_eq!(
            eval("entity.missing ? 1 : 2", &e),
            Err(ExprError::MissingField("missing".into()))
        );
        assert_eq!(
            eval("!entity.missing ? 1 : 2", &e),
            Err(ExprError::MissingField("missing".into()))
        );
        assert!(matches!(
            eval("entity.n ? 1 : 2", &e),
            Err(ExprError::TypeMismatch { .. })
        ));
        assert!(matches!(
            eval("entity.s ? 1 : 2", &e),
            Err(ExprError::TypeMismatch { .. })
        ));
        // Errors inside the chosen branch propagate.
        assert!(matches!(
            eval("entity.flag ? entity.s + 1 : 0", &e),
            Err(ExprError::TypeMismatch { .. })
        ));
        assert_eq!(
            eval("entity.flag ? entity.nope : 0", &e),
            Err(ExprError::MissingField("nope".into()))
        );
        for expr in [
            "entity.flag ? 1",
            "entity.flag ? : 2",
            "entity.flag ? 1 :",
            "entity.flag ? entity.flag ? 1 : 2 : 3",
            "entity.flag ? 1 : entity.flag ? 2 : 3",
            "entity.flag ? 1 * 2 : 3",
            "true ? 1 : 2",
            "entity.flag && entity.flag ? 1 : 2",
        ] {
            assert!(
                matches!(eval(expr, &e), Err(ExprError::Unsupported(_))),
                "expected unsupported: {expr:?} -> {:?}",
                eval(expr, &e)
            );
        }
    }

    #[test]
    fn unsupported() {
        let e = entity(json!({"count": 1}));
        for expr in [
            "",
            "entity.count * 2",
            "entity.count + 1.5",
            "entity.count + entity.count",
            "entity.count + -1",
            "entity.",
            "entity.a.b",
            "foo.count",
            "!true",
            "size(entity.list)",
        ] {
            assert!(
                matches!(eval(expr, &e), Err(ExprError::Unsupported(_))),
                "expected unsupported: {expr:?} -> {:?}",
                eval(expr, &e)
            );
        }
    }
}
