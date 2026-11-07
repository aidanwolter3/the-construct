//! A plumbing tree and its data, laid out with taffy (flexbox) and turned
//! into a [`Scene`]: quads and text runs in paint order, and the semantic
//! tree. Sizes follow the shared Compose renderer (Material 3 metrics), so a
//! frame drawn here reads like the app's.

use protocol::{
    ActionIntent, Align, DataBundle, EmbedPresentation, EntityRecord, ImageShape, Orientation,
    TextStyle, Tone, UiNode,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use taffy::prelude::*;
use taffy::{LayoutInput, LayoutOutput, compute_leaf_layout};

use crate::bind::{Ctx, fold, render};
use crate::scene::{
    Draw, IconRun, ImageRun, NO_CLIP, QuadInstance, Scene, SemanticNode, Tap, TextRun,
};
use crate::style::{CHIP_LABEL, LABEL_LARGE, Rgba, Theme, Type, type_for};
use crate::text::TextEngine;

/// Logical size and device pixels per point of a frame.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    #[serde(default = "one")]
    pub scale: f32,
}

fn one() -> f32 {
    1.0
}

impl Viewport {
    /// A phone: 390 × 844 points at 2×.
    pub const PHONE: Viewport = Viewport {
        width: 390.0,
        height: 844.0,
        scale: 2.0,
    };

    /// Pixel size of the frame.
    pub fn pixels(&self) -> (u32, u32) {
        (
            (self.width * self.scale).round().max(1.0) as u32,
            (self.height * self.scale).round().max(1.0) as u32,
        )
    }
}

/// Everything a frame is drawn from.
pub struct Input<'a> {
    pub root: &'a UiNode,
    pub data: &'a DataBundle,
    pub form: &'a BTreeMap<String, String>,
    pub route: &'a BTreeMap<String, String>,
    /// Layouts an inline `core://Layout` may draw, by id (plumbing).
    pub layouts: &'a BTreeMap<String, UiNode>,
    /// Scroll offsets (points) by the scrolling view's semantic id.
    pub scroll: &'a BTreeMap<String, f32>,
    pub viewport: Viewport,
    pub dark: bool,
    /// The entity `$item` binds throughout (a page screen's `item`).
    pub item: Option<&'a str>,
    /// Space the platform covers (top, right, bottom, left, points): the
    /// background fills it, the layout stays out of it.
    pub insets: [f32; 4],
    /// The text field being typed into (its `field`): drawn with a caret.
    pub focused: Option<&'a str>,
}

/// Why a frame couldn't be built.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderError(pub String);

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

/// Most items a list draws (a preview needs a screenful, not a feed).
const MAX_LIST_ITEMS: usize = 200;
/// How deep inline layouts may nest.
const MAX_EMBED_DEPTH: usize = 4;

/// What a leaf measures.
enum Leaf {
    Text { text: String, ty: Type },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Icon {
    Filled,
    Ring,
    Plus,
    Minus,
    Square,
    SquareRing,
}

fn icon_shape(name: &str) -> Icon {
    match name {
        "favorite" | "bookmark" | "chat_bubble" | "notifications" | "flame" | "send" => {
            Icon::Filled
        }
        "add" | "add_photo" => Icon::Plus,
        "arrow_back" | "delete" | "close" | "check" => Icon::Minus,
        "check_box" => Icon::Square,
        "check_box_outline_blank" => Icon::SquareRing,
        _ => Icon::Ring,
    }
}

/// How an element paints itself (before its children).
enum Paint {
    None,
    Quad {
        bg: Rgba,
        border: Option<Rgba>,
        radius: f32,
    },
    Text {
        text: String,
        ty: Type,
        color: Rgba,
    },
    Icon {
        name: String,
        shape: Icon,
        color: Rgba,
    },
    Spinner {
        color: Rgba,
    },
    /// A placeholder in `placeholder`, the image over it once it has loaded.
    Image {
        url: String,
        placeholder: Rgba,
        radius: f32,
    },
    Switch {
        on: bool,
        track: Rgba,
        thumb: Rgba,
    },
}

struct Sem {
    id: String,
    node_type: String,
    text: Option<String>,
    tap: Option<Tap>,
    selected: bool,
    /// `core://Gesture` handlers other than the tap, by name.
    gestures: BTreeMap<String, Value>,
}

struct Elem {
    node: NodeId,
    paint: Paint,
    sem: Option<Sem>,
    kids: Vec<usize>,
    /// Clips its children to its box (scrolling views, chip rows).
    clip: bool,
    /// A vertically scrolling view: its semantic id's offset applies.
    scrolls: bool,
    /// `core://Sticky`: pinned at the top of its scrolling view.
    sticky: bool,
    /// A carousel's track: its pages, side by side, move left by the
    /// offset at `pager:<id>` (points).
    pager: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Row,
    Column,
}

/// The build state passed down the tree.
#[derive(Clone)]
struct B<'a> {
    ctx: Ctx<'a>,
    axis: Axis,
    /// Text and icon colour set by an enclosing `core://Box`.
    color: Option<Rgba>,
    path: String,
    depth: usize,
}

impl<'a> B<'a> {
    fn child(&self, i: impl std::fmt::Display, axis: Axis) -> B<'a> {
        B {
            path: format!("{}/{i}", self.path),
            axis,
            ..self.clone()
        }
    }
}

/// A dock tab: value, label, icon, content, and the entity it was made from.
type DockEntry<'n> = (
    String,
    String,
    Option<String>,
    &'n UiNode,
    Option<(String, &'n EntityRecord)>,
);

/// Lays out frames. Holds the fonts and the measurement cache, so reuse one
/// builder for many frames.
#[derive(Default)]
pub struct SceneBuilder {
    pub text: TextEngine,
}

struct Build<'t, 'i> {
    tree: TaffyTree<Leaf>,
    elems: Vec<Elem>,
    text: &'t mut TextEngine,
    theme: Theme,
    input: &'t Input<'i>,
}

impl SceneBuilder {
    pub fn new() -> SceneBuilder {
        SceneBuilder::default()
    }

    /// A node tree (any plumbing node, JSON) with no data, at a viewport:
    /// for tests and quick checks.
    pub fn build_from_spec(
        spec: &Value,
        width: f32,
        height: f32,
        scale: f32,
    ) -> Result<Scene, RenderError> {
        let root: UiNode =
            serde_json::from_value(spec.clone()).map_err(|e| RenderError(e.to_string()))?;
        let empty = BTreeMap::new();
        let data = DataBundle::default();
        SceneBuilder::new().build(&Input {
            root: &root,
            data: &data,
            form: &empty,
            route: &empty,
            layouts: &BTreeMap::new(),
            scroll: &BTreeMap::new(),
            viewport: Viewport {
                width,
                height,
                scale,
            },
            dark: false,
            item: None,
            insets: [0.0; 4],
            focused: None,
        })
    }

    /// Lays out one frame.
    /// Lays out one frame, to place with [`Laid::emit`] (again on scroll).
    pub fn layout(&mut self, input: &Input) -> Result<Laid, RenderError> {
        let theme = Theme::new(input.dark, &input.data.entities);
        let mut b = Build {
            tree: TaffyTree::new(),
            elems: Vec::new(),
            text: &mut self.text,
            theme: theme.clone(),
            input,
        };
        let ctx = Ctx {
            entities: &input.data.entities,
            queries: &input.data.queries,
            form: input.form,
            route: input.route,
            theme: &theme,
            item: None,
            item_uri: None,
        };
        let ctx = match input
            .item
            .and_then(|u| input.data.entities.get_key_value(u))
        {
            Some((uri, e)) => ctx.with_item(uri, e),
            None => ctx,
        };
        let vp = input.viewport;
        let state = B {
            ctx,
            axis: Axis::Column,
            color: None,
            path: "root".into(),
            depth: 0,
        };
        let content = b.node(input.root, &state);
        let [it, ir, ib, il] = input.insets.map(|v| length(v.max(0.0)));
        let root_style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            padding: Rect {
                top: it,
                right: ir,
                bottom: ib,
                left: il,
            },
            size: Size {
                width: length(vp.width),
                height: length(vp.height),
            },
            ..Default::default()
        };
        let root = b.push(
            root_style,
            content.into_iter().collect(),
            Paint::None,
            None,
            false,
        );
        b.compute(root, vp.scale)?;
        let mut laid = Laid {
            tree: b.tree,
            elems: b.elems,
            root,
            theme: b.theme,
            viewport: vp,
            scroll_max: BTreeMap::new(),
            pagers: BTreeMap::new(),
        };
        laid.scroll_max = laid.scroll_extents();
        laid.pagers = laid
            .elems
            .iter()
            .filter_map(|e| {
                let id = e.pager.clone()?;
                let w = laid.tree.layout(e.node).ok()?.size.width;
                Some((id, (w, e.kids.len())))
            })
            .collect();
        Ok(laid)
    }

    /// Lays out and places one frame.
    pub fn build(&mut self, input: &Input) -> Result<Scene, RenderError> {
        Ok(self.layout(input)?.emit(input.scroll))
    }
}

