//! The `recipes_*` tables: schema, imports, and the reads behind the app
//! (docs/DECISIONS.md D41).

use std::collections::{BTreeMap, HashMap};

use pod_core::store::Migration;
use protocol::{DataBundle, EntityRecord};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Deserialize;
use serde_json::json;

use crate::format::{self, Ingredient, UnitNames};

/// The adapter's migrations. Append-only: never edit or reorder a step that
/// has shipped (staging has run it).
pub static MIGRATIONS: &[Migration] = &[
    // 1: the schema and the units.
    Migration::Sql(SCHEMA),
    // 2: imported the family collection until decision
    // `recipes-start-empty`; now a no-op. Pods that ran it keep the recipes,
    // new pods start with none. Kept so the step numbers stay the same.
    Migration::Rust(retired_import),
];

const SCHEMA: &str = "
CREATE TABLE recipes_units (
    code    TEXT PRIMARY KEY NOT NULL,
    display TEXT NOT NULL,
    plural  TEXT NOT NULL,
    kind    TEXT NOT NULL CHECK (kind IN ('volume', 'weight', 'count', 'length', 'vague')),
    to_base REAL                -- ml (volume) or g (weight); NULL otherwise
);
INSERT INTO recipes_units (code, display, plural, kind, to_base) VALUES
    ('tsp', 'tsp', 'tsp', 'volume', 4.92892),
    ('tbsp', 'Tbsp', 'Tbsp', 'volume', 14.7868),
    ('cup', 'cup', 'cups', 'volume', 236.588),
    ('fl_oz', 'fl oz', 'fl oz', 'volume', 29.5735),
    ('pt', 'pt', 'pt', 'volume', 473.176),
    ('qt', 'qt', 'qt', 'volume', 946.353),
    ('gal', 'gal', 'gal', 'volume', 3785.41),
    ('ml', 'ml', 'ml', 'volume', 1.0),
    ('l', 'l', 'l', 'volume', 1000.0),
    ('oz', 'oz', 'oz', 'weight', 28.3495),
    ('lb', 'lb', 'lb', 'weight', 453.592),
    ('g', 'g', 'g', 'weight', 1.0),
    ('pinch', 'pinch', 'pinches', 'vague', NULL),
    ('dash', 'dash', 'dashes', 'vague', NULL),
    ('splash', 'splash', 'splashes', 'vague', NULL),
    ('can', 'can', 'cans', 'count', NULL),
    ('pkg', 'pkg', 'pkg', 'count', NULL),
    ('box', 'box', 'boxes', 'count', NULL),
    ('bottle', 'bottle', 'bottles', 'count', NULL),
    ('container', 'container', 'containers', 'count', NULL),
    ('stick', 'stick', 'sticks', 'count', NULL),
    ('square', 'square', 'squares', 'count', NULL),
    ('clove', 'clove', 'cloves', 'count', NULL),
    ('bunch', 'bunch', 'bunches', 'count', NULL),
    ('head', 'head', 'heads', 'count', NULL),
    ('slice', 'slice', 'slices', 'count', NULL),
    ('sprig', 'sprig', 'sprigs', 'count', NULL),
    ('stalk', 'stalk', 'stalks', 'count', NULL),
    ('inch', 'inch', 'inches', 'length', NULL);

CREATE TABLE recipes_categories (
    slug     TEXT PRIMARY KEY NOT NULL,
    name     TEXT NOT NULL,
    position INTEGER NOT NULL
);

CREATE TABLE recipes (
    id         INTEGER PRIMARY KEY,
    slug       TEXT NOT NULL UNIQUE,     -- the URI id: recipes://recipes/<slug>
    title      TEXT NOT NULL,
    category   TEXT NOT NULL REFERENCES recipes_categories(slug),
    source     TEXT,                     -- where the recipe came from (scan, doc)
    yield_min  REAL,
    yield_max  REAL,
    yield_unit TEXT                      -- 'servings', 'dozen', 'pans', ...
);

CREATE TABLE recipes_ingredient_groups (
    id        INTEGER PRIMARY KEY,
    recipe_id INTEGER NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    name      TEXT,                      -- 'Cake', 'Topping', ...; NULL: the only group
    position  INTEGER NOT NULL
);

