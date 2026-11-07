//! The semantic tree as screen readers get it (AccessKit), and their
//! requests back into the session.

use accesskit::{Action, ActionData, ActionRequest, Node, NodeId, Role, Toggled, TreeId};
use construct_render::Viewport;
use construct_render::accessibility::{self, ROOT, Request, Tree};
use construct_render::interact::{Session, Target};

use crate::common::fixture;

fn session(name: &str) -> Session {
    let (root, data) = fixture(name);
    Session::new(name, root, data, Viewport::PHONE)
}

fn tree_of(s: &mut Session) -> Tree {
    let form = s.form.clone();
    let focused = s.focused.clone();
    let scene = s.scene().unwrap();
    accessibility::tree(scene, &form, focused.as_deref(), "The Construct")
}

fn node<'t>(t: &'t Tree, id: &str) -> (NodeId, &'t Node) {
    let (k, _) = t.nodes.iter().find(|(_, e)| e.id == id).unwrap();
    let (_, n) = t.update.nodes.iter().find(|(i, _)| i == k).unwrap();
    (*k, n)
}

fn ask(t: &Tree, id: NodeId, action: Action, data: Option<ActionData>) -> Option<Request> {
    accessibility::request(
        &t.nodes,
        &ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: id,
            data,
        },
    )
}

#[test]
fn home_reads_as_labels_buttons_a_switch_an_image_and_a_field() {
    let mut s = session("home");
    let t = tree_of(&mut s);
    let (_, root) = &t.update.nodes[0];
    assert_eq!(t.update.nodes[0].0, ROOT);
    assert_eq!(root.role(), Role::Window);
    assert_eq!(t.update.focus, ROOT);

    let (_, text) = node(&t, "root/0/0/0");
    assert_eq!(text.role(), Role::Label);
    assert_eq!(text.value(), Some("Good morning, Robin"));

    let (save, button) = node(&t, "root/0/0/2/0");
    assert_eq!(button.role(), Role::Button);
    assert_eq!(button.label(), Some("Save"));
    assert!(button.supports_action(Action::Click));
    assert_eq!(
        ask(&t, save, Action::Click, None),
        Some(Request::Tap("root/0/0/2/0".into()))
    );

    let (_, switch) = node(&t, "root/0/0/4/2");
    assert_eq!(switch.role(), Role::Switch);
    assert_eq!(switch.label(), Some("Quiet hours"));
    assert_eq!(switch.toggled(), Some(Toggled::False));

    let (_, image) = node(&t, "root/0/0/5");
    assert_eq!(image.role(), Role::Image);
    assert_eq!(image.label(), Some("The garden"));

    let (field, input) = node(&t, "field:note");
    assert_eq!(input.role(), Role::TextInput);
    assert_eq!(input.label(), Some("Add a note"));
    assert_eq!(input.value(), Some(""));
    assert_eq!(
        ask(
            &t,
            field,
            Action::SetValue,
            Some(ActionData::Value("Bring gloves".into()))
        ),
        Some(Request::SetValue {
            field: "note".into(),
            value: "Bring gloves".into()
        })
    );
    // Nothing to send yet: the Send button is there but disabled.
    let (_, send) = node(&t, "send:note");
    assert_eq!(send.role(), Role::Button);
    assert!(send.is_disabled());

    // Every node is the root's or another listed node's child, once.
    let mut seen = std::collections::HashSet::new();
    for (_, n) in &t.update.nodes {
        for c in n.children() {
            assert!(seen.insert(*c), "{c:?} listed twice");
        }
    }
    assert_eq!(seen.len(), t.update.nodes.len() - 1);
}

#[test]
fn a_focused_field_has_focus_and_its_value() {
    let mut s = session("home");
    s.tap(&Target::Id("field:note".into())).unwrap();
    s.type_text("note", "Bring gloves");
    let t = tree_of(&mut s);
    let (field, input) = node(&t, "field:note");
    assert_eq!(t.update.focus, field);
    assert_eq!(input.value(), Some("Bring gloves"));
    let (_, send) = node(&t, "send:note");
    assert!(!send.is_disabled());
}

#[test]
fn a_tappable_card_is_one_button_named_by_its_text() {
    let mut s = session("task_list");
    let t = tree_of(&mut s);
    let (_, card) = node(&t, "task_5");
    assert_eq!(card.role(), Role::Button);
    assert_eq!(card.label(), Some("Open Call the plumber"));
    // Its texts fold into it, as Compose merges a clickable's semantics.
    assert!(card.children().is_empty());
    assert!(!t.nodes.values().any(|e| e.id == "root/0/3/0/i4/0/0/0/0"));

    let (_, chips) = node(&t, "chips:filter");
    assert_eq!(chips.role(), Role::RadioGroup);
    let (_, all) = node(&t, "filter=");
    assert_eq!(all.role(), Role::RadioButton);
    assert_eq!(all.is_selected(), Some(true));

    let (list, scroll) = node(&t, "root/0/3");
    assert_eq!(scroll.role(), Role::ScrollView);
    match ask(&t, list, Action::ScrollDown, None) {
        Some(Request::Scroll { id, dy }) => {
            assert_eq!(id, "root/0/3");
            assert!(dy > 0.0);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ids_stay_put_from_frame_to_frame() {
    let mut s = session("task_list");
    let before = node(&tree_of(&mut s), "task_3").0;
    s.tap(&Target::Id("filter=todo".into())).unwrap();
    s.tap(&Target::Id("filter=".into())).unwrap();
    assert_eq!(node(&tree_of(&mut s), "task_3").0, before);
}

#[test]
fn activating_a_chip_through_the_screen_reader_sets_the_filter() {
    let mut s = session("task_list");
    let t = tree_of(&mut s);
    let (done, _) = node(&t, "filter=done");
    let Some(Request::Tap(id)) = ask(&t, done, Action::Click, None) else {
        panic!("not a tap");
    };
    s.tap(&Target::Id(id)).unwrap();
    assert_eq!(s.form.get("filter").map(String::as_str), Some("done"));
    let t = tree_of(&mut s);
    assert_eq!(node(&t, "filter=done").1.is_selected(), Some(true));
}
