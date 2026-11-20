//! The scenarios CI runs (scripts/sim_scenarios.sh), against the fixture pod.

use construct_sim::fixture::media_file;

use construct_sim::script;

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
    // Layouts built from components nest boxes in boxes: more stack than a
    // test thread's 2 MB.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let (_, results) = run(
                "pixelfed",
                "open-layout events; assert-text Events; back; open-settings; \
                 assert-text Settings; back; open-inbox; \
                 assert-text @bob commented on your post; back; assert-text Alice Lumen",
                &out_dir("layouts"),
            );
            assert_ok(&results);
        })
        .unwrap()
        .join()
        .unwrap();
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
    // The inbox is built from components, boxes in boxes (file 0007): opening
    // a thread from it takes more stack than a test thread's 2 MB.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let (_, results) = run(
                "pixelfed",
                "notify pixelfed likes Bob liked your post; open-inbox; \
                 assert-text Bob liked your post; assert-text New · 4; \
                 click @bob commented on your post; scroll-to Number 3 for me",
                &out_dir("notify"),
            );
            assert_ok(&results);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// `@core/collapsible` (the inbox's older notifications) opens and closes
/// again: Hide sets `open` to "false", which shows the closed header (#336).
#[test]
fn the_inbox_older_section_opens_and_closes() {
    // The inbox is built from components, boxes in boxes: more stack than a
    // test thread's 2 MB.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let (runner, results) = run(
                "pixelfed",
                "open-inbox; assert-text Show; assert-no-text Hide; click Show; assert-text Hide; \
         click Hide; assert-text Show; assert-no-text Hide; click Show; assert-text Hide",
                &out_dir("collapsible"),
            );
            assert_ok(&results);
            let section = &runner.session.data.entities["local://collapsible/inbox_older"];
            assert_eq!(
                section.get("open").and_then(|v| v.as_bool()),
                Some(true),
                "{section:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
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
    // The inbox is built from components, boxes in boxes: more stack than a
    // test thread's 2 MB.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
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
        })
        .unwrap()
        .join()
        .unwrap();
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

/// Text weights, capitals and line limits, and conditionals that invert
/// or test a query (decision `text-weight-caps-lines`).
#[test]
fn text_styles_and_query_conditionals() {
    let (mut runner, results) = run(
        "text_styles",
        "assert-text Move often · this week; assert-text Still to do; \
         assert-no-text All done for the week; assert-text Nothing archived yet; \
         assert-text 2 unread messages; assert-no-text Inbox clear; shot light; mode dark; shot dark",
        &out_dir("text-styles"),
    );
    assert_ok(&results);
    let scene = runner.session.scene().unwrap();
    let drawn: Vec<&str> = scene.texts().map(|t| t.text.as_str()).collect();
    assert!(drawn.contains(&"MOVE OFTEN · THIS WEEK"), "{drawn:?}");
    let notes = scene
        .texts()
        .find(|t| t.text.starts_with("Walk along"))
        .unwrap();
    assert_eq!(notes.max_lines, 2);
    assert_eq!(notes.rect[3], 2.0 * notes.line_height);
}

/// A `core://Bars` chart (feature `bars`, decision `device-versions`):
/// devices per day stacked by build, filtered by kind, and a tap on a bar
/// picks its day.
#[test]
fn a_bar_chart_filters_and_picks_a_day() {
    let (mut runner, results) = run(
        "device_versions",
        "assert-text Now: every device at its latest build; assert-text Studio MacBook; shot all; \
         click Web; assert-no-text Studio MacBook; click 16; assert-text Mon Nov 16; \
         assert-no-text Now: every device at its latest build; shot web-16",
        &out_dir("device-versions"),
    );
    assert_ok(&results);
    let scene = runner.session.scene().unwrap();
    let drawn: Vec<&str> = scene.texts().map(|t| t.text.as_str()).collect();
    // Three web devices on the 16th, one row each.
    assert!(
        drawn.iter().filter(|t| t.starts_with("Nov 1")).count() >= 3,
        "{drawn:?}"
    );
    assert!(drawn.contains(&"Now"), "{drawn:?}");
}