CREATE TABLE recipes_ingredients (
    id          INTEGER PRIMARY KEY,
    group_id    INTEGER NOT NULL REFERENCES recipes_ingredient_groups(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    qty_min     REAL,
    qty_max     REAL,                    -- ranges: 4-6 cups
    unit        TEXT REFERENCES recipes_units(code),
    extra_qty   REAL,                    -- '1 cup + 1 Tbsp'
    extra_unit  TEXT REFERENCES recipes_units(code),
    pkg_qty     REAL,                    -- '1 (19 oz) box'
    pkg_unit    TEXT REFERENCES recipes_units(code),
    name        TEXT NOT NULL,
    preparation TEXT,
    note        TEXT,
    scale_basis TEXT,                    -- 'per pan', 'per serving'
    is_optional INTEGER NOT NULL DEFAULT 0,
    is_divided  INTEGER NOT NULL DEFAULT 0,
    to_taste    INTEGER NOT NULL DEFAULT 0,
    alt_of      INTEGER REFERENCES recipes_ingredients(id) ON DELETE CASCADE  -- 'or 2 small lemons'
);

CREATE TABLE recipes_instruction_groups (
    id        INTEGER PRIMARY KEY,
    recipe_id INTEGER NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    name      TEXT,                      -- 'Crockpot method', ...; NULL: the main steps
    position  INTEGER NOT NULL
);

CREATE TABLE recipes_steps (
    id       INTEGER PRIMARY KEY,
    group_id INTEGER NOT NULL REFERENCES recipes_instruction_groups(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    text     TEXT NOT NULL,
    temp_f   INTEGER,
    time_min INTEGER,                    -- minutes
    time_max INTEGER
);

CREATE TABLE recipes_notes (
    recipe_id INTEGER NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL,
    text      TEXT NOT NULL,
    PRIMARY KEY (recipe_id, position)
);

CREATE INDEX recipes_by_category ON recipes(category);
CREATE INDEX recipes_ingredient_groups_by_recipe ON recipes_ingredient_groups(recipe_id);
CREATE INDEX recipes_ingredients_by_group ON recipes_ingredients(group_id);
CREATE INDEX recipes_instruction_groups_by_recipe ON recipes_instruction_groups(recipe_id);
CREATE INDEX recipes_steps_by_group ON recipes_steps(group_id);
";

// ---------- import ----------

/// The former step 2 (the family collection import): does nothing now
/// (decision `recipes-start-empty`).
fn retired_import(_tx: &Transaction<'_>) -> rusqlite::Result<()> {
    Ok(())
}

/// Made-up recipes for tests, one per category the layouts' chips name. Not
/// loaded by any migration: tests call [`seed_test_recipes`]. Only built for
/// tests (the `test-seed` feature, which other crates enable as a
/// dev-dependency), so the pod binary never holds it.
#[cfg(any(test, feature = "test-seed"))]
pub static TEST_SEED: &str = include_str!("../tests/data/test-seed.json");

/// Add [`TEST_SEED`] to a pod's recipes, unless it already has some. For
/// tests only.
#[cfg(any(test, feature = "test-seed"))]
pub fn seed_test_recipes(conn: &Connection) -> rusqlite::Result<()> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM recipes", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    let file = parse_import(TEST_SEED)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    import(conn, &file)
}

/// An import file (see `tests/data/test-seed.json`).
#[derive(Debug, Deserialize)]
pub struct ImportFile {
    pub categories: Vec<ImportCategory>,
    pub recipes: Vec<ImportRecipe>,
}

#[derive(Debug, Deserialize)]
pub struct ImportCategory {
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ImportRecipe {
    pub slug: String,
    pub title: String,
    /// The category's name (as in `categories`).
    pub category: String,
    pub source: Option<String>,
    #[serde(rename = "yield")]
    pub yld: Option<ImportYield>,
    pub ingredient_groups: Vec<ImportIngredientGroup>,
    pub instruction_groups: Vec<ImportInstructionGroup>,
    pub notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportYield {
    pub min: f64,
    pub max: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportIngredientGroup {
    pub name: Option<String>,
    pub ingredients: Vec<ImportIngredient>,
}

#[derive(Debug, Deserialize)]
pub struct ImportIngredient {
    pub qty_min: Option<f64>,
    pub qty_max: Option<f64>,
    pub unit: Option<String>,
    pub extra_qty: Option<f64>,
    pub extra_unit: Option<String>,
    pub pkg_qty: Option<f64>,
    pub pkg_unit: Option<String>,
    pub name: String,
    pub preparation: Option<String>,
    pub note: Option<String>,
    pub scale_basis: Option<String>,
    pub is_optional: bool,
    pub is_divided: bool,
    pub to_taste: bool,
    /// "1 large lemon or 2 small lemons": the other choices.
    #[serde(default)]
    pub alternatives: Vec<ImportIngredient>,
    /// The line as written in the source (checked by the tests, not stored).
    #[serde(default)]
    pub line: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportInstructionGroup {
    pub name: Option<String>,
    pub steps: Vec<ImportStep>,
}

#[derive(Debug, Deserialize)]
pub struct ImportStep {
    pub text: String,
    pub temp_f: Option<i64>,
    pub time_min: Option<i64>,
    pub time_max: Option<i64>,
}

/// Parse an import file.
pub fn parse_import(json: &str) -> serde_json::Result<ImportFile> {
    serde_json::from_str(json)
}

/// Insert every category and recipe of `file`.
pub fn import(conn: &Connection, file: &ImportFile) -> rusqlite::Result<()> {
    let base: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM recipes_categories",
        [],
        |r| r.get(0),
    )?;
    let mut slugs: HashMap<&str, &str> = HashMap::new();
    for (i, c) in file.categories.iter().enumerate() {
        conn.execute(
            "INSERT INTO recipes_categories (slug, name, position) VALUES (?1, ?2, ?3)",
            params![c.slug, c.name, base + i as i64],
        )?;
        slugs.insert(&c.name, &c.slug);
    }
    for r in &file.recipes {
        let category = slugs
            .get(r.category.as_str())
            .copied()
            .unwrap_or(&r.category);
        conn.execute(
            "INSERT INTO recipes (slug, title, category, source, yield_min, yield_max, yield_unit)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                r.slug,
                r.title,
                category,
                r.source,
                r.yld.as_ref().map(|y| y.min),
                r.yld.as_ref().and_then(|y| y.max),
                r.yld.as_ref().and_then(|y| y.unit.clone()),
            ],
        )?;
        let recipe_id = conn.last_insert_rowid();
        for (gi, g) in r.ingredient_groups.iter().enumerate() {
            conn.execute(
                "INSERT INTO recipes_ingredient_groups (recipe_id, name, position) VALUES (?1, ?2, ?3)",
                params![recipe_id, g.name, gi as i64],
            )?;
            let group_id = conn.last_insert_rowid();
            for (ii, ing) in g.ingredients.iter().enumerate() {
                let id = insert_ingredient(conn, group_id, ii as i64, ing, None)?;
                for alt in &ing.alternatives {
                    insert_ingredient(conn, group_id, ii as i64, alt, Some(id))?;
                }
            }
        }
        for (gi, g) in r.instruction_groups.iter().enumerate() {
            conn.execute(
                "INSERT INTO recipes_instruction_groups (recipe_id, name, position) VALUES (?1, ?2, ?3)",
                params![recipe_id, g.name, gi as i64],
            )?;
            let group_id = conn.last_insert_rowid();
            for (si, s) in g.steps.iter().enumerate() {
                conn.execute(
                    "INSERT INTO recipes_steps (group_id, position, text, temp_f, time_min, time_max)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![group_id, si as i64, s.text, s.temp_f, s.time_min, s.time_max],
                )?;
            }
        }
        for (ni, n) in r.notes.iter().enumerate() {
            conn.execute(
                "INSERT INTO recipes_notes (recipe_id, position, text) VALUES (?1, ?2, ?3)",
                params![recipe_id, ni as i64, n],
            )?;
        }
    }
    Ok(())
}

fn insert_ingredient(
    conn: &Connection,
    group_id: i64,
    position: i64,
    i: &ImportIngredient,
    alt_of: Option<i64>,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO recipes_ingredients (group_id, position, qty_min, qty_max, unit, extra_qty,
             extra_unit, pkg_qty, pkg_unit, name, preparation, note, scale_basis, is_optional,
             is_divided, to_taste, alt_of)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            group_id,
            position,
            i.qty_min,
            i.qty_max,
            i.unit,
            i.extra_qty,
            i.extra_unit,
            i.pkg_qty,
            i.pkg_unit,
            i.name,
            i.preparation,
            i.note,
            i.scale_basis,
            i.is_optional,
            i.is_divided,
            i.to_taste,
            alt_of,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

// ---------- reads ----------

pub const RECIPE_PREFIX: &str = "recipes://recipes/";
pub const ALL_QUERY: &str = "recipes:all";

pub fn recipe_uri(slug: &str) -> String {
    format!("{RECIPE_PREFIX}{slug}")
}

/// Every recipe, by category then title: one `recipes://recipes/<slug>`
/// entity each, listed in `queries["recipes:all"]`. The first recipe of a
/// category carries `category_header` (the list draws it as a heading; layouts
/// from schema 5 group by `category` instead). `search_text` (title, category
/// and ingredient names) is what the list's search matches (D43).
pub fn all_recipes(conn: &Connection) -> rusqlite::Result<DataBundle> {
    let mut bundle = DataBundle::default();
    let mut order = Vec::new();
    for (slug, entity) in recipe_entities(conn)? {
        let uri = recipe_uri(&slug);
        order.push(uri.clone());
        bundle.entities.insert(uri, entity);
    }
    bundle.queries.insert(ALL_QUERY.into(), order);
    Ok(bundle)
}

/// `(slug, entity)` for every recipe, in list order.
fn recipe_entities(conn: &Connection) -> rusqlite::Result<Vec<(String, EntityRecord)>> {
    let mut stmt = conn.prepare(
        "SELECT r.slug, r.title, c.name, r.source, r.yield_min, r.yield_max, r.yield_unit, c.slug
         FROM recipes r JOIN recipes_categories c ON c.slug = r.category
         ORDER BY c.position, r.title COLLATE NOCASE, r.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ),
            (
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ),
            r.get::<_, String>(7)?,
        ))
    })?;
    let mut ingredients = ingredient_names(conn)?;
    let mut stats = recipe_stats(conn)?;
    let mut out = Vec::new();
    let mut last_category: Option<String> = None;
    for row in rows {
        let ((slug, title, category, source), (ymin, ymax, yunit), category_slug) = row?;
        let yield_text = format::yield_text(ymin, ymax, yunit.as_deref());
        let summary = match &yield_text {
            Some(y) => format!("{category} · {y}"),
            None => category.clone(),
        };
        let mut search = vec![title.clone(), category.clone()];
        search.extend(ingredients.remove(&slug).unwrap_or_default());
        let mut e = EntityRecord::new();
        let st = stats.remove(&slug).unwrap_or_default();
        set_display_fields(
            &mut e,
            &title,
            &category,
            &category_slug,
            &st,
            yield_text.as_deref(),
        );
        e.set("id", slug.clone());
        e.set("search_text", search.join(" · "));
        e.set("title", title);
        e.set("category", category.clone());
        e.set("summary", summary);
        e.set("source", source.unwrap_or_default());
        if let Some(y) = yield_text {
            e.set("yield_text", y);
        }
        if last_category.as_deref() != Some(category.as_str()) {
            e.set("category_header", category.clone());
            last_category = Some(category);
        }
        out.push((slug, e));
    }
    Ok(out)
}

