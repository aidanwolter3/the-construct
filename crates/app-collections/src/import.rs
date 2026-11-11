//! Row edits read from a file (`edit_rows` `url`, `POST
//! /v1/collections/<c>/import`): an agent that already holds the data in a
//! file hands the pod the file instead of retyping every edit through the
//! model. Each edit keeps where it came from (`at`), so a preview names
//! the line of each invalid one.
//!
//! - **json**: an array; an item with `op` is an edit as `edit_rows` takes
//!   it, any other object a create of its `{field: value}`.
//! - **jsonl**: one such item per line (blank lines skipped).
//! - **csv**: a header of field names, then one row per record (RFC 4180:
//!   quoted cells may hold commas, quotes as `""` and line breaks). An
//!   empty cell leaves the field out. Optional `op` and `id` columns make
//!   updates, deletes and restores; a column `<ref field>.<unique field>`
//!   names a ref's row by that field (`recipe.name` → `{"name": cell}`).

use serde_json::{Map, Value, json};

/// The largest file an import reads.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

/// A file's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Jsonl,
    Csv,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s.trim().to_ascii_lowercase().as_str() {
            "json" => Some(Format::Json),
            "jsonl" | "ndjson" => Some(Format::Jsonl),
            "csv" => Some(Format::Csv),
            _ => None,
        }
    }

    /// From a `Content-Type`, when it says.
    pub fn of_content_type(ct: &str) -> Option<Format> {
        let ct = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match ct.as_str() {
            "application/json" => Some(Format::Json),
            "application/x-ndjson" | "application/jsonl" | "application/x-jsonlines" => {
                Some(Format::Jsonl)
            }
            "text/csv" => Some(Format::Csv),
            _ => None,
        }
    }

    /// From a URL's path, when its extension says.
    pub fn of_url(url: &str) -> Option<Format> {
        let path = url.split(['?', '#']).next().unwrap_or("");
        let ext = path.rsplit_once('.')?.1;
        Format::parse(ext)
    }

    /// From the text itself: `[` starts JSON, `{` JSON lines, else CSV.
    pub fn sniff(text: &str) -> Format {
        match text
            .trim_start_matches('\u{feff}')
            .trim_start()
            .chars()
            .next()
        {
            Some('[') => Format::Json,
            Some('{') => Format::Jsonl,
            _ => Format::Csv,
        }
    }
}

/// One edit and where it came from (`line 4`, `item 3`).
pub type Located = (String, Value);

/// The edits in `bytes`, each with where it came from.
pub fn parse(bytes: &[u8], format: Format) -> Result<Vec<Located>, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("the file is over {} MiB", MAX_BYTES >> 20));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8 text".to_owned())?;
    let text = text.trim_start_matches('\u{feff}');
    match format {
        Format::Json => {
            let v: Value =
                serde_json::from_str(text).map_err(|e| format!("the file is not JSON: {e}"))?;
            let Value::Array(items) = v else {
                return Err("a JSON file holds an array of edits or rows".into());
            };
            items
                .into_iter()
                .enumerate()
                .map(|(i, item)| {
                    edit_of(item)
                        .map(|e| (format!("item {}", i + 1), e))
                        .map_err(|d| format!("item {}: {d}", i + 1))
                })
                .collect()
        }
        Format::Jsonl => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                let at = format!("line {}", i + 1);
                serde_json::from_str(l)
                    .map_err(|e| format!("{at}: not JSON: {e}"))
                    .and_then(|v| edit_of(v).map_err(|d| format!("{at}: {d}")))
                    .map(|e| (at, e))
            })
            .collect(),
        Format::Csv => csv_edits(text),
    }
}

/// An item as an edit: one with `op` as it is, any other object a create.
fn edit_of(item: Value) -> Result<Value, String> {
    match item {
        Value::Object(o) if o.contains_key("op") => Ok(Value::Object(o)),
        Value::Object(o) => Ok(json!({ "op": "create", "values": o })),
        other => Err(format!("{other} is not an object")),
    }
}

