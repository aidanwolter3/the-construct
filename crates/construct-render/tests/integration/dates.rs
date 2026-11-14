//! `core://Conditional` `date` (decision `date-conditions`): rows that
//! show one thing until their moment comes and another after, against the
//! device's clock, and frames that go stale (and wake the window) at the
//! minute the answer changes.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use construct_render::dates::{Clock, wall_time};
use construct_render::interact::Session;
use construct_render::{Input, SceneBuilder, Viewport};
use protocol::{DataBundle, UiNode};
use serde_json::json;

/// A week of sessions: each row reads "Tap <day>" once its session is
/// over, "Soon <day>" before, and "Today" beside it on its day.
fn week() -> UiNode {
    let row = json!({ "type": "core://Row", "children": [
        { "type": "core://Conditional", "value": "$item.date", "time": "$item.ends", "date": "Past",
          "content": { "type": "core://Text", "style": "Body", "value_expr": "Tap $item.day" } },
        { "type": "core://Conditional", "value": "$item.date", "time": "$item.ends", "date": "Future",
          "content": { "type": "core://Text", "style": "Body", "value_expr": "Soon $item.day" } },
        { "type": "core://Conditional", "value": "$item.date", "date": "Today",
          "content": { "type": "core://Text", "style": "Caption", "value_expr": "Today" } } ] });
    serde_json::from_value(json!({ "type": "core://Column", "children": [
        { "type": "core://Text", "style": "Title", "value_expr": "${x://week/w#title}" },
        { "type": "core://List", "query_key": "rows", "item_template": row } ] }))
    .unwrap()
}

fn rows() -> DataBundle {
    serde_json::from_value(json!({
        "entities": {
            "x://week/w": { "title": "This week" },
            "x://rows/mon": { "day": "Mon", "date": "2026-11-16", "ends": "9:00 AM" },
            "x://rows/tue": { "day": "Tue", "date": "2026-11-17", "ends": "15:45" },
            "x://rows/sat": { "day": "Sat", "date": "2026-11-21", "ends": "Bonus" } },
        "queries": { "rows": ["x://rows/mon", "x://rows/tue", "x://rows/sat"] } }))
    .unwrap()
}

fn at(s: &str) -> Clock {
    Clock {
        fixed: Some(wall_time(s).unwrap()),
        zone: None,
    }
}

fn texts(s: &mut Session) -> Vec<String> {
    s.scene()
        .unwrap()
        .semantics
        .iter()
        .filter_map(|n| n.text_content.clone())
        .collect()
}

#[test]
fn rows_unlock_once_their_moment_has_come() {
    let mut s = Session::new("week", week(), rows(), Viewport::PHONE);
    s.set_clock(at("2026-11-16T08:59"));
    let t = texts(&mut s);
    for want in ["Soon Mon", "Today", "Soon Tue", "Soon Sat"] {
        assert!(t.iter().any(|x| x == want), "{want} in {t:?}");
    }
    s.set_clock(at("2026-11-16T09:00"));
    let t = texts(&mut s);
    assert!(t.iter().any(|x| x == "Tap Mon"), "{t:?}");
    assert!(!t.iter().any(|x| x == "Soon Mon"), "{t:?}");
    // The next day: Monday is no longer today, Tuesday is; its swim isn't
    // over until 15:45.
    s.set_clock(at("2026-11-17T15:44"));
    let t = texts(&mut s);
    assert!(t.iter().any(|x| x == "Soon Tue") && t.iter().any(|x| x == "Today"));
    // A time that isn't one ("Bonus") leaves the bare date: on Saturday the
    // hike is neither ahead nor over, only today; it counts once Saturday
    // is over.
    s.set_clock(at("2026-11-20T23:59"));
    assert!(texts(&mut s).iter().any(|x| x == "Soon Sat"));
    s.set_clock(at("2026-11-21T23:59"));
    let t = texts(&mut s);
    assert!(!t.iter().any(|x| x.ends_with(" Sat")), "{t:?}");
    assert!(t.iter().any(|x| x == "Today"), "{t:?}");
    s.set_clock(at("2026-11-22T00:00"));
    let t = texts(&mut s);
    assert!(t.iter().any(|x| x == "Tap Sat"), "{t:?}");
    assert!(!t.iter().any(|x| x == "Today"), "{t:?}");
    // A fixed clock never gets anywhere, so nothing wakes for it.
    assert_eq!(s.next_wake(), None);
}

#[test]
fn a_frame_is_stale_from_the_minute_an_answer_changes() {
    let root = week();
    let mut data = rows();
    let (form, route, layouts, scroll) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let mut b = SceneBuilder::new();
    b.clock = at("2026-11-16T08:30");
    let monday = wall_time("2026-11-16T09:00").unwrap() / 60;
    let lay = |b: &mut SceneBuilder, data: &DataBundle, prev| {
        let input = Input {
            root: &root,
            data,
            form: &form,
            route: &route,
            layouts: &layouts,
            scroll: &scroll,
            viewport: Viewport::PHONE,
            dark: false,
            item: None,
            insets: [0.0; 4],
            focused: None,
        };
        b.relayout(&input, prev).unwrap()
    };
    let first = lay(&mut b, &data, None);
    assert_eq!(first.changes_at, Some(monday));
    // Another entity changes: the rows are taken over, and so is when
    // they change.
    data.entities
        .get_mut("x://week/w")
        .unwrap()
        .set("title", "Next week");
    let again = lay(&mut b, &data, Some(first));
    assert_eq!(again.reuse.reused, 3);
    assert_eq!(again.changes_at, Some(monday));
    // Past that minute nothing is taken over: Monday reads "Tap" now.
    b.clock = at("2026-11-16T09:00");
    let after = lay(&mut b, &data, Some(again));
    assert_eq!(after.reuse.reused, 0);
    let scene = after.emit(&BTreeMap::new());
    let t: Vec<_> = scene
        .semantics
        .iter()
        .filter_map(|n| n.text_content.clone())
        .collect();
    assert!(t.iter().any(|x| x == "Tap Mon"), "{t:?}");
    // The next change: midnight, when Monday stops being today.
    let tuesday = wall_time("2026-11-17T00:00").unwrap() / 60;
    assert_eq!(after.changes_at, Some(tuesday));
}

#[test]
fn the_window_wakes_when_an_instant_arrives() {
    let soon = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(150);
    let root: UiNode = serde_json::from_value(json!({ "type": "core://Column", "children": [
        { "type": "core://Conditional", "value": soon.to_string(), "date": "Future",
          "content": { "type": "core://Text", "style": "Body", "value_expr": "Not yet" } } ] }))
    .unwrap();
    let mut s = Session::new("soon", root, DataBundle::default(), Viewport::PHONE);
    assert!(texts(&mut s).iter().any(|x| x == "Not yet"));
    let wake = s.next_wake().expect("a wake for the moment");
    let left = wake.saturating_duration_since(Instant::now());
    // At the start of the minute it falls in: within the next 150 s, at
    // most a minute early.
    assert!(
        left > Duration::from_secs(85) && left <= Duration::from_secs(150),
        "{left:?}"
    );
}
