//! Computed fields (decision `collection-computed-fields`): a field with
//! `compute` gets its value from the row's other fields whenever they
//! change, in one of two ways.
//!
//! - `expr`: an expression ([`crate::expr`]) evaluated inside the write,
//!   e.g. a label `entity.name + " (" + entity.quantity + ")"`.
//! - `model`: a model's answer to the field's prompt about the source
//!   fields' values, e.g. a grocery item's store section from its name. The
//!   answer for the same inputs comes from `collections_computed` (keyed by
//!   the normalized source values) inside the write; without one, the row
//!   is stored with the field empty (it reads its default) and a background
//!   task asks the [`FieldModel`], caches the answer and updates the row. A
//!   write that sets the field by hand replaces the cached answer for its
//!   inputs, so the next row with those inputs gets the person's value.
//!   Without a model configured the field keeps its default (cached answers
//!   still apply).
//!
//! A computed field still takes writes: a value set by hand stays until a
//! source field changes (or the write clears the field), and then the field
//! is computed again.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use pod_core::{BoxFuture, Pod};
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::db::{self, Row};
use crate::model::{self, Compute, Definition, Field, Kind};
use crate::{CollectionsAdapter, NAME};

/// What a `model` compute asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question<'a> {
    /// The field's prompt ("Which grocery store section is this item in?").
    pub prompt: &'a str,
    /// The source fields' names and values, in order.
    pub inputs: &'a [(String, String)],
    /// The answer's type, and a choice's options.
    pub kind: Kind,
    pub options: &'a [String],
}

/// Answers a [`Question`] with text (coerced to the field's type after).
/// `None`: no answer (an error); the field keeps its default.
pub trait FieldModel: Send + Sync + std::fmt::Debug {
    fn answer<'a>(&'a self, q: Question<'a>) -> BoxFuture<'a, Option<String>>;
}

/// The env var with the model API key; unset: no model.
pub const API_KEY_VAR: &str = "CONSTRUCT_INFER_API_KEY";
/// The env var with the model to ask; unset: no model.
pub const MODEL_VAR: &str = "CONSTRUCT_INFER_MODEL";
/// The env var with the Messages API URL.
pub const URL_VAR: &str = "CONSTRUCT_INFER_URL";
/// [`URL_VAR`] when unset.
pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";

/// A [`FieldModel`] that asks a model over the Anthropic Messages API.
pub struct MessagesModel {
    client: reqwest::Client,
    url: String,
    key: String,
    model: String,
}

impl std::fmt::Debug for MessagesModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagesModel")
            .field("url", &self.url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl MessagesModel {
    pub fn new(url: String, key: String, model: String) -> Self {
        let client = pod_core::net::builder(Duration::from_secs(20))
            .build()
            .expect("an HTTP client");
        Self {
            client,
            url,
            key,
            model,
        }
    }

    /// From [`API_KEY_VAR`], [`MODEL_VAR`] and [`URL_VAR`]; `None` unless
    /// the key and the model are both set.
    pub fn from_env() -> Option<Arc<dyn FieldModel>> {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let key = var(API_KEY_VAR)?;
        let model = var(MODEL_VAR)?;
        let url = var(URL_VAR).unwrap_or_else(|| DEFAULT_URL.to_owned());
        Some(Arc::new(Self::new(url, key, model)))
    }
}

/// The prompt a [`Question`] sends.
pub fn prompt_text(q: &Question<'_>) -> String {
    let inputs: Vec<String> = q.inputs.iter().map(|(n, v)| format!("{n}: {v}")).collect();
    let answer = match q.kind {
        Kind::Choice => format!(
            "exactly one of these options and nothing else:\n{}",
            q.options.join("\n")
        ),
        Kind::Number => "a number and nothing else".into(),
        Kind::Bool => "true or false and nothing else".into(),
        Kind::Date => "a date as YYYY-MM-DD and nothing else".into(),
        Kind::Text => "the text only, with no preamble".into(),
    };
    format!(
        "{}\n\n{}\n\nAnswer with {answer}.",
        q.prompt,
        inputs.join("\n")
    )
}

impl FieldModel for MessagesModel {
    fn answer<'a>(&'a self, q: Question<'a>) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let body = json!({
                "model": self.model,
                "max_tokens": if q.kind == Kind::Text { 300 } else { 32 },
                "messages": [{ "role": "user", "content": prompt_text(&q) }],
            });
            let resp = self
                .client
                .post(&self.url)
                .header("x-api-key", &self.key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await;
            let resp = match resp {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    tracing::warn!(status = %r.status(), "computing a collection field failed");
                    return None;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "computing a collection field failed");
                    return None;
                }
            };
            let v: Value = resp.json().await.ok()?;
            let text = v["content"]
                .as_array()?
                .iter()
                .find_map(|c| c["text"].as_str())?;
            Some(text.to_owned())
        })
    }
}

