//! The expression language of computed fields (decision
//! `collection-computed-fields`): the mutation-expression subset clients
//! evaluate for `value_expr` (D4: `entity.<field>`, JSON literals, `!`,
//! `+`/`-`, `c ? a : b`), widened just enough for a row's values:
//!
//! - `+` joins text when either side is text (`entity.name + " x"`), else
//!   adds numbers; `-` subtracts numbers; an empty value counts as `""` or
//!   `0`;
//! - a condition tests any value (empty text, `0`, `false` and empty
//!   values are false);
//! - parentheses group, and ternaries may nest.
//!
//! No calls, no loops, no access to anything but the row: an expression is
//! at most [`MAX_EXPR_CHARS`] long and nests at most [`MAX_DEPTH`] deep,
//! so evaluating one is cheap and always ends.

use serde_json::{Value, json};

/// Longest expression.
pub const MAX_EXPR_CHARS: usize = 300;
/// Deepest nesting.
pub const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    /// `entity.<field>`: a field's name, or its id once resolved.
    Field(String),
    Not(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Lit(Value),
    Field(String),
    Bang,
    Plus,
    Minus,
    Question,
    Colon,
    Open,
    Close,
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'!' => (out.push(Tok::Bang), i += 1).1,
            b'+' => (out.push(Tok::Plus), i += 1).1,
            b'-' => (out.push(Tok::Minus), i += 1).1,
            b'?' => (out.push(Tok::Question), i += 1).1,
            b':' => (out.push(Tok::Colon), i += 1).1,
            b'(' => (out.push(Tok::Open), i += 1).1,
            b')' => (out.push(Tok::Close), i += 1).1,
            b'"' => {
                let start = i;
                i += 1;
                let mut escaped = false;
                while i < b.len() && (escaped || b[i] != b'"') {
                    escaped = !escaped && b[i] == b'\\';
                    i += 1;
                }
                if i >= b.len() {
                    return Err("a text literal is missing its closing \"".into());
                }
                i += 1;
                let v: Value = serde_json::from_str(&s[start..i])
                    .map_err(|_| format!("{} is not a valid text literal", &s[start..i]))?;
                out.push(Tok::Lit(v));
            }
            b'0'..=b'9' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                let n: f64 = s[start..i]
                    .parse()
                    .map_err(|_| format!("'{}' is not a number", &s[start..i]))?;
                out.push(Tok::Lit(number(n).ok_or("a number is too large")?));
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let word = ident(s, &mut i);
                match word {
                    "true" => out.push(Tok::Lit(json!(true))),
                    "false" => out.push(Tok::Lit(json!(false))),
                    "null" => out.push(Tok::Lit(Value::Null)),
                    "entity" if b.get(i) == Some(&b'.') => {
                        i += 1;
                        let name = ident(s, &mut i);
                        if name.is_empty() {
                            return Err("entity. needs a field name".into());
                        }
                        out.push(Tok::Field(name.to_owned()));
                    }
                    w => {
                        return Err(format!(
                            "'{w}' is not part of the expression language (a field is entity.<name>; text is \"quoted\")"
                        ));
                    }
                }
            }
            _ => {
                return Err(format!(
                    "'{}' is not part of the expression language (entity.<field>, literals, !, +, -, ? :, parentheses)",
                    s[i..].chars().next().unwrap_or(' ')
                ));
            }
        }
    }
    Ok(out)
}

fn ident<'a>(s: &'a str, i: &mut usize) -> &'a str {
    let start = *i;
    let b = s.as_bytes();
    while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b[*i] == b'_') {
        *i += 1;
    }
    &s[start..*i]
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn deeper(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(format!("an expression nests at most {MAX_DEPTH} deep"))
        } else {
            Ok(())
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.deeper()?;
        let cond = self.sum()?;
        let out = if self.eat(&Tok::Question) {
            let then = self.expr()?;
            if !self.eat(&Tok::Colon) {
                return Err("a ? needs a : (cond ? a : b)".into());
            }
            let other = self.expr()?;
            Expr::Cond(Box::new(cond), Box::new(then), Box::new(other))
        } else {
            cond
        };
        self.depth -= 1;
        Ok(out)
    }

    fn sum(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            if self.eat(&Tok::Plus) {
                left = Expr::Add(Box::new(left), Box::new(self.unary()?));
            } else if self.eat(&Tok::Minus) {
                left = Expr::Sub(Box::new(left), Box::new(self.unary()?));
            } else {
                return Ok(left);
            }
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        self.deeper()?;
        let out = if self.eat(&Tok::Bang) {
            Expr::Not(Box::new(self.unary()?))
        } else if self.eat(&Tok::Minus) {
            match self.unary()? {
                Expr::Lit(Value::Number(n)) => {
                    Expr::Lit(number(-n.as_f64().unwrap_or(0.0)).unwrap_or(json!(0)))
                }
                e => Expr::Sub(Box::new(Expr::Lit(json!(0))), Box::new(e)),
            }
        } else {
            self.primary()?
        };
        self.depth -= 1;
        Ok(out)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.toks.get(self.at).cloned() {
            Some(Tok::Lit(v)) => {
                self.at += 1;
                Ok(Expr::Lit(v))
            }
            Some(Tok::Field(f)) => {
                self.at += 1;
                Ok(Expr::Field(f))
            }
            Some(Tok::Open) => {
                self.at += 1;
                let e = self.expr()?;
                if !self.eat(&Tok::Close) {
                    return Err("a ( is missing its )".into());
                }
                Ok(e)
            }
            Some(t) => Err(format!("unexpected {}", tok_text(&t))),
            None => Err("the expression ends too soon".into()),
        }
    }
}

