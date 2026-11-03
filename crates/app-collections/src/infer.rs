//! Inferred field values (decision `collection-inferred-fields`): a choice
//! field with `infer: {from, hint}` is filled in by the pod from another
//! field's text, e.g. a grocery item's store section from its name.
//!
//! A write that leaves such a field empty gets the cached answer for the
//! source text at once (`collections_inferred`, keyed by the normalized
//! text); without one, the row is stored with the field's default and a
//! background task asks the [`Classifier`], stores the answer and updates
//! the row. A write that sets the field by hand replaces the cached answer,
//! so the next row with that text gets the person's choice. Without a
//! classifier configured the field just keeps its default (cached answers
//! still apply).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use pod_core::{BoxFuture, Pod};
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::db::{self, Row};
use crate::model::{self, Definition, Field};
use crate::{CollectionsAdapter, NAME};

/// Picks one of `options` for `text` ("oat milk" → "Dairy"), `hint` saying
/// what the options are ("grocery store section"). `None`: no answer (an
/// error, or nothing fits); the field keeps its default.
pub trait Classifier: Send + Sync + std::fmt::Debug {
    fn classify<'a>(
        &'a self,
        text: &'a str,
        hint: &'a str,
        options: &'a [String],
    ) -> BoxFuture<'a, Option<String>>;
}

/// The env var with the model API key; unset: no classifier.
pub const API_KEY_VAR: &str = "CONSTRUCT_INFER_API_KEY";
/// The env var with the model to ask; unset: no classifier.
pub const MODEL_VAR: &str = "CONSTRUCT_INFER_MODEL";
/// The env var with the Messages API URL.
pub const URL_VAR: &str = "CONSTRUCT_INFER_URL";
/// [`URL_VAR`] when unset.
pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";

/// A classifier that asks a model over the Anthropic Messages API.
pub struct ModelClassifier {
    client: reqwest::Client,
    url: String,
    key: String,
    model: String,
}

impl std::fmt::Debug for ModelClassifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelClassifier")
            .field("url", &self.url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl ModelClassifier {
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
    pub fn from_env() -> Option<Arc<dyn Classifier>> {
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

impl Classifier for ModelClassifier {
    fn classify<'a>(
        &'a self,
        text: &'a str,
        hint: &'a str,
        options: &'a [String],
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let prompt = format!(
                "Classify the item by its {hint}. Answer with exactly one of these options and \
                 nothing else:\n{}\n\nItem: {text}",
                options.join("\n")
            );
            let body = json!({
                "model": self.model,
                "max_tokens": 32,
                "messages": [{ "role": "user", "content": prompt }],
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
                    tracing::warn!(status = %r.status(), "inferring a collection field failed");
                    return None;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "inferring a collection field failed");
                    return None;
                }
            };
            let v: Value = resp.json().await.ok()?;
            let answer = v["content"]
                .as_array()?
                .iter()
                .find_map(|c| c["text"].as_str())?
                .trim()
                .trim_matches(|c: char| c == '"' || c == '.' || c == '\'')
                .to_owned();
            Some(answer)
        })
    }
}

/// One row field to infer in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub row_id: String,
    pub field_id: String,
    /// The source text, and its [`model::infer_key`].
    pub text: String,
    pub key: String,
}

/// The source text of `field`'s `infer` in `values`, if any.
fn source_text(def: &Definition, field: &Field, values: &Map<String, Value>) -> Option<String> {
    let from = &field.infer.as_ref()?.from;
    def.by_id(from)?;
    values
        .get(from)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A cached answer that is still one of `field`'s options.
fn cached(
    conn: &Connection,
    def: &Definition,
    field: &Field,
    key: &str,
) -> rusqlite::Result<Option<Value>> {
    let Some(v) = db::inferred(conn, &def.name, &field.id, key)? else {
        return Ok(None);
    };
    Ok(model::coerce(field, &json!(v), model::MAX_OPTION_CHARS)
        .ok()
        .flatten())
}

/// After a write to `row` (its values `before` the write, `None` for a
/// create; `write`: the names it set), inside the write's transaction:
/// learn from a hand-set inferred field, fill empty ones from the cache,
/// and answer what's left for [`spawn`]. An inferred field is (re)inferred
/// when it is empty after a create, or when an update changes its source
/// text without setting the field (its old answer is dropped).
pub fn after_write(
    conn: &Connection,
    def: &Definition,
    before: Option<&Map<String, Value>>,
    row: &mut Row,
    write: &Map<String, Value>,
    now: i64,
) -> rusqlite::Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for field in def.live_fields().filter(|f| f.infer.is_some()) {
        let set_here = write.iter().any(|(n, v)| {
            def.field(n).is_some_and(|f| f.id == field.id)
                && !(v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()))
        });
        let text = source_text(def, field, &row.values);
        if set_here {
            // A person's choice: the answer for this text from now on.
            if let (Some(t), Some(v)) = (&text, row.values.get(&field.id).and_then(Value::as_str)) {
                db::remember_inferred(
                    conn,
                    &def.name,
                    &field.id,
                    &model::infer_key(t),
                    v,
                    db::SOURCE_EDIT,
                    now,
                )?;
            }
            continue;
        }
        let needed = match before {
            None => !row.values.contains_key(&field.id),
            Some(b) => {
                let old = source_text(def, field, b);
                old.as_deref().map(model::infer_key) != text.as_deref().map(model::infer_key)
            }
        };
        if !needed {
            continue;
        }
        row.values.remove(&field.id);
        let Some(text) = text else { continue };
        let key = model::infer_key(&text);
        match cached(conn, def, field, &key)? {
            Some(v) => {
                row.values.insert(field.id.clone(), v);
            }
            None => jobs.push(Job {
                row_id: row.id.clone(),
                field_id: field.id.clone(),
                text,
                key,
            }),
        }
    }
    Ok(jobs)
}

