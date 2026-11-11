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
    // The field, beside its 48pt Send button.
    assert!((field[2] - (400.0 - 32.0 - 48.0)).abs() < 0.01, "{field:?}");
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
    assert!((field[2] - (800.0 - 32.0 - 48.0)).abs() < 0.01, "{field:?}");
    // A click on the field focuses it; typed text fills it; Backspace removes a character.
    c.handle(Event::Tap {
        x: field[0] + 10.0,
        y: field[1] + 10.0,
    });
    assert_eq!(c.focused(), Some("note"));
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

/// A pod stand-in: one layout with a screen, and the screen's data path.
struct FakePod;

impl construct_render::interact::Loader for FakePod {
    fn layout(&self, id: &str) -> Result<(protocol::UiNode, protocol::DataBundle), String> {
        if id != "c-tasks" {
            return Err(format!("no layout {id}"));
        }
        let root = serde_json::from_value(json!({
            "type": "core://Page", "id": "c-tasks", "root": { "type": "core://Column", "children": [
                { "type": "core://Conditional", "value": "${local://nav#pushed}", "content":
                    { "type": "core://IconButton", "icon": "arrow_back", "content_description": "Back", "tag": "back",
                      "on_click": { "intent": "action.navigation.back" } } },
                { "type": "core://List", "query_key": "tasks", "item_template": {
                    "type": "core://Tappable", "tag": "open_$item.id",
                    "on_click": { "intent": "action.navigation.open", "presentation": "Push", "target_uri": "demo://tasks/$item.id" },
                    "content": { "type": "core://Text", "value_expr": "$item.title", "style": "Body" } } } ] },
            "screens": [{ "id": "task", "route": "demo://tasks/{id}", "data": "/v1/tasks/row?id=$route.id",
                "item": "demo://tasks/$route.id",
                "root": { "type": "core://Column", "children": [
                    { "type": "core://Text", "value_expr": "$item.title", "style": "Headline" },
                    { "type": "core://Text", "value_expr": "$item.notes", "style": "Body" },
                    { "type": "core://Text", "value_expr": "id $route.id", "style": "Secondary" } ] } }]
        }))
        .map_err(|e| e.to_string())?;
        let data = serde_json::from_value(json!({
            "entities": { "demo://tasks/7": { "id": "7", "title": "Fix the gate" } },
            "queries": { "tasks": ["demo://tasks/7"] } }))
        .unwrap();
        Ok((root, data))
    }

    fn data(&self, path: &str) -> Result<protocol::DataBundle, String> {
        assert_eq!(path, "/v1/tasks/row?id=7");
        Ok(serde_json::from_value(json!({
            "entities": { "demo://tasks/7": { "id": "7", "title": "Fix the gate", "notes": "The latch sticks." } },
            "queries": {} }))
        .unwrap())
    }
}

#[test]
fn a_live_session_opens_layouts_and_screens_with_their_data() {
    let home: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "home", "root": { "type": "core://Button", "label": "Tasks",
            "on_click": { "intent": "action.navigation.open", "layout": "c-tasks", "presentation": "Push" } } }))
    .unwrap();
    let mut s = Session::new("home", home, Default::default(), Viewport::PHONE);
    s.loader = Some(Box::new(FakePod));
    assert!(s.scene().unwrap().find_node("back").is_none());
    let out = s.tap(&Target::Text("Tasks".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::Opened {
            layout: "c-tasks".into()
        }]
    );
    // Pushed: the layout's own Back shows (`local://nav#pushed`).
    assert!(s.scene().unwrap().find_node("back").is_some());
    let out = s.tap(&Target::Id("open_7".into())).unwrap();
    assert_eq!(
        out.effects,
        vec![Effect::OpenedScreen {
            screen: "task".into()
        }]
    );
    let scene = s.scene().unwrap();
    // `$item` is the screen's entity, filled in by its data path; `$route` bound.
    assert!(scene.find_text("Fix the gate").is_some());
    assert!(scene.find_text("The latch sticks.").is_some());
    assert!(scene.find_text("id 7").is_some());
    s.dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    s.dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    assert_eq!(s.current(), "home");
    // A layout the pod doesn't have is reported.
    let out = s
        .dispatch(&json!({ "intent": "action.navigation.open", "layout": "c-nope", "presentation": "Push" }))
        .unwrap();
    assert!(
        matches!(&out.effects[0], Effect::LoadFailed { .. }),
        "{:?}",
        out.effects
    );
}

