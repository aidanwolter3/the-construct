//! `quantity` fields (decision `collection-primitives`): an amount and a
//! unit, such as "1 1/2 cups". Text is parsed here, deterministically (no
//! model); text that doesn't parse is kept as written, shown as is and left
//! out of sums.
//!
//! A parsed value is stored as `{amount, unit}`, or a list of them when it
//! has parts of different dimensions ("2 cups + 3 cloves"); `unit` is the
//! canonical name ([`UNITS`]), `""` for a plain count ("3"). Unparsed text is
//! stored as a string.
//!
//! - Volume (`tsp`, `tbsp`, `fl oz`, `cup`, `pint`, `quart`, `ml`, `l`) and
//!   weight (`g`, `kg`, `oz`, `lb`) convert within their dimension.
//! - Every other unit is count-like: `clove`, `can`, `slice`, `bunch`,
//!   `large`, `medium`, `small`, `piece`, `pinch`, `dash`, `""` (each), any
//!   other single word ("sprig"), and a size before it ("1 (14 oz) can" is
//!   1 of unit `(14 oz) can`). A count-like unit adds only to itself.
//!
//! [`sum`] keeps US units as written: one unit stays (4 tbsp + 2 tbsp = 6
//! tbsp); several become one amount of the largest when that is a plain
//! fraction (1/2 cup + 2 tbsp = 5/8 cup), else stay apart (1 cup + 3 tbsp).
//! [`scale`] (and a sum with metric parts) chooses a unit again: US volume as
//! cups (from 1/4 cup), tablespoons (from 1 tbsp) or teaspoons, preferring the
//! largest whose amount is a plain fraction; US weight as lb (from 16 oz) or
//! oz; metric as ml/l and g/kg. A value written by hand keeps its unit.

use serde_json::{Value, json};

/// What a unit measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    Volume,
    Weight,
    /// Adds only to the same unit.
    Count,
}

/// A built-in unit: its canonical name, plural, dimension, size in the
/// dimension's base (ml, g) and whether it is metric.
#[derive(Debug, Clone, Copy)]
pub struct Unit {
    pub name: &'static str,
    pub plural: &'static str,
    pub dim: Dim,
    pub base: f64,
    pub metric: bool,
}

const fn unit(name: &'static str, plural: &'static str, dim: Dim, base: f64, metric: bool) -> Unit {
    Unit {
        name,
        plural,
        dim,
        base,
        metric,
    }
}

/// The built-in units.
pub const UNITS: &[Unit] = &[
    unit("tsp", "tsp", Dim::Volume, 4.928_921_593_75, false),
    unit("tbsp", "tbsp", Dim::Volume, 14.786_764_781_25, false),
    unit("fl oz", "fl oz", Dim::Volume, 29.573_529_562_5, false),
    unit("cup", "cups", Dim::Volume, 236.588_236_5, false),
    unit("pint", "pints", Dim::Volume, 473.176_473, false),
    unit("quart", "quarts", Dim::Volume, 946.352_946, false),
    unit("ml", "ml", Dim::Volume, 1.0, true),
    unit("l", "l", Dim::Volume, 1000.0, true),
    unit("g", "g", Dim::Weight, 1.0, true),
    unit("kg", "kg", Dim::Weight, 1000.0, true),
    unit("oz", "oz", Dim::Weight, 28.349_523_125, false),
    unit("lb", "lb", Dim::Weight, 453.592_37, false),
    unit("clove", "cloves", Dim::Count, 1.0, false),
    unit("can", "cans", Dim::Count, 1.0, false),
    unit("slice", "slices", Dim::Count, 1.0, false),
    unit("bunch", "bunches", Dim::Count, 1.0, false),
    unit("large", "large", Dim::Count, 1.0, false),
    unit("medium", "medium", Dim::Count, 1.0, false),
    unit("small", "small", Dim::Count, 1.0, false),
    unit("piece", "pieces", Dim::Count, 1.0, false),
    unit("pinch", "pinches", Dim::Count, 1.0, false),
    unit("dash", "dashes", Dim::Count, 1.0, false),
];

