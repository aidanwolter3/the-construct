use construct_render::{Input, Scene, SceneBuilder, Viewport};
use protocol::{DataBundle, UiNode};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// A fixture's layout and data (`tests/data/<name>.json`).
pub fn fixture(name: &str) -> (UiNode, DataBundle) {
    let text = std::fs::read_to_string(data_dir().join(format!("{name}.json"))).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    (
        serde_json::from_value(v["layout"].clone()).unwrap(),
        serde_json::from_value(v["data"].clone()).unwrap_or_default(),
    )
}

pub fn build(
    root: &UiNode,
    data: &DataBundle,
    form: &BTreeMap<String, String>,
    vp: Viewport,
) -> Scene {
    SceneBuilder::new()
        .build(&Input {
            root,
            data,
            form,
            route: &BTreeMap::new(),
            layouts: &BTreeMap::new(),
            scroll: &BTreeMap::new(),
            viewport: vp,
            dark: false,
            item: None,
            insets: [0.0; 4],
        })
        .unwrap()
}

pub const EPS: f32 = 0.01;

pub fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < EPS
}