#[test]
fn images_draw_once_loaded_and_insets_move_the_content() {
    let page: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "p", "root": { "type": "core://Column", "children": [
            { "type": "core://Image", "url": "https://img.example/red.png", "size_dp": 40, "shape": "Rounded" } ] } }))
    .unwrap();
    let mut s = Session::new(
        "p",
        page,
        Default::default(),
        Viewport {
            width: 100.0,
            height: 100.0,
            scale: 1.0,
        },
    );
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let a = asked.clone();
    s.images.set_fetcher(move |u| a.lock().unwrap().push(u));
    let before = s.render_cpu().unwrap();
    assert_eq!(
        asked.lock().unwrap().as_slice(),
        ["https://img.example/red.png"]
    );
    assert_ne!(before.get_pixel(20, 20).0[..3], [255, 0, 0]);
    s.images.insert(
        "https://img.example/red.png",
        Some(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([255, 0, 0, 255]),
        )),
    );
    let after = s.render_cpu().unwrap();
    assert_eq!(after.get_pixel(20, 20).0, [255, 0, 0, 255]);
    // A status bar of 30 points: the image moves below it.
    s.set_insets([30.0, 0.0, 0.0, 0.0]);
    let inset = s.render_cpu().unwrap();
    assert_ne!(inset.get_pixel(20, 20).0[..3], [255, 0, 0]);
    assert_eq!(inset.get_pixel(20, 50).0, [255, 0, 0, 255]);
}

#[test]
fn sticky_pins_scroll_clamps_and_swipes_run_gestures() {
    let rows: Vec<_> = (0..40)
        .map(
            |i| json!({ "type": "core://Text", "value_expr": format!("Row {i}"), "style": "Body" }),
        )
        .collect();
    let mut children = vec![];
    children.push(json!({ "type": "core://Sticky", "content":
        { "type": "core://Text", "value_expr": "Pinned header", "style": "Title" } }));
    children.extend(rows);
    let page: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "p", "root": { "type": "core://Gesture",
            "on_swipe_right": { "intent": "action.local.set", "entity_uri": "demo://ui", "field": "menu", "value_expr": "true" },
            "content": { "type": "core://Column", "children": [
                { "type": "core://Text", "value_expr": "Title", "style": "Headline" },
                { "type": "core://Scroll", "content": { "type": "core://Column", "children": children } } ] } } }))
    .unwrap();
    let mut s = Session::new(
        "p",
        page,
        Default::default(),
        Viewport {
            width: 300.0,
            height: 400.0,
            scale: 1.0,
        },
    );
    let top = s
        .scene()
        .unwrap()
        .find_text("Pinned header")
        .unwrap()
        .bounds[1];
    s.scroll_by(None, 300.0).unwrap();
    let scene = s.scene().unwrap();
    // Pinned at the top of the scrolling view, and drawn over the rows.
    assert_eq!(scene.find_text("Pinned header").unwrap().bounds[1], top);
    assert!(scene.find_text("Row 0").is_none());
    // Scrolling stops at the end: a huge scroll moves only to the end, and more does nothing.
    s.scroll_by(None, 100_000.0).unwrap();
    assert!(s.scene().unwrap().find_text("Row 39").is_some());
    let out = s.scroll_by(None, 50.0).unwrap();
    assert_eq!(out.effects, vec![Effect::Nothing]);
    // A swipe right anywhere runs the gesture's handler.
    let out = s.gesture(150.0, 200.0, "swipe_right").unwrap();
    assert!(
        matches!(&out.effects[0], Effect::LocalSet { .. }),
        "{:?}",
        out.effects
    );
    assert_eq!(
        s.gesture(150.0, 200.0, "swipe_up").unwrap().effects,
        vec![Effect::Nothing]
    );
}

