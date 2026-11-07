//! Phase 3's engine half: hit-testing, taps, reflow on resize (no window).

use construct_render::Viewport;
use construct_render::interact::{Effect, Session, Target};
use serde_json::json;

use crate::common::fixture;

fn tasks() -> Session {
    let (root, data) = fixture("task_list");
    Session::new("tasks", root, data, Viewport::PHONE)
}

#[test]
fn hit_test_finds_the_button_under_a_point() {
    let mut s = tasks();
    let scene = s.scene().unwrap();
    let add = scene.find_node("add_task").unwrap().clone();
    let (cx, cy) = (
        add.bounds[0] + add.bounds[2] / 2.0,
        add.bounds[1] + add.bounds[3] / 2.0,
    );
    assert_eq!(
        scene.hit_test(cx, cy).map(|n| n.id.as_str()),
        Some("add_task")
    );
    // A point on a card's title hits the card (the tappable around it).
    let title = scene.find_text("Call the plumber").unwrap().clone();
    let hit = scene
        .hit_test(title.bounds[0] + 2.0, title.bounds[1] + 2.0)
        .unwrap();
    assert_eq!(hit.id, "task_5");
    assert!(scene.hit_test(-5.0, -5.0).is_none());
}

#[test]
fn tapping_a_chip_sets_the_filter_and_redraws() {
    let mut s = tasks();
    let before = s.render_cpu().unwrap();
    let out = s.tap(&Target::Id("filter=started".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::FormSet {
            field: "filter".into(),
            value: "started".into()
        }]
    );
    assert_eq!(s.form.get("filter").map(String::as_str), Some("started"));
    let scene = s.scene().unwrap();
    assert!(scene.find_node("filter=started").unwrap().selected);
    assert!(scene.find_node("task_2").is_none());
    let after = s.render_cpu().unwrap();
    assert!(construct_render::encode::diff_ratio(&before, &after, 0) > 0.01);
    // Tapping by its label does the same.
    let out = s.tap(&Target::Text("Done".into())).unwrap();
    assert_eq!(out.node.unwrap().id, "filter=done");
}

#[test]
fn intents_to_the_pod_are_reported_not_sent() {
    let mut s = tasks();
    let out = s.tap(&Target::Id("task_1".into())).unwrap();
    let [Effect::NotSent { intent }] = out.effects.as_slice() else {
        panic!("{:?}", out.effects);
    };
    // Bound to the item it was drawn for.
    assert_eq!(intent["target_uri"], json!("demo://tasks/1"));
}

#[test]
fn local_set_flips_a_switch() {
    let (root, data) = fixture("home");
    let mut s = Session::new("home", root, data, Viewport::PHONE);
    let sw = s.scene().unwrap().find_text("Quiet hours").unwrap().clone();
    assert!(
        !s.scene()
            .unwrap()
            .semantics
            .iter()
            .any(|n| n.node_type == "core://Switch" && n.selected)
    );
    let switch = s
        .scene()
        .unwrap()
        .semantics
        .iter()
        .find(|n| n.node_type == "core://Switch")
        .unwrap()
        .clone();
    s.tap(&Target::Point(
        switch.bounds[0] + 4.0,
        switch.bounds[1] + 4.0,
    ))
    .unwrap();
    assert_eq!(s.data.entities["demo://me"].get_bool("quiet"), Some(true));
    assert!(
        s.scene()
            .unwrap()
            .semantics
            .iter()
            .any(|n| n.node_type == "core://Switch" && n.selected)
    );
    assert!(sw.bounds[2] > 0.0);
}