/// Other names of the built-in units (lowercase; `T` and `t` are matched
/// before lowercasing: tablespoon and teaspoon).
const SYNONYMS: &[(&str, &str)] = &[
    ("teaspoon", "tsp"),
    ("teaspoons", "tsp"),
    ("tsps", "tsp"),
    ("tablespoon", "tbsp"),
    ("tablespoons", "tbsp"),
    ("tbsps", "tbsp"),
    ("tbs", "tbsp"),
    ("tbl", "tbsp"),
    ("tbls", "tbsp"),
    ("cups", "cup"),
    ("c", "cup"),
    ("fl oz", "fl oz"),
    ("fl. oz", "fl oz"),
    ("floz", "fl oz"),
    ("fluid ounce", "fl oz"),
    ("fluid ounces", "fl oz"),
    ("pints", "pint"),
    ("pt", "pint"),
    ("pts", "pint"),
    ("quarts", "quart"),
    ("qt", "quart"),
    ("qts", "quart"),
    ("milliliter", "ml"),
    ("milliliters", "ml"),
    ("millilitre", "ml"),
    ("millilitres", "ml"),
    ("liter", "l"),
    ("liters", "l"),
    ("litre", "l"),
    ("litres", "l"),
    ("gram", "g"),
    ("grams", "g"),
    ("gr", "g"),
    ("kilogram", "kg"),
    ("kilograms", "kg"),
    ("kilo", "kg"),
    ("kilos", "kg"),
    ("ounce", "oz"),
    ("ounces", "oz"),
    ("lbs", "lb"),
    ("pound", "lb"),
    ("pounds", "lb"),
    ("cloves", "clove"),
    ("cans", "can"),
    ("slices", "slice"),
    ("bunches", "bunch"),
    ("lg", "large"),
    ("med", "medium"),
    ("sm", "small"),
    ("pieces", "piece"),
    ("pc", "piece"),
    ("pcs", "piece"),
    ("pinches", "pinch"),
    ("dashes", "dash"),
    ("each", ""),
    ("ea", ""),
    ("whole", ""),
];

/// Longest unit name kept (a size and a word).
const MAX_UNIT_CHARS: usize = 40;

/// One part of a quantity: an amount of a unit (canonical name; `""`: each).
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub amount: f64,
    pub unit: String,
}

/// A parsed quantity: one part, or several of different dimensions or
/// count-like units.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Quantity {
    pub parts: Vec<Part>,
}

/// The built-in unit named `name` (canonical).
pub fn builtin(name: &str) -> Option<&'static Unit> {
    UNITS.iter().find(|u| u.name == name)
}

/// The canonical name of a unit word (`None`: not a unit word).
fn canonical(word: &str) -> Option<String> {
    let w = word.trim().trim_end_matches('.');
    match w {
        "T" | "Tb" | "TB" => return Some("tbsp".into()),
        "t" => return Some("tsp".into()),
        _ => {}
    }
    let l = w.to_lowercase();
    if let Some(u) = UNITS.iter().find(|u| u.name == l) {
        return Some(u.name.to_owned());
    }
    SYNONYMS
        .iter()
        .find(|(s, _)| *s == l)
        .map(|(_, c)| (*c).to_owned())
}

/// An unknown unit word as stored: lowercase, a plural `s` dropped.
fn singular(word: &str) -> String {
    let l = word.to_lowercase();
    if l.len() > 3 && l.ends_with('s') && !l.ends_with("ss") {
        l[..l.len() - 1].to_owned()
    } else {
        l
    }
}

/// The plural of a stored unit name.
fn plural(unit: &str) -> String {
    if let Some(u) = builtin(unit) {
        return u.plural.to_owned();
    }
    // A size before a unit: the last word takes the plural.
    let (head, last) = match unit.rsplit_once(' ') {
        Some((h, l)) => (format!("{h} "), l),
        None => (String::new(), unit),
    };
    let p = match builtin(last) {
        Some(u) => u.plural.to_owned(),
        None if last.is_empty() => String::new(),
        None if ["s", "x", "z", "ch", "sh"]
            .iter()
            .any(|e| last.ends_with(e)) =>
        {
            format!("{last}es")
        }
        None => format!("{last}s"),
    };
    format!("{head}{p}")
}

