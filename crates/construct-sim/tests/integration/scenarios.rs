//! The scenarios CI runs (scripts/sim_scenarios.sh), against the fixture pod.

use construct_sim::fixture::media_file;

use crate::common::{assert_ok, out_dir, run};

#[test]
fn liking_a_post_and_going_offline() {
    let out = out_dir("like");
    let (runner, results) = run(
        "pixelfed_sample_no_login",
        "shot feed; click Like; assert-text Unlike; shot liked; offline; shot offline",
        &out,
    );
    assert_ok(&results);
    for f in [
        "01-feed.png",
        "04-liked.png",
        "06-offline.png",
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