fn pad(h: u16, v: u16) -> Rect<LengthPercentage> {
    Rect {
        left: length(h as f32),
        right: length(h as f32),
        top: length(v as f32),
        bottom: length(v as f32),
    }
}

fn align_items(a: Align) -> AlignItems {
    match a {
        Align::Start => AlignItems::FLEX_START,
        Align::Center => AlignItems::CENTER,
        Align::End => AlignItems::FLEX_END,
    }
}

fn justify(a: Align) -> JustifyContent {
    match a {
        Align::Start => JustifyContent::FLEX_START,
        Align::Center => JustifyContent::CENTER,
        Align::End => JustifyContent::FLEX_END,
    }
}

/// Whether a node takes the full width it is given (columns, lists, fields)
/// rather than wrapping its content.
fn fills_width(n: &UiNode) -> bool {
    match n {
        UiNode::Column { .. }
        | UiNode::Row { .. }
        | UiNode::List { .. }
        | UiNode::Divider {}
        | UiNode::TextField { .. }
        | UiNode::Scroll { .. }
        | UiNode::ChoiceChips { .. }
        | UiNode::Carousel { .. }
        | UiNode::MediaPicker { .. }
        | UiNode::Layout { .. }
        | UiNode::Container { .. }
        | UiNode::Workspace { .. }
        | UiNode::Page { .. }
        | UiNode::Dock { .. }
        | UiNode::Drawer { .. }
        | UiNode::AppFrame { .. } => true,
        UiNode::Image { size_dp, .. } => size_dp.is_none(),
        UiNode::Box {
            fill,
            width_dp,
            content,
            ..
        } => *fill || (width_dp.is_none() && fills_width(content)),
        UiNode::Tappable { content, .. }
        | UiNode::Gesture { content, .. }
        | UiNode::Conditional { content, .. }
        | UiNode::Sticky { content } => fills_width(content),
        _ => false,
    }
}

/// Whether a node grows to the height its column leaves.
fn fills_height(n: &UiNode) -> bool {
    match n {
        UiNode::Scroll { .. }
        | UiNode::Dock { .. }
        | UiNode::Drawer { .. }
        | UiNode::AppFrame { .. }
        | UiNode::Workspace { .. }
        | UiNode::Page { .. } => true,
        UiNode::Box { fill, .. } => *fill,
        UiNode::Layout {
            height_dp: None, ..
        } => true,
        UiNode::Tappable { content, .. }
        | UiNode::Gesture { content, .. }
        | UiNode::Conditional { content, .. } => fills_height(content),
        UiNode::Column { children, .. } => children.iter().any(fills_height),
        _ => false,
    }
}

/// A row child that shrinks and wraps (Compose `RowPart.Flexible`).
fn row_flexible(n: &UiNode) -> bool {
    match n {
        UiNode::Text { .. } | UiNode::Container { .. } | UiNode::Column { .. } => true,
        UiNode::Conditional { content, .. }
        | UiNode::Tappable { content, .. }
        | UiNode::Gesture { content, .. } => row_flexible(content),
        _ => false,
    }
}

/// `style` adjusted for where the node sits: its parent's axis.
fn placed(mut style: Style, n: &UiNode, axis: Axis) -> Style {
    let wide = fills_width(n);
    let tall = fills_height(n);
    match axis {
        Axis::Column => {
            style.flex_shrink = 0.0;
            if wide {
                style.align_self = Some(AlignSelf::STRETCH);
            }
            if tall {
                style.flex_grow = 1.0;
                style.flex_shrink = 1.0;
                style.flex_basis = length(0.0);
                style.min_size.height = length(0.0);
            }
        }
        Axis::Row => {
            // As the Compose row (`NodeRow`): text-like children shrink and
            // wrap to what the controls leave; controls keep their width; an
            // unsized Spacer (set where it is built) takes the leftover; a
            // field fills the row.
            style.flex_grow = style.flex_grow.max(0.0);
            if row_flexible(n) {
                style.flex_shrink = 1.0;
                style.min_size.width = length(0.0);
            } else if wide && !matches!(n, UiNode::Column { .. } | UiNode::Container { .. }) {
                style.flex_grow = 1.0;
                style.flex_shrink = 1.0;
                style.flex_basis = length(0.0);
                style.min_size.width = length(0.0);
            } else {
                style.flex_shrink = 0.0;
            }
        }
    }
    style
}

fn column(gap: u16, padding: Rect<LengthPercentage>, align: AlignItems) -> Style {
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        gap: Size {
            width: length(0.0),
            height: length(gap as f32),
        },
        padding,
        align_items: align,
        ..Default::default()
    }
}

fn row(gap: u16, padding: Rect<LengthPercentage>, align: AlignItems) -> Style {
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Row,
        gap: Size {
            width: length(gap as f32),
            height: length(0.0),
        },
        padding,
        align_items: align,
        ..Default::default()
    }
}

