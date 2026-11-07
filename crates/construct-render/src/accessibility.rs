//! The scene's semantic tree for screen readers, through AccessKit: every
//! frame's [`SemanticNode`]s become an AccessKit tree (roles, labels,
//! values, states, bounds), and what a screen reader asks for (activate,
//! focus, scroll, set a text field's value) comes back as a [`Request`]
//! the window runs on its session. The window hands the tree to the
//! platform (`accesskit_winit`: VoiceOver on macOS, Narrator on Windows,
//! Orca on Linux).
//!
//! As Compose merges a clickable's descendants into it, an interactive
//! node is one element: its label is its own text, else its descendants'
//! text, and its descendants aren't listed. A dialog's or panel's scrim
//! hides everything drawn under it.

use std::collections::{BTreeMap, HashMap, HashSet};

use accesskit::{
    Action, ActionData, ActionRequest, Affine, Node, NodeId, Rect, Role, ScrollUnit, Toggled,
    TreeId, TreeInfo, TreeUpdate,
};

use crate::scene::{Scene, SemanticNode, Tap};

/// The window: the tree's root, holding the layout's nodes.
pub const ROOT: NodeId = NodeId(0);

/// A scene as an AccessKit tree, and which semantic node each AccessKit
/// node stands for (for the requests that come back).
pub struct Tree {
    pub update: TreeUpdate,
    pub nodes: HashMap<NodeId, Exposed>,
}

/// A semantic node a screen reader can act on.
#[derive(Debug, Clone, PartialEq)]
pub struct Exposed {
    /// [`SemanticNode::id`].
    pub id: String,
    /// The form field it edits (a text field).
    pub field: Option<String>,
    /// Its height in points (a scrolling view scrolls by about a page).
    pub height: f32,
}

/// What a screen reader asked the session to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Activate (or focus, for a text field) the node with this id, as a
    /// tap on it does.
    Tap(String),
    /// Scroll the scrolling view with this id by `dy` points (positive:
    /// content moves up).
    Scroll { id: String, dy: f32 },
    /// Replace a text field's value.
    SetValue { field: String, value: String },
}

/// The tree for a scene: `scale` is device pixels per point (AccessKit
/// wants pixels), `form` the session's form values (a text field's
/// value), `focused` the text field being typed into.
pub fn tree(
    scene: &Scene,
    form: &BTreeMap<String, String>,
    focused: Option<&str>,
    title: &str,
) -> Tree {
    let sem = &scene.semantics;
    // Nodes drawn before the last scrim are under it.
    let first = sem
        .iter()
        .rposition(|n| n.node_type == "scrim")
        .unwrap_or(0);
    let shown = |i: usize| {
        let [_, _, w, h] = sem[i].bounds;
        i >= first && w > 0.0 && h > 0.0
    };
    // Each node's nearest listed ancestor; an interactive node's
    // descendants fold into it.
    let mut listed = vec![false; sem.len()];
    let mut owner: Vec<Option<usize>> = vec![None; sem.len()];
    let mut merged: Vec<Option<usize>> = vec![None; sem.len()];
    for i in 0..sem.len() {
        let parent = sem[i].parent.filter(|&p| p < i);
        merged[i] = parent
            .and_then(|p| merged[p].or_else(|| (listed[p] && sem[p].interactive).then_some(p)));
        owner[i] = parent.and_then(|p| if listed[p] { Some(p) } else { owner[p] });
        listed[i] = shown(i) && merged[i].is_none();
    }
    // Text folded into each interactive node, in reading order.
    let mut folded: Vec<Vec<&str>> = vec![Vec::new(); sem.len()];
    for i in 0..sem.len() {
        if let (Some(m), Some(t)) = (merged[i], sem[i].text_content.as_deref()) {
            folded[m].push(t);
        }
    }

    let mut ids = Ids::default();
    let node_ids: Vec<NodeId> = sem.iter().map(|n| ids.next(&n.id)).collect();
    let mut children: Vec<Vec<NodeId>> = vec![Vec::new(); sem.len()];
    let mut top = Vec::new();
    for i in (0..sem.len()).filter(|&i| listed[i]) {
        match owner[i] {
            Some(p) => children[p].push(node_ids[i]),
            None => top.push(node_ids[i]),
        }
    }

    let mut nodes = Vec::new();
    let mut exposed = HashMap::new();
    let mut focus = ROOT;
    for i in (0..sem.len()).filter(|&i| listed[i]) {
        let n = &sem[i];
        let field = match &n.tap {
            Some(Tap::Focus { field }) => Some(field.clone()),
            _ => None,
        };
        let mut node = describe(n, field.as_deref(), form, &folded[i]);
        node.set_children(std::mem::take(&mut children[i]));
        if field.is_some() && field.as_deref() == focused {
            focus = node_ids[i];
        }
        exposed.insert(
            node_ids[i],
            Exposed {
                id: n.id.clone(),
                field,
                height: n.bounds[3],
            },
        );
        nodes.push((node_ids[i], node));
    }

    let mut root = Node::new(Role::Window);
    root.set_label(title);
    root.set_transform(Affine::scale(scene.scale.max(0.01) as f64));
    root.set_bounds(rect([0.0, 0.0, scene.width, scene.height]));
    root.set_children(top);
    nodes.insert(0, (ROOT, root));
    let mut info = TreeInfo::new(ROOT);
    info.toolkit_name = Some("The Construct".into());
    info.toolkit_version = Some(env!("CARGO_PKG_VERSION").into());
    Tree {
        update: TreeUpdate {
            nodes,
            tree: Some(info),
            tree_id: TreeId::ROOT,
            focus,
        },
        nodes: exposed,
    }
}

