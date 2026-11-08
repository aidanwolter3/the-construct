//! `lookup` computes (decision `collection-lookups`): a field's value from
//! the rows of another collection, kept as editable rules (`section_rules`:
//! `pattern` → `section`). The input (the row's `from` field) is matched
//! against each rule's key, and the best match's value is the answer:
//!
//! - `exact`: the whole input, ignoring case and spacing;
//! - `contains`: the key's words appear in the input as whole words, in
//!   order ("milk" matches "Oat milk", not "Buttermilk");
//! - `regex`: the key is a regular expression found in the input (lowercase,
//!   spaces collapsed; case-insensitive). The `regex` crate matches in
//!   linear time; a pattern is at most [`MAX_PATTERN_CHARS`] and compiles
//!   within [`REGEX_SIZE_LIMIT`]. An invalid one is skipped (and reported by
//!   `describe_collection`).
//!
//! When several rules match, the longest key wins ("coconut milk" over
//! "milk"), then the oldest rule. With `learn`, a later kind's answer (the
//! model's) and a hand edit of the field become rules (see [`learn`]).

use regex::{Regex, RegexBuilder};
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::Caps;
use crate::db::{self, Row};
use crate::model::{self, Compute, Definition, Match};

/// Longest `regex` key.
pub const MAX_PATTERN_CHARS: usize = 200;
/// What one compiled `regex` key may take (bytes), so no pattern is costly.
pub const REGEX_SIZE_LIMIT: usize = 64 * 1024;

/// One rule: a row of the lookup's collection.
#[derive(Debug, Clone)]
struct Rule {
    /// The key as normalized text ([`model::cache_key`]).
    norm: String,
    words: Vec<String>,
    regex: Option<Regex>,
    value: Value,
}

/// A rule whose key can't be used (a `regex` that doesn't compile).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub row_id: String,
    pub key: String,
    pub error: String,
}

/// The rules of one lookup, ready to match.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    mode: Match,
    rules: Vec<Rule>,
    pub invalid: Vec<Invalid>,
}

/// The words of `s`: its runs of letters and digits, lowercase.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// A value as a key or an input text (`None`: empty).
pub fn text_of(v: &Value) -> Option<String> {
    let s = match v {
        Value::Null => return None,
        Value::String(s) => s.trim().to_owned(),
        other => other.to_string(),
    };
    (!s.is_empty()).then_some(s)
}