#[test]
fn a_focused_field_draws_a_caret_after_its_text() {
    let page: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "p", "root": { "type": "core://Column", "children": [
            { "type": "core://TextField", "field": "q", "placeholder": "Search" } ] } }))
    .unwrap();
    let mut s = Session::new(
        "p",
        page,
        Default::default(),
        Viewport {
            width: 300.0,
            height: 200.0,
            scale: 1.0,
        },
    );
    let quads = |s: &mut Session| s.scene().unwrap().quads().count();
    let before = quads(&mut s);
    s.tap(&Target::Id("field:q".into())).unwrap();
    assert_eq!(s.focused.as_deref(), Some("q"));
    assert_eq!(quads(&mut s), before + 1, "a caret");
    s.type_text("q", "soup");
    let caret_x = |s: &mut Session| {
        s.scene()
            .unwrap()
            .quads()
            .find(|q| q.rect[2] == 2.0)
            .map(|q| q.rect[0])
            .unwrap()
    };
    let after_soup = caret_x(&mut s);
    s.type_text("q", "soup and bread");
    assert!(caret_x(&mut s) > after_soup + 20.0);
}

#[test]
fn panels_draw_over_their_layout_and_a_scrim_tap_closes_them() {
    let home: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "home", "root": { "type": "core://Column", "children": [
            { "type": "core://Text", "value_expr": "Home underneath", "style": "Title" },
            { "type": "core://Button", "label": "Settings",
              "on_click": { "intent": "action.navigation.open", "layout": "c-tasks", "presentation": "Panel" } } ] } }))
    .unwrap();
    let mut s = Session::new("home", home, Default::default(), Viewport::PHONE);
    s.loader = Some(Box::new(FakePod));
    s.tap(&Target::Text("Settings".into())).unwrap();
    let scene = s.scene().unwrap();
    // Home is drawn but can't be tapped; the panel is 86% of 390 wide at most 340.
    assert!(scene.find_text("Home underneath").is_none());
    assert!(scene.find_node("scrim").is_some());
    let row = scene.find_node("open_7").unwrap().bounds;
    assert!(row[0] >= 0.0 && row[0] + row[2] <= 340.0 + 0.01, "{row:?}");
    // A screen opens inside the panel, and Back returns to the panel.
    s.tap(&Target::Id("open_7".into())).unwrap();
    let title = s.scene().unwrap().find_text("Fix the gate").unwrap().bounds;
    assert!(title[0] < 340.0);
    assert!(s.scene().unwrap().find_node("scrim").is_some());
    s.dispatch(&json!({ "intent": "action.navigation.back" }))
        .unwrap();
    assert!(s.scene().unwrap().find_node("open_7").is_some());
    // Tapping outside the panel closes it.
    s.tap(&Target::Point(380.0, 400.0)).unwrap();
    assert_eq!(s.current(), "home");
    assert!(s.scene().unwrap().find_text("Home underneath").is_some());
}

#[test]
fn a_pressed_tappable_is_drawn_pressed_until_let_go() {
    let mut s = tasks();
    let scene = s.scene().unwrap();
    let before = scene.quads().count();
    let title = scene.find_text("Call the plumber").unwrap().clone();
    let (x, y) = (title.bounds[0] + 2.0, title.bounds[1] + 2.0);
    let card = scene.hit_test(x, y).unwrap().clone();
    assert_eq!(card.node_type, "core://Tappable");
    let add = scene.find_node("add_task").unwrap().bounds;

    // Down on the card: one wash over its box, on top of what it drew.
    assert!(s.press(x, y));
    assert_eq!(s.pressed().map(|p| p.id.as_str()), Some(card.id.as_str()));
    let scene = s.scene().unwrap();
    assert_eq!(scene.quads().count(), before + 1);
    let wash = scene
        .quads()
        .find(|q| q.rect == card.bounds && q.bg_color[3] > 0.0 && q.bg_color[3] < 0.2)
        .expect("a wash over the card");
    assert!(wash.radii[0] > 0.0, "rounded as the card is: {wash:?}");

    // Let go: drawn as before.
    assert!(s.release_press());
    assert_eq!(s.scene().unwrap().quads().count(), before);

    // A button is not a row: nothing is drawn pressed.
    assert!(!s.press(add[0] + add[2] / 2.0, add[1] + add[3] / 2.0));
    assert_eq!(s.scene().unwrap().quads().count(), before);

    // A tap ends the touch.
    assert!(s.press(x, y));
    s.tap(&Target::Point(x, y)).unwrap();
    assert!(s.pressed().is_none());
}