/// A recipe's numbers for the list's meta line and the card's summary strip.
#[derive(Debug, Default, Clone, PartialEq)]
struct RecipeStats {
    ingredients: i64,
    steps: i64,
    /// The hottest oven temperature a step names.
    oven_f: Option<i64>,
    /// The minutes the main (first) instruction group names, summed; later
    /// groups are often another method ("Crockpot method").
    minutes: Option<i64>,
}

fn recipe_stats(conn: &Connection) -> rusqlite::Result<HashMap<String, RecipeStats>> {
    let mut out: HashMap<String, RecipeStats> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT r.slug, COUNT(*) FROM recipes_ingredients i
         JOIN recipes_ingredient_groups g ON g.id = i.group_id
         JOIN recipes r ON r.id = g.recipe_id
         WHERE i.alt_of IS NULL GROUP BY r.slug",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (slug, n) = row?;
        out.entry(slug).or_default().ingredients = n;
    }
    let mut stmt = conn.prepare(
        "SELECT r.slug, COUNT(*), MAX(s.temp_f),
                SUM(CASE WHEN g.position = (SELECT MIN(position) FROM recipes_instruction_groups
                                            WHERE recipe_id = r.id)
                         THEN s.time_min END)
         FROM recipes_steps s
         JOIN recipes_instruction_groups g ON g.id = s.group_id
         JOIN recipes r ON r.id = g.recipe_id
         GROUP BY r.slug",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })?;
    for row in rows {
        let (slug, steps, oven_f, minutes) = row?;
        let st = out.entry(slug).or_default();
        st.steps = steps;
        st.oven_f = oven_f;
        st.minutes = minutes.filter(|&m| m > 0);
    }
    Ok(out)
}

