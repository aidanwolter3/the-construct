//! Bindings, as the shared UI resolves them (`Interpolate.kt`): `$item.<f>`
//! (and `entity.<f>`) against the current item, `${<uri>#<f>}` against the
//! entities, `$form.<f>` and `$route.<f>`. Not a CEL evaluator.

use protocol::EntityRecord;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::style::Theme;

/// What a binding reads: the data, the form values, the route and the
/// current item.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub entities: &'a BTreeMap<String, EntityRecord>,
    pub queries: &'a BTreeMap<String, Vec<String>>,
    pub form: &'a BTreeMap<String, String>,
    pub route: &'a BTreeMap<String, String>,
    pub theme: &'a Theme,
    pub item: Option<&'a EntityRecord>,
    pub item_uri: Option<&'a str>,
}

impl<'a> Ctx<'a> {
    pub fn with_item(self, uri: &'a str, item: &'a EntityRecord) -> Ctx<'a> {
        Ctx {
            item: Some(item),
            item_uri: Some(uri),
            ..self
        }
    }

    /// An item's field; `uri` is the item's own URI unless it has one (as
    /// the shared UI adds it to each list item).
    fn item_value(&self, item: &EntityRecord, f: &str) -> Option<Value> {
        match item.get(f) {
            Some(v) => Some(v.clone()),
            None if f == "uri" => self.item_uri.map(|u| Value::String(u.into())),
            None => None,
        }
    }

    pub fn entity(&self, uri: &str) -> Option<&'a EntityRecord> {
        self.entities.get(uri)
    }

    fn field(&self, uri: &str, field: &str) -> Option<Value> {
        if uri == "local://theme" {
            return self
                .theme
                .token_text(field)
                .map(|s| Value::String(s.into()));
        }
        self.entities.get(uri)?.get(field).cloned()
    }

    /// Text with every binding replaced (missing entity fields: "", a
    /// missing `$item` field: left as written, as the shared UI does).
    pub fn text(&self, expr: &str) -> String {
        self.text_with(expr, &mut render)
    }

    /// [`Ctx::text`], with each bound item and entity value drawn by
    /// `draw` (a `core://Text` `format` draws dates with it).
    pub fn text_with(&self, expr: &str, draw: &mut dyn FnMut(&Value) -> String) -> String {
        if let Some(item) = self.item
            && let Some(f) = item_field(expr, true)
        {
            return self
                .item_value(item, f)
                .map(|v| draw(&v))
                .unwrap_or_default();
        }
        let mut s = self.tokens(expr, draw);
        while let Some(start) = s.find("${") {
            let Some(len) = s[start..].find('}') else {
                break;
            };
            let inner = &s[start + 2..start + len];
            let value = match inner.split_once('#') {
                Some((uri, f)) => self.field(uri, f).map(|v| draw(&v)).unwrap_or_default(),
                None => String::new(),
            };
            s.replace_range(start..start + len + 1, &value);
        }
        s
    }

    /// `$item.`, `$form.` and `$route.` tokens replaced.
    fn tokens(&self, s: &str, draw: &mut dyn FnMut(&Value) -> String) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let mut done = false;
            for prefix in ["item.", "form.", "route."] {
                if let Some(tail) = after.strip_prefix(prefix) {
                    let n = tail
                        .char_indices()
                        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
                        .map_or(tail.len(), |(j, _)| j);
                    let name = &tail[..n];
                    let value = match prefix {
                        "item." => self
                            .item
                            .and_then(|it| self.item_value(it, name))
                            .filter(|v| !v.is_null())
                            .map(|v| draw(&v)),
                        "form." => Some(self.form.get(name).cloned().unwrap_or_default()),
                        _ => self.route.get(name).cloned(),
                    };
                    if n > 0
                        && let Some(v) = value
                    {
                        out.push_str(&v);
                        rest = &tail[n..];
                        done = true;
                    }
                    break;
                }
            }
            if !done {
                out.push('$');
                rest = after;
            }
        }
        out.push_str(rest);
        out
    }

    /// A raw value: an exact `$item.<f>` or `$form.<f>` reads the value
    /// itself; other text is interpolated.
    pub fn value(&self, expr: &str) -> Value {
        if let Some(f) = item_field(expr, true) {
            return self
                .item
                .and_then(|i| self.item_value(i, f))
                .unwrap_or(Value::Null);
        }
        if let Some(f) = expr.trim().strip_prefix("$form.") {
            return Value::String(self.form.get(f).cloned().unwrap_or_default());
        }
        Value::String(self.text(expr))
    }

    /// `core://Conditional` and `show_when`: truthy, or equal to `equals`.
    pub fn shows(&self, value: &str, equals: Option<&str>) -> bool {
        let v = self.value(value);
        match equals {
            Some(e) => render(&v) == self.text(e),
            None => truthy(&v),
        }
    }

    /// A colour: `#RRGGBB` or a binding resolving to one.
    pub fn color(&self, expr: &str) -> Option<crate::style::Rgba> {
        crate::style::Rgba::hex(&render(&self.value(expr)))
    }

    /// A list binding: a field holding URIs, or a literal list.
    pub fn list(&self, b: &protocol::ListBinding) -> Vec<String> {
        match b {
            protocol::ListBinding::List(l) => l.clone(),
            protocol::ListBinding::Expr(e) => match self.value(e) {
                Value::Array(a) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                Value::String(s) if !s.is_empty() => vec![s],
                _ => Vec::new(),
            },
        }
    }
}

