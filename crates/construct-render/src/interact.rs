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
use std::time::{Duration, Instant};

use crate::cpu::CpuRenderer;
use crate::layout::{Input, Laid, RenderError, SceneBuilder, Viewport};
use crate::scene::{Draw, QuadInstance, Scene, SemanticNode, Tap};
use crate::style::Rgba;

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
    /// An `action.device` command ran on the device.
    Device {
        command: String,
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

/// Where an entry is drawn (decision `edge-overlays-and-gestures`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// The whole screen: the root, or a pushed layout.
    Full,
    /// A sheet, panel or overlay from this edge, over a scrim.
    Edge(protocol::Edge),
}

/// How long a layout or screen takes to slide in or out (the shared UI's
/// `FRAME_SLIDE_MS`).
const SLIDE: Duration = Duration::from_millis(280);
/// A sheet's share of the screen's height, and a panel's of its width (at
/// most [`PANEL_MAX_WIDTH`]), as in the shared UI.
const SHEET_HEIGHT_FRACTION: f32 = 0.85;
const PANEL_WIDTH_FRACTION: f32 = 0.86;
const PANEL_MAX_WIDTH: f32 = 340.0;
/// A sheet's handle strip.
const HANDLE_STRIP: f32 = 16.0;

/// Material's standard easing, `cubic-bezier(0.4, 0, 0.2, 1)`
/// (`FastOutSlowInEasing`).
fn fast_out_slow_in(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let (x1, y1, x2, y2) = (0.4f32, 0.0f32, 0.2f32, 1.0f32);
    let bez = |a: f32, b: f32, u: f32| {
        3.0 * a * u * (1.0 - u).powi(2) + 3.0 * b * u * u * (1.0 - u) + u.powi(3)
    };
    // Solve x(u) = t by bisection, then y(u).
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if bez(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(y1, y2, (lo + hi) / 2.0)
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
    /// Scroll offsets of this entry's scrolling views (by semantic id).
    scroll: BTreeMap<String, f32>,
    frame: Frame,
    /// A sheet's height or a panel's width (`size_dp`).
    size: Option<f32>,
    /// A screen opened inside its layout's sheet or panel.
    in_frame: bool,
    opened: Instant,
    closing: Option<Instant>,
    /// Laid out at this size (points, with these insets).
    laid: Option<(Laid, [f32; 2])>,
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
            scroll: BTreeMap::new(),
            frame: Frame::Full,
            size: None,
            in_frame: false,
            opened: Instant::now(),
            closing: None,
            laid: None,
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
    /// An `action.device` command the device answers with data (a layout's
    /// versions); `None` when this loader doesn't run it.
    fn device(&self, _command: &str, _app: &str, _id: &str) -> Option<Result<DataBundle, String>> {
        None
    }
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
    pub layouts: BTreeMap<String, UiNode>,
    pub viewport: Viewport,
    pub dark: bool,
    builder: SceneBuilder,
    cpu: CpuRenderer,
    scene: Option<Scene>,
    /// Slide layouts in and out (a window); off, every frame is settled
    /// (tests, previews).
    pub animate: bool,
    /// The text field being typed into (drawn with a caret).
    pub focused: Option<String>,
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
            layouts: BTreeMap::new(),
            viewport,
            dark: false,
            builder: SceneBuilder::new(),
            cpu: CpuRenderer::new(),
            scene: None,
            animate: false,
            focused: None,
            loader: None,
            images: crate::images::ImageStore::new(),
            insets: [0.0; 4],
        }
    }

    /// The id of the layout on top of the stack.
    pub fn current(&self) -> &str {
        &self.stack[self.top_index()].id
    }

    /// The entry taps go to: the last one not closing.
    fn top_index(&self) -> usize {
        self.stack
            .iter()
            .rposition(|e| e.closing.is_none())
            .unwrap_or(0)
    }

    /// The first entry of the sheet or panel `i` is drawn in (`i` itself
    /// unless it is a screen opened inside one).
    fn group_start(&self, mut i: usize) -> usize {
        while i > 0 && self.stack[i].in_frame {
            i -= 1;
        }
        i
    }

    /// How far entry `i` has slid in (0 to 1), or out when closing.
    fn progress(&self, i: usize, now: Instant) -> f32 {
        if !self.animate || i == 0 {
            return if self.stack[i].closing.is_some() {
                0.0
            } else {
                1.0
            };
        }
        let e = &self.stack[i];
        let t = |since: Instant| now.duration_since(since).as_secs_f32() / SLIDE.as_secs_f32();
        match e.closing {
            Some(c) => 1.0 - fast_out_slow_in(t(c)),
            None => fast_out_slow_in(t(e.opened)),
        }
    }

    /// Whether a slide is still moving (draw again next frame).
    pub fn animating(&self) -> bool {
        if !self.animate {
            return false;
        }
        let now = Instant::now();
        self.stack
            .iter()
            .skip(1)
            .any(|e| now.duration_since(e.closing.unwrap_or(e.opened)) < SLIDE)
    }

    /// The box a sheet or panel group draws in, and the box and insets its
    /// layout gets (points, settled: no slide).
    fn frame_rects(&self, i: usize) -> ([f32; 4], [f32; 4], [f32; 4]) {
        let g = self.group_start(i);
        let (w, h) = (self.viewport.width, self.viewport.height);
        let [it, _, ib, _] = self.insets;
        let e = &self.stack[g];
        match e.frame {
            Frame::Full => ([0.0, 0.0, w, h], [0.0, 0.0, w, h], self.insets),
            Frame::Edge(edge) => match edge {
                protocol::Edge::Bottom => {
                    let sh = e.size.unwrap_or(h * SHEET_HEIGHT_FRACTION).min(h);
                    let f = [0.0, h - sh, w, sh];
                    (
                        f,
                        [0.0, h - sh + HANDLE_STRIP, w, sh - HANDLE_STRIP],
                        [0.0, 0.0, ib, 0.0],
                    )
                }
                protocol::Edge::Top => {
                    let sh = e.size.unwrap_or(h * SHEET_HEIGHT_FRACTION).min(h);
                    (
                        [0.0, 0.0, w, sh],
                        [0.0, 0.0, w, sh - HANDLE_STRIP],
                        [it, 0.0, 0.0, 0.0],
                    )
                }
                protocol::Edge::Start | protocol::Edge::End => {
                    let pw = e
                        .size
                        .unwrap_or((w * PANEL_WIDTH_FRACTION).min(PANEL_MAX_WIDTH))
                        .min(w);
                    let x = if edge == protocol::Edge::Start {
                        0.0
                    } else {
                        w - pw
                    };
                    let f = [x, 0.0, pw, h];
                    (f, f, [it, 0.0, ib, 0.0])
                }
            },
        }
    }

    /// Lays out entry `i` for its box, with its device data.
    fn layout_entry(&mut self, i: usize) -> Result<(), RenderError> {
        let (_, content, insets) = self.frame_rects(i);
        let size = [content[2], content[3]];
        if self.stack[i].laid.as_ref().is_some_and(|(_, s)| *s == size) {
            return Ok(());
        }
        let e = &self.stack[i];
        let group = &self.stack[self.group_start(i)];
        let presentation = if i == 0 {
            "root"
        } else if e.in_frame {
            "screen"
        } else {
            match e.frame {
                Frame::Full => "push",
                Frame::Edge(protocol::Edge::Bottom | protocol::Edge::Top) => "sheet",
                Frame::Edge(_) => "panel",
            }
        };
        let mut nav = protocol::EntityRecord::new();
        nav.set("can_go_back", i > 0);
        nav.set(
            "pushed",
            presentation == "push" || (presentation == "screen" && group.frame == Frame::Full),
        );
        nav.set("inline", false);
        nav.set("in_frame", group.frame != Frame::Full);
        nav.set("presentation", presentation);
        self.data.entities.insert("local://nav".into(), nav);
        let mut screen = protocol::EntityRecord::new();
        screen.set("loading", false);
        screen.set("error", e.error.clone().unwrap_or_default());
        self.data.entities.insert("local://screen".into(), screen);
        let vp = Viewport {
            width: size[0],
            height: size[1],
            scale: self.viewport.scale,
        };
        let e = &self.stack[i];
        let laid = self.builder.layout(&Input {
            root: &e.root,
            data: &self.data,
            form: &self.form,
            route: &e.route,
            layouts: &self.layouts,
            scroll: &e.scroll,
            viewport: vp,
            dark: self.dark,
            item: e.item.as_deref(),
            insets,
            focused: self.focused.as_deref(),
        })?;
        self.stack[i].laid = Some((laid, size));
        Ok(())
    }

    /// Lays out the current frame (cached until something changes; a
    /// scroll only places the laid-out frames again). Draws the top
    /// full-screen layout, and over it any sheets, panels and overlays with
    /// their scrim; only the top one's nodes can be tapped.
    pub fn scene(&mut self) -> Result<&Scene, RenderError> {
        let now = Instant::now();
        let animate = self.animate;
        let before = self.stack.len();
        let mut k = 0;
        self.stack.retain(|e| {
            k += 1;
            k == 1
                || e.closing
                    .is_none_or(|c| animate && now.duration_since(c) < SLIDE)
        });
        if self.stack.len() != before || self.animating() {
            self.scene = None;
        }
        // (`if let` returning the borrow here is refused by today's borrow
        // checker: the scene is built below on the other path.)
        #[allow(clippy::unnecessary_unwrap)]
        if self.scene.is_some() {
            return Ok(self.scene.as_ref().expect("checked"));
        }
        self.load_inline_layouts();
        // From the top down to the first settled full-screen layout.
        let mut visible = Vec::new();
        for i in (0..self.stack.len()).rev() {
            visible.push(i);
            let e = &self.stack[i];
            if i == 0 || (e.frame == Frame::Full && !e.in_frame && self.progress(i, now) >= 1.0) {
                break;
            }
        }
        visible.reverse();
        for &i in &visible {
            self.layout_entry(i)?;
        }
        let (w, h) = (self.viewport.width, self.viewport.height);
        let top = self.top_index();
        let top_group = self.group_start(top);
        let theme = self.stack[visible[0]]
            .laid
            .as_ref()
            .expect("laid out")
            .0
            .theme()
            .clone();
        let mut scene = Scene {
            width: w,
            height: h,
            scale: self.viewport.scale,
            background: theme.background.0,
            ..Default::default()
        };
        let mut keep_from = 0;
        for &i in &visible {
            let p = self.progress(i, now);
            let (frame, content, _) = self.frame_rects(i);
            let e = &self.stack[i];
            if i == top_group {
                keep_from = scene.semantics.len();
            }
            // Where the entry's frame sits now (sliding in or out).
            let g = self.group_start(i);
            let gp = self.progress(g, now);
            let slide = |frame: Frame, size: [f32; 2], p: f32| -> (f32, f32) {
                let q = 1.0 - p;
                match frame {
                    Frame::Full => (q * w, 0.0),
                    Frame::Edge(protocol::Edge::Bottom) => (0.0, q * size[1]),
                    Frame::Edge(protocol::Edge::Top) => (0.0, -q * size[1]),
                    Frame::Edge(protocol::Edge::Start) => (-q * size[0], 0.0),
                    Frame::Edge(protocol::Edge::End) => (q * size[0], 0.0),
                }
            };
            let group_frame = self.stack[g].frame;
            let (fx, fy) = if g == 0 {
                (0.0, 0.0)
            } else {
                slide(group_frame, [frame[2], frame[3]], gp)
            };
            let frame_now = [frame[0] + fx, frame[1] + fy, frame[2], frame[3]];
            let (laid, _) = e.laid.as_ref().expect("laid out");
            if e.in_frame {
                // A screen inside its layout's box slides in from the side.
                let dx = (1.0 - p) * frame[2];
                let r = [
                    content[0] + fx + dx,
                    content[1] + fy,
                    content[2],
                    content[3],
                ];
                let mut q = QuadInstance::fill(r, theme.background, 0.0);
                q.clip = frame_now;
                scene.draws.push(Draw::Quad(q));
                laid.emit_into(&mut scene, &e.scroll, (r[0], r[1]), frame_now);
                continue;
            }
            match e.frame {
                Frame::Full => {
                    if i > 0 {
                        // Covers what is under it as it slides in.
                        let mut q = QuadInstance::fill(frame_now, theme.background, 0.0).shadow(
                            Rgba([0.0, 0.0, 0.0, 0.18]),
                            [0.0, 0.0],
                            8.0,
                            0.0,
                        );
                        q.clip = [0.0, 0.0, w, h];
                        scene.draws.push(Draw::Quad(q));
                    }
                    laid.emit_into(
                        &mut scene,
                        &e.scroll,
                        (frame_now[0], frame_now[1]),
                        [0.0, 0.0, w, h],
                    );
                }
                Frame::Edge(edge) => {
                    let scrim =
                        QuadInstance::fill([0.0, 0.0, w, h], Rgba([0.0, 0.0, 0.0, 0.32 * p]), 0.0);
                    scene.draws.push(Draw::Quad(scrim));
                    if i == top_group {
                        scene.semantics.push(SemanticNode {
                            id: "scrim".into(),
                            node_type: "scrim".into(),
                            bounds: [0.0, 0.0, w, h],
                            text_content: None,
                            interactive: true,
                            tap: Some(Tap::Intent {
                                intent: json!({ "intent": "action.navigation.back", "frame": true }),
                            }),
                            selected: false,
                            parent: None,
                            gestures: BTreeMap::new(),
                        });
                    }
                    let radii = match edge {
                        protocol::Edge::Bottom => [16.0, 16.0, 0.0, 0.0],
                        protocol::Edge::Top => [0.0, 0.0, 16.0, 16.0],
                        protocol::Edge::Start => [0.0, 16.0, 16.0, 0.0],
                        protocol::Edge::End => [16.0, 0.0, 0.0, 16.0],
                    };
                    let mut surface = QuadInstance::fill(frame_now, theme.background, 0.0).shadow(
                        Rgba([0.0, 0.0, 0.0, 0.25]),
                        [0.0, 2.0],
                        8.0,
                        0.0,
                    );
                    surface.radii = radii;
                    scene.draws.push(Draw::Quad(surface));
                    if matches!(edge, protocol::Edge::Bottom | protocol::Edge::Top) {
                        // The handle, on the sheet's inner side.
                        let hy = if edge == protocol::Edge::Bottom {
                            frame_now[1] + 8.0
                        } else {
                            frame_now[1] + frame_now[3] - 12.0
                        };
                        let handle = QuadInstance::fill(
                            [frame_now[0] + frame_now[2] / 2.0 - 16.0, hy, 32.0, 4.0],
                            theme.muted_text.with_alpha(0.4),
                            2.0,
                        );
                        scene.draws.push(Draw::Quad(handle));
                    }
                    let origin = (content[0] + fx, content[1] + fy);
                    let mut clip = frame_now;
                    if matches!(edge, protocol::Edge::Bottom | protocol::Edge::Top) {
                        clip = [origin.0, origin.1, content[2], content[3]];
                    }
                    laid.emit_into(&mut scene, &e.scroll, origin, clip);
                }
            }
        }
        // Only the top layout (or sheet, with its scrim) answers taps.
        if keep_from > 0 {
            scene.semantics.drain(..keep_from);
            for n in &mut scene.semantics {
                n.parent = n.parent.and_then(|p| p.checked_sub(keep_from));
            }
        }
        for d in &scene.draws {
            if let Draw::Image(i) = d {
                self.images.request(&i.url);
            }
        }
        self.scene = Some(scene);
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

    /// Lays the frame out again next time (data, form or size changed).
    pub fn invalidate(&mut self) {
        for e in &mut self.stack {
            e.laid = None;
        }
        self.scene = None;
    }

    /// New platform insets (points): the next frame lays out again.
    pub fn set_insets(&mut self, insets: [f32; 4]) {
        if insets != self.insets {
            self.insets = insets;
            self.invalidate();
        }
    }

    /// A new viewport (a window resize): the next frame lays out again.
    pub fn resize(&mut self, viewport: Viewport) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.invalidate();
        }
    }

    pub fn set_form(&mut self, field: &str, value: &str) {
        self.form.insert(field.to_owned(), value.to_owned());
        self.invalidate();
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
        // A tap anywhere but a field ends typing (the field's tap sets it again).
        self.focused = None;
        let effects = match node.tap.clone() {
            None => vec![Effect::Nothing],
            Some(Tap::SetForm { field, value }) => {
                self.set_form(&field, &value);
                vec![Effect::FormSet { field, value }]
            }
            Some(Tap::Focus { field }) => {
                self.focused = Some(field.clone());
                vec![Effect::Focused { field }]
            }
            Some(Tap::Intent { intent }) => {
                let mut out = Vec::new();
                self.run(&intent, &mut out);
                out
            }
        };
        self.invalidate();
        Ok(Outcome {
            node: Some(node),
            effects,
        })
    }

    /// Runs an intent as a tap would (system Back, a test).
    pub fn dispatch(&mut self, intent: &Value) -> Result<Outcome, String> {
        let mut effects = Vec::new();
        self.run(intent, &mut effects);
        self.invalidate();
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
        let ti = self.top_index();
        let top = &mut self.stack[ti];
        let max = top
            .laid
            .as_ref()
            .and_then(|(l, _)| l.scroll_max.get(&id).copied())
            .unwrap_or(f32::MAX);
        let before = top.scroll.get(&id).copied().unwrap_or(0.0).min(max);
        let offset = (before + dy).clamp(0.0, max);
        if offset == before {
            // At an end: nothing moves (a fling stops, a pull may be a swipe).
            return Ok(Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            });
        }
        top.scroll.insert(id.clone(), offset);
        self.scene = None;
        Ok(Outcome {
            node: None,
            effects: vec![Effect::Scrolled { id, offset }],
        })
    }

    /// A swipe (`swipe_up`, `swipe_down`, `swipe_left`, `swipe_right`) or a
    /// `long_press` at a point: the innermost `core://Gesture` there with that
    /// handler runs it.
    pub fn gesture(&mut self, x: f32, y: f32, kind: &str) -> Result<Outcome, String> {
        let scene = self.scene().map_err(|e| e.to_string())?;
        let found = scene.semantics.iter().rev().find(|n| {
            let [bx, by, bw, bh] = n.bounds;
            x >= bx && x < bx + bw && y >= by && y < by + bh && n.gestures.contains_key(kind)
        });
        let Some(node) = found.cloned() else {
            return Ok(Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            });
        };
        let mut effects = Vec::new();
        self.run(&node.gestures[kind], &mut effects);
        self.invalidate();
        Ok(Outcome {
            node: Some(node),
            effects,
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
                    if let Some(root) = self.layout_root(id, out) {
                        let route = intent["params"]
                            .as_object()
                            .map(|m| {
                                m.iter()
                                    .map(|(k, v)| (k.clone(), crate::bind::render(v)))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut entry = Entry::layout(id, root, route);
                        entry.frame = match intent["presentation"].as_str() {
                            Some("Sheet") => Frame::Edge(protocol::Edge::Bottom),
                            Some("Panel") => Frame::Edge(protocol::Edge::Start),
                            Some("Overlay") => Frame::Edge(
                                serde_json::from_value(intent["edge"].clone()).unwrap_or_default(),
                            ),
                            _ => Frame::Full,
                        };
                        entry.size = intent["size_dp"].as_f64().map(|v| v as f32);
                        self.stack.push(entry);
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
            "action.device" => {
                let command = intent["command"].as_str().unwrap_or("").to_owned();
                let app = intent["app"]
                    .as_str()
                    .or(intent["layout"].as_str())
                    .unwrap_or("")
                    .to_owned();
                let id = intent["id"].as_str().unwrap_or("").to_owned();
                match self
                    .loader
                    .as_ref()
                    .and_then(|l| l.device(&command, &app, &id))
                {
                    Some(Ok(d)) => {
                        merge(&mut self.data, d);
                        out.push(Effect::Device { command });
                    }
                    Some(Err(error)) => out.push(Effect::LoadFailed {
                        what: command,
                        error,
                    }),
                    None => out.push(Effect::NotSent {
                        intent: intent.clone(),
                    }),
                }
            }
            "action.data.load" => {
                let path = intent["path"].as_str().unwrap_or("").to_owned();
                self.load_data(&path, out);
            }
            "action.navigation.back" => {
                let top = self.top_index();
                if top == 0 {
                    out.push(Effect::Nothing);
                } else {
                    // A scrim tap closes the whole sheet or panel; Back
                    // closes the top screen.
                    let from = if intent["frame"] == true {
                        self.group_start(top)
                    } else {
                        top
                    };
                    let now = Instant::now();
                    self.stack.truncate(top + 1);
                    if self.animate {
                        for e in &mut self.stack[from..] {
                            e.closing.get_or_insert(now);
                        }
                    } else {
                        self.stack.truncate(from.max(1));
                    }
                    self.focused = None;
                    out.push(Effect::Back);
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
        let top = &self.stack[self.top_index()];
        let Some((screen, route)) = top
            .screens
            .iter()
            .find_map(|s| match_route(&s.route, uri).map(|r| (s.clone(), r)))
        else {
            return false;
        };
        let (id, screens, frame, size) = (top.id.clone(), top.screens.clone(), top.frame, top.size);
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
            scroll: BTreeMap::new(),
            in_frame: frame != Frame::Full,
            frame,
            size,
            opened: Instant::now(),
            closing: None,
            laid: None,
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
        let ti = self.top_index();
        let top = &mut self.stack[ti];
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
        let top = &self.stack[self.top_index()];
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
        json!({ "layout": self.current(), "form": self.form,
                "scroll": self.stack.last().map(|e| e.scroll.clone()).unwrap_or_default(),
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
