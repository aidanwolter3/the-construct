//! The scenarios CI runs (scripts/sim_scenarios.sh), against the fixture pod.

use construct_sim::fixture::media_file;

use crate::common::{assert_ok, out_dir, run};

#[test]
fn liking_a_post_and_going_offline() {
    let out = out_dir("like");
    let (runner, results) = run(
        "pixelfed_sample_no_login",
        "shot feed; click Like; assert-text Unlike; assert-text 90; shot liked; offline; shot offline",
        &out,
    );
    assert_ok(&results);
    for f in [
        "01-feed.png",
        "05-liked.png",
        "07-offline.png",
        "steps.log",
        "steps.json",
    ] {
        assert!(out.join(f).is_file(), "{f}");
    }
    let conn = &runner.session.data.entities["local://connection"];
    assert_eq!(conn.get("label").and_then(|v| v.as_str()), Some("Offline"));
    let steps: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("steps.json")).unwrap()).unwrap();
    assert_eq!(steps[0]["file"], "01-feed.png");
    assert_eq!(steps[1]["status"], "ok");
}

#[test]
fn a_write_offline_is_undone() {
    let (runner, results) = run(
        "pixelfed_sample_no_login",
        "offline; click Like; idle; assert-no-text Unlike",
        &out_dir("offline-like"),
    );
    assert_ok(&results);
    assert!(runner.session.last_error.is_some());
}

#[test]
fn layouts_open_over_home_and_back_pops_them() {
    let (_, results) = run(
        "pixelfed",
        "open-layout events; assert-text Events; back; open-settings; assert-text Settings; back; \
         open-inbox; assert-text @bob commented on your post; back; assert-text Alice Lumen",
        &out_dir("layouts"),
    );
    assert_ok(&results);
}

#[test]
fn a_post_screen_scrolls_to_its_comments_and_takes_typing() {
    let (runner, results) = run(
        "pixelfed",
        "click tag:comment:pixelfed://posts/781254316723458049; scroll-to Which lens?; \
         type tag:textfield:comment Golden light!; assert-text Golden light!",
        &out_dir("comments"),
    );
    assert_ok(&results);
    assert_eq!(runner.session.form["comment"], "Golden light!");
}

#[test]
fn a_notification_arrives_and_opens_its_thread() {
    let (_, results) = run(
        "pixelfed",
        "notify pixelfed likes Bob liked your post; open-inbox; assert-text Bob liked your post; \
         click @bob commented on your post; scroll-to Number 3 for me",
        &out_dir("notify"),
    );
    assert_ok(&results);
}

#[test]
fn a_failing_step_stops_the_run_and_saves_the_screen() {
    let out = out_dir("fail");
    let (_, results) = run(
        "pixelfed_sample_no_login",
        "assert-text Nobody wrote this; shot never",
        &out,
    );
    assert_eq!(results.len(), 1);
    assert!(!results[0].ok);
    assert!(
        results[0].message.contains("Sunrise over Millbrook"),
        "{}",
        results[0].message
    );
    assert!(out.join("01-FAILED.png").is_file());
    let log = std::fs::read_to_string(out.join("steps.log")).unwrap();
    assert!(log.ends_with("RESULT: FAILED\n"), "{log}");
}

#[test]
fn fixture_images_come_from_the_fake_pixelfeds_media() {
    assert_eq!(
        media_file("http://127.0.0.1:8081/storage/avatar-bob.png"),
        Some("avatar-bob.png")
    );
    assert_eq!(
        media_file("http://pod/v1/pixelfed/media/carousel-1.png?w=2"),
        Some("carousel-1.png")
    );
    assert_eq!(
        media_file("https://pixelfed.example/storage/../secret"),
        None
    );
    assert_eq!(media_file("https://cdn.example/a.png"), None);
}

#[test]
fn picked_photos_show_and_post() {
    let (runner, results) = run(
        "pixelfed",
        "click \"New post\"; pick fake-pixelfed/fixtures/media/carousel-1.png fake-pixelfed/fixtures/media/carousel-2.png; \
         click Add photos; assert-text Photo 2 of 2; click tag:media-remove:sim-1; assert-no-text Photo 2 of 2; \
         click Share; idle; assert-text Posting",
        &out_dir("pick"),
    );
    assert_ok(&results);
    let post = runner.session.data.queries["timeline:home"][0].clone();
    assert_eq!(
        runner.session.data.entities[&post].get("media_uris"),
        Some(&serde_json::json!(["construct://attachments/sim-2"]))
    );
}

