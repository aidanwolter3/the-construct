//! An instant swap (decision `instant-swap`): `action.construct.swap` asks
//! the loader to show an edit group's screens before or after it, with the
//! row's bindings read; a loader that can't swap does nothing (the Undo or
//! Redo beside it still goes to the pod).

use construct_render::Viewport;
use construct_render::interact::{Done, Effect, Loader, Session, Target, Write, Written};
use protocol::{DataBundle, UiNode};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// The swaps a loader was asked for: (group, to).
type Swaps = Arc<Mutex<Vec<(String, String)>>>;

struct Swapper {
    swaps: Swaps,
    can: bool,
}

impl Loader for Swapper {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        Err(format!("no layout {id}"))
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        Ok(DataBundle::default())
    }
    fn send(&self, _write: Write, done: Done) -> bool {
        done(Written {
            result: Ok(json!({})),
            refreshed: Vec::new(),
        });
        true
    }
    fn swap(&self, group: &str, to: &str) -> bool {
        self.swaps
            .lock()
            .unwrap()
            .push((group.to_owned(), to.to_owned()));
        self.can
    }
}

fn session(can: bool) -> (Session, Swaps) {
    let root: UiNode = serde_json::from_value(json!({ "type": "core://List", "query_key": "fixed",
        "item_template": { "type": "core://Button", "label": "Undo",
            "on_click": { "intent": "action.construct.swap", "group": "$item.edit_group", "to": "before" } } }))
    .unwrap();
    let data: DataBundle = serde_json::from_value(json!({
        "entities": { "collections://anticipations/a1": { "edit_group": "eg_1" } },
        "queries": { "fixed": ["collections://anticipations/a1"] } }))
    .unwrap();
    let swaps = Arc::new(Mutex::new(Vec::new()));
    let mut s = Session::new("home", root, data, Viewport::PHONE);
    s.animate = false;
    s.loader = Some(Box::new(Swapper {
        swaps: swaps.clone(),
        can,
    }));
    (s, swaps)
}

#[test]
fn undo_swaps_the_rows_group_on_the_device() {
    let (mut s, swaps) = session(true);
    let out = s.tap(&Target::Text("Undo".into())).unwrap();
    assert_eq!(
        *swaps.lock().unwrap(),
        vec![("eg_1".to_owned(), "before".to_owned())]
    );
    assert!(
        out.effects.contains(&Effect::Device {
            command: "construct_swap before".into()
        }),
        "{:?}",
        out.effects
    );
}

#[test]
fn a_loader_that_cant_swap_does_nothing() {
    let (mut s, swaps) = session(false);
    let out = s.tap(&Target::Text("Undo".into())).unwrap();
    assert_eq!(swaps.lock().unwrap().len(), 1);
    assert!(
        out.effects.iter().all(|e| matches!(e, Effect::Nothing)),
        "{:?}",
        out.effects
    );
}