/// `(tint, ink)` of a category: a light fill and the text colour drawn on it
/// (D44). Unknown categories are grey.
pub fn category_colors(slug: &str) -> (&'static str, &'static str) {
    match slug {
        "appetizers-sides-salads" => ("#E3F1E0", "#2F6B2A"),
        "main-dishes-meat-casseroles" => ("#FBE6DF", "#A3361B"),
        "side-dishes-rice-grains" => ("#F3EBD3", "#6E5A12"),
        "desserts" => ("#F6E3EF", "#93306C"),
        "breakfast" => ("#FBF0D0", "#8A5A00"),
        "baking-breads-crusts-frosting" => ("#F1E7DA", "#7A4F22"),
        "snacks-confections" => ("#FDE9D6", "#A04E0C"),
        "drinks" => ("#DFF0F2", "#1F6B74"),
        _ => ("#EEEFF2", "#4A4D55"),
    }
}

/// A category name without its parenthesised examples:
/// `"Main Dishes (Meat/Casseroles)"` -> `"Main Dishes"`.
pub fn short_category(name: &str) -> String {
    match name.split_once(" (") {
        Some((short, _)) if !short.trim().is_empty() => short.trim().to_owned(),
        _ => name.to_owned(),
    }
}

/// The fields layouts from schema 6 draw (D44): `category_short`, `color` /
/// `ink` (the category's fill and text colours), `initial`, `meta` (the
/// list's one-line summary), and the card's summary strip
/// (`ingredient_count`, `step_count`, `oven_short`, `time_short`).
fn set_display_fields(
    e: &mut EntityRecord,
    title: &str,
    category: &str,
    category_slug: &str,
    st: &RecipeStats,
    yield_text: Option<&str>,
) {
    let (tint, ink) = category_colors(category_slug);
    e.set("category_short", short_category(category));
    e.set("color", tint);
    e.set("ink", ink);
    let initial = title
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    e.set("initial", initial);
    let mut meta = vec![match st.ingredients {
        1 => "1 ingredient".to_owned(),
        n => format!("{n} ingredients"),
    }];
    meta.extend(st.oven_f.map(|t| format!("{t}°F")));
    meta.extend(st.minutes.map(|m| format::minutes_text(m, None)));
    if meta.len() < 3 {
        meta.extend(yield_text.map(str::to_owned));
    }
    e.set("meta", meta.join(" · "));
    e.set("ingredient_count", st.ingredients);
    e.set("step_count", st.steps);
    e.set(
        "oven_short",
        st.oven_f.map_or("–".to_owned(), |t| format!("{t}°")),
    );
    e.set(
        "time_short",
        st.minutes.map_or("–".to_owned(), format::minutes_short),
    );
}