// ---------- parsing ----------

/// Text with unicode fractions spelled out and dashes made plain.
fn normalize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        let frac = match c {
            '½' => Some("1/2"),
            '⅓' => Some("1/3"),
            '⅔' => Some("2/3"),
            '¼' => Some("1/4"),
            '¾' => Some("3/4"),
            '⅛' => Some("1/8"),
            '⅜' => Some("3/8"),
            '⅝' => Some("5/8"),
            '⅞' => Some("7/8"),
            '⅙' => Some("1/6"),
            '⅚' => Some("5/6"),
            '⅕' => Some("1/5"),
            _ => None,
        };
        match frac {
            Some(f) => {
                out.push(' ');
                out.push_str(f);
                out.push(' ');
            }
            None if c == '–' || c == '—' => out.push('-'),
            None if c == '⁄' => out.push('/'),
            None => out.push(c),
        }
    }
    out
}

/// Tokens: numbers, fractions, words, `(`…`)` groups, `-` and `+`.
fn tokens(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == ',' {
            i += 1;
        } else if c == '(' {
            let start = i;
            while i < chars.len() && chars[i] != ')' {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            out.push(chars[start..i].iter().collect());
        } else if c == '-' || c == '+' {
            out.push(c.to_string());
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '/')
            {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            let start = i;
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '(' | ',' | '-' | '+')
                && !chars[i].is_ascii_digit()
            {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        }
    }
    out
}

/// A number token: `3`, `1.5`, `1/2`.
fn number(t: &str) -> Option<f64> {
    if let Some((n, d)) = t.split_once('/') {
        let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
        return (d > 0.0 && n >= 0.0).then(|| n / d);
    }
    let v = t.parse::<f64>().ok()?;
    (v.is_finite() && v >= 0.0).then_some(v)
}

fn is_fraction(t: &str) -> bool {
    t.contains('/') && number(t).is_some()
}

/// An amount at `toks[*i..]`: "a"/"an", `2`, `1 1/2`, `1-1/2`, `1/2`, a range
/// `2-3` or `2 to 3` (its upper end). Advances `i`.
fn amount(toks: &[String], i: &mut usize) -> Option<f64> {
    let t = toks.get(*i)?;
    let l = t.to_lowercase();
    if l == "a" || l == "an" || l == "one" {
        *i += 1;
        return Some(1.0);
    }
    let mut v = number(t)?;
    *i += 1;
    let whole = !t.contains('/') && !t.contains('.');
    // A mixed number: "1 1/2", "1-1/2".
    if whole {
        if let Some(f) = toks.get(*i).filter(|x| is_fraction(x)) {
            v += number(f).unwrap_or(0.0);
            *i += 1;
            return Some(v);
        }
        if matches!(toks.get(*i).map(String::as_str), Some("-" | "and"))
            && let Some(f) = toks.get(*i + 1).filter(|x| is_fraction(x))
        {
            v += number(f).unwrap_or(0.0);
            *i += 2;
            return Some(v);
        }
    }
    // A range: the upper end (enough for a shopping list).
    let sep = toks.get(*i).map(|s| s.to_lowercase());
    if matches!(sep.as_deref(), Some("-" | "to" | "or")) {
        let mut j = *i + 1;
        if let Some(hi) = amount(toks, &mut j) {
            *i = j;
            return Some(v.max(hi));
        }
    }
    Some(v)
}