/// One semantic node as an AccessKit node (without its children).
fn describe(
    n: &SemanticNode,
    field: Option<&str>,
    form: &BTreeMap<String, String>,
    folded: &[&str],
) -> Node {
    let text = n.text_content.clone().filter(|t| !t.trim().is_empty());
    let label = text.clone().or_else(|| {
        let joined = folded.join(", ");
        (!joined.is_empty()).then_some(joined)
    });
    let role = match n.node_type.as_str() {
        _ if field.is_some() => Role::TextInput,
        "core://Switch" => Role::Switch,
        "core://ChoiceChips/option" => Role::RadioButton,
        "core://ChoiceChips" => Role::RadioGroup,
        "core://Scroll" => Role::ScrollView,
        "core://List" => Role::List,
        "core://Image" => Role::Image,
        "core://Dock/tab" => Role::Tab,
        "menu" => Role::MenuItem,
        // The Send button next to a text field (`send:<field>`).
        "core://TextField" => Role::Button,
        _ if n.interactive => Role::Button,
        _ if label.is_some() => Role::Label,
        _ => Role::GenericContainer,
    };
    let mut node = Node::new(role);
    node.set_bounds(rect(n.bounds));
    match role {
        Role::TextInput => {
            let field = field.unwrap_or_default();
            node.set_value(form.get(field).cloned().unwrap_or_default());
            if let Some(hint) = n.hint.as_deref().filter(|h| !h.is_empty()) {
                node.set_placeholder(hint);
                node.set_label(hint);
            }
            node.add_action(Action::Focus);
            node.add_action(Action::SetValue);
        }
        // A label's text is its value (AccessKit's convention).
        Role::Label => node.set_value(label.unwrap_or_default()),
        Role::Switch => {
            if let Some(l) = label {
                node.set_label(l);
            }
            node.set_toggled(Toggled::from(n.selected));
        }
        Role::RadioButton | Role::Tab => {
            if let Some(l) = label {
                node.set_label(l);
            }
            node.set_selected(n.selected);
            if role == Role::RadioButton {
                node.set_toggled(Toggled::from(n.selected));
            }
        }
        Role::ScrollView => {
            node.add_action(Action::ScrollDown);
            node.add_action(Action::ScrollUp);
        }
        _ if n.node_type == "scrim" => node.set_label("Dismiss"),
        _ => {
            if let Some(l) = label {
                node.set_label(l);
            }
        }
    }
    if n.interactive && role != Role::TextInput {
        node.add_action(Action::Click);
    } else if role == Role::Button && !n.interactive {
        node.set_disabled();
    }
    node
}

/// What a screen reader's request asks of the session, if anything.
pub fn request(nodes: &HashMap<NodeId, Exposed>, req: &ActionRequest) -> Option<Request> {
    let n = nodes.get(&req.target_node)?;
    match req.action {
        Action::Click | Action::Focus => Some(Request::Tap(n.id.clone())),
        Action::SetValue | Action::ReplaceSelectedText => {
            let Some(ActionData::Value(v)) = &req.data else {
                return None;
            };
            Some(Request::SetValue {
                field: n.field.clone()?,
                value: v.to_string(),
            })
        }
        Action::ScrollDown | Action::ScrollUp => {
            let step = match req.data {
                Some(ActionData::ScrollUnit(ScrollUnit::Item)) => 48.0,
                _ => (n.height * 0.8).max(48.0),
            };
            let dy = if req.action == Action::ScrollDown {
                step
            } else {
                -step
            };
            Some(Request::Scroll {
                id: n.id.clone(),
                dy,
            })
        }
        _ => None,
    }
}

fn rect([x, y, w, h]: [f32; 4]) -> Rect {
    Rect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64)
}

/// AccessKit ids from semantic ids: the same node keeps its id from frame
/// to frame (a screen reader keeps its place); a repeated id gets another.
#[derive(Default)]
struct Ids {
    taken: HashSet<u64>,
}

impl Ids {
    fn next(&mut self, id: &str) -> NodeId {
        let mut h = fnv(id.as_bytes());
        while h == ROOT.0 || !self.taken.insert(h) {
            h = fnv(&h.to_le_bytes());
        }
        NodeId(h)
    }
}

/// FNV-1a: stable across runs and builds, unlike std's hasher.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sem(id: &str, node_type: &str, text: Option<&str>, tap: bool) -> SemanticNode {
        SemanticNode {
            id: id.into(),
            node_type: node_type.into(),
            bounds: [0.0, 0.0, 100.0, 40.0],
            text_content: text.map(Into::into),
            interactive: tap,
            tap: tap.then(|| Tap::Intent {
                intent: serde_json::json!({ "intent": "action.navigation.back" }),
            }),
            selected: false,
            parent: None,
            gestures: BTreeMap::new(),
            hint: None,
        }
    }

    #[test]
    fn a_scrim_hides_what_is_under_it_and_repeated_ids_stay_apart() {
        let scene = Scene {
            width: 390.0,
            height: 844.0,
            scale: 2.0,
            semantics: vec![
                sem("under", "core://Button", Some("Under"), true),
                sem("scrim", "scrim", None, true),
                sem("ok", "core://Button", Some("OK"), true),
                sem("ok", "core://Button", Some("OK"), true),
            ],
            ..Default::default()
        };
        let t = tree(&scene, &BTreeMap::new(), None, "The Construct");
        let ids: Vec<&str> = t.nodes.values().map(|e| e.id.as_str()).collect();
        assert!(!ids.contains(&"under"));
        assert_eq!(ids.iter().filter(|i| **i == "ok").count(), 2);
        let root = &t.update.nodes[0].1;
        assert_eq!(root.children().len(), 3);
        assert_eq!(root.transform(), Some(&Affine::scale(2.0)));
    }
}