#[test]
fn resize_reflows_wider() {
    let mut s = tasks();
    let narrow = s.scene().unwrap().find_node("task_4").unwrap().bounds;
    let tall = s
        .scene()
        .unwrap()
        .find_text("Book the van for the move to Oakridge Lane")
        .unwrap()
        .bounds;
    s.resize(Viewport {
        width: 800.0,
        height: 844.0,
        scale: 2.0,
    });
    let wide = s.scene().unwrap().find_node("task_4").unwrap().bounds;
    let one_line = s
        .scene()
        .unwrap()
        .find_text("Book the van for the move to Oakridge Lane")
        .unwrap()
        .bounds;
    assert!((wide[2] - (800.0 - 32.0)).abs() < 0.01, "{wide:?}");
    assert!(wide[2] > narrow[2]);
    assert!(
        one_line[3] < tall[3],
        "the title fits on one line once wider"
    );
    // Many resizes keep one scene, rebuilt from scratch each time.
    for w in [400.0, 500.0, 600.0, 390.0] {
        s.resize(Viewport {
            width: w,
            height: 844.0,
            scale: 2.0,
        });
        assert!((s.scene().unwrap().width - w).abs() < 0.01);
    }
}

#[test]
fn scrolling_moves_the_list() {
    let (root, mut data) = fixture("task_list");
    // Enough tasks to overflow the screen.
    for i in 10..30 {
        let uri = format!("demo://tasks/{i}");
        let mut e = data.entities["demo://tasks/2"].clone();
        e.set("id", i.to_string());
        data.entities.insert(uri.clone(), e);
        data.queries.get_mut("tasks:all").unwrap().push(uri);
    }
    let mut s = Session::new("tasks", root, data, Viewport::PHONE);
    let y0 = s.scene().unwrap().find_node("task_1").unwrap().bounds[1];
    let out = s.scroll_by(None, 100.0).unwrap();
    assert!(matches!(&out.effects[0], Effect::Scrolled { offset, .. } if *offset == 100.0));
    let y1 = s.scene().unwrap().find_node("task_1").map(|n| n.bounds[1]);
    assert!(y1.is_none_or(|y| y < y0), "{y0} -> {y1:?}");
}

#[test]
fn window_events_drive_the_session() {
    use construct_render::viewer::{Controller, Event};
    let (root, data) = fixture("home");
    let mut c = Controller::new(Session::new("home", root, data, Viewport::PHONE));
    // A 400pt window at 2x, then 800pt: the root reflows.
    c.handle(Event::Resized {
        width_px: 800,
        height_px: 1688,
        scale: 2.0,
    });
    let field = c
        .session
        .scene()
        .unwrap()
        .find_node("field:note")
        .unwrap()
        .bounds;
    assert!((field[2] - (400.0 - 32.0)).abs() < 0.01);
    c.handle(Event::Resized {
        width_px: 1600,
        height_px: 1688,
        scale: 2.0,
    });
    let field = c
        .session
        .scene()
        .unwrap()
        .find_node("field:note")
        .unwrap()
        .bounds;
    assert!((field[2] - (800.0 - 32.0)).abs() < 0.01, "{field:?}");
    // A click on the field focuses it; typed text fills it; Backspace removes a character.
    c.handle(Event::Tap {
        x: field[0] + 10.0,
        y: field[1] + 10.0,
    });
    assert_eq!(c.focused.as_deref(), Some("note"));
    c.handle(Event::Text("Buy ".into()));
    c.handle(Event::Text("figs!".into()));
    c.handle(Event::Backspace);
    assert_eq!(c.session.form["note"], "Buy figs");
    assert!(c.session.scene().unwrap().find_text("Buy figs").is_some());
    // A click elsewhere unfocuses; typing then does nothing.
    c.handle(Event::Tap { x: 5.0, y: 5.0 });
    assert!(c.handle(Event::Text("x".into())).is_none());
    // Back at the root does nothing.
    let out = c.handle(Event::Back).unwrap();
    assert_eq!(out.effects, vec![Effect::Nothing]);
}

#[test]
fn opening_a_layout_pushes_it_and_back_pops() {
    let (root, data) = fixture("task_list");
    let (home, _) = fixture("home");
    let mut s = Session::new("tasks", root, data, Viewport::PHONE);
    s.layouts.insert("new_task".into(), home);
    let out = s.tap(&Target::Id("add_task".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::Opened {
            layout: "new_task".into()
        }]
    );
    assert_eq!(s.current(), "new_task");
    assert!(s.scene().unwrap().find_text("Quiet hours").is_some());
    s.dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    assert_eq!(s.current(), "tasks");
}
