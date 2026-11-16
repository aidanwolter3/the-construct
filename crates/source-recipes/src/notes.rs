//! The `recipes_notes` MCP tool (decision `recipe-notes`): Claude lists,
//! adds, changes and removes a recipe's notes (`recipes_notes`), which the
//! recipe screen shows under "Notes". The app has no note editor.

use pod_core::Pod;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::NAME;

/// The longest note, in characters.
pub const MAX_NOTE_CHARS: usize = 2000;

/// The tool's definition.
pub fn tool_def() -> Value {
    json!({
        "name": "recipes_notes",
        "description": "Read or change a recipe's notes, which its screen shows under Notes (cooking tips, \
    substitutions, how it went). recipe: the recipe's id (its slug, the id field of the recipes all feed), its \
    title, or part of one title (several matches answer them, to pick one's id). op list answers the recipe and its notes, numbered from 1. op add {recipe, text} adds a note at the \
    end. op update {recipe, n, text} rewrites note n. op remove {recipe, n} removes note n (the later ones move \
    up). Keep the user's own words; one note per thought.",
        "inputSchema": {"type": "object", "properties": {
            "op": {"type": "string", "enum": ["list", "add", "update", "remove"]},
            "recipe": {"type": "string", "description": "The recipe's id (slug), title or part of its title"},
            "n": {"type": "integer", "description": "The note's number (update, remove), from 1"},
            "text": {"type": "string", "description": "The note (add, update)"}},
            "required": ["op", "recipe"]},
    })
}

fn bad(detail: impl Into<String>) -> Value {
    json!({"error": "invalid_request", "detail": detail.into()})
}

/// What the tool answers on a pod-side failure.
fn unavailable() -> Value {
    json!({"error": "unavailable", "detail": "the recipes can't be read or written"})
}

/// Why a write was refused (inside the transaction).
enum Refused {
    NotFound(String),
    Ambiguous(String, Vec<(String, String)>),
    NoNote(usize, usize),
}

/// Run the tool.
pub async fn tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let op = args["op"].as_str().unwrap_or("").to_owned();
    if !matches!(op.as_str(), "list" | "add" | "update" | "remove") {
        return Err(bad("\"op\" is list, add, update or remove"));
    }
    let recipe = args["recipe"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("\"recipe\" is the recipe's id or title"))?
        .to_owned();
    let n = match args.get("n") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n >= 1)
                .ok_or_else(|| bad("\"n\" is a note's number, from 1"))? as usize,
        ),
    };
    if matches!(op.as_str(), "update" | "remove") && n.is_none() {
        return Err(bad(format!("op {op} needs \"n\", the note's number")));
    }
    let text = args.get("text").and_then(Value::as_str).map(str::trim);
    let text = if matches!(op.as_str(), "add" | "update") {
        let t = text
            .filter(|t| !t.is_empty())
            .ok_or_else(|| bad(format!("op {op} needs \"text\", the note")))?;
        if t.chars().count() > MAX_NOTE_CHARS {
            return Err(bad(format!(
                "a note is at most {MAX_NOTE_CHARS} characters"
            )));
        }
        Some(t.to_owned())
    } else {
        None
    };

    let write = op != "list";
    let result = pod
        .blocking(move |s| {
            s.transaction(|tx| {
                let (id, slug, title) = match find_recipe(tx, &recipe)? {
                    Found::One(id, slug, title) => (id, slug, title),
                    Found::None => return Ok(Err(Refused::NotFound(recipe))),
                    Found::Several(hits) => return Ok(Err(Refused::Ambiguous(recipe, hits))),
                };
                let notes = notes(tx, id)?;
                match (op.as_str(), n) {
                    ("add", _) => {
                        let mut all = notes;
                        all.push(text.unwrap_or_default());
                        write_notes(tx, id, &all)?;
                    }
                    ("update", Some(n)) => {
                        let mut all = notes;
                        let Some(note) = all.get_mut(n - 1) else {
                            return Ok(Err(Refused::NoNote(n, all.len())));
                        };
                        *note = text.unwrap_or_default();
                        write_notes(tx, id, &all)?;
                    }
                    ("remove", Some(n)) => {
                        let mut all = notes;
                        if n > all.len() {
                            return Ok(Err(Refused::NoNote(n, all.len())));
                        }
                        all.remove(n - 1);
                        write_notes(tx, id, &all)?;
                    }
                    _ => {}
                }
                Ok(Ok((slug, title, self::notes(tx, id)?)))
            })
        })
        .await
        .map_err(|_| unavailable())?;
    let (slug, title, notes) = match result {
        Ok(v) => v,
        Err(Refused::NotFound(r)) => {
            return Err(json!({"error": "recipe_not_found", "recipe": r,
                "detail": "no recipe has that id or title; the recipes all feed lists them"}));
        }
        Err(Refused::Ambiguous(r, hits)) => {
            let hits: Vec<Value> = hits
                .into_iter()
                .map(|(id, title)| json!({"id": id, "title": title}))
                .collect();
            return Err(
                json!({"error": "ambiguous_recipe", "recipe": r, "matches": hits,
                "detail": "several recipes' titles hold that; pass one's id"}),
            );
        }
        Err(Refused::NoNote(n, count)) => {
            return Err(json!({"error": "note_not_found",
                "detail": format!("the recipe has {count} notes; there is no note {n}")}));
        }
    };
    if write {
        pod.publish_data_all(NAME);
    }
    let notes: Vec<Value> = notes
        .into_iter()
        .enumerate()
        .map(|(i, text)| json!({"n": i + 1, "text": text}))
        .collect();
    Ok(json!({"ok": true, "recipe": {"id": slug, "title": title}, "notes": notes}))
}