/// A model's answer as a value of `field`: text trimmed; other types also
/// without surrounding quotes or a final period. `None`: it isn't one.
pub fn coerce_answer(field: &Field, answer: &str, max_chars: usize) -> Option<Value> {
    let a = answer.trim();
    let a = if field.kind == Kind::Text {
        a
    } else {
        a.trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == '`')
            .trim()
    };
    model::coerce(field, &json!(a), max_chars).ok().flatten()
}

/// One row field to compute with the model in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub row_id: String,
    pub field_id: String,
    /// The source values (as text, in `from` order) and their cache key.
    pub inputs: Vec<String>,
    pub key: String,
}

/// A value as text for a model or a cache key (`None`: empty).
fn as_text(v: &Value) -> Option<String> {
    let s = match v {
        Value::Null => return None,
        Value::String(s) => s.trim().to_owned(),
        other => other.to_string(),
    };
    (!s.is_empty()).then_some(s)
}

/// A field's value in `values` (empty: its default).
fn value_of(def: &Definition, id: &str, values: &Map<String, Value>) -> Value {
    values
        .get(id)
        .cloned()
        .or_else(|| def.by_id(id).and_then(|f| f.default.clone()))
        .unwrap_or(Value::Null)
}

/// A `model` compute's inputs in `values` and their cache key; `None` when
/// every source is empty (nothing to ask about).
fn inputs(
    def: &Definition,
    from: &[String],
    values: &Map<String, Value>,
) -> Option<(Vec<String>, String)> {
    let texts: Vec<String> = from
        .iter()
        .map(|id| as_text(&value_of(def, id, values)).unwrap_or_default())
        .collect();
    if texts.iter().all(String::is_empty) {
        return None;
    }
    let key = texts
        .iter()
        .map(|t| model::cache_key(t))
        .collect::<Vec<_>>()
        .join("\u{1f}");
    Some((texts, key))
}

/// What the sources of `compute` read in `values`, to tell whether they
/// changed.
fn source_state(def: &Definition, compute: &Compute, values: &Map<String, Value>) -> Vec<Value> {
    compute
        .sources()
        .iter()
        .map(|id| value_of(def, id, values))
        .collect()
}

/// A cached answer that still fits `field`.
fn cached(
    conn: &Connection,
    def: &Definition,
    field: &Field,
    key: &str,
    max_chars: usize,
) -> rusqlite::Result<Option<Value>> {
    let Some(v) = db::computed(conn, &def.name, &field.id, key)? else {
        return Ok(None);
    };
    Ok(model::coerce(field, &v, max_chars).ok().flatten())
}

/// Evaluate an `expr` compute of `field` over `values`: its value, or
/// `None` (empty) when it has none or it doesn't fit the type.
pub fn eval_expr(
    def: &Definition,
    field: &Field,
    values: &Map<String, Value>,
    max_chars: usize,
) -> Option<Value> {
    let e = field.compute.as_ref()?.parsed()?;
    let v = crate::expr::eval(&e, &|id| value_of(def, id, values)).ok()?;
    model::coerce(field, &v, max_chars).ok().flatten()
}

/// Compute `field` of `row` now: an expression is evaluated; a model's
/// answer comes from the cache, else a [`Job`] is answered (the field is
/// left empty until then).
fn recompute(
    conn: &Connection,
    def: &Definition,
    field: &Field,
    row: &mut Row,
    max_chars: usize,
) -> rusqlite::Result<Option<Job>> {
    row.values.remove(&field.id);
    match &field.compute {
        Some(Compute::Expr { .. }) => {
            if let Some(v) = eval_expr(def, field, &row.values, max_chars) {
                row.values.insert(field.id.clone(), v);
            }
            Ok(None)
        }
        Some(Compute::Model { from, .. }) => {
            let Some((texts, key)) = inputs(def, from, &row.values) else {
                return Ok(None);
            };
            match cached(conn, def, field, &key, max_chars)? {
                Some(v) => {
                    row.values.insert(field.id.clone(), v);
                    Ok(None)
                }
                None => Ok(Some(Job {
                    row_id: row.id.clone(),
                    field_id: field.id.clone(),
                    inputs: texts,
                    key,
                })),
            }
        }
        None => Ok(None),
    }
}