/// A regex key compiled with the limits.
fn compile(key: &str) -> Result<Regex, String> {
    if key.chars().count() > MAX_PATTERN_CHARS {
        return Err(format!(
            "a pattern is at most {MAX_PATTERN_CHARS} characters"
        ));
    }
    RegexBuilder::new(key)
        .case_insensitive(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

impl Rules {
    /// The rules of lookup `compute` (empty for another kind, or when its
    /// collection or fields are gone).
    pub fn load(conn: &Connection, compute: &Compute) -> rusqlite::Result<Rules> {
        let Compute::Lookup {
            collection,
            key,
            value,
            mode,
            ..
        } = compute
        else {
            return Ok(Rules::default());
        };
        let mut out = Rules {
            mode: *mode,
            ..Rules::default()
        };
        let Some((_, other)) = db::latest_def(conn, collection)? else {
            return Ok(out);
        };
        let (Some(kf), Some(vf)) = (
            other.by_id(key).filter(|f| f.live()),
            other.by_id(value).filter(|f| f.live()),
        ) else {
            return Ok(out);
        };
        for r in db::all_rows(conn, collection, false)? {
            let Some(k) = r.values.get(&kf.id).and_then(text_of) else {
                continue;
            };
            let v = r
                .values
                .get(&vf.id)
                .cloned()
                .unwrap_or_else(|| vf.empty_value());
            if text_of(&v).is_none() {
                continue;
            }
            let regex = if *mode == Match::Regex {
                match compile(&k) {
                    Ok(re) => Some(re),
                    Err(error) => {
                        out.invalid.push(Invalid {
                            row_id: r.id.clone(),
                            key: k,
                            error,
                        });
                        continue;
                    }
                }
            } else {
                None
            };
            out.rules.push(Rule {
                norm: model::cache_key(&k),
                words: words(&k),
                regex,
                value: v,
            });
        }
        Ok(out)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The value of the best rule matching `input`: the longest key, then
    /// the oldest rule.
    pub fn find(&self, input: &str) -> Option<&Value> {
        let norm = model::cache_key(input);
        if norm.is_empty() {
            return None;
        }
        let input_words = words(input);
        let mut best: Option<&Rule> = None;
        for r in &self.rules {
            let hit = match self.mode {
                Match::Exact => r.norm == norm,
                Match::Contains => {
                    !r.words.is_empty()
                        && input_words
                            .windows(r.words.len())
                            .any(|w| w == r.words.as_slice())
                }
                Match::Regex => r.regex.as_ref().is_some_and(|re| re.is_match(&norm)),
            };
            if hit && best.is_none_or(|b| r.norm.chars().count() > b.norm.chars().count()) {
                best = Some(r);
            }
        }
        best.map(|r| &r.value)
    }
}

/// One rule key ready to match inputs under a mode, as [`Rules::find`]
/// matches (an invalid `regex` key matches nothing).
#[derive(Debug, Clone)]
pub struct KeyMatcher {
    mode: Match,
    norm: String,
    words: Vec<String>,
    regex: Option<Regex>,
}

impl KeyMatcher {
    pub fn new(mode: Match, key: &str) -> Self {
        Self {
            mode,
            norm: model::cache_key(key),
            words: words(key),
            regex: (mode == Match::Regex).then(|| compile(key).ok()).flatten(),
        }
    }

    /// Whether the key matches `input`.
    pub fn matches(&self, input: &str) -> bool {
        let norm = model::cache_key(input);
        if norm.is_empty() {
            return false;
        }
        match self.mode {
            Match::Exact => self.norm == norm,
            Match::Contains => {
                !self.words.is_empty()
                    && words(input)
                        .windows(self.words.len())
                        .any(|w| w == self.words.as_slice())
            }
            Match::Regex => self.regex.as_ref().is_some_and(|re| re.is_match(&norm)),
        }
    }
}

/// Whether a rule with key `key` matches `input` under `mode`.
pub fn key_matches(mode: Match, key: &str, input: &str) -> bool {
    KeyMatcher::new(mode, key).matches(input)
}

/// The key a learned rule gets for `input`: the normalized input; for a
/// `regex` lookup, that text escaped and anchored (`^oat milk$`), so it
/// matches only that input, as an `exact` rule would.
pub fn learned_key(mode: Match, input: &str) -> String {
    let norm = model::cache_key(input);
    match mode {
        Match::Regex => format!("^{}$", regex::escape(&norm)),
        Match::Exact | Match::Contains => norm,
    }
}

/// Teach lookup `compute` that `input` answers `value` (a value of the
/// computed field), inside the write: a rule (key: [`learned_key`], value)
/// is added to its collection, unless its rules already answer that. A rule
/// with that key already there is changed only when `overwrite` (a hand
/// edit; a model never replaces a rule). Within the caps, else nothing is
/// learned. Answers whether a rule was added or changed.
pub fn learn(
    conn: &Connection,
    compute: &Compute,
    input: &str,
    value: &Value,
    overwrite: bool,
    now: i64,
    caps: Caps,
) -> rusqlite::Result<bool> {
    let Compute::Lookup {
        collection,
        key,
        value: value_id,
        mode,
        ..
    } = compute
    else {
        return Ok(false);
    };
    let Some((_, other)) = db::latest_def(conn, collection)? else {
        return Ok(false);
    };
    let (Some(kf), Some(vf)) = (
        other.by_id(key).filter(|f| f.live()).cloned(),
        other.by_id(value_id).filter(|f| f.live()).cloned(),
    ) else {
        return Ok(false);
    };
    let Some(v) = model::coerce(&vf, value, caps.text_chars).ok().flatten() else {
        return Ok(false);
    };
    let k = learned_key(*mode, input);
    if k.is_empty() || k.chars().count() > caps.text_chars {
        return Ok(false);
    }
    let same_key = |r: &Row| {
        r.values
            .get(&kf.id)
            .and_then(text_of)
            .is_some_and(|t| model::cache_key(&t) == model::cache_key(&k))
    };
    let rows = db::all_rows(conn, collection, false)?;
    if let Some(mut r) = rows.into_iter().find(same_key) {
        if !overwrite || r.values.get(&vf.id) == Some(&v) {
            return Ok(false);
        }
        r.values.insert(vf.id.clone(), v);
        r.updated_at = now;
        db::save_row(conn, collection, &r)?;
        return Ok(true);
    }
    if Rules::load(conn, compute)?.find(input) == Some(&v) {
        return Ok(false);
    }
    let mut values = Map::new();
    values.insert(kf.id.clone(), json!(k));
    values.insert(vf.id.clone(), v);
    let bytes = Value::Object(values.clone()).to_string().len() as i64;
    if crate::check_usage(conn, caps, 1, bytes)?.is_err() {
        tracing::info!(collection, "a lookup rule was not learned: the pod is full");
        return Ok(false);
    }
    db::insert_row(
        conn,
        collection,
        &Row {
            id: db::new_id("r"),
            client_id: None,
            values,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            filler: None,
        },
    )?;
    Ok(true)
}

/// For `describe_collection`: each lookup of `def` with its collection, its
/// number of usable rules and the rules it skips (`{id, key, error}`).
pub fn report(conn: &Connection, def: &Definition) -> rusqlite::Result<Vec<Value>> {
    let mut out = Vec::new();
    for f in def.live_fields() {
        for c in &f.compute {
            let Compute::Lookup { collection, .. } = c else {
                continue;
            };
            let rules = Rules::load(conn, c)?;
            let invalid: Vec<Value> = rules
                .invalid
                .iter()
                .map(|i| json!({ "id": i.row_id, "key": i.key, "error": i.error }))
                .collect();
            out.push(json!({ "field": f.name, "collection": collection,
                "rules": rules.len(), "invalid_rules": invalid }));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(mode: Match, pairs: &[(&str, &str)]) -> Rules {
        let mut out = Rules {
            mode,
            ..Rules::default()
        };
        for (k, v) in pairs {
            let regex = (mode == Match::Regex).then(|| compile(k).unwrap());
            out.rules.push(Rule {
                norm: model::cache_key(k),
                words: words(k),
                regex,
                value: json!(v),
            });
        }
        out
    }

    #[test]
    fn contains_matches_whole_words_and_the_longest_key_wins() {
        let r = rules(
            Match::Contains,
            &[
                ("milk", "Dairy"),
                ("coconut milk", "Pantry"),
                ("cucumber", "Produce"),
            ],
        );
        assert_eq!(r.find("Oat Milk"), Some(&json!("Dairy")));
        assert_eq!(r.find("  coconut   MILK (can)"), Some(&json!("Pantry")));
        assert_eq!(r.find("English cucumber"), Some(&json!("Produce")));
        assert_eq!(r.find("Buttermilk"), None, "whole words only");
        assert_eq!(r.find("cucumbers"), None);
        assert_eq!(r.find(""), None);
    }

    #[test]
    fn exact_ignores_case_and_spacing() {
        let r = rules(Match::Exact, &[("oat milk", "Dairy")]);
        assert_eq!(r.find(" Oat  MILK "), Some(&json!("Dairy")));
        assert_eq!(r.find("oat milk 2%"), None);
    }

    #[test]
    fn regex_matches_case_insensitively_and_bad_patterns_are_refused() {
        let r = rules(
            Match::Regex,
            &[
                ("cucumbers?", "Produce"),
                ("^(english )?cucumbers?$", "Deli"),
            ],
        );
        assert_eq!(r.find("English Cucumber"), Some(&json!("Deli")));
        assert_eq!(r.find("pickled cucumbers"), Some(&json!("Produce")));
        assert!(compile("(unclosed").is_err());
        assert!(compile(&"a".repeat(MAX_PATTERN_CHARS + 1)).is_err());
        // Big repetitions exceed the size limit instead of costing time.
        assert!(compile("(\\w{100}){100}").is_err());
    }

    #[test]
    fn key_matches_agrees_with_find() {
        assert!(key_matches(Match::Contains, "milk", "Oat Milk"));
        assert!(!key_matches(Match::Contains, "milk", "Buttermilk"));
        assert!(key_matches(Match::Exact, "oat milk", " OAT  milk "));
        assert!(!key_matches(Match::Exact, "oat milk", "oat milk 2%"));
        assert!(key_matches(Match::Regex, "^cucumbers?$", "Cucumbers"));
        assert!(!key_matches(Match::Regex, "(unclosed", "(unclosed"));
        assert!(!key_matches(Match::Exact, "", ""));
    }

    #[test]
    fn learned_keys_match_only_their_input() {
        assert_eq!(learned_key(Match::Exact, " Oat  Milk "), "oat milk");
        assert_eq!(learned_key(Match::Regex, "Milk (2%)"), "^milk \\(2%\\)$");
        let re = compile(&learned_key(Match::Regex, "Milk (2%)")).unwrap();
        assert!(re.is_match("milk (2%)") && !re.is_match("oat milk (2%)"));
    }
}