#[test]
fn a_spinner_turns_while_the_session_animates() {
    let root: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Column", "children": [{ "type": "core://Progress", "size_dp": 48 }] }))
    .unwrap();
    let head = |s: &mut Session| {
        let scene = s.scene().unwrap();
        assert!(scene.spinning);
        scene.quads().next().unwrap().rect
    };
    // Still for previews and tests: the same frame every time.
    let mut s = Session::new("home", root, Default::default(), Viewport::PHONE);
    let still = head(&mut s);
    assert!(!s.animating());
    // In a window it keeps drawing, and the arc's head moves.
    s.animate = true;
    let first = head(&mut s);
    assert!(s.animating());
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert_ne!(head(&mut s), first);
    assert_eq!(still[2], first[2], "same dots, turned");
}

/// Android's stretch at an end (`scroll::Stretch`): the content draws
/// longer, away from the end it was pulled past, inside the view.
#[test]
fn a_stretch_spreads_the_list_from_the_pulled_end() {
    let (root, mut data) = fixture("task_list");
    for i in 10..30 {
        let uri = format!("demo://tasks/{i}");
        let mut e = data.entities["demo://tasks/2"].clone();
        e.set("id", i.to_string());
        data.entities.insert(uri.clone(), e);
        data.queries.get_mut("tasks:all").unwrap().push(uri);
    }
    let mut s = Session::new("tasks", root, data, Viewport::PHONE);
    let card = |s: &mut Session| s.scene().unwrap().find_node("task_2").unwrap().bounds;
    let before = card(&mut s);
    let view = s.scroll_view_at(before[0] + 1.0, before[1] + 1.0).unwrap();
    assert_eq!(view.offset, 0.0);
    assert!(view.max > 0.0);

    // Pulled past the top: further down and taller, the view's top fixed.
    s.set_stretch(&view.id, -0.1);
    if let Ok(dir) = std::env::var("STRETCH_PNG") {
        s.render_cpu()
            .unwrap()
            .save(format!("{dir}/stretched.png"))
            .unwrap();
    }
    let top = card(&mut s);
    assert!(top[1] > before[1], "{before:?} -> {top:?}");
    assert!((top[3] - before[3] * 1.1).abs() < 0.5, "{top:?}");
    // The view itself stays put.
    let stretched = s.scroll_view_at(before[0] + 1.0, before[1] + 1.0).unwrap();
    assert_eq!(stretched.height, view.height);

    // Past the bottom: pulled up toward it.
    s.set_scroll(&view.id, view.max);
    let last = |s: &mut Session| s.scene().unwrap().find_node("task_29").unwrap().bounds;
    let end = last(&mut s);
    s.set_stretch(&view.id, 0.1);
    let bottom = last(&mut s);
    assert!(bottom[1] < end[1], "{end:?} -> {bottom:?}");

    // Let go: drawn as before.
    s.set_scroll(&view.id, 0.0);
    s.set_stretch(&view.id, 0.0);
    assert_eq!(card(&mut s), before);
}

#[test]
fn a_fields_clear_button_empties_it_and_keeps_it_focused() {
    let root = serde_json::from_value(
        json!({ "type": "core://TextField", "field": "q", "placeholder": "Search" }),
    )
    .unwrap();
    let mut s = Session::new("search", root, Default::default(), Viewport::PHONE);
    // Nothing to clear: no button.
    assert!(s.scene().unwrap().find_node("clear:q").is_none());
    s.tap(&Target::Id("field:q".into())).unwrap();
    s.type_text("q", "soup");
    let field = s.scene().unwrap().find_node("field:q").unwrap().bounds;
    let clear = s.scene().unwrap().find_node("clear:q").unwrap().bounds;
    // At the field's right end, inside it.
    assert!(clear[0] + clear[2] <= field[0] + field[2]);
    assert!(clear[0] > field[0] + field[2] / 2.0);
    // A point on it hits the button, not the field around it.
    let (x, y) = (clear[0] + clear[2] / 2.0, clear[1] + clear[3] / 2.0);
    let out = s.tap(&Target::Point(x, y)).unwrap();
    assert!(!s.form.contains_key("q"));
    assert_eq!(s.focused.as_deref(), Some("q"));
    assert_eq!(Session::focused_field(&out), Some("q"));
    assert!(s.scene().unwrap().find_node("clear:q").is_none());
    s.type_text("q", "bread");
    assert_eq!(s.form["q"], "bread");
}