/// After a write to `row` (its values `before` the write, `None` for a
/// create; `write`: the names it set), inside the write's transaction. A
/// computed field is computed again when a create leaves it empty, when the
/// write clears it, or when a source field changed and the write doesn't
/// set it; a model field set by hand teaches the cache. Answers the jobs
/// for [`spawn`].
pub fn after_write(
    conn: &Connection,
    def: &Definition,
    before: Option<&Map<String, Value>>,
    row: &mut Row,
    write: &Map<String, Value>,
    now: i64,
    max_chars: usize,
) -> rusqlite::Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for field in def.live_fields() {
        let Some(compute) = &field.compute else {
            continue;
        };
        let written = write
            .iter()
            .find(|(n, _)| def.field(n).is_some_and(|f| f.id == field.id))
            .map(|(_, v)| v);
        let cleared =
            written.is_some_and(|v| v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()));
        if written.is_some() && !cleared {
            // A value set by hand: kept, and (model) the answer for these
            // inputs from now on.
            if let (Compute::Model { from, .. }, Some(v)) = (compute, row.values.get(&field.id))
                && let Some((_, key)) = inputs(def, from, &row.values)
            {
                db::remember_computed(conn, &def.name, &field.id, &key, v, db::SOURCE_EDIT, now)?;
            }
            continue;
        }
        let needed = cleared
            || match before {
                None => true,
                Some(b) => source_state(def, compute, b) != source_state(def, compute, &row.values),
            };
        if needed && let Some(job) = recompute(conn, def, field, row, max_chars)? {
            jobs.push(job);
        }
    }
    Ok(jobs)
}

/// After a change that adds or changes field `field_id`'s compute, inside
/// `tx`: an expression is evaluated for every live row; a model fills the
/// rows where the field is empty (from the cache, else at most `limit` jobs
/// for [`spawn`]): a value already there may be a person's.
pub fn backfill(
    tx: &Connection,
    def: &Definition,
    field_id: &str,
    limit: usize,
    max_chars: usize,
) -> rusqlite::Result<Vec<Job>> {
    let Some(field) = def
        .by_id(field_id)
        .filter(|f| f.live() && f.compute.is_some())
    else {
        return Ok(Vec::new());
    };
    let is_expr = matches!(field.compute, Some(Compute::Expr { .. }));
    let mut jobs = Vec::new();
    for mut r in db::all_rows(tx, &def.name, false)? {
        if !is_expr && r.values.contains_key(&field.id) {
            continue;
        }
        let old = r.values.get(&field.id).cloned();
        match recompute(tx, def, field, &mut r, max_chars)? {
            Some(job) if jobs.len() < limit => jobs.push(job),
            _ => {}
        }
        if r.values.get(&field.id) != old.as_ref() {
            db::save_row_values(tx, &def.name, &r)?;
        }
    }
    Ok(jobs)
}

