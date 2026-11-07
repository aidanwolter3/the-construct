//! `core://MediaPicker` in a live session: picked files are staged through
//! the loader, drawn as thumbnails with a remove button, and a create's
//! `attachments` hands their ids over (D49, D51).

use construct_render::Viewport;
use construct_render::interact::{Effect, Loader, Session, StageDone, Staged, Target};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A loader that stages at once and keeps what it was given.
#[derive(Default)]
struct Log {
    staged: Vec<PathBuf>,
    discarded: Vec<String>,
    taken: Vec<Value>,
}

struct Engine(Arc<Mutex<Log>>);

impl Loader for Engine {
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
        Err(format!("no layout {id}"))
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        Ok(DataBundle::default())
    }
    fn take(&self, intent: &Value) -> bool {
        self.0.lock().unwrap().taken.push(intent.clone());
        true
    }
    fn stage(&self, file: &Path, done: StageDone) -> bool {
        let mut log = self.0.lock().unwrap();
        log.staged.push(file.to_owned());
        let id = format!("att{}", log.staged.len());
        drop(log);
        if file.extension().is_some_and(|e| e == "txt") {
            done(Err("that isn't a photo".into()));
        } else {
            done(Ok(Staged {
                uri: format!("construct://attachments/{id}"),
                url: format!("file:///staged/{id}.jpg"),
                id,
                width: 400,
                height: 300,
            }));
        }
        true
    }
    fn discard(&self, id: &str) {
        self.0.lock().unwrap().discarded.push(id.to_owned());
    }
}

fn compose() -> UiNode {
    serde_json::from_value(json!({ "type": "core://Column", "children": [
        { "type": "core://MediaPicker", "field": "photos", "max_items": 2, "label": "Add photos" },
        { "type": "core://TextField", "field": "caption", "placeholder": "Caption" },
        { "type": "core://Button", "label": "Share", "on_click": {
            "intent": "action.mutation.create", "query_key": "timeline", "position": "start",
            "entity_uri": "x://posts/pending-$nonce",
            "entity": { "caption": "$form?.caption", "media_uris": "$attachments.photos" },
            "attachments": { "photos": "$form.photos" },
            "endpoint": "/v1/posts/create",
            "payload": { "caption": "$form?.caption", "media": "$attachments.photos" } } }
    ] }))
    .unwrap()
}

fn session() -> (Session, Arc<Mutex<Log>>) {
    let log = Arc::new(Mutex::new(Log::default()));
    let mut s = Session::new("home", compose(), DataBundle::default(), Viewport::PHONE);
    s.loader = Some(Box::new(Engine(log.clone())));
    (s, log)
}

fn labels(s: &mut Session) -> Vec<String> {
    s.scene()
        .unwrap()
        .semantics
        .iter()
        .filter_map(|n| n.text_content.clone())
        .collect()
}

#[test]
fn picked_photos_are_staged_shown_and_posted() {
    let (mut s, log) = session();
    let out = s.tap(&Target::Id("media:photos".into())).unwrap();
    assert!(
        out.effects.contains(&Effect::PickMedia {
            field: "photos".into(),
            max: 2
        }),
        "{:?}",
        out.effects
    );
    s.picked(vec![Ok(PathBuf::from("/picked/a.jpg"))]);
    s.poll();
    assert_eq!(s.media_ids("photos"), vec!["att1"]);
    assert!(!s.busy());
    // A thumbnail with its remove button, and room for one more.
    let shown = labels(&mut s);
    for l in ["Photo 1 of 1", "Remove photo 1", "Add photos"] {
        assert!(shown.iter().any(|t| t == l), "{l} not in {shown:?}");
    }
    // The remove button sits on its photo's top right corner.
    let bounds = |s: &mut Session, id: &str| {
        let scene = s.scene().unwrap();
        scene.semantics.iter().find(|n| n.id == id).unwrap().bounds
    };
    let tile = bounds(&mut s, "media:photos/0");
    let rm = bounds(&mut s, "media-remove:att1");
    assert!(
        rm[0] > tile[0] && rm[0] + rm[2] <= tile[0] + tile[2] && rm[1] >= tile[1],
        "{rm:?} not on {tile:?}"
    );
    let url = s.data.entities["construct://attachments/att1"].get_str("url");
    assert_eq!(url, Some("file:///staged/att1.jpg"));

    // The second fills the picker: its add tile goes; the picker asks for
    // what is left.
    let out = s.tap(&Target::Id("media:photos".into())).unwrap();
    assert!(out.effects.contains(&Effect::PickMedia {
        field: "photos".into(),
        max: 1
    }));
    s.picked(vec![
        Ok(PathBuf::from("/picked/b.png")),
        Ok(PathBuf::from("/picked/c.png")),
    ]);
    s.poll();
    assert_eq!(s.media_ids("photos"), vec!["att1", "att2"]);
    assert_eq!(log.lock().unwrap().discarded, vec!["att3"]);
    assert!(!labels(&mut s).iter().any(|t| t == "Add photos"));

    // Share: the engine gets the ids; the post shows them at once; the
    // picker is empty again.
    s.tap(&Target::Text("Share".into())).unwrap();
    let taken = log.lock().unwrap().taken[0].clone();
    assert_eq!(taken["attachments"]["photos"], json!(["att1", "att2"]));
    assert_eq!(taken["payload"]["media"], "$attachments.photos");
    let post = s.data.queries["timeline"][0].clone();
    assert_eq!(
        s.data.entities[&post].get("media_uris"),
        Some(&json!([
            "construct://attachments/att1",
            "construct://attachments/att2"
        ]))
    );
    assert!(s.media_ids("photos").is_empty());
}

#[test]
fn a_photo_can_be_removed_and_a_bad_one_is_reported() {
    let (mut s, log) = session();
    s.tap(&Target::Id("media:photos".into())).unwrap();
    s.picked(vec![
        Ok(PathBuf::from("/picked/a.jpg")),
        Ok(PathBuf::from("/picked/notes.txt")),
    ]);
    s.poll();
    assert_eq!(s.media_ids("photos"), vec!["att1"]);
    // The failure shows in a notice.
    assert!(labels(&mut s).iter().any(|t| t == "Couldn't add a photo"));
    s.tap(&Target::Text("OK".into())).unwrap();

    s.tap(&Target::Id("media-remove:att1".into())).unwrap();
    assert!(s.media_ids("photos").is_empty());
    assert_eq!(log.lock().unwrap().discarded, vec!["att1"]);
    assert!(!s.data.entities.contains_key("construct://attachments/att1"));
}

#[test]
fn a_picker_without_a_loader_does_nothing() {
    let mut s = Session::new("home", compose(), DataBundle::default(), Viewport::PHONE);
    s.tap(&Target::Id("media:photos".into())).unwrap();
    assert!(!s.pick_here(2));
    s.picked(vec![Ok(PathBuf::from("/picked/a.jpg"))]);
    s.poll();
    assert!(s.media_ids("photos").is_empty());
}