fn fixed(w: f32, h: f32) -> Style {
    Style {
        size: Size {
            width: length(w),
            height: length(h),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

fn node_type(n: &UiNode) -> String {
    n.type_tag()
}

/// An intent's JSON with `$item.` and `$route.` tokens and entity bindings
/// bound (`value_expr` is left for the engine, as the shared UI does).
/// `$form.`, `$form?.` and `$nonce` stay: the session binds them when the
/// tap runs (`Session::bind_form`), with the form as it is then.
fn bind_intent(intent: &ActionIntent, ctx: &Ctx) -> Value {
    fn walk(v: &Value, ctx: &Ctx, key: Option<&str>) -> Value {
        match v {
            Value::String(s) if key != Some("value_expr") => Value::String(
                ctx.text(&s.replace("$form.", "\u{1}form."))
                    .replace('\u{1}', "$"),
            ),
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, ctx, None)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (k.clone(), walk(x, ctx, Some(k))))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    walk(
        &serde_json::to_value(intent).unwrap_or(Value::Null),
        ctx,
        None,
    )
}

/// The average colour a blurhash starts with (its DC component), for an
/// image's placeholder.
fn blurhash_color(hash: &str) -> Option<Rgba> {
    const CHARS: &str =
        "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";
    let digits = hash.get(2..6)?;
    let mut v: u32 = 0;
    for c in digits.chars() {
        v = v * 83 + CHARS.find(c)? as u32;
    }
    Some(Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

impl<'i> Build<'_, 'i> {
    fn push(
        &mut self,
        style: Style,
        kids: Vec<usize>,
        paint: Paint,
        sem: Option<Sem>,
        clip: bool,
    ) -> usize {
        let ids: Vec<NodeId> = kids.iter().map(|k| self.elems[*k].node).collect();
        let node = self
            .tree
            .new_with_children(style, &ids)
            .expect("taffy node");
        self.elems.push(Elem {
            node,
            paint,
            sem,
            kids,
            clip,
            scrolls: false,
            sticky: false,
            pager: None,
        });
        self.elems.len() - 1
    }

    fn text_leaf(
        &mut self,
        style: Style,
        text: String,
        ty: Type,
        color: Rgba,
        sem: Option<Sem>,
    ) -> usize {
        let node = self
            .tree
            .new_leaf_with_context(
                style,
                Leaf::Text {
                    text: text.clone(),
                    ty,
                },
            )
            .expect("taffy leaf");
        self.elems.push(Elem {
            node,
            paint: Paint::Text { text, ty, color },
            sem,
            kids: Vec::new(),
            clip: false,
            scrolls: false,
            sticky: false,
            pager: None,
        });
        self.elems.len() - 1
    }

    fn sem(&self, b: &B, n: &UiNode, text: Option<String>, tap: Option<Tap>) -> Sem {
        Sem {
            id: b.path.clone(),
            node_type: node_type(n),
            text,
            tap,
            selected: false,
            gestures: BTreeMap::new(),
        }
    }

    fn fg(&self, b: &B) -> Rgba {
        b.color.unwrap_or(self.theme.on_background)
    }

    fn compute(&mut self, root: usize, scale: f32) -> Result<(), RenderError> {
        let Build { tree, text, .. } = self;
        let root = self.elems[root].node;
        tree.compute_layout_with_measure(
            root,
            Size::MAX_CONTENT,
            |input: LayoutInput, _id, ctx: Option<&mut Leaf>, style: &Style| -> LayoutOutput {
                compute_leaf_layout(
                    input,
                    style,
                    |_, _| 0.0,
                    |known, avail| match ctx {
                        Some(Leaf::Text { text: s, ty }) => {
                            let max = known.width.or(match avail.width {
                                AvailableSpace::Definite(w) => Some(w),
                                AvailableSpace::MinContent => Some(0.0),
                                AvailableSpace::MaxContent => None,
                            });
                            let m = text.measure(s, *ty, max, scale);
                            Size {
                                width: known.width.unwrap_or(m.width),
                                height: known.height.unwrap_or(m.height),
                            }
                        }
                        None => Size::ZERO,
                    },
                )
            },
        )
        .map_err(|e| RenderError(format!("layout failed: {e}")))
    }

    /// Builds `n`, sized for where it sits in its parent ([`placed`]);
    /// `None` when it draws nothing (a hidden conditional).
    fn node<'c>(&mut self, n: &'c UiNode, b: &B<'c>) -> Option<usize> {
        let out = self.build_node(n, b)?;
        let node = self.elems[out].node;
        let style = self.tree.style(node).cloned().unwrap_or_default();
        let _ = self.tree.set_style(node, placed(style, n, b.axis));
        Some(out)
    }

    /// Builds `n` itself ([`Build::node`] places it).
    fn build_node<'c>(&mut self, n: &'c UiNode, b: &B<'c>) -> Option<usize> {
        let ctx = b.ctx;
        let theme = self.theme.clone();
        let out = match n {
            UiNode::Page { root, .. } => {
                let kid = self.node(root, &b.child(0, Axis::Column));
                self.push(
                    column(0, pad(0, 0), AlignItems::FLEX_START),
                    kid.into_iter().collect(),
                    Paint::None,
                    None,
                    false,
                )
            }
            UiNode::Workspace { title, slots, .. } => {
                let mut kids = Vec::new();
                let t = type_for(TextStyle::Title);
                let style = Style {
                    padding: pad(16, 12),
                    ..Default::default()
                };
                let fg = self.fg(b);
                let sem = Sem {
                    id: format!("{}/title", b.path),
                    node_type: "core://Text".into(),
                    text: Some(title.clone()),
                    tap: None,
                    selected: false,
                    gestures: BTreeMap::new(),
                };
                kids.push(self.text_leaf(style, ctx.text(title), t, fg, Some(sem)));
                for (i, s) in slots.iter().enumerate() {
                    kids.extend(self.node(&s.content, &b.child(i, Axis::Column)));
                }
                self.push(
                    column(0, pad(0, 0), AlignItems::FLEX_START),
                    kids,
                    Paint::None,
                    None,
                    false,
                )
            }
            UiNode::Scroll { content, .. } => {
                let kid = self.node(content, &b.child(0, Axis::Column));
                let mut style = column(0, pad(0, 0), AlignItems::FLEX_START);
                style.overflow = taffy::Point {
                    x: taffy::Overflow::Hidden,
                    y: taffy::Overflow::Scroll,
                };
                style.scrollbar_width = 0.0;
                let i = self.push(
                    style,
                    kid.into_iter().collect(),
                    Paint::None,
                    Some(self.sem(b, n, None, None)),
                    true,
                );
                self.elems[i].scrolls = true;
                i
            }
            UiNode::Column {
                children,
                spacing_dp,
                padding_h_dp,
                padding_v_dp,
                align,
            } => {
                let kids = self.children(children, b, Axis::Column);
                self.push(
                    column(
                        *spacing_dp,
                        pad(*padding_h_dp, *padding_v_dp),
                        align_items(*align),
                    ),
                    kids,
                    Paint::None,
                    None,
                    false,
                )
            }
            UiNode::Row {
                children,
                spacing_dp,
                padding_h_dp,
                padding_v_dp,
                align,
            } => {
                let kids = self.children(children, b, Axis::Row);
                let a = align.map_or(AlignItems::CENTER, align_items);
                self.push(
                    row(*spacing_dp, pad(*padding_h_dp, *padding_v_dp), a),
                    kids,
                    Paint::None,
                    None,
                    false,
                )
            }
            UiNode::Container {
                orientation,
                padding_dp,
                children,
            } => {
                let p = pad(*padding_dp, *padding_dp);
                match orientation {
                    Orientation::Vertical => {
                        let kids = self.children(children, b, Axis::Column);
                        self.push(
                            column(0, p, AlignItems::FLEX_START),
                            kids,
                            Paint::None,
                            None,
                            false,
                        )
                    }
                    Orientation::Horizontal => {
                        let kids = self.children(children, b, Axis::Row);
                        self.push(
                            row(0, p, AlignItems::CENTER),
                            kids,
                            Paint::None,
                            None,
                            false,
                        )
                    }
                }
            }
            UiNode::Text {
                value_expr,
                style,
                count,
                ..
            } => {
                let mut text = ctx.text(value_expr);
                if let Some(c) = count {
                    let n = self.count(c, &ctx);
                    text = match (n, &c.zero_text) {
                        (Some(0), Some(z)) => ctx.text(z),
                        (Some(n), _) => text.replace("$count", &n.to_string()),
                        (None, _) => String::new(),
                    };
                }
                let ty = type_for(*style);
                let color = if *style == TextStyle::Secondary && b.color.is_none() {
                    theme.muted_text
                } else {
                    self.fg(b)
                };
                let sem = self.sem(b, n, Some(text.clone()), None);
                self.text_leaf(Style::default(), text, ty, color, Some(sem))
            }
            UiNode::Button { label, on_click } => {
                let label = ctx.text(label);
                let l = self.text_leaf(
                    Style::default(),
                    label.clone(),
                    LABEL_LARGE,
                    theme.on_primary,
                    None,
                );
                let tap = Tap::Intent {
                    intent: bind_intent(on_click, &ctx),
                };
                let mut style = row(0, pad(24, 10), AlignItems::CENTER);
                style.justify_content = JustifyContent::CENTER;
                style.min_size.height = length(40.0);
                let sem = self.sem(b, n, Some(label), Some(tap));
                self.push(
                    style,
                    vec![l],
                    Paint::Quad {
                        bg: theme.primary,
                        border: None,
                        radius: 20.0,
                    },
                    Some(sem),
                    false,
                )
            }
            UiNode::IconButton {
                icon,
                content_description,
                on_click,
                active,
                active_icon,
                active_tint,
                active_content_description,
                size_dp,
                tag,
            } => {
                let on = active.as_deref().is_some_and(|a| ctx.shows(a, None));
                let name = if on {
                    active_icon.as_deref().unwrap_or(icon)
                } else {
                    icon
                };
                let color = if on {
                    active_tint
                        .as_deref()
                        .and_then(|t| ctx.color(t))
                        .unwrap_or(self.fg(b))
                } else {
                    self.fg(b)
                };
                let desc = if on {
                    active_content_description
                        .as_deref()
                        .unwrap_or(content_description)
                } else {
                    content_description
                };
                let size = size_dp.map_or(24.0, f32::from);
                let glyph = self.icon(name, color, size);
                let mut style = column(0, pad(0, 0), AlignItems::CENTER);
                style.justify_content = JustifyContent::CENTER;
                let side = (size + 16.0).max(40.0);
                style.size = Size {
                    width: length(side),
                    height: length(side),
                };
                let tap = Tap::Intent {
                    intent: bind_intent(on_click, &ctx),
                };
                let mut sem = self.sem(b, n, Some(ctx.text(desc)), Some(tap));
                if let Some(t) = tag {
                    sem.id = ctx.text(t);
                }
                sem.selected = on;
                self.push(style, vec![glyph], Paint::None, Some(sem), false)
            }
            UiNode::Chip { text, icon, tone } => {
                let (bg, fg) = match tone {
                    Tone::Neutral => (theme.surface_variant, self.fg(b)),
                    Tone::Warning => (theme.warning_surface, theme.warning_text),
                };
                let mut kids = Vec::new();
                if let Some(i) = icon {
                    kids.push(self.icon(i, fg, 14.0));
                }
                let label = ctx.text(text);
                let ty = type_for(TextStyle::Secondary);
                kids.push(self.text_leaf(Style::default(), label.clone(), ty, fg, None));
                let mut style = row(4, pad(8, 4), AlignItems::CENTER);
                style.min_size.height = length(24.0);
                let sem = self.sem(b, n, Some(label), None);
                self.push(
                    style,
                    kids,
                    Paint::Quad {
                        bg,
                        border: None,
                        radius: 6.0,
                    },
                    Some(sem),
                    false,
                )
            }
            UiNode::Badge { text, icon } => {
                let fg = b.color.unwrap_or(theme.muted_text);
                let label = ctx.text(text);
                let i = self.icon(icon, fg, 14.0);
                let t = self.text_leaf(
                    Style::default(),
                    label.clone(),
                    type_for(TextStyle::Secondary),
                    fg,
                    None,
                );
                let sem = self.sem(b, n, Some(label), None);
                self.push(
                    row(4, pad(0, 0), AlignItems::CENTER),
                    vec![i, t],
                    Paint::None,
                    Some(sem),
                    false,
                )
            }
            UiNode::Spacer { size_dp } => {
                let mut style = Style::default();
                match (size_dp, b.axis) {
                    (Some(s), Axis::Row) => style.size.width = length(*s as f32),
                    (Some(s), Axis::Column) => style.size.height = length(*s as f32),
                    (None, Axis::Row) => style.flex_grow = 1.0,
                    (None, Axis::Column) => {}
                }
                self.push(style, Vec::new(), Paint::None, None, false)
            }
            UiNode::Divider {} => {
                let style = Style {
                    size: Size {
                        width: auto(),
                        height: length(1.0),
                    },
                    ..Default::default()
                };
                self.push(
                    style,
                    Vec::new(),
                    Paint::Quad {
                        bg: theme.outline,
                        border: None,
                        radius: 0.0,
                    },
                    None,
                    false,
                )
            }
            UiNode::Conditional {
                value,
                content,
                equals,
            } => {
                if !ctx.shows(value, equals.as_deref()) {
                    return None;
                }
                return self.node(content, b);
            }
            UiNode::Sticky { content } => {
                let kid = self.node(content, &b.child(0, Axis::Column));
                let i = self.push(
                    column(0, pad(0, 0), AlignItems::STRETCH),
                    kid.into_iter().collect(),
                    Paint::None,
                    None,
                    false,
                );
                self.elems[i].sticky = true;
                i
            }
            UiNode::Tappable {
                on_click,
                content,
                content_description,
                tag,
            } => {
                let kid = self.node(content, &b.child(0, Axis::Column));
                let tap = Tap::Intent {
                    intent: bind_intent(on_click, &ctx),
                };
                let mut sem = self.sem(
                    b,
                    n,
                    content_description.as_deref().map(|d| ctx.text(d)),
                    Some(tap),
                );
                if let Some(t) = tag {
                    sem.id = ctx.text(t);
                }
                self.push(
                    column(0, pad(0, 0), AlignItems::FLEX_START),
                    kid.into_iter().collect(),
                    Paint::None,
                    Some(sem),
                    false,
                )
            }
            UiNode::Gesture {
                content,
                on_tap,
                on_long_press,
                on_swipe_up,
                on_swipe_down,
                on_swipe_left,
                on_swipe_right,
                content_description,
            } => {
                let kid = self.node(content, &b.child(0, Axis::Column));
                let tap = on_tap.as_ref().map(|i| Tap::Intent {
                    intent: bind_intent(i, &ctx),
                });
                let mut sem = self.sem(
                    b,
                    n,
                    content_description.as_deref().map(|d| ctx.text(d)),
                    tap,
                );
                for (name, intent) in [
                    ("long_press", on_long_press),
                    ("swipe_up", on_swipe_up),
                    ("swipe_down", on_swipe_down),
                    ("swipe_left", on_swipe_left),
                    ("swipe_right", on_swipe_right),
                ] {
                    if let Some(i) = intent {
                        sem.gestures.insert(name.to_owned(), bind_intent(i, &ctx));
                    }
                }
                self.push(
                    column(0, pad(0, 0), AlignItems::FLEX_START),
                    kid.into_iter().collect(),
                    Paint::None,
                    Some(sem),
                    false,
                )
            }
            UiNode::Box {
                content,
                background,
                content_color,
                border_color,
                corner_dp,
                padding_h_dp,
                padding_v_dp,
                width_dp,
                height_dp,
                align,
                fill,
            } => {
                let mut inner = b.child(0, Axis::Column);
                if let Some(c) = content_color.as_deref().and_then(|c| ctx.color(c)) {
                    inner.color = Some(c);
                }
                let kid = self.node(content, &inner);
                let mut style = column(0, pad(*padding_h_dp, *padding_v_dp), align_items(*align));
                if width_dp.is_some() || height_dp.is_some() || *fill {
                    style.justify_content = justify(*align);
                }
                if let Some(w) = width_dp {
                    style.size.width = length(*w as f32);
                }
                if let Some(h) = height_dp {
                    style.size.height = length(*h as f32);
                }
                let bg = background.as_deref().and_then(|c| ctx.color(c));
                let border = border_color.as_deref().and_then(|c| ctx.color(c));
                let paint = if bg.is_some() || border.is_some() {
                    Paint::Quad {
                        bg: bg.unwrap_or(Rgba::TRANSPARENT),
                        border,
                        radius: *corner_dp as f32,
                    }
                } else {
                    Paint::None
                };
                self.push(
                    style,
                    kid.into_iter().collect(),
                    paint,
                    Some(self.sem(b, n, None, None)),
                    false,
                )
            }
            UiNode::Image {
                url,
                shape,
                size_dp,
                aspect_ratio,
                content_description,
                blurhash,
            } => {
                let color = blurhash
                    .as_deref()
                    .and_then(|h| blurhash_color(&ctx.text(h)))
                    .unwrap_or(theme.surface_variant);
                let mut style = Style::default();
                let radius = match shape {
                    ImageShape::Square => 0.0,
                    ImageShape::Rounded => 8.0,
                    ImageShape::Circle => f32::MAX,
                };
                match size_dp {
                    Some(s) => {
                        style.size = Size {
                            width: length(*s as f32),
                            height: length(*s as f32),
                        }
                    }
                    None => style.aspect_ratio = Some(aspect_ratio.unwrap_or(1.0)),
                }
                let desc = content_description.as_deref().map(|d| ctx.text(d));
                let sem = self.sem(b, n, desc, None);
                self.push(
                    style,
                    Vec::new(),
                    Paint::Image {
                        url: ctx.text(url),
                        placeholder: color,
                        radius,
                    },
                    Some(sem),
                    false,
                )
            }
            UiNode::TextField {
                field,
                placeholder,
                on_submit,
                lines,
                ..
            } => {
                let value = ctx.form.get(field).cloned().unwrap_or_default();
                let (shown, color) = if value.is_empty() {
                    (ctx.text(placeholder), theme.muted_text)
                } else {
                    (value.clone(), self.fg(b))
                };
                let mut tstyle = Style {
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    min_size: Size {
                        width: length(0.0),
                        height: auto(),
                    },
                    ..Default::default()
                };
                tstyle.flex_basis = length(0.0);
                let mut kids =
                    vec![self.text_leaf(tstyle, shown, type_for(TextStyle::Body), color, None)];
                let blank = value.trim().is_empty();
                let focused = self.input.focused == Some(field.as_str());
                if focused {
                    // The caret, after the text typed so far.
                    let at = if value.is_empty() {
                        0.0
                    } else {
                        self.text
                            .measure(
                                &value,
                                type_for(TextStyle::Body),
                                None,
                                self.input.viewport.scale,
                            )
                            .width
                    };
                    let caret = Style {
                        position: Position::Absolute,
                        inset: Rect {
                            left: length(16.0 + at + 1.0),
                            right: auto(),
                            top: length(18.0),
                            bottom: auto(),
                        },
                        size: Size {
                            width: length(2.0),
                            height: length(20.0),
                        },
                        ..Default::default()
                    };
                    kids.push(self.push(
                        caret,
                        Vec::new(),
                        Paint::Quad {
                            bg: theme.primary,
                            border: None,
                            radius: 1.0,
                        },
                        None,
                        false,
                    ));
                }
                let mut style = row(8, pad(16, 0), AlignItems::CENTER);
                let extra = lines.map_or(0.0, |l| (l.saturating_sub(1)).min(4) as f32 * 20.0);
                style.min_size.height = length(56.0 + extra);
                // Room for a short placeholder ("Qty") on one line.
                style.min_size.width = length(72.0);
                let mut sem = self.sem(
                    b,
                    n,
                    Some(value),
                    Some(Tap::Focus {
                        field: field.clone(),
                    }),
                );
                sem.id = format!("field:{field}");
                if on_submit.is_some() {
                    // Next to the field, as the shared UI's Send button.
                    style.flex_grow = 1.0;
                    style.flex_shrink = 1.0;
                    style.flex_basis = length(0.0);
                    style.min_size.width = length(72.0);
                }
                let boxed = self.push(
                    style,
                    kids,
                    Paint::Quad {
                        bg: Rgba::TRANSPARENT,
                        border: Some(if focused {
                            theme.primary
                        } else {
                            theme.outline
                        }),
                        radius: 4.0,
                    },
                    Some(sem),
                    false,
                );
                let Some(submit) = on_submit else {
                    return Some(boxed);
                };
                let glyph = self.icon(
                    "send",
                    if blank {
                        theme.muted_text
                    } else {
                        theme.primary
                    },
                    24.0,
                );
                let mut bstyle = column(0, pad(0, 0), AlignItems::CENTER);
                bstyle.justify_content = JustifyContent::CENTER;
                bstyle.size = Size {
                    width: length(48.0),
                    height: length(48.0),
                };
                bstyle.flex_shrink = 0.0;
                // Sends, then empties the field (`form.clear` is the session's own step).
                let tap = (!blank).then(|| Tap::Intent {
                    intent: json!({ "intent": "action.sequence", "steps": [
                        bind_intent(submit, &ctx),
                        { "intent": "form.clear", "field": field },
                    ] }),
                });
                let mut ssem = self.sem(b, n, Some("Send".into()), tap);
                ssem.id = format!("send:{field}");
                let send = self.push(bstyle, vec![glyph], Paint::None, Some(ssem), false);
                let outer = row(0, pad(0, 0), AlignItems::CENTER);
                self.push(outer, vec![boxed, send], Paint::None, None, false)
            }
            UiNode::ChoiceChips {
                field,
                options,
                padding_h_dp,
                ..
            } => {
                let current = ctx.form.get(field).cloned().unwrap_or_default();
                let mut kids = Vec::new();
                for o in options {
                    let selected = o.value == current;
                    let label = ctx.text(&o.label);
                    let fg = if selected {
                        theme.token("accent").unwrap_or(theme.primary)
                    } else {
                        self.fg(b)
                    };
                    let t = self.text_leaf(Style::default(), label.clone(), CHIP_LABEL, fg, None);
                    let mut style = row(0, pad(16, 0), AlignItems::CENTER);
                    style.size.height = length(32.0);
                    style.flex_shrink = 0.0;
                    let paint = if selected {
                        Paint::Quad {
                            bg: theme
                                .token("accent_surface")
                                .unwrap_or(theme.surface_variant),
                            border: None,
                            radius: 8.0,
                        }
                    } else {
                        Paint::Quad {
                            bg: Rgba::TRANSPARENT,
                            border: Some(theme.outline),
                            radius: 8.0,
                        }
                    };
                    let sem = Sem {
                        id: format!("{field}={}", o.value),
                        node_type: "core://ChoiceChips/option".into(),
                        text: Some(label),
                        tap: Some(Tap::SetForm {
                            field: field.clone(),
                            value: o.value.clone(),
                        }),
                        selected,
                        gestures: BTreeMap::new(),
                    };
                    kids.push(self.push(style, vec![t], paint, Some(sem), false));
                }
                let style = row(8, pad(*padding_h_dp, 0), AlignItems::CENTER);
                let mut sem = self.sem(b, n, None, None);
                sem.id = format!("chips:{field}");
                self.push(style, kids, Paint::None, Some(sem), true)
            }
            UiNode::List {
                query_key,
                item_template,
                empty_text,
                filter,
                group_by,
                where_,
                columns,
                spacing_dp,
                padding_h_dp,
                focus,
                indent,
            } => {
                let Some(all) = ctx.queries.get(query_key) else {
                    // Not loaded yet: nothing, as the app shows while loading.
                    return Some(self.push(Style::default(), Vec::new(), Paint::None, None, false));
                };
                let mut uris: Vec<&String> = all
                    .iter()
                    .filter(|u| ctx.entity(u).is_some())
                    .filter(|u| {
                        let e = ctx.entity(u).expect("filtered");
                        where_.as_ref().is_none_or(|w| {
                            w.conditions().iter().all(|c| {
                                let want = ctx.text(&c.equals);
                                want.trim().is_empty()
                                    || e.get(&c.field).map(render).as_deref() == Some(want.as_str())
                            })
                        })
                    })
                    .collect();
                let mut no_match = None;
                if let Some(f) = filter {
                    let q = fold(&ctx.text(&f.value));
                    let words: Vec<&str> = q.split_whitespace().collect();
                    if !words.is_empty() {
                        uris.retain(|u| {
                            let e = ctx.entity(u).expect("filtered");
                            words.iter().all(|w| {
                                f.fields.iter().any(|field| {
                                    e.get(field).is_some_and(|v| fold(&render(v)).contains(w))
                                })
                            })
                        });
                        no_match = f.no_match_text.clone();
                    }
                }
                uris.truncate(MAX_LIST_ITEMS);
                let mut kids = Vec::new();
                if uris.is_empty()
                    && let Some(t) = no_match.as_ref().or(empty_text.as_ref())
                {
                    {
                        let text = ctx.text(t);
                        let sem = Sem {
                            id: format!("{}/empty", b.path),
                            node_type: "core://Text".into(),
                            text: Some(text.clone()),
                            tap: None,
                            selected: false,
                            gestures: BTreeMap::new(),
                        };
                        let style = Style {
                            padding: pad(16, 24),
                            ..Default::default()
                        };
                        kids.push(self.text_leaf(
                            style,
                            text,
                            type_for(TextStyle::Body),
                            theme.muted_text,
                            Some(sem),
                        ));
                    }
                }
                let cols = (*columns).max(1) as usize;
                let mut last_group: Option<String> = None;
                let mut row_cells: Vec<usize> = Vec::new();
                let flush = |this: &mut Self, cells: &mut Vec<usize>, kids: &mut Vec<usize>| {
                    if cells.is_empty() {
                        return;
                    }
                    let mut style = row(*spacing_dp, pad(0, 0), AlignItems::FLEX_START);
                    style.align_self = Some(AlignSelf::STRETCH);
                    style.flex_shrink = 0.0;
                    // Empty cells keep the grid's columns equal.
                    while cells.len() < cols {
                        let s = Style {
                            flex_grow: 1.0,
                            flex_basis: length(0.0),
                            ..Default::default()
                        };
                        cells.push(this.push(s, Vec::new(), Paint::None, None, false));
                    }
                    kids.push(this.push(style, std::mem::take(cells), Paint::None, None, false));
                };
                for (i, uri) in uris.iter().enumerate() {
                    let item = ctx.entity(uri).expect("filtered");
                    let ib = B {
                        ctx: ctx.with_item(uri, item),
                        ..b.child(format!("i{i}"), Axis::Column)
                    };
                    if let Some(g) = group_by {
                        let v = item.get(&g.field).map(render).unwrap_or_default();
                        if last_group.as_deref() != Some(v.as_str()) {
                            last_group = Some(v);
                            flush(self, &mut row_cells, &mut kids);
                            let hb = B {
                                path: format!("{}/h{i}", b.path),
                                ..ib.clone()
                            };
                            kids.extend(self.node(&g.header, &hb));
                        }
                        if g.headers_only {
                            continue;
                        }
                    }
                    let Some(mut cell) = self.node(item_template, &ib) else {
                        continue;
                    };
                    let focused = focus
                        .as_ref()
                        .is_some_and(|f| ctx.text(&f.uri) == uri.as_str());
                    let mut style = column(0, pad(0, 0), AlignItems::FLEX_START);
                    if let Some(ind) = indent {
                        let steps = item
                            .get(&ind.field)
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0)
                            .clamp(0.0, ind.max.unwrap_or(4) as f32 as f64);
                        style.padding.left = length(steps as f32 * ind.step_dp as f32);
                    }
                    let paint = match focus.as_ref().filter(|_| focused) {
                        Some(f) => Paint::Quad {
                            bg: f
                                .background
                                .as_deref()
                                .and_then(Rgba::hex)
                                .unwrap_or(theme.surface_variant),
                            border: None,
                            radius: 0.0,
                        },
                        None => Paint::None,
                    };
                    // Every item fills its column, as the app's list does.
                    let tstyle = &mut self
                        .tree
                        .style(self.elems[cell].node)
                        .cloned()
                        .unwrap_or_default();
                    tstyle.align_self = Some(AlignSelf::STRETCH);
                    tstyle.flex_shrink = 0.0;
                    let _ = self.tree.set_style(self.elems[cell].node, tstyle.clone());
                    if indent.is_some() || focused {
                        cell = self.push(style, vec![cell], paint, None, false);
                    }
                    if cols > 1 {
                        let wrap = Style {
                            flex_grow: 1.0,
                            flex_basis: length(0.0),
                            min_size: Size {
                                width: length(0.0),
                                height: auto(),
                            },
                            display: Display::Flex,
                            flex_direction: FlexDirection::Column,
                            ..Default::default()
                        };
                        let w = self.push(wrap, vec![cell], Paint::None, None, false);
                        row_cells.push(w);
                        if row_cells.len() == cols {
                            flush(self, &mut row_cells, &mut kids);
                        }
                    } else {
                        kids.push(cell);
                    }
                }
                flush(self, &mut row_cells, &mut kids);
                let mut sem = self.sem(b, n, None, None);
                sem.id = format!("list:{query_key}");
                self.push(
                    column(*spacing_dp, pad(*padding_h_dp, 0), AlignItems::STRETCH),
                    kids,
                    Paint::None,
                    Some(sem),
                    false,
                )
            }
            UiNode::Carousel {
                items,
                item_template,
                aspect_ratio,
                aspect_field,
                min_aspect,
                max_aspect,
                tag,
            } => {
                let uris = ctx.list(items);
                let first = uris.first().and_then(|u| ctx.entity(u).map(|e| (u, e)));
                let mut aspect = *aspect_ratio;
                if let (Some(f), Some((_, e))) = (aspect_field, first)
                    && let Some(a) = e.get(f).and_then(Value::as_f64)
                {
                    aspect = a;
                }
                let aspect = aspect
                    .max(min_aspect.unwrap_or(0.1))
                    .min(max_aspect.unwrap_or(10.0));
                let id = match tag {
                    Some(t) => ctx.text(t),
                    None => b.path.clone(),
                };
                // The page shown: set by the session when a swipe settles.
                let count = uris.len();
                let page = (self
                    .input
                    .scroll
                    .get(&format!("page:{id}"))
                    .copied()
                    .unwrap_or(0.0)
                    .round()
                    .max(0.0) as usize)
                    .min(count.saturating_sub(1));
                // Every page, side by side on a track the carousel clips.
                let mut pages = Vec::new();
                for (i, uri) in uris.iter().enumerate() {
                    let Some(e) = ctx.entity(uri) else {
                        continue;
                    };
                    let ib = B {
                        ctx: ctx.with_item(uri, e),
                        ..b.child(format!("p{i}"), Axis::Column)
                    };
                    if let Some(k) = self.node(item_template, &ib) {
                        // A page is the box's width, as tall as the track.
                        let mut ps = column(0, pad(0, 0), AlignItems::STRETCH);
                        ps.size.width = percent(1.0);
                        ps.flex_shrink = 0.0;
                        // Its content fills it (the shared UI's `fillMaxSize`),
                        // whatever its own aspect ratio.
                        let node = self.elems[k].node;
                        let mut st = self.tree.style(node).cloned().unwrap_or_default();
                        st.flex_grow = 1.0;
                        st.flex_shrink = 1.0;
                        st.align_self = Some(AlignSelf::STRETCH);
                        st.aspect_ratio = None;
                        st.size = Size {
                            width: auto(),
                            height: auto(),
                        };
                        st.min_size.height = length(0.0);
                        let _ = self.tree.set_style(node, st);
                        pages.push(self.push(ps, vec![k], Paint::None, None, false));
                    }
                }
                // As tall as the box (its height comes from its width and
                // aspect ratio), the pages side by side; no percentage
                // heights, which an aspect-ratio box can leave unresolved.
                let mut track_style = row(0, pad(0, 0), AlignItems::STRETCH);
                track_style.flex_grow = 1.0;
                track_style.flex_shrink = 1.0;
                track_style.flex_basis = length(0.0);
                track_style.min_size.height = length(0.0);
                let track = self.push(track_style, pages, Paint::None, None, false);
                self.elems[track].pager = Some(id.clone());
                let mut kids = vec![track];
                if count > 1 {
                    let label = format!("{}/{count}", page + 1);
                    let sem = Sem {
                        id: format!("page:{id}"),
                        node_type: "page".into(),
                        text: Some(label.clone()),
                        tap: None,
                        selected: false,
                        gestures: BTreeMap::new(),
                    };
                    let t = self.text_leaf(
                        Style::default(),
                        label,
                        type_for(TextStyle::Caption),
                        Rgba::rgb(255, 255, 255),
                        Some(sem),
                    );
                    let mut style = row(0, pad(9, 3), AlignItems::CENTER);
                    style.position = Position::Absolute;
                    style.inset = Rect {
                        left: auto(),
                        right: length(12.0),
                        top: length(12.0),
                        bottom: auto(),
                    };
                    kids.push(self.push(
                        style,
                        vec![t],
                        Paint::Quad {
                            bg: Rgba([0.0, 0.0, 0.0, 0.6]),
                            border: None,
                            radius: 12.0,
                        },
                        None,
                        false,
                    ));
                }
                let mut style = column(0, pad(0, 0), AlignItems::STRETCH);
                style.aspect_ratio = Some(aspect as f32);
                // A full width (stretching doesn't apply to a box with an
                // aspect ratio), its height from the ratio.
                style.size.width = percent(1.0);
                let mut sem = self.sem(b, n, None, None);
                sem.id = id.clone();
                let frame = self.push(style, kids, Paint::None, Some(sem), true);
                // In a column that stretches it (an aspect ratio alone, placed
                // in a parent's column, can leave it no width).
                let outer = column(0, pad(0, 0), AlignItems::STRETCH);
                if count <= 1 {
                    return Some(self.push(outer, vec![frame], Paint::None, None, false));
                }
                // Dots below the image, as the shared UI draws them.
                let mut dots = Vec::new();
                for i in 0..count {
                    let c = if i == page {
                        Rgba::rgb(0x00, 0x95, 0xF6)
                    } else {
                        Rgba::rgb(0xC7, 0xC7, 0xC7)
                    };
                    dots.push(self.push(
                        fixed(6.0, 6.0),
                        Vec::new(),
                        Paint::Quad {
                            bg: c,
                            border: None,
                            radius: 3.0,
                        },
                        None,
                        false,
                    ));
                }
                let mut ds = row(5, pad(0, 0), AlignItems::CENTER);
                ds.align_self = Some(AlignSelf::CENTER);
                ds.padding.top = length(10.0);
                let sem = Sem {
                    id: format!("dots:{id}"),
                    node_type: "dots".into(),
                    text: Some(format!("Page {} of {count}", page + 1)),
                    tap: None,
                    selected: false,
                    gestures: BTreeMap::new(),
                };
                let dots = self.push(ds, dots, Paint::None, Some(sem), false);
                self.push(outer, vec![frame, dots], Paint::None, None, false)
            }
            UiNode::Progress { size_dp } => {
                let s = size_dp.map_or(16.0, f32::from);
                let i = self.push(
                    fixed(s, s),
                    Vec::new(),
                    Paint::Spinner {
                        color: theme.muted_text,
                    },
                    None,
                    false,
                );
                self.elems[i].sem = Some(self.sem(b, n, None, None));
                i
            }
            UiNode::Switch {
                checked,
                on_click,
                content_description,
            } => {
                let on = ctx.shows(checked, None);
                let (track, thumb) = if on {
                    (theme.primary, theme.on_primary)
                } else {
                    (theme.switch_track_off, theme.switch_outline)
                };
                let tap = Tap::Intent {
                    intent: bind_intent(on_click, &ctx),
                };
                let mut sem = self.sem(
                    b,
                    n,
                    content_description.as_deref().map(|d| ctx.text(d)),
                    Some(tap),
                );
                sem.selected = on;
                self.push(
                    fixed(52.0, 32.0),
                    Vec::new(),
                    Paint::Switch { on, track, thumb },
                    Some(sem),
                    false,
                )
            }
            UiNode::MediaPicker { field, label, .. } => {
                let plus = self.icon("add_photo", theme.muted_text, 24.0);
                let mut tile = column(0, pad(0, 0), AlignItems::CENTER);
                tile.justify_content = JustifyContent::CENTER;
                tile.size = Size {
                    width: length(80.0),
                    height: length(80.0),
                };
                let mut sem = self.sem(
                    b,
                    n,
                    label.as_deref().map(|l| ctx.text(l)),
                    Some(Tap::Focus {
                        field: field.clone(),
                    }),
                );
                sem.id = format!("media:{field}");
                let t = self.push(
                    tile,
                    vec![plus],
                    Paint::Quad {
                        bg: Rgba::TRANSPARENT,
                        border: Some(theme.outline),
                        radius: 8.0,
                    },
                    Some(sem),
                    false,
                );
                self.push(
                    row(8, pad(0, 0), AlignItems::CENTER),
                    vec![t],
                    Paint::None,
                    None,
                    false,
                )
            }
            UiNode::Layout {
                layout,
                show_when,
                equals,
                presentation,
                height_dp,
                ..
            } => {
                if show_when
                    .as_deref()
                    .is_some_and(|w| !ctx.shows(w, equals.as_deref()))
                    || *presentation != EmbedPresentation::Inline
                {
                    // Presented layouts (sheets, panels, overlays) are not
                    // drawn in a preview frame.
                    return None;
                }
                let id = ctx.text(layout);
                let mut style = column(0, pad(0, 0), AlignItems::STRETCH);
                if let Some(h) = height_dp {
                    style.size.height = length(*h as f32);
                }
                let inner = self
                    .input
                    .layouts
                    .get(&id)
                    .filter(|_| b.depth < MAX_EMBED_DEPTH);
                let kid = match inner {
                    Some(l) => {
                        let lb = B {
                            depth: b.depth + 1,
                            ..b.child(format!("layout:{id}"), Axis::Column)
                        };
                        self.node(l, &lb)
                    }
                    None => Some(self.placeholder(&format!("Layout {id}"))),
                };
                let mut sem = self.sem(b, n, None, None);
                sem.id = format!("layout:{id}");
                let kids: Vec<usize> = kid.into_iter().collect();
                // The embedded root stretches to the embed's height.
                for k in &kids {
                    let node = self.elems[*k].node;
                    let mut s = self.tree.style(node).cloned().unwrap_or_default();
                    s.flex_grow = 1.0;
                    let _ = self.tree.set_style(node, s);
                }
                self.push(style, kids, Paint::None, Some(sem), true)
            }
            UiNode::Dock {
                field,
                before,
                query_key,
                item,
                max_items,
                after,
            } => {
                // The tabs: `before`, one per entity (bound), then `after`.
                let mut tabs: Vec<DockEntry> = before
                    .iter()
                    .map(|t| {
                        (
                            t.value.clone(),
                            t.label.clone(),
                            t.icon.clone(),
                            t.content.as_ref(),
                            None,
                        )
                    })
                    .collect();
                if let (Some(q), Some(it)) = (query_key, item)
                    && let Some(list) = ctx.queries.get(q)
                {
                    for u in list.iter().take(max_items.map_or(8, usize::from)) {
                        if let Some(e) = ctx.entity(u) {
                            let c = ctx.with_item(u, e);
                            tabs.push((
                                c.text(&it.value),
                                c.text(&it.label),
                                it.icon.as_ref().map(|i| c.text(i)),
                                it.content.as_ref(),
                                Some((u.clone(), e)),
                            ));
                        }
                    }
                }
                tabs.extend(after.iter().map(|t| {
                    (
                        t.value.clone(),
                        t.label.clone(),
                        t.icon.clone(),
                        t.content.as_ref(),
                        None,
                    )
                }));
                let selected = ctx
                    .form
                    .get(field)
                    .filter(|v| tabs.iter().any(|t| &t.0 == *v))
                    .cloned()
                    .or_else(|| tabs.first().map(|t| t.0.clone()))
                    .unwrap_or_default();
                let mut kids = Vec::new();
                if let Some(t) = tabs.iter().find(|t| t.0 == selected) {
                    let tb = match &t.4 {
                        Some((u, e)) => B {
                            ctx: ctx.with_item(u, e),
                            ..b.child("tab", Axis::Column)
                        },
                        None => b.child("tab", Axis::Column),
                    };
                    if let Some(k) = self.node(t.3, &tb) {
                        let node = self.elems[k].node;
                        let mut s = self.tree.style(node).cloned().unwrap_or_default();
                        s.flex_grow = 1.0;
                        s.flex_shrink = 1.0;
                        s.flex_basis = length(0.0);
                        s.min_size.height = length(0.0);
                        s.align_self = Some(AlignSelf::STRETCH);
                        let _ = self.tree.set_style(node, s);
                        kids.push(k);
                    }
                }
                let mut bar = Vec::new();
                for (value, label, icon, _, _) in &tabs {
                    let on = *value == selected;
                    let color = if on { theme.primary } else { theme.muted_text };
                    let cell = vec![
                        self.icon(icon.as_deref().unwrap_or("settings"), color, 22.0),
                        self.text_leaf(
                            Style::default(),
                            label.clone(),
                            type_for(TextStyle::Caption),
                            color,
                            None,
                        ),
                    ];
                    let mut s = column(2, pad(0, 8), AlignItems::CENTER);
                    s.flex_grow = 1.0;
                    s.flex_basis = length(0.0);
                    let sem = Sem {
                        id: format!("{field}={value}"),
                        node_type: "core://Dock/tab".into(),
                        text: Some(label.clone()),
                        tap: Some(Tap::SetForm {
                            field: field.clone(),
                            value: value.clone(),
                        }),
                        selected: on,
                        gestures: BTreeMap::new(),
                    };
                    bar.push(self.push(s, cell, Paint::None, Some(sem), false));
                }
                let mut bs = row(0, pad(0, 0), AlignItems::CENTER);
                bs.align_self = Some(AlignSelf::STRETCH);
                bs.flex_shrink = 0.0;
                kids.push(self.push(
                    bs,
                    bar,
                    Paint::Quad {
                        bg: theme.surface_variant,
                        border: None,
                        radius: 0.0,
                    },
                    None,
                    false,
                ));
                self.push(
                    column(0, pad(0, 0), AlignItems::STRETCH),
                    kids,
                    Paint::None,
                    Some(self.sem(b, n, None, None)),
                    false,
                )
            }
            UiNode::Drawer {
                field,
                before,
                after,
                ..
            } => {
                let pages: Vec<&protocol::DrawerPage> = before.iter().chain(after).collect();
                let selected = ctx.form.get(field).cloned();
                let page = pages
                    .iter()
                    .find(|p| Some(&p.value) == selected.as_ref())
                    .or(pages.first());
                let menu = self.icon("settings", self.fg(b), 24.0);
                let mut ms = column(0, pad(8, 8), AlignItems::FLEX_START);
                ms.flex_shrink = 0.0;
                let mut sem = self.sem(b, n, Some("Menu".into()), None);
                sem.id = format!("drawer:{field}");
                let mut kids = vec![self.push(ms, vec![menu], Paint::None, Some(sem), false)];
                if let Some(p) = page {
                    let k = self.node(&p.content, &b.child("page", Axis::Column));
                    kids.extend(k);
                }
                self.push(
                    column(0, pad(0, 0), AlignItems::STRETCH),
                    kids,
                    Paint::None,
                    None,
                    false,
                )
            }
            UiNode::AppFrame { app } => self.placeholder(&format!("App {}", ctx.text(app))),
            UiNode::Unknown(raw) => {
                let t = raw.get("type").and_then(Value::as_str).unwrap_or("unknown");
                self.placeholder(t)
            }
        };
        Some(out)
    }

    fn children<'c>(&mut self, children: &'c [UiNode], b: &B<'c>, axis: Axis) -> Vec<usize> {
        children
            .iter()
            .enumerate()
            .filter_map(|(i, c)| self.node(c, &b.child(i, axis)))
            .collect()
    }

    fn icon(&mut self, name: &str, color: Rgba, size: f32) -> usize {
        self.push(
            fixed(size, size),
            Vec::new(),
            Paint::Icon {
                name: name.to_owned(),
                shape: icon_shape(name),
                color,
            },
            None,
            false,
        )
    }

    /// A quiet box naming what can't be drawn here.
    fn placeholder(&mut self, label: &str) -> usize {
        let t = self.text_leaf(
            Style::default(),
            label.to_owned(),
            type_for(TextStyle::Secondary),
            self.theme.muted_text,
            None,
        );
        let mut s = column(0, pad(12, 12), AlignItems::CENTER);
        s.justify_content = JustifyContent::CENTER;
        s.min_size.height = length(48.0);
        self.push(
            s,
            vec![t],
            Paint::Quad {
                bg: Rgba::TRANSPARENT,
                border: Some(self.theme.outline),
                radius: 8.0,
            },
            None,
            false,
        )
    }

    fn count(&self, c: &protocol::TextCount, ctx: &Ctx) -> Option<usize> {
        let list = ctx.queries.get(&c.query_key)?;
        Some(
            list.iter()
                .filter_map(|u| ctx.entity(u))
                .filter(|e| {
                    c.where_.iter().all(|w| {
                        let want = ctx.text(&w.equals);
                        want.trim().is_empty()
                            || e.get(&w.field).map(render).as_deref() == Some(want.as_str())
                    })
                })
                .count(),
        )
    }
}