/// The `monitor` source's blueprint `server_health`: the status, the checks,
/// the devices chart and list and the counters from core components; where
/// monitoring is off (the source's 403) it says so and still shows the devices.
#[test]
fn server_health_shows_checks_and_devices_and_says_when_monitoring_is_off() {
    // Components nest boxes in boxes: more stack than a test thread's 2 MB.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let (_, results) = run(
                "server_health",
                "assert-text 1 check needs a look; assert-text Server up 6 h; assert-text Checks; \
                 assert-text Wrong pod codes and unknown tokens; assert-text Disk space; \
                 assert-no-text Server monitoring is off for this pod; shot light; \
                 scroll-to Studio MacBook; scroll-to Ahead of server; \
                 click Web; assert-no-text Studio MacBook; shot devices; \
                 scroll-to All counters; scroll-to pods (temporary); assert-text 7 now; \
                 mode dark; shot dark",
                &out_dir("server-health"),
            );
            assert_ok(&results);
            let (_, results) = run(
                "server_health_off",
                "assert-text Server monitoring is off for this pod; assert-no-text Checks; \
                 assert-no-text All clear; assert-no-text needs a look; assert-no-text All counters; \
                 assert-text Devices per day; assert-text 5 devices; shot light; \
                 scroll-to Studio MacBook; scroll-to Ahead of server; mode dark; shot dark",
                &out_dir("server-health-off"),
            );
            assert_ok(&results);
        })
        .unwrap()
        .join()
        .unwrap();
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

/// Panels (decision `wide-screen-panels`): on a desktop window a push from
/// the inline meals layout opens beside it, deeper pushes the oldest column
/// out of sight and Back brings it back; on a phone the same tap pushes.
#[test]
fn a_push_from_a_panel_host_opens_beside_it_on_a_desktop_only() {
    let d = "crates/construct-sim/data";
    let (_, results) = run(
        "home",
        &format!(
            "screen desktop; construct {d}/construct_panels.json; wait 300; \
             hover All meals; click All meals; assert-text Every recipe, with what goes in it; \
             assert-text This week's meals; assert-text Today; click Lentil soup; \
             assert-text Recipe: Lentil soup; assert-no-text This week's meals; back; \
             assert-text This week's meals; assert-no-text Recipe: Lentil soup; \
             screen phone; click All meals; assert-text Every recipe, with what goes in it; \
             assert-no-text This week's meals"
        ),
        &out_dir("panels"),
    );
    assert_ok(&results);
}

/// Scroll anchors on a switched pod (decision `content-addressed-construct`):
/// rows added above the scroll position don't move the rows on screen, and
/// when the first row showing goes, the next one stays where it was.
#[test]
fn rows_added_above_the_scroll_position_dont_move_the_visible_row() {
    let d = "crates/construct-sim/data";
    let out = out_dir("anchor");
    let (mut runner, results) = run(
        "home",
        &format!("construct {d}/construct_v1.json; click Recipes; scroll-to Item 30"),
        &out,
    );
    assert_ok(&results);
    let y = |runner: &mut construct_sim::run::Runner, t: &str| {
        let scene = runner.session.scene().unwrap();
        let n = scene
            .semantics
            .iter()
            .find(|n| n.text_content.as_deref() == Some(t));
        n.map(|n| n.bounds[1])
    };
    let item30 = y(&mut runner, "Item 30");
    // The first row showing: the one the scroll is anchored to.
    let first = (1..30)
        .find(|n| y(&mut runner, &format!("Item {n}")).is_some())
        .unwrap();
    assert!(first > 1, "scrolled");
    let next = format!("Item {}", first + 1);
    let next_y = y(&mut runner, &next);
    assert!(
        item30.is_some() && next_y.is_some(),
        "{first} {item30:?} {next_y:?}"
    );

    let more = script::parse(&format!(
        "construct {d}/construct_anchor_above.json; wait 300; assert-text Item 30"
    ))
    .unwrap();
    assert_ok(&runner.run(&more));
    let near = |a: Option<f32>, b: Option<f32>| match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < 0.5,
        _ => false,
    };
    assert!(near(y(&mut runner, "Item 30"), item30));
    assert!(near(y(&mut runner, &next), next_y));
    assert_eq!(y(&mut runner, "New item 5"), None);

    // The anchored row goes: the one after it keeps its place.
    let mut gone: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            crate::common::root()
                .join(d)
                .join("construct_anchor_above.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let uri = format!("sim://items/{first}");
    gone["data"]["queries"]["sim:items"]
        .as_array_mut()
        .unwrap()
        .retain(|u| u.as_str() != Some(uri.as_str()));
    std::fs::create_dir_all(&out).unwrap();
    let file = out.join("construct_anchor_gone.json");
    std::fs::write(&file, gone.to_string()).unwrap();
    let step = script::parse(&format!(
        "construct {}; wait 300; assert-no-text Item {first}",
        file.display()
    ))
    .unwrap();
    assert_ok(&runner.run(&step));
    assert!(near(y(&mut runner, &next), next_y));
}