/// A unit at `toks[*i..]`: a size `(14 oz)` and/or a unit word (two-word
/// units first), else `""`. `None`: what follows isn't a unit.
fn unit_at(toks: &[String], i: &mut usize) -> Option<String> {
    let mut size = String::new();
    if let Some(t) = toks.get(*i).filter(|t| t.starts_with('(')) {
        let inner = t.trim_start_matches('(').trim_end_matches(')');
        size = format!(
            "({})",
            inner
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        );
        *i += 1;
    }
    let word = |k: usize| {
        toks.get(k)
            .filter(|t| !is_separator(t) && !t.starts_with('('))
    };
    let mut name: Option<String> = None;
    if let (Some(a), Some(b)) = (word(*i), word(*i + 1))
        && let Some(c) = canonical(&format!("{a} {b}"))
    {
        name = Some(c);
        *i += 2;
    }
    if name.is_none()
        && let Some(a) = word(*i)
    {
        if let Some(c) = canonical(a) {
            name = Some(c);
            *i += 1;
        } else if a.chars().all(char::is_alphabetic) && a.chars().count() <= 20 {
            name = Some(singular(a));
            *i += 1;
        } else {
            return None;
        }
    }
    let name = name.unwrap_or_default();
    // "of" after a unit ("a pinch of"): nothing follows it here.
    let out = match (size.is_empty(), name.is_empty()) {
        (true, _) => name,
        (false, true) => size,
        (false, false) => format!("{size} {name}"),
    };
    (out.chars().count() <= MAX_UNIT_CHARS).then_some(out)
}

fn is_separator(t: &str) -> bool {
    matches!(t.to_lowercase().as_str(), "plus" | "+" | "and" | "&")
}

/// Parse quantity text: terms joined by `plus`, `+` or `and`, each an
/// amount and an optional unit. `None`: not a quantity (kept as text).
pub fn parse(text: &str) -> Option<Quantity> {
    let norm = normalize_text(text);
    let toks = tokens(&norm);
    if toks.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut i = 0;
    loop {
        let a = amount(&toks, &mut i)?;
        let u = unit_at(&toks, &mut i)?;
        parts.push(Part { amount: a, unit: u });
        match toks.get(i) {
            None => break,
            Some(t) if is_separator(t) => i += 1,
            Some(_) => return None,
        }
    }
    if parts.len() == 1 {
        return Some(Quantity { parts });
    }
    // "1/4 cup plus 2 tbsp": one amount.
    Some(sum(std::iter::once(&Quantity { parts })))
}

// ---------- values ----------

/// Rounded to 6 decimals, so sums store no float noise.
fn tidy(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn part_json(p: &Part) -> Value {
    let a = tidy(p.amount);
    let amount = if a.fract() == 0.0 && a.abs() < 9.0e15 {
        json!(a as i64)
    } else {
        json!(a)
    };
    json!({ "amount": amount, "unit": p.unit })
}

impl Quantity {
    /// As stored: `{amount, unit}`, or a list of them.
    pub fn to_value(&self) -> Value {
        match self.parts.as_slice() {
            [one] => part_json(one),
            many => Value::Array(many.iter().map(part_json).collect()),
        }
    }

    /// As people read it: "1 1/2 cups", "2 cups + 3 cloves".
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .map(part_text)
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// One stored part `{amount, unit}` (a unit name normalized).
fn part_of(v: &Value) -> Option<Part> {
    let o = v.as_object()?;
    let amount = match o.get("amount")? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => number(s.trim())?,
        _ => return None,
    };
    if !amount.is_finite() || amount < 0.0 {
        return None;
    }
    let unit = match o.get("unit") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            let s = s.trim();
            canonical(s).unwrap_or_else(|| {
                s.split_whitespace()
                    .map(|w| {
                        if w.starts_with('(') {
                            w.to_lowercase()
                        } else {
                            singular(w)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        }
        _ => return None,
    };
    (unit.chars().count() <= MAX_UNIT_CHARS).then_some(Part { amount, unit })
}

/// A stored (or written) structured value: `{amount, unit}` or a list of
/// them. `None` for text and anything else.
pub fn from_value(v: &Value) -> Option<Quantity> {
    match v {
        Value::Object(_) => Some(Quantity {
            parts: vec![part_of(v)?],
        }),
        Value::Array(a) if !a.is_empty() => Some(Quantity {
            parts: a.iter().map(part_of).collect::<Option<Vec<_>>>()?,
        }),
        _ => None,
    }
}

/// A written value as stored: text is parsed (kept as text when it doesn't
/// parse), a number is a count, `{amount, unit}` or a list of them is
/// checked. `Ok(None)`: empty.
pub fn coerce(v: &Value, max_chars: usize) -> Result<Option<Value>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => {
            let s = s.trim();
            if s.chars().count() > max_chars {
                return Err(format!("text is at most {max_chars} characters"));
            }
            Ok(Some(parse(s).map_or_else(|| json!(s), |q| q.to_value())))
        }
        Value::Number(n) => {
            let a = n.as_f64().filter(|a| a.is_finite() && *a >= 0.0).ok_or_else(|| {
                format!("{v} is not a quantity (an amount is at least 0)")
            })?;
            Ok(Some(
                Quantity {
                    parts: vec![Part {
                        amount: a,
                        unit: String::new(),
                    }],
                }
                .to_value(),
            ))
        }
        Value::Object(_) | Value::Array(_) => from_value(v)
            .map(|q| Some(q.to_value()))
            .ok_or_else(|| {
                format!("{v} is not a quantity: text (\"1 1/2 cups\"), a number, {{amount, unit}} or a list of those")
            }),
        Value::Bool(_) => Err(format!("{v} is not a quantity")),
    }
}

/// A stored value as text: a quantity formatted, text as written.
pub fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => from_value(other).map_or_else(String::new, |q| q.text()),
    }
}

