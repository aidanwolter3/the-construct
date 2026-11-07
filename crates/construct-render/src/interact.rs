//! A layout with its state (form values, scroll offsets, a back stack),
//! drawn frame by frame and driven by virtual taps and scrolls: what the
//! desktop window, the Android surface and the pod's MCP previews run.
//!
//! Taps change only device state, as the app does: a chip sets its form
//! value, `action.local.set` changes the local copy of an entity, opening a
//! layout pushes it. Intents that would reach the pod (mutations, sign-in,
//! device commands) are reported, never sent.

use image::RgbaImage;
use protocol::{DataBundle, PageScreen, UiNode};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::cpu::CpuRenderer;
use crate::layout::{Input, RenderError, SceneBuilder, Viewport};
use crate::scene::{Scene, SemanticNode, Tap};

/// What a tap or scroll aims at.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A semantic node id (`filter=started`, a `tag`, a path).
    Id(String),
    /// The node showing this text.
    Text(String),
    /// A point in logical points.
    Point(f32, f32),
}

/// What an interaction did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    FormSet {
        field: String,
        value: String,
    },
    Focused {
        field: String,
    },
    LocalSet {
        entity_uri: String,
        fields: BTreeMap<String, Value>,
    },
    Opened {
        layout: String,
    },
    OpenedScreen {
        screen: String,
    },
    /// A layout or a screen's data couldn't be loaded.
    LoadFailed {
        what: String,
        error: String,
    },
    Back,
    Scrolled {
        id: String,
        offset: f32,
    },
    /// Would go to the pod or the platform; a preview doesn't send it.
    NotSent {
        intent: Value,
    },
    Nothing,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub node: Option<SemanticNode>,
    pub effects: Vec<Effect>,
}

struct Entry {
    id: String,
    root: UiNode,
    route: BTreeMap<String, String>,
    /// A page screen's `item` (an entity URI).
    item: Option<String>,
    /// The screens of the layout this entry belongs to.
    screens: Vec<PageScreen>,
    /// The last load of this entry's data failed (`local://screen#error`).
    error: Option<String>,
}

impl Entry {
    fn layout(id: &str, root: UiNode, route: BTreeMap<String, String>) -> Entry {
        let screens = match &root {
            UiNode::Page { screens, .. } => screens.clone(),
            _ => Vec::new(),
        };
        Entry {
            id: id.to_owned(),
            root,
            route,
            item: None,
            screens,
            error: None,
        }
    }
}

/// Where a live session gets what it opens: another layout of the pod, and a
/// data path's answer (`crate::remote` on devices). Calls block.
pub trait Loader: Send {
    /// A layout's plumbing root and its data (`GET /v1/workspaces/<id>`).
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String>;
    /// A data path's answer (a screen's `data`, `action.data.load`).
    fn data(&self, path: &str) -> Result<DataBundle, String>;
}

/// `uri` against a route pattern (`collections://tasks/{id}`): the captured
/// parameters, or `None`.
pub fn match_route(pattern: &str, uri: &str) -> Option<BTreeMap<String, String>> {
    let p: Vec<&str> = pattern.split('/').collect();
    let u: Vec<&str> = uri.split('/').collect();
    if p.len() != u.len() {
        return None;
    }
    let mut out = BTreeMap::new();
    for (a, b) in p.iter().zip(&u) {
        if let Some(name) = a.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            let ok = !b.is_empty()
                && b.len() <= 64
                && b.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if !ok {
                return None;
            }
            out.insert(name.to_owned(), (*b).to_owned());
        } else if a != b {
            return None;
        }
    }
    Some(out)
}

fn merge(into: &mut DataBundle, from: DataBundle) {
    into.entities.extend(from.entities);
    into.queries.extend(from.queries);
}

