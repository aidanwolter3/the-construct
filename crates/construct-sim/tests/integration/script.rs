use construct_sim::screen::Screen;
use construct_sim::script::{Direction, Presentation, Step, parse, split_paths, split_type_arg};

fn steps(s: &str) -> Vec<Step> {
    parse(s).unwrap().into_iter().map(|l| l.step).collect()
}

#[test]
fn commands_split_on_semicolons_and_newlines_and_skip_comments() {
    assert_eq!(
        steps("shot a; # a comment\nclick Count: 2;; back"),
        vec![
            Step::Shot("a".into()),
            Step::Click("Count: 2".into()),
            Step::Back
        ]
    );
}

#[test]
fn arguments_parse_as_the_compose_simulator_reads_them() {
    assert_eq!(
        steps("wait-text 3000 Which lens?")[0],
        Step::WaitText {
            ms: 3000,
            text: "Which lens?".into()
        }
    );
    assert_eq!(
        steps("type \"Search posts\" cats and dogs")[0],
        Step::Type {
            target: "Search posts".into(),
            text: "cats and dogs".into()
        }
    );
    assert_eq!(
        steps("open-layout events SHEET")[0],
        Step::OpenLayout {
            layout: "events".into(),
            how: Presentation::Sheet
        }
    );
    assert_eq!(
        steps("pin-version pixelfed v2; pin-version pixelfed latest"),
        vec![
            Step::PinVersion {
                layout: "pixelfed".into(),
                version: Some(2)
            },
            Step::PinVersion {
                layout: "pixelfed".into(),
                version: None
            }
        ]
    );
    assert_eq!(
        steps("swipe-left tag:carousel:x")[0],
        Step::Swipe(Direction::Left, "tag:carousel:x".into())
    );
    assert_eq!(
        steps("notify pixelfed likes Bob liked your post")[0],
        Step::Notify {
            app: "pixelfed".into(),
            channel: "likes".into(),
            title: "Bob liked your post".into()
        }
    );
    assert_eq!(steps("wait-images")[0], Step::WaitImages(10_000));
    assert_eq!(
        split_paths("a.png \"my photo.jpg\"  b.webp").unwrap(),
        vec!["a.png", "my photo.jpg", "b.webp"]
    );
    assert_eq!(split_type_arg("field"), None);
}

#[test]
fn bad_scripts_name_the_entry() {
    for (script, needle) in [
        ("shot", "'shot' needs a name"),
        ("back now", "takes no argument"),
        ("wait soon", "'wait' needs"),
        ("wait-text Which lens?", "'wait-text' needs"),
        ("open-layout a drawer", "'open-layout' needs"),
        ("toggle-channel likes", "'toggle-channel' needs"),
        ("fly away", "unknown command 'fly'"),
    ] {
        let e = parse(script).unwrap_err();
        assert!(e.contains(needle), "{script}: {e}");
    }
}

#[test]
fn screens_are_the_compose_presets_or_sizes() {
    let phone = Screen::parse("phone").unwrap();
    assert_eq!((phone.width, phone.height, phone.density), (390, 844, 3.0));
    let custom = Screen::parse("600x400").unwrap();
    assert_eq!(
        (custom.width, custom.height, custom.density),
        (600, 400, 2.0)
    );
    assert!(Screen::parse("0x5").is_err());
    assert!(Screen::parse("watch").is_err());
}