/// Whether a stored value is text that didn't parse.
pub fn is_unparsed(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}

// ---------- arithmetic ----------

/// The sum of `qs`: amounts of one dimension convert and add (volume and
/// weight; metric only when every part is), count-like units add to
/// themselves. Volume, then weight, then count-like units in the order first
/// seen. US units keep the units the parts were written in ([`combine`]).
pub fn sum<'a>(qs: impl IntoIterator<Item = &'a Quantity>) -> Quantity {
    sum_in(qs, true)
}

/// One dimension's parts while summing: the amount per built-in unit seen.
#[derive(Default)]
struct Totals {
    units: Vec<(&'static Unit, f64)>,
}

impl Totals {
    fn base(&self) -> f64 {
        self.units.iter().map(|(u, a)| a * u.base).sum()
    }
}

/// [`sum`]; `keep_units` false picks a unit afresh ([`best`]) as scaling does.
fn sum_in<'a>(qs: impl IntoIterator<Item = &'a Quantity>, keep_units: bool) -> Quantity {
    let mut volume: Option<Totals> = None;
    let mut weight: Option<Totals> = None;
    let mut counts: Vec<Part> = Vec::new();
    for q in qs {
        for p in &q.parts {
            match builtin(&p.unit).filter(|u| u.dim != Dim::Count) {
                Some(u) => {
                    let slot = if u.dim == Dim::Volume {
                        &mut volume
                    } else {
                        &mut weight
                    };
                    let t = slot.get_or_insert_with(Totals::default);
                    match t.units.iter_mut().find(|(seen, _)| seen.name == u.name) {
                        Some((_, a)) => *a += p.amount,
                        None => t.units.push((u, p.amount)),
                    }
                }
                None => match counts.iter_mut().find(|c| c.unit == p.unit) {
                    Some(c) => c.amount += p.amount,
                    None => counts.push(p.clone()),
                },
            }
        }
    }
    let mut parts = Vec::new();
    for (dim, t) in [(Dim::Volume, volume), (Dim::Weight, weight)] {
        let Some(t) = t else { continue };
        let metric = t.units.iter().all(|(u, _)| u.metric);
        if keep_units && t.units.iter().all(|(u, _)| !u.metric) {
            parts.extend(combine(t));
        } else {
            parts.push(best(dim, t.base(), metric));
        }
    }
    parts.extend(counts);
    Quantity { parts }
}