fn item_field(expr: &str, allow_entity: bool) -> Option<&str> {
    let e = expr.trim();
    let rest = e
        .strip_prefix("$item.")
        .or_else(|| allow_entity.then(|| e.strip_prefix("entity.")).flatten())?;
    let ok = !rest.is_empty()
        && rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(rest)
}

/// A value as drawn: strings unquoted, null as "", others as JSON.
pub fn render(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// False for missing, null, false, 0, "", "0", "false" and an empty array.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => {
            let s = s.trim();
            !(s.is_empty() || s == "0" || s == "false" || s.parse::<f64>() == Ok(0.0))
        }
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// Lowercased with common accents dropped, for list filters ("creme"
/// finds "Crème").
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(v: Value) -> EntityRecord {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn resolves_item_entity_form_and_route() {
        let mut entities = BTreeMap::new();
        entities.insert("demo://people/1".to_owned(), record(json!({"name": "Ada"})));
        let form = BTreeMap::from([("q".to_owned(), "soup".to_owned())]);
        let route = BTreeMap::from([("id".to_owned(), "7".to_owned())]);
        let queries = BTreeMap::new();
        let theme = Theme::new(false, &entities);
        let item = record(json!({"title": "Tea", "n": 3, "gone": null}));
        let ctx = Ctx {
            entities: &entities,
            queries: &queries,
            form: &form,
            route: &route,
            theme: &theme,
            item: None,
            item_uri: None,
        }
        .with_item("demo://x/1", &item);
        assert_eq!(ctx.text("$item.title"), "Tea");
        assert_eq!(ctx.text("$item.n cups of $item.title"), "3 cups of Tea");
        assert_eq!(ctx.text("$item.gone left"), "$item.gone left");
        assert_eq!(ctx.text("Hi ${demo://people/1#name}!"), "Hi Ada!");
        assert_eq!(ctx.text("${demo://people/2#name}"), "");
        assert_eq!(ctx.text("q=$form.q, id=$route.id"), "q=soup, id=7");
        assert_eq!(ctx.text("${local://theme#accent}"), "#4A2C82");
        assert_eq!(ctx.text("costs $5"), "costs $5");
        assert!(ctx.shows("$item.n", None));
        assert!(!ctx.shows("$item.gone", None));
        assert!(ctx.shows("$form.q", Some("soup")));
    }

    #[test]
    fn an_entity_binding_can_name_the_item_s_ref() {
        // A collection ref holds a row id; a layout reads through it
        // (decision `collection-primitives`): `$item.` is bound first.
        let mut entities = BTreeMap::new();
        entities.insert(
            "collections://ingredients/r1".to_owned(),
            record(json!({"name": "Chicken broth"})),
        );
        let (form, route, queries) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
        let theme = Theme::new(false, &entities);
        let item = record(json!({"ingredient": "r1", "amount_text": "4 cups", "other": null}));
        let ctx = Ctx {
            entities: &entities,
            queries: &queries,
            form: &form,
            route: &route,
            theme: &theme,
            item: None,
            item_uri: None,
        }
        .with_item("collections://recipe_lines/r9", &item);
        assert_eq!(
            ctx.text("$item.amount_text ${collections://ingredients/$item.ingredient#name}"),
            "4 cups Chicken broth"
        );
        // An empty ref, or a row not loaded, reads as nothing.
        assert_eq!(
            ctx.text("${collections://ingredients/$item.other#name}"),
            ""
        );
        assert_eq!(ctx.text("${collections://ingredients/r2#name}"), "");
    }

    #[test]
    fn truthiness_matches_the_shared_ui() {
        for v in [
            json!(null),
            json!(false),
            json!(0),
            json!(""),
            json!("0"),
            json!([]),
        ] {
            assert!(!truthy(&v), "{v}");
        }
        for v in [json!(true), json!(1), json!("x"), json!([1]), json!({})] {
            assert!(truthy(&v), "{v}");
        }
    }
}