fn tok_text(t: &Tok) -> String {
    match t {
        Tok::Lit(v) => v.to_string(),
        Tok::Field(f) => format!("entity.{f}"),
        Tok::Bang => "!".into(),
        Tok::Plus => "+".into(),
        Tok::Minus => "-".into(),
        Tok::Question => "?".into(),
        Tok::Colon => ":".into(),
        Tok::Open => "(".into(),
        Tok::Close => ")".into(),
    }
}

/// Parse `s` (field references as written: names on input, ids when
/// stored).
pub fn parse(s: &str) -> Result<Expr, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("the expression is empty".into());
    }
    if s.chars().count() > MAX_EXPR_CHARS {
        return Err(format!(
            "an expression is at most {MAX_EXPR_CHARS} characters"
        ));
    }
    let mut p = Parser {
        toks: lex(s)?,
        at: 0,
        depth: 0,
    };
    let e = p.expr()?;
    if let Some(t) = p.peek() {
        return Err(format!("unexpected {} after the expression", tok_text(t)));
    }
    Ok(e)
}

impl Expr {
    /// The fields it reads, in order, each once.
    pub fn fields(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.walk(&mut |f| {
            if !out.iter().any(|o| o == f) {
                out.push(f.to_owned());
            }
        });
        out
    }

    fn walk(&self, f: &mut dyn FnMut(&str)) {
        match self {
            Expr::Lit(_) => {}
            Expr::Field(n) => f(n),
            Expr::Not(a) => a.walk(f),
            Expr::Add(a, b) | Expr::Sub(a, b) => {
                a.walk(f);
                b.walk(f);
            }
            Expr::Cond(c, a, b) => {
                c.walk(f);
                a.walk(f);
                b.walk(f);
            }
        }
    }

    /// The same expression with each field reference mapped by `f` (names
    /// to ids on input, ids to names for people).
    pub fn map_fields(&self, f: &dyn Fn(&str) -> Result<String, String>) -> Result<Expr, String> {
        let bx = |e: &Expr| e.map_fields(f).map(Box::new);
        Ok(match self {
            Expr::Lit(v) => Expr::Lit(v.clone()),
            Expr::Field(n) => Expr::Field(f(n)?),
            Expr::Not(a) => Expr::Not(bx(a)?),
            Expr::Add(a, b) => Expr::Add(bx(a)?, bx(b)?),
            Expr::Sub(a, b) => Expr::Sub(bx(a)?, bx(b)?),
            Expr::Cond(c, a, b) => Expr::Cond(bx(c)?, bx(a)?, bx(b)?),
        })
    }

    fn prec(&self) -> u8 {
        match self {
            Expr::Cond(..) => 0,
            Expr::Add(..) | Expr::Sub(..) => 1,
            Expr::Not(_) => 2,
            Expr::Lit(Value::Number(n)) if n.as_f64().is_some_and(|x| x < 0.0) => 2,
            Expr::Lit(_) | Expr::Field(_) => 3,
        }
    }

    fn child(&self, min: u8) -> String {
        if self.prec() < min {
            format!("({self})")
        } else {
            self.to_string()
        }
    }
}

impl std::fmt::Display for Expr {
    /// The expression as text that parses back to it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expr::Lit(v) => write!(f, "{v}"),
            Expr::Field(n) => write!(f, "entity.{n}"),
            Expr::Not(a) => write!(f, "!{}", a.child(2)),
            Expr::Add(a, b) => write!(f, "{} + {}", a.child(1), b.child(2)),
            Expr::Sub(a, b) => write!(f, "{} - {}", a.child(1), b.child(2)),
            Expr::Cond(c, a, b) => write!(f, "{} ? {} : {}", c.child(1), a.child(0), b.child(0)),
        }
    }
}

/// Whether `v` counts as true in a condition.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn text(v: &Value) -> Result<String, String> {
    match v {
        Value::Null => Ok(String::new()),
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err("only text, numbers and true/false join as text".into()),
    }
}

fn num(v: &Value) -> Result<f64, String> {
    match v {
        Value::Null => Ok(0.0),
        Value::Number(n) => n.as_f64().ok_or_else(|| "not a number".into()),
        other => Err(format!("{other} is not a number")),
    }
}

/// A JSON number from `f`, integral when it is.
fn number(f: f64) -> Option<Value> {
    if !f.is_finite() {
        return None;
    }
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        Some(json!(f as i64))
    } else {
        serde_json::Number::from_f64(f).map(Value::Number)
    }
}