/// US amounts of one dimension, in the units they were written in: one unit
/// stays as it is (4 tbsp + 2 tbsp = 6 tbsp); several make one amount of the
/// largest when that is a plain fraction to eighths (1/2 cup + 2 tbsp = 5/8
/// cup), else stay apart, largest first, each smaller amount's whole multiples
/// of the next larger unit carried up (1 cup + 3 tbsp; 1 cup + 19 tbsp = 2 cups
/// + 3 tbsp).
fn combine(mut t: Totals) -> Vec<Part> {
    t.units.sort_by(|a, b| {
        b.0.base
            .partial_cmp(&a.0.base)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let part = |u: &Unit, amount: f64| Part {
        amount,
        unit: u.name.to_owned(),
    };
    if let [(u, a)] = t.units.as_slice() {
        return vec![part(u, *a)];
    }
    let largest = t.units[0].0;
    let whole = t.base() / largest.base;
    if nice(whole).is_some_and(|(_, _, d)| d <= 8) {
        return vec![part(largest, whole)];
    }
    for i in (1..t.units.len()).rev() {
        let (small, a) = t.units[i];
        let big = t.units[i - 1].0;
        let k = (a * small.base / big.base + 1e-9).floor();
        if k >= 1.0 {
            t.units[i].1 = (a - k * big.base / small.base).max(0.0);
            t.units[i - 1].1 += k;
        }
    }
    t.units
        .iter()
        .filter(|(_, a)| *a > 1e-6)
        .map(|(u, a)| part(u, *a))
        .collect()
}

/// `q` times `k`, in a sensible unit (`k` = 1: unchanged).
pub fn scale(q: &Quantity, k: f64) -> Quantity {
    if (k - 1.0).abs() < 1e-9 {
        return q.clone();
    }
    let scaled = Quantity {
        parts: q
            .parts
            .iter()
            .map(|p| Part {
                amount: p.amount * k,
                unit: p.unit.clone(),
            })
            .collect(),
    };
    sum_in(std::iter::once(&scaled), false)
}

/// A total `base` (ml or g) of `dim` in the unit people would use.
fn best(dim: Dim, base: f64, metric: bool) -> Part {
    let pick = |name: &str| {
        let u = builtin(name).expect("a built-in unit");
        Part {
            amount: base / u.base,
            unit: u.name.to_owned(),
        }
    };
    match (dim, metric) {
        (Dim::Volume, true) => pick(if base >= 1000.0 { "l" } else { "ml" }),
        (Dim::Weight, true) => pick(if base >= 1000.0 { "kg" } else { "g" }),
        (Dim::Weight, false) => {
            let lb = pick("lb");
            if lb.amount >= 1.0 && nice(lb.amount).is_some() {
                lb
            } else {
                pick("oz")
            }
        }
        _ => {
            // US volume: the largest of cup (from 1/4), tbsp (from 1) and tsp
            // whose amount is a plain fraction; else the largest that fits.
            let options = [("cup", 0.25), ("tbsp", 1.0), ("tsp", 0.0)];
            let fits: Vec<Part> = options
                .iter()
                .map(|(n, min)| (pick(n), *min))
                .filter(|(p, min)| p.amount + 1e-9 >= *min)
                .map(|(p, _)| p)
                .collect();
            fits.iter()
                .find(|p| nice(p.amount).is_some())
                .or(fits.first())
                .cloned()
                .unwrap_or_else(|| pick("tsp"))
        }
    }
}

/// `x` as a whole number and a plain fraction (halves, thirds, quarters,
/// eighths, sixteenths): `(whole, numerator, denominator)`.
fn nice(x: f64) -> Option<(u64, u64, u64)> {
    if !x.is_finite() || x < 0.0 {
        return None;
    }
    let whole = (x + 1e-9).floor();
    let frac = x - whole;
    if frac < 1e-6 {
        return Some((whole as u64, 0, 1));
    }
    for den in [2u64, 3, 4, 8, 16] {
        let num = (frac * den as f64).round();
        if num >= 1.0 && num < den as f64 && (frac - num / den as f64).abs() < 0.004 {
            let n = num as u64;
            let g = gcd(n, den);
            return Some((whole as u64, n / g, den / g));
        }
        if num >= den as f64 && (frac - 1.0).abs() < 0.004 {
            return Some((whole as u64 + 1, 0, 1));
        }
    }
    None
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// An amount as text: "1 1/2", "3/4", "2"; decimals when it isn't a plain
/// fraction ("0.35"); metric units always as decimals.
fn amount_text(x: f64, decimals: bool) -> String {
    if !decimals && let Some((w, n, d)) = nice(x) {
        return match (w, n) {
            (w, 0) => w.to_string(),
            (0, n) => format!("{n}/{d}"),
            (w, n) => format!("{w} {n}/{d}"),
        };
    }
    let s = format!("{:.2}", tidy(x));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.into() }
}

fn part_text(p: &Part) -> String {
    let metric = builtin(&p.unit).is_some_and(|u| u.metric);
    let a = amount_text(p.amount, metric);
    if p.unit.is_empty() {
        return a;
    }
    let one = p.amount <= 1.0 + 1e-9;
    let unit = if one { p.unit.clone() } else { plural(&p.unit) };
    format!("{a} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Quantity {
        parse(s).unwrap_or_else(|| panic!("{s} parses"))
    }

    fn one(s: &str) -> (f64, String) {
        let q = q(s);
        assert_eq!(q.parts.len(), 1, "{s}");
        (tidy(q.parts[0].amount), q.parts[0].unit.clone())
    }

    #[test]
    fn text_parses_amounts_and_units() {
        assert_eq!(one("1 1/2 cups"), (1.5, "cup".into()));
        assert_eq!(one("2 tbsp"), (2.0, "tbsp".into()));
        assert_eq!(one("2 Tbsp."), (2.0, "tbsp".into()));
        assert_eq!(one("1 T"), (1.0, "tbsp".into()));
        assert_eq!(one("1 t"), (1.0, "tsp".into()));
        assert_eq!(one("3 teaspoons"), (3.0, "tsp".into()));
        assert_eq!(one("3"), (3.0, String::new()));
        assert_eq!(one("1 (14 oz) can"), (1.0, "(14 oz) can".into()));
        assert_eq!(one("1 (14-ounce) cans"), (1.0, "(14-ounce) can".into()));
        assert_eq!(one("a pinch"), (1.0, "pinch".into()));
        assert_eq!(one("½ cup"), (0.5, "cup".into()));
        assert_eq!(one("1½ cups"), (1.5, "cup".into()));
        assert_eq!(one("1-1/2 cups"), (1.5, "cup".into()));
        assert_eq!(one("1 and 1/2 cups"), (1.5, "cup".into()));
        assert_eq!(one("2-3 cloves"), (3.0, "clove".into()));
        assert_eq!(one("2 to 3 cloves"), (3.0, "clove".into()));
        assert_eq!(one("1.25 lbs"), (1.25, "lb".into()));
        assert_eq!(one("8 fl oz"), (8.0, "fl oz".into()));
        assert_eq!(one("2 large"), (2.0, "large".into()));
        assert_eq!(one("3 sprigs"), (3.0, "sprig".into()));
        assert_eq!(one("250 ml"), (250.0, "ml".into()));
        // Several terms of one dimension: one amount.
        assert_eq!(one("1/4 cup plus 2 tbsp"), (0.375, "cup".into()));
        assert_eq!(q("2 cups + 3 cloves").text(), "2 cups + 3 cloves");
        for bad in [
            "to taste",
            "salt",
            "",
            "some",
            "2 cups chopped fine",
            "1/0 cup",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn text_formats_with_fractions_and_plurals() {
        assert_eq!(q("1 1/2 cups").text(), "1 1/2 cups");
        assert_eq!(q("1/2 cup").text(), "1/2 cup");
        assert_eq!(q("1 cup").text(), "1 cup");
        assert_eq!(q("3").text(), "3");
        assert_eq!(q("2 (14 oz) cans").text(), "2 (14 oz) cans");
        assert_eq!(q("2 bunches").text(), "2 bunches");
        assert_eq!(q("2 pinches").text(), "2 pinches");
        assert_eq!(q("0.35 cup").text(), "0.35 cup");
        assert_eq!(q("1.5 l").text(), "1.5 l");
    }

    #[test]
    fn sums_convert_within_a_dimension_and_keep_the_units_written() {
        // One unit stays as written.
        let s = sum([&q("4 T")]);
        assert_eq!(s.text(), "4 tbsp");
        let s = sum([&q("4 tbsp"), &q("2 tbsp")]);
        assert_eq!(s.text(), "6 tbsp");
        let s = sum([&q("48 tsp")]);
        assert_eq!(s.text(), "48 tsp");
        let s = sum([&q("4 cups"), &q("2 1/4 cups")]);
        assert_eq!(s.text(), "6 1/4 cups");
        let s = sum([&q("12 oz"), &q("8 oz")]);
        assert_eq!(s.text(), "20 oz");
        // Several units: the largest, when a plain fraction to eighths.
        let s = sum([&q("1/2 cup"), &q("2 tbsp")]);
        assert_eq!(s.text(), "5/8 cup");
        let s = sum([&q("1 lb"), &q("8 oz")]);
        assert_eq!(s.text(), "1 1/2 lb");
        // Else apart, largest first, whole larger units carried up.
        let s = sum([&q("1 cup"), &q("3 T")]);
        assert_eq!(s.text(), "1 cup + 3 tbsp");
        let s = sum([&q("2 cups"), &q("1 tbsp")]);
        assert_eq!(s.text(), "2 cups + 1 tbsp");
        let s = sum([&q("1/3 cup"), &q("1 tbsp")]);
        assert_eq!(s.text(), "1/3 cup + 1 tbsp");
        let s = sum([&q("1 cup"), &q("19 tbsp")]);
        assert_eq!(s.text(), "2 cups + 3 tbsp");
        let s = sum([&q("1 tbsp"), &q("4 tsp")]);
        assert_eq!(s.text(), "2 tbsp + 1 tsp");
        let s = sum([&q("1 lb"), &q("3 oz")]);
        assert_eq!(s.text(), "1 lb + 3 oz");
        // A sum's parts sum again to the same.
        let s = sum([&s, &q("0 oz")]);
        assert_eq!(s.text(), "1 lb + 3 oz");
        // Metric picks a unit; count-like units stay apart.
        let s = sum([&q("500 g"), &q("750 g")]);
        assert_eq!(s.text(), "1.25 kg");
        let s = sum([&q("2 cups"), &q("3 cloves"), &q("1 clove")]);
        assert_eq!(s.text(), "2 cups + 4 cloves");
        let s = sum([&q("1 can"), &q("1 (14 oz) can")]);
        assert_eq!(s.text(), "1 can + 1 (14 oz) can");
        let s = sum([&q("2"), &q("1")]);
        assert_eq!(s.text(), "3");
        assert_eq!(sum(std::iter::empty()).parts.len(), 0);
    }

    #[test]
    fn scaling_multiplies_and_picks_a_unit() {
        assert_eq!(scale(&q("1/3 cup"), 3.0).text(), "1 cup");
        assert_eq!(scale(&q("1 tbsp"), 0.5).text(), "1 1/2 tsp");
        assert_eq!(scale(&q("2 cloves"), 1.5).text(), "3 cloves");
        assert_eq!(scale(&q("3 tbsp"), 4.0).text(), "3/4 cup");
        assert_eq!(scale(&q("48 tsp"), 1.0).text(), "48 tsp", "unchanged at 1");
    }

    #[test]
    fn values_round_trip_and_coerce() {
        let v = q("1 1/2 cups").to_value();
        assert_eq!(v, json!({"amount": 1.5, "unit": "cup"}));
        assert_eq!(from_value(&v).unwrap().text(), "1 1/2 cups");
        assert_eq!(
            coerce(&json!("to taste"), 99),
            Ok(Some(json!("to taste"))),
            "text that doesn't parse is kept"
        );
        assert_eq!(
            coerce(&json!(3), 99),
            Ok(Some(json!({"amount": 3, "unit": ""})))
        );
        assert_eq!(
            coerce(&json!({"amount": "2", "unit": "Cups"}), 99),
            Ok(Some(json!({"amount": 2, "unit": "cup"})))
        );
        assert!(coerce(&json!({"amount": -1, "unit": "cup"}), 99).is_err());
        assert!(coerce(&json!(true), 99).is_err());
        assert_eq!(coerce(&json!(" "), 99), Ok(None));
        let two = q("2 cups + 3 cloves").to_value();
        assert!(two.is_array());
        assert_eq!(text_of(&two), "2 cups + 3 cloves");
        assert_eq!(text_of(&json!("to taste")), "to taste");
        assert!(is_unparsed(&json!("to taste")) && !is_unparsed(&v));
    }
}