/// Jobs for every live row whose field `field_id` is empty (a field that
/// just got `infer`), filling from the cache inside `tx`; at most `limit`
/// rows are queued for the classifier.
pub fn backfill(
    tx: &Connection,
    def: &Definition,
    field_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<Job>> {
    let Some(field) = def.by_id(field_id).filter(|f| f.infer.is_some()) else {
        return Ok(Vec::new());
    };
    let mut jobs = Vec::new();
    for mut r in db::all_rows(tx, &def.name, false)? {
        if r.values.contains_key(&field.id) {
            continue;
        }
        let Some(text) = source_text(def, field, &r.values) else {
            continue;
        };
        let key = model::infer_key(&text);
        match cached(tx, def, field, &key)? {
            Some(v) => {
                r.values.insert(field.id.clone(), v);
                db::save_row_values(tx, &def.name, &r)?;
            }
            None if jobs.len() < limit => jobs.push(Job {
                row_id: r.id.clone(),
                field_id: field.id.clone(),
                text,
                key,
            }),
            None => {}
        }
    }
    Ok(jobs)
}

/// Classify `jobs` of `collection` in the background (each distinct text
/// once), store the answers and update the rows that still need them.
/// Without a classifier, nothing happens.
pub fn spawn(pod: &Pod, collection: &str, jobs: Vec<Job>) {
    if jobs.is_empty() {
        return;
    }
    let Some(classifier) = pod
        .adapter::<CollectionsAdapter>()
        .and_then(|a| a.classifier.clone())
    else {
        return;
    };
    let weak = Arc::downgrade(pod);
    let collection = collection.to_owned();
    tokio::spawn(async move {
        // (field id, key) → (text, rows), in a stable order.
        let mut groups: BTreeMap<(String, String), (String, Vec<String>)> = BTreeMap::new();
        for j in jobs {
            groups
                .entry((j.field_id, j.key))
                .or_insert_with(|| (j.text, Vec::new()))
                .1
                .push(j.row_id);
        }
        for ((field_id, key), (text, row_ids)) in groups {
            let Some(pod) = weak.upgrade() else { return };
            let name = collection.clone();
            let Ok(Some((_, def))) = pod
                .blocking(move |s| s.with_conn(|c| db::latest_def(c, &name)))
                .await
            else {
                return;
            };
            let Some(field) = def.by_id(&field_id).filter(|f| f.live()).cloned() else {
                continue;
            };
            let Some(infer) = field.infer.clone() else {
                continue;
            };
            drop(pod);
            let answer = classifier
                .classify(&text, &infer.hint, &field.options)
                .await;
            let Some(value) = answer
                .and_then(|a| model::coerce(&field, &json!(a), model::MAX_OPTION_CHARS).ok())
                .flatten()
            else {
                tracing::info!(collection, field = field.name, "no inferred value");
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
                        db::remember_inferred(
                            tx,
                            &name,
                            &field.id,
                            &key,
                            value.as_str().unwrap_or(""),
                            db::SOURCE_MODEL,
                            now,
                        )?;
                        // The cache may hold a person's answer now (it wins).
                        let value = cached(tx, &def, field, &key)?.unwrap_or(value);
                        let mut n = 0;
                        let mut seen = HashSet::new();
                        for id in row_ids {
                            if !seen.insert(id.clone()) {
                                continue;
                            }
                            let Some(mut r) = db::row(tx, &name, &id)? else {
                                continue;
                            };
                            // Changed meanwhile: set by hand, or another text.
                            if r.deleted_at.is_some()
                                || r.values.contains_key(&field.id)
                                || source_text(&def, field, &r.values).map(|t| model::infer_key(&t))
                                    != Some(key.clone())
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
                Err(e) => tracing::warn!(?e, "storing an inferred value failed"),
            }
        }
    });
}