#[test]
fn dark_mode_draws_the_dark_palette() {
    let out = out_dir("dark");
    let (mut runner, results) = run(
        "pixelfed",
        "mode dark; shot feed; open-inbox; assert-text @bob commented on your post; shot inbox; \
         mode light; shot light",
        &out,
    );
    assert_ok(&results);
    let page = |runner: &mut construct_sim::run::Runner| {
        runner.session.scene().map(|s| s.background).unwrap()
    };
    // Light again: Material's background, as the Compose app draws.
    assert_eq!(
        page(&mut runner),
        construct_render::style::Rgba::rgb(0xFE, 0xFB, 0xFF).0
    );
    runner.session.set_dark(true);
    // Dark: the theme's page token.
    assert_eq!(
        page(&mut runner),
        construct_render::style::Rgba::hex("#1A1A1C").unwrap().0
    );
    for f in ["02-feed.png", "05-inbox.png", "07-light.png"] {
        assert!(out.join(f).is_file(), "{f}");
    }
}

#[test]
fn a_deck_of_swipe_cards_goes_one_card_at_a_time() {
    let (runner, results) = run(
        "swipe_deck",
        "assert-no-text All black; drag-card 130 Weekend flannel; release; \
         drag-card -60 Chore coat layers; release; assert-text Chore coat layers; \
         drag-card -130 Chore coat layers; release; assert-text All black; \
         swipe-right All black; assert-text All done",
        &out_dir("swipe-deck"),
    );
    assert_ok(&results);
    let verdict = |uri: &str| {
        runner.session.data.entities[uri]
            .get("verdict")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    assert_eq!(verdict("demo://cards/1").as_deref(), Some("Like"));
    assert_eq!(verdict("demo://cards/2").as_deref(), Some("Pass"));
    assert_eq!(verdict("demo://cards/3").as_deref(), Some("Like"));
}

#[test]
fn a_session_unlocks_at_the_minute_it_is_over() {
    let (_, results) = run(
        "week_sessions",
        "clock 2026-11-16T08:59; assert-no-text Mark Mon; clock 2026-11-16T09:00; \
         assert-text Mark Mon Group class done; assert-no-text Mark Tue; \
         clock 2026-11-17T15:44; assert-no-text Mark Tue Swim done; \
         clock 2026-11-17T15:45; assert-text Mark Tue Swim done; assert-no-text Mark Wed",
        &out_dir("date-unlock"),
    );
    assert_ok(&results);
}

#[test]
fn a_home_embeds_a_whole_layout_and_centres_its_tiles() {
    let (mut runner, results) = run(
        "tiles_home",
        "scroll-to Photos; assert-text Read more; assert-text Everything else",
        &out_dir("tiles-home"),
    );
    assert_ok(&results);
    let scene = runner.session.scene().unwrap();
    // The embedded layout ran its whole height: its last card sits above
    // the section after it.
    let text = |t: &str| scene.find_text(t).unwrap().bounds;
    assert!(text("Read more")[1] < text("Everything else")[1]);
    // The tiles are centred on the phone: as much room left as right.
    let tile = |t: &str| scene.find_node(&format!("tile:{t}")).unwrap().bounds;
    let (first, last) = (tile("Calendar"), tile("Photos"));
    let (left, right) = (first[0], 390.0 - (last[0] + last[2]));
    assert!((left - right).abs() < 0.5, "{left} {right}");
}

/// The construct (decision `content-addressed-construct`): an edit to the
/// layout on screen waits while a field is focused, then keeps the stack,
/// the form and the scroll position; a link taken out closes what it led to,
/// back to the deepest entry that still resolves.
#[test]
fn a_construct_edit_keeps_the_stack_and_a_removed_link_pops_back() {
    let d = "crates/construct-sim/data";
    let (runner, results) = run(
        "home",
        &format!(
            "construct {d}/construct_v1.json; click Recipes; type note Hello there; scroll-to Item 30; \
             click Hello there; construct {d}/construct_v2.json; assert-no-text Recipes (edited); \
             click Item 30; back; assert-text Recipes (edited); assert-text Item 30; \
             assert-text Hello there; click Item 30; assert-text Detail; \
             construct {d}/construct_v4.json; wait 600; assert-text Recipes (edited); \
             assert-no-text Detail; construct {d}/construct_v3.json; wait 600; \
             assert-text Nothing linked here now; assert-no-text Recipes (edited)"
        ),
        &out_dir("construct"),
    );
    assert_ok(&results);
    assert_eq!(
        runner.session.form.get("note").map(String::as_str),
        Some("Hello there")
    );
}