/// Decision `screen-sizes`: one layout on three windows. The size is device
/// data, the recipes fill as many columns as fit, and on a desktop the page
/// stops at its `max_width_dp`, centred.
#[test]
fn one_layout_fits_a_phone_a_tablet_and_a_desktop() {
    let (mut runner, results) = run(
        "screen_sizes",
        "assert-text compact window, 390 points wide; assert-no-text Shown only on a wide window; \
         screen tablet; assert-text medium window, 820 points wide; \
         screen desktop; assert-text expanded window, 1280 points wide; \
         assert-text Shown only on a wide window",
        &out_dir("screen-sizes"),
    );
    assert_ok(&results);
    let scene = runner.session.scene().unwrap();
    let card = |i: u32| scene.find_node(&format!("recipe:{i}")).unwrap().bounds;
    // 1040 - 2 × 16 padding = 1008 for the cards: four of at least 240.
    let (first, fourth, fifth) = (card(1), card(4), card(5));
    assert!((first[1] - fourth[1]).abs() < 0.5, "{first:?} {fourth:?}");
    assert!(fifth[1] > first[1], "the fifth starts a new row");
    let (left, right) = (first[0], 1280.0 - (fourth[0] + fourth[2]));
    assert!((left - right).abs() < 0.5, "centred: {left} {right}");
    assert!((left - (120.0 + 16.0)).abs() < 0.5, "{left}");
}

/// Feature `list_trailing`: a grid of sessions ends with an add tile, right
/// after the last session in its row; an empty grid still draws it.
#[test]
fn a_grid_ends_with_its_trailing_add_tile() {
    let (mut runner, results) = run(
        "list_trailing",
        "assert-text No extras yet",
        &out_dir("list-trailing"),
    );
    assert_ok(&results);
    let scene = runner.session.scene().unwrap();
    let at = |tag: &str| scene.find_node(tag).unwrap().bounds;
    // Five sessions in four columns: the add tile is the second cell of the
    // second row, as wide as a session.
    let (tue, fri, add) = (at("session:2"), at("session:5"), at("add:week"));
    assert!((add[1] - fri[1]).abs() < 0.5, "{add:?} {fri:?}");
    assert!((add[0] - tue[0]).abs() < 0.5, "{add:?} {tue:?}");
    assert!((add[2] - tue[2]).abs() < 0.5, "{add:?} {tue:?}");
    let extra = at("add:extras");
    assert!((extra[0] - at("session:1")[0]).abs() < 0.5, "{extra:?}");
}

/// A page made of the core set (decision `element-roles-and-reuse`):
/// sections of grouped lists, rows with something at either end, a list
/// that is its rows or its empty note, a row that ticks, chips that
/// choose, callouts in a tone.
#[test]
fn a_page_of_core_components_ticks_and_chooses() {
    // Components nest boxes in boxes; an unoptimized build's frames don't
    // fit a test thread's 2 MB that deep, so the test brings its own stack.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let (mut runner, results) = run(
                "core_set",
                "assert-text Garden week; assert-text Nothing archived yet.; \
                 assert-no-text Nothing to do.; shot light; click Stake the tomatoes; \
                 click Morning; shot ticked; mode dark; shot dark",
                &out_dir("core-set"),
            );
            assert_ok(&results);
            let scene = runner.session.scene().unwrap();
            // A row with only a trailing value starts where a plain row does.
            let x = |text: &str| scene.texts().find(|t| t.text == text).unwrap().rect[0];
            assert_eq!(x("Compost"), x("Tool rack"));
            // A leading child moves the title in; the trailing one sits at
            // the end.
            assert!(x("Herb bed") > x("Tool rack"));
            assert!(x("Turned Sunday") > x("Compost") + 100.0);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// `core:tab_shell` (decision `element-roles-and-reuse`): one list of tabs
/// is a dock on a phone and a rail on a wide window; the first tab shows
/// before any is chosen, and a tap opens another.
#[test]
fn tabs_are_a_dock_on_a_phone_and_a_rail_on_a_wide_window() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let (mut runner, results) = run(
                "tab_shell",
                "assert-text Water the seedlings; assert-no-text Allotment; shot today; \
                 click Garden; assert-text Tomato bed; assert-no-text Call the nursery; \
                 shot garden; screen desktop; assert-text Allotment; assert-text Tomato bed; \
                 shot wide",
                &out_dir("tab-shell"),
            );
            assert_ok(&results);
            // On the wide window the tabs are down the side, above one another.
            let scene = runner.session.scene().unwrap();
            let at = |text: &str| scene.texts().find(|t| t.text == text).unwrap().rect;
            assert_eq!(at("Today")[0], at("More")[0]);
            assert!(at("Today")[1] < at("More")[1]);
            assert!(at("Garden beds")[0] > at("Today")[0] + 150.0);
        })
        .unwrap()
        .join()
        .unwrap();
}