fn intersect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)]
}

fn intersects(a: [f32; 4], b: [f32; 4]) -> bool {
    let r = intersect(a, b);
    r[2] > 0.0 && r[3] > 0.0 || (a[2] == 0.0 || a[3] == 0.0)
}

/// A sticky node pinned at the top of its scrolling view: drawn after the
/// view's other content, over the page background.
struct Pinned {
    elem: usize,
    origin: (f32, f32),
    clip: [f32; 4],
    parent_sem: Option<usize>,
    /// The top of the view it is pinned in (the band above it is filled).
    view_top: f32,
}

/// A sticky node this close to the top of its view when nothing is
/// scrolled (a column's padding) pins where it rests, so it never moves;
/// one further down pins at the top.
const STICKY_REST: f32 = 24.0;

/// What emitting knows about the place it is in.
#[derive(Clone, Copy)]
struct At {
    /// The top of the nearest scrolling view, for sticky nodes.
    scroll_top: Option<f32>,
    /// How far that view is scrolled.
    scrolled: f32,
    /// The parent's box (a sticky node stays inside it).
    parent: [f32; 4],
}

/// A laid-out frame: the taffy tree and what each node paints. Scrolling
/// only places it again ([`Laid::emit`]), without laying it out.
pub struct Laid {
    tree: TaffyTree<Leaf>,
    elems: Vec<Elem>,
    root: usize,
    theme: Theme,
    viewport: Viewport,
    /// The farthest each scrolling view (by semantic id) can scroll.
    pub scroll_max: BTreeMap<String, f32>,
    /// Each carousel (by semantic id): its page width and page count.
    pub pagers: BTreeMap<String, (f32, usize)>,
}