/// Ask the model for `jobs` of `collection` in the background (each
/// distinct input once), cache the answers and update the rows that still
/// need them. Without a model, nothing happens.
pub fn spawn(pod: &Pod, collection: &str, jobs: Vec<Job>) {
    if jobs.is_empty() {
        return;
    }
    let Some(adapter) = pod.adapter::<CollectionsAdapter>() else {
        return;
    };
    let Some(field_model) = adapter.model.clone() else {
        return;
    };
    let max_chars = adapter.caps().text_chars;
    let weak = Arc::downgrade(pod);
    let collection = collection.to_owned();
    tokio::spawn(async move {
        // (field id, key) → (inputs, rows), in a stable order.
        let mut groups: BTreeMap<(String, String), (Vec<String>, Vec<String>)> = BTreeMap::new();
        for j in jobs {
            groups
                .entry((j.field_id, j.key))
                .or_insert_with(|| (j.inputs, Vec::new()))
                .1
                .push(j.row_id);
        }
        for ((field_id, key), (texts, row_ids)) in groups {
            let Some(pod) = weak.upgrade() else { return };
            let name = collection.clone();
            let Ok(Some((_, def))) = pod
                .blocking(move |s| s.with_conn(|c| db::latest_def(c, &name)))
                .await
            else {
                return;
            };
            drop(pod);
            let Some(field) = def.by_id(&field_id).filter(|f| f.live()).cloned() else {
                continue;
            };
            let Some(Compute::Model { from, prompt }) = field.compute.clone() else {
                continue;
            };
            if from.len() != texts.len() {
                continue;
            }
            let named: Vec<(String, String)> = from
                .iter()
                .map(|id| def.name_of(id))
                .zip(texts.iter().cloned())
                .collect();
            let answer = field_model
                .answer(Question {
                    prompt: &prompt,
                    inputs: &named,
                    kind: field.kind,
                    options: &field.options,
                })
                .await;
            let Some(value) = answer.and_then(|a| coerce_answer(&field, &a, max_chars)) else {
                tracing::info!(collection, field = field.name, "no computed value");
                continue;
            };
            let Some(pod) = weak.upgrade() else { return };
            let name = collection.clone();
            let updated = pod
                .blocking(move |s| {
                    s.transaction(|tx| {
                        let now = pod_core::util::now_secs();
                        let Some((_, def)) = db::latest_def(tx, &name)? else {
                            return Ok(0);
                        };
                        let Some(field) = def.by_id(&field_id).filter(|f| f.live()) else {
                            return Ok(0);
                        };
                        let Some(Compute::Model { from, .. }) = &field.compute else {
                            return Ok(0);
                        };
                        db::remember_computed(
                            tx,
                            &name,
                            &field.id,
                            &key,
                            &value,
                            db::SOURCE_MODEL,
                            now,
                        )?;
                        // The cache may hold a person's answer now (it wins).
                        let value = cached(tx, &def, field, &key, max_chars)?.unwrap_or(value);
                        let mut n = 0;
                        let mut seen = HashSet::new();
                        for id in row_ids {
                            if !seen.insert(id.clone()) {
                                continue;
                            }
                            let Some(mut r) = db::row(tx, &name, &id)? else {
                                continue;
                            };
                            // Changed meanwhile: set by hand, or other inputs.
                            if r.deleted_at.is_some()
                                || r.values.contains_key(&field.id)
                                || inputs(&def, from, &r.values).map(|(_, k)| k).as_deref()
                                    != Some(key.as_str())
                            {
                                continue;
                            }
                            r.values.insert(field.id.clone(), value.clone());
                            r.updated_at = now;
                            db::save_row(tx, &name, &r)?;
                            n += 1;
                        }
                        Ok(n)
                    })
                })
                .await;
            match updated {
                Ok(0) => {}
                Ok(_) => pod.publish_data_all(NAME),
                Err(e) => tracing::warn!(?e, "storing a computed value failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_names_the_inputs_and_the_answer_type() {
        let inputs = vec![("name".to_owned(), "Oat milk".to_owned())];
        let options = vec!["Produce".to_owned(), "Dairy".to_owned()];
        let q = Question {
            prompt: "Which grocery store section is this item in?",
            inputs: &inputs,
            kind: Kind::Choice,
            options: &options,
        };
        let p = prompt_text(&q);
        assert!(p.starts_with("Which grocery store section"), "{p}");
        assert!(
            p.contains("name: Oat milk") && p.contains("Produce\nDairy"),
            "{p}"
        );
        let text = Question {
            kind: Kind::Text,
            ..q.clone()
        };
        assert!(prompt_text(&text).contains("the text only"));
    }

    #[test]
    fn answers_are_coerced_to_the_field_type() {
        let choice = model::parse_field(
            &json!({"name": "s", "type": "choice", "options": ["Produce", "Dairy"]}),
            "f1".into(),
            99,
        )
        .unwrap();
        assert_eq!(
            coerce_answer(&choice, " \"dairy.\" ", 99),
            Some(json!("Dairy"))
        );
        assert_eq!(coerce_answer(&choice, "Hardware", 99), None);
        let text =
            model::parse_field(&json!({"name": "t", "type": "text"}), "f2".into(), 99).unwrap();
        assert_eq!(
            coerce_answer(&text, "  Two cups, sifted.\n", 99),
            Some(json!("Two cups, sifted."))
        );
        let n =
            model::parse_field(&json!({"name": "n", "type": "number"}), "f3".into(), 99).unwrap();
        assert_eq!(coerce_answer(&n, "3.", 99), Some(json!(3)));
    }
}
