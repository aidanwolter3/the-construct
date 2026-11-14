//! `core://SwipeCard` in a deck (`core://List` `stack`): the card follows
//! the finger, springs back short of its threshold and flies off past it.

use construct_render::Viewport;
use construct_render::interact::{Effect, Session};
use serde_json::{Value, json};

fn verdict(v: &str) -> Value {
    json!({ "intent": "action.local.set", "entity_uri": "$item.uri", "field": "verdict",
            "value_expr": format!("\"{v}\"") })
}

fn deck() -> Session {
    let page: protocol::UiNode = serde_json::from_value(json!({
        "type": "core://Page", "id": "p", "root": { "type": "core://Column", "children": [
            { "type": "core://List", "query_key": "cards", "stack": true, "padding_h_dp": 16,
              "where": [{ "field": "verdict", "equals": "Unseen" }],
              "empty_text": "That's the deck.",
              "item_template": { "type": "core://SwipeCard",
                "on_swipe_right": verdict("Like"), "on_swipe_left": verdict("Pass"),
                "content": { "type": "core://Box", "corner_dp": 18, "padding_h_dp": 16,
                    "padding_v_dp": 16, "background": "#FFFFFF", "border_color": "#E4E4EA",
                    "content": { "type": "core://Column", "children": [
                        { "type": "core://Text", "value_expr": "$item.title", "style": "Headline" },
                        { "type": "core://Text", "value_expr": "Linen and denim", "style": "Body" }
                    ] } } } } ] } }))
    .unwrap();
    let card = |t: &str| json!({ "title": t, "verdict": "Unseen" });
    let data = serde_json::from_value(json!({
        "entities": { "demo://cards/a": card("Card A"), "demo://cards/b": card("Card B"),
                      "demo://cards/c": card("Card C") },
        "queries": { "cards": ["demo://cards/a", "demo://cards/b", "demo://cards/c"] } }))
    .unwrap();
    Session::new("p", page, data, Viewport::PHONE)
}

/// The top card's id and the centre of its title.
fn top_card(s: &mut Session) -> (String, (f32, f32)) {
    let b = s.scene().unwrap().find_text("Card A").unwrap().bounds;
    let at = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
    (s.card_at(at.0, at.1).expect("a card"), at)
}

fn left_of(s: &mut Session, text: &str) -> f32 {
    s.scene().unwrap().find_text(text).unwrap().bounds[0]
}

#[test]
fn a_deck_draws_its_top_card_over_the_next() {
    let mut s = deck();
    let scene = s.scene().unwrap();
    assert!(scene.find_text("Card A").is_some());
    assert!(scene.find_text("Card B").is_some(), "the next peeks behind");
    assert!(scene.find_text("Card C").is_none(), "one card at a time");
    let a = scene.find_text("Card A").unwrap().bounds;
    let b = scene.find_text("Card B").unwrap().bounds;
    // Behind, a little lower and narrower; the top card is hit first.
    assert!(b[1] > a[1] && b[0] > a[0], "{a:?} {b:?}");
    let (cx, cy) = (a[0] + 2.0, a[1] + 2.0);
    let (id, _) = top_card(&mut s);
    assert_eq!(s.card_at(cx, cy).as_deref(), Some(id.as_str()));
}

#[test]
fn a_card_follows_the_finger_and_springs_back_short_of_its_threshold() {
    let mut s = deck();
    let (id, _) = top_card(&mut s);
    let x0 = left_of(&mut s, "Card A");
    let quads = |s: &mut Session| {
        s.scene()
            .unwrap()
            .draws
            .iter()
            .filter(|d| matches!(d, construct_render::scene::Draw::Quad(_)))
            .count()
    };
    let still = quads(&mut s);
    s.drag_card(&id, 40.0);
    assert_eq!(left_of(&mut s, "Card A"), x0 + 40.0);
    assert_eq!(quads(&mut s), still + 1, "tinted toward its side");
    assert!(s.release_card(&id, 0.0).is_none(), "nothing runs");
    assert_eq!(s.card_offset(&id), 0.0);
    assert_eq!(left_of(&mut s, "Card A"), x0);
    assert_eq!(quads(&mut s), still);
}

#[test]
fn past_its_threshold_a_card_flies_off_and_the_next_shows() {
    let mut s = deck();
    let (id, _) = top_card(&mut s);
    let x0 = left_of(&mut s, "Card A");
    s.drag_card(&id, 120.0);
    let out = s.release_card(&id, 0.0).expect("the swipe ran");
    match &out.effects[..] {
        [Effect::LocalSet { fields, .. }] => assert_eq!(fields["verdict"], "Like"),
        e => panic!("{e:?}"),
    }
    let scene = s.scene().unwrap();
    assert!(scene.find_text("Card A").is_none());
    assert!(scene.find_text("Card C").is_some());
    // The next card is in the top place, not where the last one went.
    assert_eq!(left_of(&mut s, "Card B"), x0);
}

#[test]
fn a_fling_commits_from_half_the_threshold_and_a_plain_swipe_still_works() {
    let mut s = deck();
    let (id, _) = top_card(&mut s);
    s.drag_card(&id, -60.0);
    let out = s.release_card(&id, -2000.0).expect("flung off");
    assert!(
        matches!(&out.effects[..], [Effect::LocalSet { fields, .. }] if fields["verdict"] == "Pass")
    );
    // A swipe that isn't a drag (a screen reader's) runs the side's intent.
    let b = s.scene().unwrap().find_text("Card B").unwrap().bounds;
    let out = s.gesture(b[0] + 2.0, b[1] + 2.0, "swipe_right").unwrap();
    assert!(matches!(&out.effects[..], [Effect::LocalSet { .. }]));
    assert!(s.scene().unwrap().find_text("Card C").is_some());
    assert!(s.scene().unwrap().find_text("Card B").is_none());
}

#[test]
fn in_a_window_the_card_flies_off_before_its_intent_runs() {
    let mut s = deck();
    s.animate = true;
    let (id, _) = top_card(&mut s);
    s.drag_card(&id, 120.0);
    assert!(s.release_card(&id, 0.0).is_none(), "on its way");
    assert!(s.animating());
    assert!(s.take_swiped().is_empty());
    std::thread::sleep(std::time::Duration::from_millis(250));
    // Off the screen, still the same card, until its intent has run.
    let _ = s.scene().unwrap();
    assert!(s.card_offset(&id) > 300.0);
    let out = s.take_swiped();
    assert_eq!(out.len(), 1);
    assert_eq!(s.card_offset(&id), 0.0);
    let scene = s.scene().unwrap();
    assert!(scene.find_text("Card A").is_none());
    assert!(scene.find_text("Card C").is_some());
}