/// What a `recipe` argument names.
enum Found {
    One(i64, String, String),
    None,
    /// Several titles hold it: their slugs and titles.
    Several(Vec<(String, String)>),
}

/// The recipe a slug, a title (any case) or one recipe's part of a title
/// names.
fn find_recipe(conn: &Connection, key: &str) -> rusqlite::Result<Found> {
    let row = |r: &rusqlite::Row<'_>| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
    for sql in [
        "SELECT id, slug, title FROM recipes WHERE slug = ?1",
        "SELECT id, slug, title FROM recipes WHERE title = ?1 COLLATE NOCASE ORDER BY id LIMIT 1",
    ] {
        if let Some((id, slug, title)) = conn.query_row(sql, [key], row).optional()? {
            return Ok(Found::One(id, slug, title));
        }
    }
    let pattern = format!(
        "%{}%",
        key.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut stmt = conn.prepare(
        "SELECT id, slug, title FROM recipes WHERE title LIKE ?1 ESCAPE '\\' ORDER BY title LIMIT 11",
    )?;
    let mut hits: Vec<(i64, String, String)> = stmt
        .query_map([pattern], row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(match hits.len() {
        0 => Found::None,
        1 => {
            let (id, slug, title) = hits.remove(0);
            Found::One(id, slug, title)
        }
        _ => Found::Several(hits.into_iter().map(|(_, s, t)| (s, t)).collect()),
    })
}

/// A recipe's notes, in order.
pub fn notes(conn: &Connection, recipe_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT text FROM recipes_notes WHERE recipe_id = ?1 ORDER BY position")?;
    stmt.query_map([recipe_id], |r| r.get(0))?.collect()
}

/// Replace a recipe's notes with `all`, numbered from 0.
fn write_notes(tx: &Transaction<'_>, recipe_id: i64, all: &[String]) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM recipes_notes WHERE recipe_id = ?1",
        [recipe_id],
    )?;
    for (i, text) in all.iter().enumerate() {
        tx.execute(
            "INSERT INTO recipes_notes (recipe_id, position, text) VALUES (?1, ?2, ?3)",
            params![recipe_id, i as i64, text],
        )?;
    }
    Ok(())
}
