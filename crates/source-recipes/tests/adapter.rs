//! The recipes adapter on a pod: its layout files, its migrations (a new
//! pod starts empty) and its two endpoints, over the made-up test recipes.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pod_core::{AppState, Catalog, Source};
use protocol::{DataBundle, UiNode, WorkspaceSpec};
use source_recipes::{FILES, RecipesAdapter};
use tower::ServiceExt;

fn adapters() -> Vec<Arc<dyn Source>> {
    vec![Arc::new(RecipesAdapter::new())]
}

/// A pod with the made-up test recipes.
fn seeded() -> Arc<AppState> {
    let state = Arc::new(AppState::in_memory(adapters()));
    state
        .store()
        .with_conn(source_recipes::db::seed_test_recipes)
        .unwrap();
    state
}

fn recipe_count(state: &AppState) -> i64 {
    state
        .store()
        .with_conn(|c| c.query_row("SELECT COUNT(*) FROM recipes", [], |r| r.get(0)))
        .unwrap()
}

#[test]
fn every_layout_file_is_embedded() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts/recipes");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|f| f.unwrap().file_name().into_string().unwrap())
        .collect();
    on_disk.sort();
    let embedded: Vec<String> = FILES.iter().map(|f| format!("{}.json", f.name)).collect();
    assert_eq!(
        on_disk, embedded,
        "build.rs embeds every file in layouts/recipes"
    );
    for f in FILES {
        let text = std::fs::read_to_string(dir.join(format!("{}.json", f.name))).unwrap();
        assert_eq!(text, f.text);
    }
}

#[test]
fn layout_files_parse_and_validate() {
    let catalog = Catalog::new(adapters()).unwrap();
    let core = pod_core::construct::core::load_with(&catalog, FILES).unwrap();
    assert_eq!(core.names["@core/recipes"].len(), FILES.len());
    // The newest recipes layout pins the search box and chips.
    let newest = core.current("@core/recipes").unwrap();
    assert!(core.features[newest].contains(protocol::feature::STICKY));
}

/// The recipes blueprint as a layout of the pod, drawn as a device draws it.
async fn recipes(state: &Arc<AppState>) -> (StatusCode, serde_json::Value) {
    let id = pod_core::testing::use_blueprint(state, "recipes").await;
    pod_core::testing::layout_for(&pod_core::app(state.clone()), &id, None).await
}

async fn get(state: &Arc<AppState>, path: &str) -> (StatusCode, serde_json::Value) {
    get_as(state, path, &protocol::features_header()).await
}

