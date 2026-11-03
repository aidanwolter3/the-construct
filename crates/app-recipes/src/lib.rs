//! The recipes adapter (docs/DECISIONS.md D41): the family recipe collection,
//! kept in the pod's own SQLite (`recipes_*` tables), served as the `recipes`
//! app. There is no upstream and no sign-in.
//!
//! - `GET /v1/workspaces/recipes`: every recipe by category
//!   (`queries["recipes:all"]`, `recipes://recipes/<slug>` entities).
//! - `GET /v1/recipes/screens/recipe?id=<slug>`: one recipe's screen data
//!   ([`db::recipe_screen`]).
//!
//! The recipes arrive by migration: step 2 imports `data/import-0001.json`.

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
use pod_core::store::Migration;
use pod_core::{AppAdapter, AppInfo, AuthPolicy, BoxFuture, Hydration, Pod, RequestCtx};
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

    fn migrations(&self) -> &'static [Migration] {
        db::MIGRATIONS
    }

    fn hydrate<'a>(
        &'a self,
        pod: &'a Pod,
        _app_id: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
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