// SAFETY: taffy's `CompactLength` holds a raw pointer only for CSS `calc()`
// values, which this crate never builds (every length is `length`,
// `percent` or `auto`). The tree owns everything else it holds, so moving a
// laid-out frame to another thread (the pod keeps previews behind a mutex)
// shares nothing.
unsafe impl Send for Laid {}

impl Laid {
    /// How far each scrolling view can scroll (its content past its height).
    fn scroll_extents(&self) -> BTreeMap<String, f32> {
        self.elems
            .iter()
            .filter(|e| e.scrolls)
            .filter_map(|e| {
                let s = e.sem.as_ref()?;
                let h = self.tree.layout(e.node).ok()?.size.height;
                Some((s.id.clone(), (self.content_height(e) - h).max(0.0)))
            })
            .collect()
    }

    /// The scene at these scroll offsets.
    pub fn emit(&self, scroll: &BTreeMap<String, f32>) -> Scene {
        let vp = self.viewport;
        let mut scene = Scene {
            width: vp.width,
            height: vp.height,
            scale: vp.scale,
            background: self.theme.background.0,
            ..Default::default()
        };
        self.emit_into(&mut scene, scroll, (0.0, 0.0), NO_CLIP);
        scene
    }

    /// Places the frame into `scene` at `offset`, drawing nothing outside
    /// `clip` (a sheet or panel over another layout).
    pub fn emit_into(
        &self,
        scene: &mut Scene,
        scroll: &BTreeMap<String, f32>,
        offset: (f32, f32),
        clip: [f32; 4],
    ) {
        let vp = self.viewport;
        let at = At {
            scroll_top: None,
            scrolled: 0.0,
            parent: [offset.0, offset.1, vp.width, vp.height],
        };
        let mut pinned = Vec::new();
        self.emit_at(
            self.root,
            offset,
            clip,
            None,
            at,
            scroll,
            &mut pinned,
            scene,
        );
    }