/// `$route.<name>` replaced in every string of a node (a screen's tree).
fn bind_route(v: &mut Value, route: &BTreeMap<String, String>) {
    match v {
        Value::String(s) if s.contains("$route.") => {
            for (k, val) in route {
                *s = s.replace(&format!("$route.{k}"), val);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| bind_route(x, route)),
        Value::Object(m) => m.values_mut().for_each(|x| bind_route(x, route)),
        _ => {}
    }
}

/// A live layout.
pub struct Session {
    stack: Vec<Entry>,
    pub data: DataBundle,
    pub form: BTreeMap<String, String>,
    pub scroll: BTreeMap<String, f32>,
    pub layouts: BTreeMap<String, UiNode>,
    pub viewport: Viewport,
    pub dark: bool,
    builder: SceneBuilder,
    cpu: CpuRenderer,
    scene: Option<Scene>,
    /// Loads what taps open (a live session); `None`: only what is given.
    pub loader: Option<Box<dyn Loader>>,
    /// The images frames show, loaded by the store's fetcher.
    pub images: crate::images::ImageStore,
    /// Space the platform covers (status bar, camera cutout, navigation
    /// bar), in points: top, right, bottom, left.
    pub insets: [f32; 4],
}

impl Session {
    pub fn new(id: &str, root: UiNode, data: DataBundle, viewport: Viewport) -> Session {
        Session {
            stack: vec![Entry::layout(id, root, BTreeMap::new())],
            data,
            form: BTreeMap::new(),
            scroll: BTreeMap::new(),
            layouts: BTreeMap::new(),
            viewport,
            dark: false,
            builder: SceneBuilder::new(),
            cpu: CpuRenderer::new(),
            scene: None,
            loader: None,
            images: crate::images::ImageStore::new(),
            insets: [0.0; 4],
        }
    }

    /// The id of the layout on top of the stack.
    pub fn current(&self) -> &str {
        &self.stack.last().expect("never empty").id
    }

    /// Lays out the current frame (cached until something changes).
    pub fn scene(&mut self) -> Result<&Scene, RenderError> {
        if self.scene.is_none() {
            self.load_inline_layouts();
            // Device data a layout reads about where it is drawn (as the
            // shared UI's `local://nav` and `local://screen`).
            let depth = self.stack.len();
            let top = self.stack.last().expect("never empty");
            let mut nav = protocol::EntityRecord::new();
            nav.set("can_go_back", depth > 1);
            nav.set("pushed", depth > 1);
            nav.set("inline", false);
            nav.set("in_frame", false);
            nav.set("presentation", if depth > 1 { "push" } else { "root" });
            self.data.entities.insert("local://nav".into(), nav);
            let mut screen = protocol::EntityRecord::new();
            screen.set("loading", false);
            screen.set("error", top.error.clone().unwrap_or_default());
            self.data.entities.insert("local://screen".into(), screen);
            let scene = self.builder.build(&Input {
                root: &top.root,
                data: &self.data,
                form: &self.form,
                route: &top.route,
                layouts: &self.layouts,
                scroll: &self.scroll,
                viewport: self.viewport,
                dark: self.dark,
                item: top.item.as_deref(),
                insets: self.insets,
            })?;
            for d in &scene.draws {
                if let crate::scene::Draw::Image(i) = d {
                    self.images.request(&i.url);
                }
            }
            self.scene = Some(scene);
        }
        Ok(self.scene.as_ref().expect("just built"))
    }

    /// Draws the current frame on the CPU.
    pub fn render_cpu(&mut self) -> Result<RgbaImage, RenderError> {
        self.scene()?;
        let scene = self.scene.as_ref().expect("built");
        self.cpu.images = Some(self.images.clone());
        Ok(self.cpu.render(scene, &mut self.builder.text))
    }

    /// The fonts, for a GPU renderer drawing this session's scenes.
    pub fn text_engine(&mut self) -> &mut crate::text::TextEngine {
        &mut self.builder.text
    }

    /// The scene and the fonts together (a GPU frame needs both).
    pub fn scene_and_text(
        &mut self,
    ) -> Result<(&Scene, &mut crate::text::TextEngine), RenderError> {
        self.scene()?;
        Ok((self.scene.as_ref().expect("built"), &mut self.builder.text))
    }

    /// New platform insets (points): the next frame lays out again.
    pub fn set_insets(&mut self, insets: [f32; 4]) {
        if insets != self.insets {
            self.insets = insets;
            self.scene = None;
        }
    }

    /// A new viewport (a window resize): the next frame lays out again.
    pub fn resize(&mut self, viewport: Viewport) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.scene = None;
        }
    }

    pub fn set_form(&mut self, field: &str, value: &str) {
        self.form.insert(field.to_owned(), value.to_owned());
        self.scene = None;
    }

    fn find(&mut self, target: &Target) -> Result<Option<SemanticNode>, String> {
        let scene = self.scene().map_err(|e| e.to_string())?;
        let found = match target {
            Target::Id(id) => scene.find_node(id),
            Target::Text(t) => scene.find_text(t),
            Target::Point(x, y) => scene.hit_test(*x, *y),
        };
        let Some(found) = found else {
            return Ok(None);
        };
        // Tapping a label taps the button around it.
        let index = scene
            .semantics
            .iter()
            .position(|n| std::ptr::eq(n, found))
            .expect("from this scene");
        Ok(Some(
            scene
                .interactive_for(index)
                .unwrap_or(&scene.semantics[index])
                .clone(),
        ))
    }

    /// A tap on `target`.
    pub fn tap(&mut self, target: &Target) -> Result<Outcome, String> {
        let Some(node) = self.find(target)? else {
            return Err(format!("nothing at {target:?}"));
        };
        let effects = match node.tap.clone() {
            None => vec![Effect::Nothing],
            Some(Tap::SetForm { field, value }) => {
                self.set_form(&field, &value);
                vec![Effect::FormSet { field, value }]
            }
            Some(Tap::Focus { field }) => vec![Effect::Focused { field }],
            Some(Tap::Intent { intent }) => {
                let mut out = Vec::new();
                self.run(&intent, &mut out);
                out
            }
        };
        self.scene = None;
        Ok(Outcome {
            node: Some(node),
            effects,
        })
    }

    /// Runs an intent as a tap would (system Back, a test).
    pub fn dispatch(&mut self, intent: &Value) -> Result<Outcome, String> {
        let mut effects = Vec::new();
        self.run(intent, &mut effects);
        self.scene = None;
        Ok(Outcome {
            node: None,
            effects,
        })
    }

    /// Types into a text field (`field:<name>` or the form field name).
    pub fn type_text(&mut self, field: &str, text: &str) -> Outcome {
        let field = field.strip_prefix("field:").unwrap_or(field);
        self.set_form(field, text);
        Outcome {
            node: None,
            effects: vec![Effect::FormSet {
                field: field.to_owned(),
                value: text.to_owned(),
            }],
        }
    }

    /// Scrolls the scrolling view at or around `target` by `dy` points.
    pub fn scroll_by(&mut self, target: Option<&Target>, dy: f32) -> Result<Outcome, String> {
        let scene = self.scene().map_err(|e| e.to_string())?;
        let scroll_view = |n: &SemanticNode| n.node_type == "core://Scroll";
        let id = match target {
            Some(Target::Id(id)) => Some(id.clone()),
            Some(Target::Point(x, y)) => scene
                .semantics
                .iter()
                .rev()
                .filter(|n| scroll_view(n))
                .find(|n| {
                    let [bx, by, bw, bh] = n.bounds;
                    *x >= bx && *x < bx + bw && *y >= by && *y < by + bh
                })
                .map(|n| n.id.clone()),
            _ => scene
                .semantics
                .iter()
                .find(|n| scroll_view(n))
                .map(|n| n.id.clone()),
        };
        let Some(id) = id else {
            return Err("no scrolling view there".into());
        };
        let offset = (self.scroll.get(&id).copied().unwrap_or(0.0) + dy).max(0.0);
        self.scroll.insert(id.clone(), offset);
        self.scene = None;
        Ok(Outcome {
            node: None,
            effects: vec![Effect::Scrolled { id, offset }],
        })
    }

    fn run(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        match intent["intent"].as_str().unwrap_or("") {
            "action.sequence" => {
                for s in intent["steps"].as_array().into_iter().flatten() {
                    self.run(s, out);
                }
            }
            "action.confirm" => {
                // A preview confirms: the dialog is the app's own.
                self.run(&intent["then"], out);
            }
            "action.local.set" => {
                let uri = intent["entity_uri"].as_str().unwrap_or("").to_owned();
                let mut fields = BTreeMap::new();
                let mut updates = vec![(intent["field"].clone(), intent["value_expr"].clone())];
                for a in intent["also"].as_array().into_iter().flatten() {
                    updates.push((a["field"].clone(), a["value_expr"].clone()));
                }
                let entity = self.data.entities.entry(uri.clone()).or_default();
                for (f, e) in updates {
                    let (Some(f), Some(e)) = (f.as_str(), e.as_str()) else {
                        continue;
                    };
                    let v = eval(e, entity);
                    entity.set(f, v.clone());
                    fields.insert(f.to_owned(), v);
                }
                out.push(Effect::LocalSet {
                    entity_uri: uri,
                    fields,
                });
            }
            "action.navigation.open" => {
                let layout = intent["layout"].as_str().filter(|l| !l.is_empty());
                let target = intent["target_uri"].as_str().filter(|t| !t.is_empty());
                if let Some(id) = layout {
                    // Sheets, panels and overlays are drawn full screen.
                    if let Some(root) = self.layout_root(id, out) {
                        let route = intent["params"]
                            .as_object()
                            .map(|m| {
                                m.iter()
                                    .map(|(k, v)| (k.clone(), crate::bind::render(v)))
                                    .collect()
                            })
                            .unwrap_or_default();
                        self.stack.push(Entry::layout(id, root, route));
                        out.push(Effect::Opened {
                            layout: id.to_owned(),
                        });
                        if let Some(t) = target {
                            self.open_screen(t, out);
                        }
                    }
                } else if let Some(t) = target {
                    if !self.open_screen(t, out) {
                        out.push(Effect::NotSent {
                            intent: intent.clone(),
                        });
                    }
                } else {
                    out.push(Effect::NotSent {
                        intent: intent.clone(),
                    });
                }
            }
            "action.data.load" => {
                let path = intent["path"].as_str().unwrap_or("").to_owned();
                self.load_data(&path, out);
            }
            "action.navigation.back" => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                    out.push(Effect::Back);
                } else {
                    out.push(Effect::Nothing);
                }
            }
            _ => out.push(Effect::NotSent {
                intent: intent.clone(),
            }),
        }
    }

    /// A layout's root: given, or loaded (its data merged).
    fn layout_root(&mut self, id: &str, out: &mut Vec<Effect>) -> Option<UiNode> {
        if let Some(r) = self.layouts.get(id) {
            return Some(r.clone());
        }
        let loaded = self.loader.as_ref()?.layout(id);
        match loaded {
            Ok((root, data)) => {
                merge(&mut self.data, data);
                self.layouts.insert(id.to_owned(), root.clone());
                Some(root)
            }
            Err(error) => {
                out.push(Effect::LoadFailed {
                    what: format!("layout {id}"),
                    error,
                });
                None
            }
        }
    }

    /// Opens the screen of the top layout whose route matches `uri`, loading
    /// its data; false when no screen matches.
    fn open_screen(&mut self, uri: &str, out: &mut Vec<Effect>) -> bool {
        let top = self.stack.last().expect("never empty");
        let Some((screen, route)) = top
            .screens
            .iter()
            .find_map(|s| match_route(&s.route, uri).map(|r| (s.clone(), r)))
        else {
            return false;
        };
        let (id, screens) = (top.id.clone(), top.screens.clone());
        let mut root = serde_json::to_value(&screen.root).unwrap_or(Value::Null);
        bind_route(&mut root, &route);
        let Ok(root) = serde_json::from_value::<UiNode>(root) else {
            return false;
        };
        let item = screen.item.as_ref().map(|i| {
            let mut v = Value::String(i.clone());
            bind_route(&mut v, &route);
            crate::bind::render(&v)
        });
        self.stack.push(Entry {
            id,
            root,
            route: route.clone(),
            item,
            screens,
            error: None,
        });
        out.push(Effect::OpenedScreen {
            screen: screen.id.clone(),
        });
        if let Some(path) = &screen.data {
            let mut v = Value::String(path.clone());
            bind_route(&mut v, &route);
            self.load_data(&crate::bind::render(&v), out);
        }
        true
    }

    fn load_data(&mut self, path: &str, out: &mut Vec<Effect>) {
        let Some(loader) = self.loader.as_ref() else {
            out.push(Effect::NotSent {
                intent: json!({ "intent": "action.data.load", "path": path }),
            });
            return;
        };
        let result = loader.data(path);
        let top = self.stack.last_mut().expect("never empty");
        match result {
            Ok(d) => {
                top.error = None;
                merge(&mut self.data, d);
            }
            Err(error) => {
                top.error = Some(error.clone());
                out.push(Effect::LoadFailed {
                    what: path.to_owned(),
                    error,
                });
            }
        }
    }

    /// Loads the layouts inline `core://Layout`s of the top entry draw.
    fn load_inline_layouts(&mut self) {
        if self.loader.is_none() {
            return;
        }
        let top = self.stack.last().expect("never empty");
        let wanted: Vec<String> = top
            .root
            .walk()
            .into_iter()
            .filter_map(|n| match n {
                UiNode::Layout {
                    layout,
                    presentation: protocol::EmbedPresentation::Inline,
                    ..
                } if !layout.contains('$') => Some(layout.clone()),
                _ => None,
            })
            .filter(|l| !self.layouts.contains_key(l))
            .collect();
        let mut ignored = Vec::new();
        for id in wanted {
            self.layout_root(&id, &mut ignored);
        }
    }

    /// The field a tap focused, for platforms that show a keyboard.
    pub fn focused_field(outcome: &Outcome) -> Option<&str> {
        outcome.effects.iter().find_map(|e| match e {
            Effect::Focused { field } => Some(field.as_str()),
            _ => None,
        })
    }

    /// A summary of the state, for agents: the layout shown and the form.
    pub fn state(&self) -> Value {
        json!({ "layout": self.current(), "form": self.form, "scroll": self.scroll,
                "stack": self.stack.iter().map(|e| e.id.clone()).collect::<Vec<_>>() })
    }
}

/// The value of a `value_expr` for `action.local.set`, against the entity
/// as it is: `true`, `false`, numbers, quoted text, `entity.<f>` or
/// `!entity.<f>` (also `$item.`); anything else is the text itself.
fn eval(expr: &str, entity: &protocol::EntityRecord) -> Value {
    let e = expr.trim();
    let field = |s: &str| {
        s.strip_prefix("entity.")
            .or_else(|| s.strip_prefix("$item."))
            .map(str::to_owned)
    };
    match e {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(n) = e.parse::<i64>() {
        return json!(n);
    }
    if let Some(f) = e.strip_prefix('!').and_then(field) {
        return Value::Bool(!entity.get(&f).is_some_and(crate::bind::truthy));
    }
    if let Some(f) = field(e) {
        return entity.get(&f).cloned().unwrap_or(Value::Null);
    }
    if e.len() >= 2
        && (e.starts_with('"') && e.ends_with('"') || e.starts_with('\'') && e.ends_with('\''))
    {
        return Value::String(e[1..e.len() - 1].to_owned());
    }
    Value::String(e.to_owned())
}