/// Every recipe's distinct ingredient names (alternatives included), by slug,
/// in recipe order.
fn ingredient_names(conn: &Connection) -> rusqlite::Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT r.slug, i.name FROM recipes_ingredients i
         JOIN recipes_ingredient_groups g ON g.id = i.group_id
         JOIN recipes r ON r.id = g.recipe_id
         ORDER BY r.id, g.position, i.position, i.id",
    )?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (slug, name) = row?;
        let names = out.entry(slug).or_default();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(out)
}

/// The query keys of a recipe screen.
pub fn ingredients_query(slug: &str) -> String {
    format!("recipe:{slug}:ingredients")
}
pub fn steps_query(slug: &str) -> String {
    format!("recipe:{slug}:steps")
}
pub fn notes_query(slug: &str) -> String {
    format!("recipe:{slug}:notes")
}

/// The data of one recipe's screen, or `None` for an unknown slug: the
/// recipe entity (the same one the list has) and three lists, ingredients
/// (`recipes://ingredients/<id>`), steps (`recipes://steps/<id>`) and notes
/// (`recipes://notes/<recipe>-<n>`). The first item of each list carries
/// `section` ("Ingredients", ...); the first item of a named group carries
/// `group` ("Topping", ...).
pub fn recipe_screen(conn: &Connection, slug: &str) -> rusqlite::Result<Option<DataBundle>> {
    let id: Option<i64> = conn
        .query_row("SELECT id FROM recipes WHERE slug = ?1", [slug], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(id) = id else {
        return Ok(None);
    };
    let mut bundle = DataBundle::default();
    if let Some((_, e)) = recipe_entities(conn)?.into_iter().find(|(s, _)| s == slug) {
        bundle.entities.insert(recipe_uri(slug), e);
    }

    let units = units(conn)?;
    let mut ingredients = Vec::new();
    for (uri, e) in ingredient_entities(conn, id, &units)? {
        ingredients.push(uri.clone());
        bundle.entities.insert(uri, e);
    }
    bundle.queries.insert(ingredients_query(slug), ingredients);

    let mut steps = Vec::new();
    for (uri, e) in step_entities(conn, id)? {
        steps.push(uri.clone());
        bundle.entities.insert(uri, e);
    }
    bundle.queries.insert(steps_query(slug), steps);

    let mut notes = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT position, text FROM recipes_notes WHERE recipe_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    for (n, row) in rows.enumerate() {
        let (pos, text) = row?;
        let mut e = EntityRecord::new();
        e.set("text", text);
        if n == 0 {
            e.set("section", "Notes");
        }
        let uri = format!("recipes://notes/{id}-{pos}");
        notes.push(uri.clone());
        bundle.entities.insert(uri, e);
    }
    bundle.queries.insert(notes_query(slug), notes);
    Ok(Some(bundle))
}

/// code -> display names.
pub fn units(conn: &Connection) -> rusqlite::Result<HashMap<String, UnitNames>> {
    let mut stmt = conn.prepare("SELECT code, display, plural FROM recipes_units")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            UnitNames {
                one: r.get(1)?,
                many: r.get(2)?,
            },
        ))
    })?;
    rows.collect()
}