    /// The theme the frame was laid out with.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    fn content_height(&self, e: &Elem) -> f32 {
        e.kids
            .iter()
            .filter_map(|k| self.tree.layout(self.elems[*k].node).ok())
            .map(|kl| kl.location.y + kl.size.height)
            .fold(0.0, f32::max)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_at(
        &self,
        i: usize,
        origin: (f32, f32),
        clip: [f32; 4],
        parent_sem: Option<usize>,
        at: At,
        scroll: &BTreeMap<String, f32>,
        pinned: &mut Vec<Pinned>,
        scene: &mut Scene,
    ) {
        let e = &self.elems[i];
        let l = self.tree.layout(e.node).expect("laid out");
        let x = origin.0 + l.location.x;
        let y = origin.1 + l.location.y;
        let (w, h) = (l.size.width, l.size.height);
        if e.sticky
            && let Some(top) = at.scroll_top
            && at.scrolled > 0.0
        {
            // Pinned at the top of the scrolling view (or where it rests,
            // when that is just below it), but never past the end of its
            // parent (it scrolls away with it).
            let rest = y + at.scrolled - top;
            let line = top
                + if rest <= STICKY_REST {
                    rest.max(0.0)
                } else {
                    0.0
                };
            let parent_bottom = at.parent[1] + at.parent[3];
            let to = line.min(parent_bottom - h);
            if to > y {
                pinned.push(Pinned {
                    elem: i,
                    origin: (origin.0, origin.1 + (to - y)),
                    clip,
                    parent_sem,
                    view_top: top,
                });
                return;
            }
        }
        let rect = [x, y, w, h];
        let visible = intersects(rect, clip);
        if visible {
            self.paint(e, rect, l, clip, scene);
        }
        let mut sem_index = parent_sem;
        if let Some(s) = &e.sem
            && visible
        {
            let shown = intersect(rect, clip);
            scene.semantics.push(SemanticNode {
                id: s.id.clone(),
                node_type: s.node_type.clone(),
                bounds: shown,
                text_content: s.text.clone().filter(|t| !t.is_empty()),
                interactive: s.tap.is_some(),
                tap: s.tap.clone(),
                selected: s.selected,
                parent: parent_sem,
                gestures: s.gestures.clone(),
            });
            sem_index = Some(scene.semantics.len() - 1);
        }
        let inner_clip = if e.clip { intersect(clip, rect) } else { clip };
        let mut oy = 0.0;
        let mut kid_at = At {
            scroll_top: at.scroll_top,
            scrolled: at.scrolled,
            parent: rect,
        };
        let mut own_pins = Vec::new();
        let scrolls = e.scrolls && e.sem.is_some();
        if scrolls && let Some(s) = &e.sem {
            let max = (self.content_height(e) - h).max(0.0);
            oy = scroll.get(&s.id).copied().unwrap_or(0.0).clamp(0.0, max);
            kid_at.scroll_top = Some(y);
            kid_at.scrolled = oy;
        }
        let ox = e
            .pager
            .as_ref()
            .and_then(|id| scroll.get(&format!("pager:{id}")).copied())
            .unwrap_or(0.0);
        for k in &e.kids {
            let pins = if scrolls { &mut own_pins } else { &mut *pinned };
            self.emit_at(
                *k,
                (x - ox, y - oy),
                inner_clip,
                sem_index,
                kid_at,
                scroll,
                pins,
                scene,
            );
        }
        // Sticky nodes pinned in this view go over its content.
        for p in own_pins {
            let pe = &self.elems[p.elem];
            if let Ok(pl) = self.tree.layout(pe.node) {
                let r = [
                    p.origin.0 + pl.location.x,
                    p.origin.1 + pl.location.y,
                    pl.size.width,
                    pl.size.height,
                ];
                // Over the content scrolling under it, up to the view's top.
                let above = (r[1] - p.view_top).clamp(0.0, STICKY_REST);
                let band = [r[0], r[1] - above, r[2], r[3] + above];
                let mut q = QuadInstance::fill(band, self.theme.background, 0.0);
                q.clip = p.clip;
                scene.draws.push(Draw::Quad(q));
            }
            let free = At {
                scroll_top: None,
                scrolled: 0.0,
                parent: rect,
            };
            let mut none = Vec::new();
            self.emit_at(
                p.elem,
                p.origin,
                p.clip,
                p.parent_sem,
                free,
                scroll,
                &mut none,
                scene,
            );
        }
    }