/// Evaluate `e`, reading fields with `get`. `Err`: why it has no value
/// (the field is then left empty).
pub fn eval(e: &Expr, get: &dyn Fn(&str) -> Value) -> Result<Value, String> {
    match e {
        Expr::Lit(v) => Ok(v.clone()),
        Expr::Field(n) => Ok(get(n)),
        Expr::Not(a) => Ok(json!(!truthy(&eval(a, get)?))),
        Expr::Add(a, b) => {
            let (a, b) = (eval(a, get)?, eval(b, get)?);
            if a.is_string() || b.is_string() {
                Ok(json!(format!("{}{}", text(&a)?, text(&b)?)))
            } else {
                number(num(&a)? + num(&b)?).ok_or_else(|| "the sum is too large".into())
            }
        }
        Expr::Sub(a, b) => number(num(&eval(a, get)?)? - num(&eval(b, get)?)?)
            .ok_or_else(|| "the difference is too large".into()),
        Expr::Cond(c, a, b) => {
            if truthy(&eval(c, get)?) {
                eval(a, get)
            } else {
                eval(b, get)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &str, row: Value) -> Result<Value, String> {
        let e = parse(s)?;
        eval(&e, &|f| row.get(f).cloned().unwrap_or(Value::Null))
    }

    #[test]
    fn the_client_subset_evaluates_the_same() {
        let row = json!({"count": 5, "is_liked": true, "name": "a"});
        assert_eq!(run("0", row.clone()), Ok(json!(0)));
        assert_eq!(run("\"x\"", row.clone()), Ok(json!("x")));
        assert_eq!(run("entity.name", row.clone()), Ok(json!("a")));
        assert_eq!(run("!entity.is_liked", row.clone()), Ok(json!(false)));
        assert_eq!(run("entity.count + 1", row.clone()), Ok(json!(6)));
        assert_eq!(run("entity.count - 7", row.clone()), Ok(json!(-2)));
        assert_eq!(run("entity.missing + 2", row.clone()), Ok(json!(2)));
        assert_eq!(
            run(
                "entity.is_liked ? entity.count - 1 : entity.count + 1",
                row.clone()
            ),
            Ok(json!(4))
        );
        assert_eq!(run("\"a?b:c\"", row), Ok(json!("a?b:c")));
    }

    #[test]
    fn text_joins_conditions_test_any_value_and_parentheses_group() {
        let label =
            "entity.quantity ? entity.name + \" (\" + entity.quantity + \")\" : entity.name";
        assert_eq!(
            run(label, json!({"name": "Apples", "quantity": "6"})),
            Ok(json!("Apples (6)"))
        );
        assert_eq!(run(label, json!({"name": "Milk"})), Ok(json!("Milk")));
        assert_eq!(
            run(label, json!({"name": "Milk", "quantity": "  "})),
            Ok(json!("Milk"))
        );
        assert_eq!(run("entity.n + \" x\"", json!({"n": 2})), Ok(json!("2 x")));
        assert_eq!(run("10 - (2 + 3)", json!({})), Ok(json!(5)));
        assert_eq!(run("10 - 2 + 3", json!({})), Ok(json!(11)));
        assert_eq!(run("-entity.n + 1", json!({"n": 4})), Ok(json!(-3)));
        assert_eq!(run("entity.n + 0.5", json!({"n": 1})), Ok(json!(1.5)));
        assert_eq!(
            run("entity.a ? 1 : entity.b ? 2 : 3", json!({"b": true})),
            Ok(json!(2))
        );
        assert!(run("entity.flag + 1", json!({"flag": true})).is_err());
        assert!(run("entity.s - 1", json!({"s": "x"})).is_err());
    }

    #[test]
    fn only_the_small_language_parses() {
        for bad in [
            "",
            "entity.",
            "name",
            "size(entity.list)",
            "entity.a * 2",
            "entity.a ? 1",
            "(entity.a",
            "entity.a entity.b",
            "\"open",
            "entity.a.b",
            "1 ; 2",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert!(parse(&"(".repeat(20)).is_err(), "too deep");
        assert!(parse(&format!("\"{}\"", "x".repeat(MAX_EXPR_CHARS))).is_err());
    }

    #[test]
    fn rendering_parses_back_to_the_same_expression() {
        for s in [
            "entity.quantity ? entity.name + \" (\" + entity.quantity + \")\" : entity.name",
            "10 - (2 + 3)",
            "!(entity.a + 1)",
            "(entity.a ? 1 : 2) + 3",
            "entity.a ? entity.b ? 1 : 2 : 3",
            "entity.n + -1",
            "\"quote \\\" inside\"",
        ] {
            let e = parse(s).unwrap();
            assert_eq!(parse(&e.to_string()).unwrap(), e, "{s} -> {e}");
        }
        let e = parse("entity.name + entity.qty + entity.name").unwrap();
        assert_eq!(e.fields(), vec!["name".to_owned(), "qty".to_owned()]);
        let ids = e
            .map_fields(&|n| Ok(if n == "name" { "f1" } else { "f2" }.to_owned()))
            .unwrap();
        assert_eq!(ids.to_string(), "entity.f1 + entity.f2 + entity.f1");
    }
}
