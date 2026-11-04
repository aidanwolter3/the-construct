//! The recipes adapter (docs/DECISIONS.md D41): a recipe collection, kept in
//! the pod's own SQLite (`recipes_*` tables), served as the `recipes`
//! app. There is no upstream and no sign-in.
//!
//! - `GET /v1/workspaces/recipes`: the `all` feed, every recipe by category
//!   (`queries["recipes:all"]`, `recipes://recipes/<slug>` entities).
//! - `GET /v1/recipes/screens/recipe?id=<slug>`: one recipe's screen data
//!   ([`db::recipe_screen`]).
//!
//! A new pod starts with no recipes (decision `recipes-start-empty`); pods
//! that ran the old family import (migration step 2) keep theirs. Tests seed
//! made-up ones with [`db::seed_test_recipes`].

pub mod db;
pub mod format;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use pod_core::layouts::LayoutSource;
use pod_core::store::Step;
use pod_core::{
    AppAdapter, AppInfo, AuthPolicy, BoxFuture, EntityDoc, FeedSpec, Hydration, PathSpec, Pod,
    RequestCtx, SourceManifest,
};
use serde::Deserialize;
use serde_json::json;

/// The adapter name: the prefix of its tables and routes.
pub const NAME: &str = "recipes";

/// The recipes app.
pub const APP: AppInfo = AppInfo {
    id: "recipes",
    title: "Recipes",
    icon: "recipe",
    dev: false,
    auth: AuthPolicy::Public,
    feeds: &[(NAME, ALL_FEED)],
};

/// Every recipe, by category (`AppInfo::feeds` entry `("recipes", "all")`).
pub const ALL_FEED: &str = "all";

/// What layouts may use of the recipes (decision
/// `custom-apps-and-data-sources`).
pub const MANIFEST: SourceManifest = SourceManifest {
    title: "Recipes",
    sign_in: None,
    entity_schemes: &["recipes"],
    feeds: &[FeedSpec {
        name: ALL_FEED,
        query_keys: &[db::ALL_QUERY],
        doc: "Every recipe, by category then title.",
    }],
    data_paths: &[PathSpec {
        pattern: "/v1/recipes/screens/recipe?id={id}",
        query_keys: &[
            "recipe:{id}:ingredients",
            "recipe:{id}:steps",
            "recipe:{id}:notes",
        ],
        doc: "One recipe: its ingredients, steps and notes.",
    }],
    actions: &[],
    entities: &[
        EntityDoc {
            uri: "recipes://recipes/{id}",
            doc: "A recipe (the list's and the recipe screen's).",
            fields: &[
                ("id", "the recipe slug"),
                ("title", "name"),
                ("category", "category name"),
                ("category_short", "category name without its examples"),
                ("category_header", "set on the first recipe of a category"),
                ("summary", "category · yield"),
                ("meta", "one line: ingredients, oven, time or yield"),
                ("source", "who it is from"),
                ("yield_text", "e.g. 4-6 servings"),
                ("search_text", "title, category and ingredient names"),
                ("color", "the category's fill, #RRGGBB"),
                ("ink", "text colour on that fill, #RRGGBB"),
                ("initial", "first letter of the title"),
                ("ingredient_count", "number of ingredients"),
                ("step_count", "number of steps"),
                ("oven_short", "hottest oven temperature, or –"),
                ("time_short", "main method's time, or –"),
            ],
        },
        EntityDoc {
            uri: "recipes://ingredients/{id}",
            doc: "An ingredient line of a recipe.",
            fields: &[
                ("text", "the whole line"),
                ("name", "the ingredient"),
                ("amount", "quantity and unit"),
                ("thing", "what to use"),
                ("details", "preparation notes"),
                ("section", "\"Ingredients\" on the first line"),
                ("group", "group name on a group's first line"),
            ],
        },
        EntityDoc {
            uri: "recipes://steps/{id}",
            doc: "An instruction step of a recipe.",
            fields: &[
                ("number", "step number"),
                ("text", "the instruction"),
                ("section", "\"Instructions\" on the first step"),
                ("group", "group name on a group's first step"),
                ("has_cues", "it names a temperature or a time"),
                ("temp_text", "e.g. 350°F"),
                ("time_text", "e.g. 25-30 min"),
                ("temp_f", "temperature, °F"),
                ("time_min", "minutes, lower bound"),
                ("time_max", "minutes, upper bound"),
            ],
        },
        EntityDoc {
            uri: "recipes://notes/{id}",
            doc: "A note of a recipe.",
            fields: &[
                ("text", "the note"),
                ("section", "\"Notes\" on the first note"),
            ],
        },
    ],
    dev: false,
};

// `FILES`: every `layouts/recipes/*.json`, in number order (`build.rs`).
include!(concat!(env!("OUT_DIR"), "/layout_files.rs"));

/// The recipes adapter.
#[derive(Debug, Default)]
pub struct RecipesAdapter;

impl RecipesAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl AppAdapter for RecipesAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn apps(&self) -> Vec<AppInfo> {
        vec![APP]
    }

    fn layout_files(&self) -> &'static [LayoutSource] {
        FILES
    }

    fn router(&self) -> Router<Pod> {
        Router::new().route("/v1/recipes/screens/recipe", get(recipe_screen))
    }

    fn migrations(&self) -> &'static [Step] {
        db::MIGRATIONS
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            if feed != ALL_FEED {
                return pod_core::unknown_feed(NAME, feed);
            }
            match pod.blocking(|s| s.with_conn(db::all_recipes)).await {
                Ok(bundle) => Hydration::Data(bundle),
                Err(e) => Hydration::Error(e.into_response()),
            }
        })
    }
}

#[derive(Deserialize)]
struct RecipeParams {
    #[serde(default)]
    id: String,
}

/// `GET /v1/recipes/screens/recipe?id=<slug>`: the recipe screen's data, a
/// `DataBundle` (see [`db::recipe_screen`]). Unknown recipe: 404
/// `recipe_not_found`.
async fn recipe_screen(State(pod): State<Pod>, Query(p): Query<RecipeParams>) -> Response {
    let slug = p.id.trim().to_owned();
    let valid = (1..=64).contains(&slug.len())
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return pod_core::json_error(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_request", "expected": { "id": "recipe id" } }),
        );
    }
    let lookup = slug.clone();
    match pod
        .blocking(move |s| s.with_conn(|c| db::recipe_screen(c, &lookup)))
        .await
    {
        Ok(Some(bundle)) => Json(bundle).into_response(),
        Ok(None) => pod_core::json_error(
            StatusCode::NOT_FOUND,
            json!({ "error": "recipe_not_found", "id": slug }),
        ),
        Err(e) => e.into_response(),
    }
}
