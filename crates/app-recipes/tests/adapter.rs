//! The recipes adapter on a pod: its layout files, its migrations (schema +
//! import) and its two endpoints.

use std::sync::Arc;

use app_recipes::{FILES, RecipesAdapter};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pod_core::layouts::parse_files;
use pod_core::{AppAdapter, AppState, Catalog};
use protocol::{DataBundle, UiNode, WorkspaceSpec};
use tower::ServiceExt;

fn adapters() -> Vec<Arc<dyn AppAdapter>> {
    vec![Arc::new(RecipesAdapter::new())]
}

#[test]
fn every_file_in_layouts_recipes_is_embedded() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../layouts/recipes");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|f| f.unwrap().file_name().into_string().unwrap())
        .collect();
    on_disk.sort();
    let embedded: Vec<String> = FILES.iter().map(|f| format!("{}.json", f.name)).collect();
    assert_eq!(
        on_disk, embedded,
        "list every layouts/recipes file in app_recipes::FILES"
    );
    for f in FILES {
        let text = std::fs::read_to_string(dir.join(format!("{}.json", f.name))).unwrap();
        assert_eq!(text, f.text);
    }
}

#[test]
fn layout_files_parse_and_validate() {
    let catalog = Catalog::new(adapters()).unwrap();
    let files = parse_files(&catalog, FILES).unwrap();
    assert_eq!(files.len(), FILES.len());
    assert_eq!(files[0].schema, 3);
    assert_eq!(files.last().unwrap().schema, protocol::SCHEMA_VERSION);
}

async fn get(state: &Arc<AppState>, path: &str) -> (StatusCode, serde_json::Value) {
    get_as(state, path, protocol::SCHEMA_VERSION).await
}

async fn get_as(state: &Arc<AppState>, path: &str, schema: u32) -> (StatusCode, serde_json::Value) {
    let req = Request::get(path)
        .header(protocol::SCHEMA_HEADER, schema.to_string())
        .body(Body::empty())
        .unwrap();
    let resp = pod_core::app(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn the_workspace_lists_every_recipe_and_a_screen_loads_one() {
    let state = Arc::new(AppState::in_memory(adapters()));
    assert_eq!(state.store().adapter_version("recipes").unwrap(), 2);

    let (status, body) = get(&state, "/v1/workspaces/recipes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
    assert_eq!(spec.queries["recipes:all"].len(), 81);
    let salad = &spec.entities["recipes://recipes/three-bean-salad"];
    assert_eq!(salad.get_str("title"), Some("Three Bean Salad"));

    let (status, body) = get(&state, "/v1/recipes/screens/recipe?id=moussaka").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data: DataBundle = serde_json::from_value(body).unwrap();
    assert_eq!(
        data.entities["recipes://recipes/moussaka"].get_str("yield_text"),
        Some("Serves 8")
    );
    let first = &data.queries["recipe:moussaka:ingredients"][0];
    assert_eq!(
        data.entities[first].get_str("text"),
        Some("8 garlic cloves, finely grated (divided)")
    );
    assert_eq!(
        data.entities[first].get_str("group"),
        Some("Eggplant and Lamb")
    );
    assert_eq!(data.queries["recipe:moussaka:steps"].len(), 7);

    let (status, body) = get(&state, "/v1/recipes/screens/recipe?id=nope").await;
    assert_eq!(
        (status, &body["error"]),
        (StatusCode::NOT_FOUND, &"recipe_not_found".into())
    );
    let (status, _) = get(&state, "/v1/recipes/screens/recipe?id=..%2Fx").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Schema 5 clients get the search box (0003, D43), schema 4 clients
/// tappable rows (0002, D42) and schema 3 clients the "View" buttons (0001).
#[tokio::test]
async fn each_schema_gets_the_newest_layout_it_draws() {
    let state = Arc::new(AppState::in_memory(adapters()));
    for (schema, search, tappable) in [
        (protocol::SCHEMA_VERSION, true, true),
        (4, false, true),
        (3, false, false),
    ] {
        let (status, body) = get_as(&state, "/v1/workspaces/recipes", schema).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
        assert_eq!(protocol::required_schema(&spec.workspace), schema);
        let walk = spec.workspace.walk();
        let types: Vec<String> = walk.iter().map(|n| n.type_tag()).collect();
        assert_eq!(
            types.iter().any(|t| t == "core://Tappable"),
            tappable,
            "schema {schema}"
        );
        assert_eq!(
            types.iter().any(|t| t == "core://Button"),
            !tappable,
            "schema {schema}"
        );
        let filter = walk.iter().find_map(|n| match n {
            UiNode::List {
                filter: Some(f), ..
            } => Some(f),
            _ => None,
        });
        assert_eq!(filter.is_some(), search, "schema {schema}");
        if let Some(f) = filter {
            // The box edits the form value the filter reads, and the field exists.
            assert_eq!(f.value, "$form.q");
            assert!(walk.iter().any(|n| matches!(n,
                UiNode::TextField { field, on_submit: None, .. } if field == "q")));
            let recipe = spec.entities.values().next().unwrap();
            for field in &f.fields {
                assert!(recipe.get_str(field).is_some(), "{field}");
            }
        }
    }
}

#[tokio::test]
async fn reopening_the_store_does_not_import_twice() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let state = Arc::new(AppState::open_dir(dir.path(), adapters()).unwrap());
        let n: i64 = state
            .store()
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM recipes", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 81);
    }
}