/// As a client drawing `features` (a [`protocol::FEATURES_HEADER`] value).
async fn get_as(
    state: &Arc<AppState>,
    path: &str,
    features: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::get(path)
        .header(protocol::FEATURES_HEADER, features)
        .body(Body::empty())
        .unwrap();
    let resp = pod_core::app(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn the_workspace_lists_every_recipe_and_a_screen_loads_one() {
    let state = seeded();
    assert_eq!(state.store().adapter_version("recipes").unwrap(), 2);

    let (status, body) = recipes(&state).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
    assert_eq!(spec.queries["recipes:all"].len(), 8);
    let salad = &spec.entities["recipes://recipes/test-garden-salad"];
    assert_eq!(salad.get_str("title"), Some("Test Garden Salad"));

    let (status, body) = get(&state, "/v1/recipes/screens/recipe?id=test-garden-salad").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data: DataBundle = serde_json::from_value(body).unwrap();
    assert_eq!(
        data.entities["recipes://recipes/test-garden-salad"].get_str("yield_text"),
        Some("Serves 4-6")
    );
    let first = &data.queries["recipe:test-garden-salad:ingredients"][0];
    assert_eq!(
        data.entities[first].get_str("text"),
        Some("1 head romaine lettuce, chopped")
    );
    assert_eq!(data.entities[first].get_str("group"), Some("Salad"));
    assert_eq!(data.queries["recipe:test-garden-salad:steps"].len(), 6);

    let (status, body) = get(&state, "/v1/recipes/screens/recipe?id=nope").await;
    assert_eq!(
        (status, &body["error"]),
        (StatusCode::NOT_FOUND, &"recipe_not_found".into())
    );
    let (status, _) = get(&state, "/v1/recipes/screens/recipe?id=..%2Fx").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The newest recipes layout: the search box and chips pinned at the top.
#[tokio::test]
async fn each_client_gets_the_newest_layout_it_draws() {
    let state = seeded();
    // A build from before issue #126 (no `page`, but the workspace's own
    // features): these check the workspace itself, not the plumbing a
    // client that draws it gets (decision `plumbing-and-porcelain`).
    let without = |missing: &[&str]| {
        protocol::FEATURES
            .iter()
            .filter(|f| !missing.contains(f) && **f != "page")
            .chain(&protocol::EXPANDED_FEATURES)
            .copied()
            .collect::<Vec<_>>()
            .join(",")
    };
    let _ = without;
    for (missing, sticky_drawn, search, tappable) in [(&[][..], true, true, true)] {
        let schema = format!("{missing:?}");
        let (status, body) = recipes(&state).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let spec: WorkspaceSpec = serde_json::from_value(body).unwrap();
        for f in protocol::required_features(&spec.workspace) {
            assert!(!missing.contains(&f), "{schema}: {f}");
        }
        let walk = spec.workspace.walk();
        let types: Vec<String> = walk.iter().map(|n| n.type_tag()).collect();
        // Schema 11: the search box and chips are pinned together.
        let sticky = walk.iter().find_map(|n| match n {
            UiNode::Sticky { content } => Some(content.walk()),
            _ => None,
        });
        assert_eq!(sticky.is_some(), sticky_drawn, "{schema}");
        if let Some(pinned) = sticky {
            assert!(pinned.iter().any(|n| matches!(n, UiNode::TextField { .. })));
            assert!(
                pinned
                    .iter()
                    .any(|n| matches!(n, UiNode::ChoiceChips { .. }))
            );
        }
        // A recipe row is a tappable row, not a button (the page's own
        // chrome may hold buttons).
        let rows: Vec<String> = walk
            .iter()
            .find_map(|n| match n {
                UiNode::List {
                    item_template,
                    filter: Some(_),
                    ..
                } => Some(item_template.walk().iter().map(|n| n.type_tag()).collect()),
                _ => None,
            })
            .unwrap();
        assert!(!types.is_empty());
        assert_eq!(
            rows.iter().any(|t| t == "core://Tappable"),
            tappable,
            "{schema}"
        );
        assert_eq!(
            rows.iter().any(|t| t == "core://Button"),
            !tappable,
            "{schema}"
        );
        let filter = walk.iter().find_map(|n| match n {
            UiNode::List {
                filter: Some(f), ..
            } => Some(f),
            _ => None,
        });
        assert_eq!(filter.is_some(), search, "{schema}");
        if let Some(f) = filter {
            // The box edits the form value the filter reads, and the field exists.
            assert_eq!(f.value, "$form.q");
            assert!(walk.iter().any(|n| matches!(n,
                UiNode::TextField { field, on_submit: None, .. } if field == "q")));
            let recipe = spec
                .entities
                .iter()
                .find(|(k, _)| k.starts_with("recipes://recipes/"))
                .map(|(_, e)| e)
                .unwrap();
            for field in &f.fields {
                assert!(recipe.get_str(field).is_some(), "{field}");
            }
        }
        let chips = walk.iter().find_map(|n| match n {
            UiNode::ChoiceChips { field, options, .. } => Some((field, options)),
            _ => None,
        });
        assert_eq!(chips.is_some(), search, "{schema}");
        if let Some((field, options)) = chips {
            // Every chip but "All" picks a category that has recipes, through
            // the list's `where`.
            let where_ = walk
                .iter()
                .find_map(|n| match n {
                    UiNode::List {
                        where_: Some(w), ..
                    } => w.conditions().first(),
                    _ => None,
                })
                .unwrap();
            assert_eq!(where_.equals, format!("$form.{field}"));
            let shorts: std::collections::HashSet<&str> = spec
                .entities
                .iter()
                .filter(|(k, _)| k.starts_with("recipes://recipes/"))
                .map(|(_, e)| e)
                .filter_map(|e| e.get_str(&where_.field))
                .collect();
            assert_eq!(options[0].value, "");
            for o in &options[1..] {
                assert!(shorts.contains(o.value.as_str()), "{}", o.value);
            }
            assert_eq!(options.len() - 1, shorts.len(), "a chip per category");
        }
    }
}

/// A new pod has no recipes; recipes a pod holds stay when it is reopened
/// (the retired import, migration step 2, runs once and adds nothing).
#[tokio::test]
async fn a_new_pod_starts_empty_and_keeps_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open_dir(dir.path(), adapters()).unwrap();
    assert_eq!(state.store().adapter_version("recipes").unwrap(), 2);
    assert_eq!(recipe_count(&state), 0);
    let (status, body) = recipes(&Arc::new(state)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["queries"]["recipes:all"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{body}"
    );
    let state = AppState::open_dir(dir.path(), adapters()).unwrap();
    state
        .store()
        .with_conn(source_recipes::db::seed_test_recipes)
        .unwrap();
    drop(state);
    let state = AppState::open_dir(dir.path(), adapters()).unwrap();
    assert_eq!(recipe_count(&state), 8);
}
