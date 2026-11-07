//! The bundled demo layout (`demo/welcome.json`), which the viewer and the
//! Android activity open when given nothing else.

use protocol::{DataBundle, UiNode};
use serde_json::Value;

const WELCOME: &str = include_str!("../demo/welcome.json");

/// `{"layout", "data"}` as a layout id, its root and its data.
pub fn parse(v: &Value) -> Result<(String, UiNode, DataBundle), serde_json::Error> {
    let root: UiNode = serde_json::from_value(v["layout"].clone())?;
    let data: DataBundle = match v.get("data") {
        Some(d) => serde_json::from_value(d.clone())?,
        None => DataBundle::default(),
    };
    let id = v["layout"]["id"].as_str().unwrap_or("layout").to_owned();
    Ok((id, root, data))
}

pub fn welcome() -> Result<(String, UiNode, DataBundle), serde_json::Error> {
    parse(&serde_json::from_str(WELCOME)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_demo_lays_out() {
        let (id, root, data) = super::welcome().unwrap();
        assert_eq!(id, "welcome");
        let mut s = crate::Session::new(&id, root, data, crate::Viewport::PHONE);
        let scene = s.scene().unwrap();
        assert!(scene.find_text("Draw SDF quads").is_some());
        assert!(scene.find_node("show=open").is_some());
    }
}