struct IngredientRow {
    id: i64,
    group_id: i64,
    group_name: Option<String>,
    alt_of: Option<i64>,
    fields: Ingredient,
}

fn ingredient_entities(
    conn: &Connection,
    recipe_id: i64,
    units: &HashMap<String, UnitNames>,
) -> rusqlite::Result<Vec<(String, EntityRecord)>> {
    let unit = |code: Option<String>| code.and_then(|c| units.get(&c).cloned());
    let mut stmt = conn.prepare(
        "SELECT i.id, g.id, g.name, i.alt_of, i.qty_min, i.qty_max, i.unit, i.extra_qty,
                i.extra_unit, i.pkg_qty, i.pkg_unit, i.name, i.preparation, i.note,
                i.scale_basis, i.is_optional, i.is_divided, i.to_taste
         FROM recipes_ingredients i JOIN recipes_ingredient_groups g ON g.id = i.group_id
         WHERE g.recipe_id = ?1
         ORDER BY g.position, i.position, i.alt_of IS NOT NULL, i.id",
    )?;
    let rows: Vec<IngredientRow> = stmt
        .query_map([recipe_id], |r| {
            let pkg_unit: Option<String> = r.get(10)?;
            Ok(IngredientRow {
                id: r.get(0)?,
                group_id: r.get(1)?,
                group_name: r.get(2)?,
                alt_of: r.get(3)?,
                fields: Ingredient {
                    qty_min: r.get(4)?,
                    qty_max: r.get(5)?,
                    unit: unit(r.get(6)?),
                    extra_qty: r.get(7)?,
                    extra_unit: unit(r.get(8)?),
                    pkg_qty: r.get(9)?,
                    pkg_unit: pkg_unit
                        .clone()
                        .and_then(|c| units.get(&c).cloned().map(|n| (c, n))),
                    name: r.get(11)?,
                    preparation: r.get(12)?,
                    note: r.get(13)?,
                    scale_basis: r.get(14)?,
                    is_optional: r.get(15)?,
                    is_divided: r.get(16)?,
                    to_taste: r.get(17)?,
                },
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut alternatives: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.alt_of.is_some()) {
        alternatives
            .entry(r.alt_of.unwrap_or_default())
            .or_default()
            .push(format::ingredient(&r.fields));
    }
    let mut out = Vec::new();
    let mut last_group = None;
    for r in rows.iter().filter(|r| r.alt_of.is_none()) {
        let mut text = format::ingredient(&r.fields);
        for alt in alternatives.get(&r.id).into_iter().flatten() {
            text.push_str(" or ");
            text.push_str(alt);
        }
        let (amount, mut thing, details) = format::ingredient_parts(&r.fields);
        for alt in alternatives.get(&r.id).into_iter().flatten() {
            thing.push_str(" or ");
            thing.push_str(alt);
        }
        let mut e = EntityRecord::new();
        e.set("text", text);
        e.set("name", r.fields.name.clone());
        e.set("amount", amount);
        e.set("thing", thing);
        e.set("details", details);
        if out.is_empty() {
            e.set("section", "Ingredients");
        }
        if last_group != Some(r.group_id) {
            if let Some(name) = &r.group_name {
                e.set("group", name.clone());
            }
            last_group = Some(r.group_id);
        }
        out.push((format!("recipes://ingredients/{}", r.id), e));
    }
    Ok(out)
}

fn step_entities(
    conn: &Connection,
    recipe_id: i64,
) -> rusqlite::Result<Vec<(String, EntityRecord)>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, g.id, g.name, s.text, s.temp_f, s.time_min, s.time_max
         FROM recipes_steps s JOIN recipes_instruction_groups g ON g.id = s.group_id
         WHERE g.recipe_id = ?1
         ORDER BY g.position, s.position",
    )?;
    let rows = stmt.query_map([recipe_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    let mut last_group = None;
    let mut number = 0;
    for row in rows {
        let (id, group_id, group_name, text, temp_f, time_min, time_max) = row?;
        let mut e = EntityRecord::new();
        if out.is_empty() {
            e.set("section", "Instructions");
        }
        if last_group != Some(group_id) {
            if let Some(name) = group_name {
                e.set("group", name);
            }
            last_group = Some(group_id);
            number = 0;
        }
        number += 1;
        e.set("number", number);
        e.set("text", text);
        if temp_f.is_some() || time_min.is_some() {
            e.set("has_cues", true);
        }
        if let Some(t) = temp_f {
            e.set("temp_text", format!("{t}°F"));
        }
        if let Some(m) = time_min {
            e.set("time_text", format::minutes_text(m, time_max));
        }
        e.set("temp_f", json!(temp_f));
        e.set("time_min", json!(time_min));
        e.set("time_max", json!(time_max));
        out.push((format!("recipes://steps/{id}"), e));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        seed_test_recipes(&conn).unwrap();
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn recipes_carry_display_fields() {
        let conn = db();
        let bundle = recipe_screen(&conn, "placeholder-pot-pie")
            .unwrap()
            .unwrap();
        let r = &bundle.entities["recipes://recipes/placeholder-pot-pie"];
        let get = |e: &EntityRecord, f: &str| e.get(f).cloned().unwrap_or_default();
        assert_eq!(get(r, "category_short"), "Main Dishes");
        assert_eq!(get(r, "color"), "#FBE6DF");
        assert_eq!(get(r, "initial"), "P");
        assert_eq!(get(r, "meta"), "9 ingredients · 425°F · 55 min");
        assert_eq!(get(r, "ingredient_count"), 9);
        assert_eq!(get(r, "step_count"), 7);
        assert_eq!(get(r, "oven_short"), "425°");
        assert_eq!(get(r, "time_short"), "55m");
        let ingredients: Vec<_> = bundle.queries["recipe:placeholder-pot-pie:ingredients"]
            .iter()
            .map(|u| &bundle.entities[u])
            .collect();
        assert_eq!(get(ingredients[0], "amount"), "2 cups");
        assert_eq!(get(ingredients[0], "thing"), "cooked chicken");
        assert_eq!(get(ingredients[0], "details"), "diced");
        assert_eq!(get(ingredients[2], "amount"), "⅓ cup");
        assert_eq!(get(ingredients[6], "amount"), "½ cup + 2 Tbsp");
        assert_eq!(get(ingredients[6], "details"), "divided");
        assert_eq!(
            get(ingredients[7], "thing"),
            "large lemon or 2 small lemons"
        );
        assert_eq!(get(ingredients[8], "amount"), "1 (9\")");
        let steps: Vec<_> = bundle.queries["recipe:placeholder-pot-pie:steps"]
            .iter()
            .map(|u| &bundle.entities[u])
            .collect();
        assert_eq!(get(steps[4], "temp_text"), "425°F");
        assert_eq!(get(steps[4], "time_text"), "30 min");
        assert!(steps[1].get("temp_text").is_none());
        let all = all_recipes(&conn).unwrap();
        let brownies = &all.entities["recipes://recipes/example-brownies"];
        assert_eq!(get(brownies, "meta"), "6 ingredients · 350°F · 25 min");
        // Without an oven or a time, the yield fills the meta line.
        let mix = &all.entities["recipes://recipes/imaginary-trail-mix"];
        assert_eq!(get(mix, "meta"), "4 ingredients · Makes 4 cups");
        assert_eq!(get(mix, "time_short"), "–");
        assert_eq!(short_category("Snacks/Confections"), "Snacks/Confections");
    }

    #[test]
    fn the_test_seed_has_a_recipe_in_each_of_8_categories() {
        let conn = db();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recipes"), 8);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recipes_categories"), 8);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM recipes r LEFT JOIN recipes_categories c ON c.slug = r.category
                 WHERE c.slug IS NULL"
            ),
            0,
            "every recipe is in a known category"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM recipes_ingredients i
                 WHERE (i.unit IS NOT NULL AND i.unit NOT IN (SELECT code FROM recipes_units))
                    OR (i.extra_unit IS NOT NULL AND i.extra_unit NOT IN (SELECT code FROM recipes_units))
                    OR (i.pkg_unit IS NOT NULL AND i.pkg_unit NOT IN (SELECT code FROM recipes_units))"
            ),
            0,
            "every unit is a known unit"
        );
        // Seeding again adds nothing.
        seed_test_recipes(&conn).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recipes"), 8);
    }

    /// Formatting the stored fields gives back the import file's lines, so no
    /// ingredient lost anything in the import.
    #[test]
    fn ingredients_format_back_to_the_source_lines() {
        let conn = db();
        let file = parse_import(TEST_SEED).unwrap();
        let mut checked = 0;
        for r in &file.recipes {
            let bundle = recipe_screen(&conn, &r.slug).unwrap().unwrap();
            let got: Vec<&str> = bundle.queries[&ingredients_query(&r.slug)]
                .iter()
                .map(|u| bundle.entities[u].get_str("text").unwrap())
                .collect();
            let want: Vec<&str> = r
                .ingredient_groups
                .iter()
                .flat_map(|g| &g.ingredients)
                .map(|i| i.line.as_deref().unwrap())
                .collect();
            assert_eq!(got, want, "{}", r.title);
            checked += got.len();
        }
        assert!(checked > 40, "{checked}");
    }

    #[test]
    fn a_recipe_screen_has_sections_groups_and_notes() {
        let conn = db();
        let slug = "test-garden-salad";
        let b = recipe_screen(&conn, slug).unwrap().unwrap();
        let recipe = &b.entities["recipes://recipes/test-garden-salad"];
        assert_eq!(recipe.get_str("title"), Some("Test Garden Salad"));
        assert_eq!(recipe.get_str("yield_text"), Some("Serves 4-6"));

        let items = |q: String| -> Vec<&EntityRecord> {
            b.queries[&q].iter().map(|u| &b.entities[u]).collect()
        };
        let ing = items(ingredients_query(slug));
        assert_eq!(ing[0].get_str("section"), Some("Ingredients"));
        assert_eq!(ing[0].get_str("group"), Some("Salad"));
        assert_eq!(
            ing[0].get_str("text"),
            Some("1 head romaine lettuce, chopped")
        );
        let groups: Vec<&str> = ing.iter().filter_map(|e| e.get_str("group")).collect();
        assert_eq!(groups, ["Salad", "Dressing", "Croutons"]);
        assert!(ing[1..].iter().all(|e| e.get("section").is_none()));

        let steps = items(steps_query(slug));
        assert_eq!(steps[0].get_str("section"), Some("Instructions"));
        assert_eq!(steps[0].get("group"), None);
        assert_eq!(steps[4].get_str("group"), Some("For the croutons"));
        let numbers: Vec<i64> = steps.iter().map(|e| e.get_i64("number").unwrap()).collect();
        assert_eq!(numbers, [1, 2, 3, 4, 1, 2]);

        let notes = items(notes_query(slug));
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].get_str("section"), Some("Notes"));

        assert!(recipe_screen(&conn, "no-such-recipe").unwrap().is_none());
    }

    #[test]
    fn steps_keep_temperatures_and_times() {
        let conn = db();
        let (temp, lo, hi): (Option<i64>, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT s.temp_f, s.time_min, s.time_max FROM recipes_steps s
                 JOIN recipes_instruction_groups g ON g.id = s.group_id
                 JOIN recipes r ON r.id = g.recipe_id
                 WHERE r.slug = 'example-brownies' AND s.text LIKE 'Bake%'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((temp, lo, hi), (Some(350), Some(25), Some(30)));
    }

    #[test]
    fn the_list_heads_each_category_once() {
        let conn = db();
        let b = all_recipes(&conn).unwrap();
        let order = &b.queries[ALL_QUERY];
        assert_eq!(order.len(), 8);
        let headers: Vec<&str> = order
            .iter()
            .filter_map(|u| b.entities[u].get_str("category_header"))
            .collect();
        assert_eq!(
            headers,
            [
                "Appetizers/Sides/Salads",
                "Main Dishes (Meat/Casseroles)",
                "Side Dishes (Rice/Grains)",
                "Desserts",
                "Breakfast",
                "Baking (Breads/Crusts/Frosting)",
                "Snacks/Confections",
                "Drinks",
            ]
        );
        // Search finds recipes by ingredient (D43): only one uses hibiscus.
        let with = |word: &str| -> Vec<&str> {
            order
                .iter()
                .filter(|u| {
                    b.entities[*u]
                        .get_str("search_text")
                        .unwrap()
                        .to_lowercase()
                        .contains(word)
                })
                .map(|u| u.strip_prefix(RECIPE_PREFIX).unwrap())
                .collect()
        };
        assert_eq!(with("hibiscus"), ["pretend-lemonade"]);
        assert_eq!(
            with("flour"),
            ["example-brownies", "demo-pancakes", "mock-loaf"]
        );
        // Slugs are valid route parameters.
        for u in order {
            let slug = u.strip_prefix(RECIPE_PREFIX).unwrap();
            assert!(
                (1..=64).contains(&slug.len())
                    && slug
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{slug}"
            );
        }
    }
}