    fn paint(
        &self,
        e: &Elem,
        rect: [f32; 4],
        l: &taffy::Layout,
        clip: [f32; 4],
        scene: &mut Scene,
    ) {
        let [x, y, w, h] = rect;
        match &e.paint {
            Paint::None => {}
            Paint::Quad { bg, border, radius } => {
                let r = radius.min(w.min(h) / 2.0);
                let mut q = QuadInstance::fill(rect, *bg, r);
                if let Some(b) = border {
                    q = q.border(*b, 1.0);
                }
                q.clip = clip;
                scene.draws.push(Draw::Quad(q));
            }
            Paint::Text { text, ty, color } => {
                if text.is_empty() {
                    return;
                }
                let px = x + l.padding.left;
                let py = y + l.padding.top;
                let pw = (w - l.padding.left - l.padding.right).max(0.0);
                let ph = (h - l.padding.top - l.padding.bottom).max(0.0);
                scene.draws.push(Draw::Text(TextRun {
                    text: text.clone(),
                    face: ty.face,
                    size: ty.size,
                    line_height: ty.line_height,
                    color: *color,
                    rect: [px, py, pw, ph],
                    clip,
                }));
            }
            Paint::Icon { name, color, .. } if crate::icons::paths(name).is_some() => {
                let s = w.min(h);
                scene.draws.push(Draw::Icon(IconRun {
                    name: name.clone(),
                    rect: [x + (w - s) / 2.0, y + (h - s) / 2.0, s, s],
                    color: *color,
                    clip,
                }));
            }
            Paint::Icon { shape, color, .. } => {
                let s = w.min(h);
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                let unit = s / 24.0;
                let mut push = |r: [f32; 4], bg: Rgba, border: Option<(Rgba, f32)>, rad: f32| {
                    let mut q = QuadInstance::fill(r, bg, rad);
                    if let Some((c, bw)) = border {
                        q = q.border(c, bw);
                    }
                    q.clip = clip;
                    scene.draws.push(Draw::Quad(q));
                };
                let d = 18.0 * unit;
                let circle = [cx - d / 2.0, cy - d / 2.0, d, d];
                match shape {
                    Icon::Filled => push(circle, *color, None, d / 2.0),
                    Icon::Ring => push(
                        circle,
                        Rgba::TRANSPARENT,
                        Some((*color, 2.0 * unit)),
                        d / 2.0,
                    ),
                    Icon::Square => push(circle, *color, None, 3.0 * unit),
                    Icon::SquareRing => push(
                        circle,
                        Rgba::TRANSPARENT,
                        Some((*color, 2.0 * unit)),
                        3.0 * unit,
                    ),
                    Icon::Plus | Icon::Minus => {
                        let t = 2.0 * unit;
                        push([cx - d / 2.0, cy - t / 2.0, d, t], *color, None, t / 2.0);
                        if *shape == Icon::Plus {
                            push([cx - t / 2.0, cy - d / 2.0, t, d], *color, None, t / 2.0);
                        }
                    }
                }
            }
            Paint::Image {
                url,
                placeholder,
                radius,
            } => {
                let r = radius.min(w.min(h) / 2.0);
                let mut q = QuadInstance::fill(rect, *placeholder, r);
                q.clip = clip;
                scene.draws.push(Draw::Quad(q));
                if url.starts_with("http://") || url.starts_with("https://") {
                    scene.draws.push(Draw::Image(ImageRun {
                        url: url.clone(),
                        rect,
                        radius: r,
                        clip,
                    }));
                }
            }
            Paint::Spinner { color } => {
                let bw = (w / 8.0).max(1.5);
                let mut q = QuadInstance::fill(rect, Rgba::TRANSPARENT, w / 2.0).border(*color, bw);
                q.clip = clip;
                scene.draws.push(Draw::Quad(q));
            }
            Paint::Switch { on, track, thumb } => {
                // Material 3: off is an outlined track with a small grey
                // thumb; on is a filled track with a large light thumb.
                let mut q = QuadInstance::fill(rect, *track, h / 2.0);
                if !*on {
                    q = q.border(self.theme.switch_outline, 2.0);
                }
                q.clip = clip;
                scene.draws.push(Draw::Quad(q));
                let d = if *on { h - 8.0 } else { h - 16.0 };
                let tx = if *on { x + w - 4.0 - d } else { x + 8.0 };
                let mut t = QuadInstance::fill([tx, y + (h - d) / 2.0, d, d], *thumb, d / 2.0);
                t.clip = clip;
                scene.draws.push(Draw::Quad(t));
            }
        }
    }
}