fn csv_edits(text: &str) -> Result<Vec<Located>, String> {
    let mut records = csv_records(text)?.into_iter();
    let Some((_, header)) = records.next() else {
        return Err("the CSV file is empty".into());
    };
    let header: Vec<String> = header.iter().map(|h| h.trim().to_owned()).collect();
    for (i, h) in header.iter().enumerate() {
        if h.is_empty() {
            return Err(format!("line 1: column {} has no name", i + 1));
        }
        if header[..i].contains(h) {
            return Err(format!("line 1: column '{h}' appears twice"));
        }
    }
    let mut out = Vec::new();
    for (line, cells) in records {
        let at = format!("line {line}");
        if cells.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        if cells.len() != header.len() {
            return Err(format!(
                "{at}: {} cells, but the header has {} columns",
                cells.len(),
                header.len()
            ));
        }
        let mut op = "create".to_owned();
        let mut id = None;
        let mut values = Map::new();
        for (h, cell) in header.iter().zip(cells) {
            match h.as_str() {
                "op" if !cell.trim().is_empty() => op = cell.trim().to_owned(),
                "id" if !cell.trim().is_empty() => id = Some(cell.trim().to_owned()),
                "op" | "id" => {}
                _ if cell.trim().is_empty() => {}
                _ => match h.split_once('.') {
                    Some((field, key)) => {
                        values.insert(field.to_owned(), json!({ key: cell }));
                    }
                    None => {
                        values.insert(h.clone(), json!(cell));
                    }
                },
            }
        }
        let mut edit = json!({ "op": op });
        if let Some(id) = id {
            edit["id"] = json!(id);
        }
        if !values.is_empty() {
            edit["values"] = Value::Object(values);
        }
        out.push((at, edit));
    }
    Ok(out)
}

/// RFC 4180 records with the line each starts on.
fn csv_records(text: &str) -> Result<Vec<(usize, Vec<String>)>, String> {
    let mut out = Vec::new();
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut line = 1;
    let mut start = 1;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    cell.push('\n');
                }
                _ => cell.push(c),
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() => quoted = true,
            ',' => cells.push(std::mem::take(&mut cell)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                cells.push(std::mem::take(&mut cell));
                out.push((start, std::mem::take(&mut cells)));
                line += 1;
                start = line;
                any = false;
            }
            _ => cell.push(c),
        }
    }
    if quoted {
        return Err(format!("line {start}: a quoted cell is never closed"));
    }
    if any {
        cells.push(cell);
        out.push((start, cells));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_reads_quotes_refs_and_ops() {
        let csv = "name,qty,recipe.name,op,id\r\n\"Flour, plain\",\"1 1/2 cups\",Bread,,\n\nSalt,,Bread,update,r1\n\"Say \"\"hi\"\"\nthere\",2,,,\n";
        let e = parse(csv.as_bytes(), Format::Csv).unwrap();
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].0, "line 2");
        assert_eq!(
            e[0].1,
            json!({"op": "create", "values": {"name": "Flour, plain", "qty": "1 1/2 cups", "recipe": {"name": "Bread"}}})
        );
        assert_eq!(
            e[1].1,
            json!({"op": "update", "id": "r1", "values": {"name": "Salt", "recipe": {"name": "Bread"}}})
        );
        assert_eq!(e[2].0, "line 5");
        assert_eq!(e[2].1["values"]["name"], "Say \"hi\"\nthere");
        assert!(
            parse(b"a,b\n1\n", Format::Csv)
                .unwrap_err()
                .starts_with("line 2:")
        );
        assert!(parse(b"a,a\n1,2\n", Format::Csv).is_err());
    }

    #[test]
    fn json_and_jsonl_take_edits_or_rows() {
        let j = br#"[{"title": "A"}, {"op": "delete", "id": "r1"}]"#;
        let e = parse(j, Format::Json).unwrap();
        assert_eq!(
            e[0],
            (
                "item 1".into(),
                json!({"op": "create", "values": {"title": "A"}})
            )
        );
        assert_eq!(e[1].1["op"], "delete");
        let l = b"{\"title\": \"A\"}\n\n{\"title\": \"B\"}\n";
        let e = parse(l, Format::Jsonl).unwrap();
        assert_eq!(e[1].0, "line 3");
        assert!(
            parse(b"[1]", Format::Json)
                .unwrap_err()
                .starts_with("item 1")
        );
        assert_eq!(Format::sniff(" [1]"), Format::Json);
        assert_eq!(Format::sniff("{}"), Format::Jsonl);
        assert_eq!(Format::sniff("a,b"), Format::Csv);
        assert_eq!(
            Format::of_url("https://x.example/a.csv?x=1"),
            Some(Format::Csv)
        );
    }
}
