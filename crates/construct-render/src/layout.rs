//! A plumbing tree and its data, laid out with taffy (flexbox) and turned
//! into a [`Scene`]: quads and text runs in paint order, and the semantic
//! tree. Sizes follow the shared Compose renderer (Material 3 metrics), so a
//! frame drawn here reads like the app's.

use protocol::{
    ActionIntent, Align, BorderStyle, ButtonSize, ButtonStyle, DataBundle, EmbedPresentation,
    EntityRecord, ImageShape, Justify, Orientation, TextStyle, Tone, UiNode,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use taffy::prelude::*;
use taffy::{LayoutInput, LayoutOutput, compute_leaf_layout};

use crate::bind::{Ctx, ReadLog, Reads, fold, render};
use crate::markdown::{self, Block, Inline, Mark};
use crate::scene::{
    Draw, IconRun, ImageRun, NO_CLIP, QuadInstance, Scene, SemanticNode, Tap, TextRun,
};
use crate::style::{CHIP_LABEL, LABEL_LARGE, Rgba, Theme, Type, type_for};
use crate::text::{RichSpan, TextEngine};

/// Logical size and device pixels per point of a frame.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    #[serde(default = "one")]
    pub scale: f32,
}

/// When less stack than this is left, building or drawing a node moves to
/// a fresh piece of [`STACK_GROWTH`] bytes.
/// The deepest a node may sit once components are expanded. The pod caps
/// each file and component nesting, but expanded trees can still run about
/// a thousand deep; taffy's layout recurses per level, so a deeper node is
/// left out rather than risk a stack overflow (a crash from one layout
/// would repeat on every start).
pub const MAX_BUILD_DEPTH: usize = 256;
/// The most characters one `core://Text` draws (it is shaped on every
/// layout pass): a page's text is far shorter, and the rest is cut.
pub const MAX_TEXT_CHARS: usize = 20_000;

/// `text` cut to [`MAX_TEXT_CHARS`] characters.
pub fn cap_text(mut text: String) -> String {
    if let Some((i, _)) = text.char_indices().nth(MAX_TEXT_CHARS) {
        text.truncate(i);
        text.push('…');
    }
    text
}

const STACK_RED_ZONE: usize = 256 * 1024;
const STACK_GROWTH: usize = 4 * 1024 * 1024;

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

/// A layout opened beside the inline `core://Layout` it was opened from
/// (`panels: true` on an expanded screen; decision `wide-screen-panels`):
/// its id and its `$route`.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub layout: String,
    pub route: BTreeMap<String, String>,
}

/// The panels open beside each panel host (by the semantic id of its
/// inline `core://Layout`'s content), leftmost first.
pub type Panels = BTreeMap<String, Vec<Panel>>;

/// The inline embeds a semantic path runs through: for each `layout:<id>`
/// segment, the chain of embedded layouts up to it and the path up to it.
/// Ids that aren't paths (`field:…`, `layout:<id>` itself) have none.
pub fn embed_prefixes(id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !id.starts_with("root") || !id.contains("/layout:") {
        return out;
    }
    let mut chain = String::new();
    let mut end = 0;
    for seg in id.split('/') {
        end += seg.len();
        if seg.starts_with("layout:") {
            if !chain.is_empty() {
                chain.push('/');
            }
            chain.push_str(seg);
            out.push((chain.clone(), id[..end].to_owned()));
        }
        end += 1;
    }
    out
}

/// No panels open (an [`Input`] that draws none).
pub static NO_PANELS: Panels = BTreeMap::new();

/// At most this many columns show for a panel host: the embed and its
/// panels, the oldest moving out of sight first.
const PANEL_COLUMNS: usize = 2;

/// The semantic id of panel `k` of the host at `host`.
pub fn panel_id(host: &str, k: usize) -> String {
    format!("{host}~{k}")
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
    /// The panels open beside inline layouts with `panels: true`.
    pub panels: &'a Panels,
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

/// How wide a layout that doesn't adapt to the window draws at most
/// (points): about a large phone's width, centred in the room it is given
/// (decision `phone-width-layouts`).
pub const PHONE_WIDTH_DP: f32 = 480.0;

/// Whether a layout's own tree adapts to the window itself, so it may use
/// all the width it is given: it tests `local://device` (its size, size
/// class or platform), caps a column (`max_width_dp`), lays a list out in
/// automatic columns (`min_column_dp`) or hosts panels. Layouts it embeds
/// are not looked into: each answers for itself. Any other layout was
/// written for a phone and draws at most [`PHONE_WIDTH_DP`] wide.
pub fn adapts_to_window(root: &UiNode) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let adapts = match n {
            UiNode::Conditional { value, .. } => value.contains("local://device"),
            UiNode::Column { max_width_dp, .. } => *max_width_dp > 0,
            UiNode::List { min_column_dp, .. } => *min_column_dp > 0,
            UiNode::Layout { panels, .. } => *panels,
            _ => false,
        };
        if adapts {
            return true;
        }
        stack.extend(n.children());
    }
    false
}

/// What a leaf measures.
enum Leaf {
    Text {
        text: String,
        ty: Type,
        spans: Vec<RichSpan>,
    },
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
        "favorite" | "bookmark" | "chat_bubble" | "notifications" | "flame" | "send"
        | "shopping_cart" | "stapler" => Icon::Filled,
        "add" | "add_photo" => Icon::Plus,
        "arrow_back" | "delete" | "close" | "check" => Icon::Minus,
        "check_box" => Icon::Square,
        "check_box_outline_blank" => Icon::SquareRing,
        _ => Icon::Ring,
    }
}

/// How an element paints itself (before its children).
#[derive(Clone)]
enum Paint {
    None,
    Quad {
        bg: Rgba,
        border: Option<Rgba>,
        /// The border in dashes (`core://Box` `border_style: Dashed`).
        dashed: bool,
        radius: f32,
    },
    Text {
        text: String,
        ty: Type,
        color: Rgba,
        spans: Vec<RichSpan>,
    },
    Icon {
        name: String,
        shape: Icon,
        color: Rgba,
    },
    Spinner {
        color: Rgba,
    },
    /// A determinate ring (feature `progress_ring`): a quiet track round
    /// the circle, `frac` of it in `color` clockwise from the top, `thick`
    /// wide.
    Ring {
        frac: f32,
        color: Rgba,
        track: Rgba,
        thick: f32,
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

#[derive(Clone)]
struct Sem {
    id: String,
    node_type: String,
    text: Option<String>,
    tap: Option<Tap>,
    selected: bool,
    /// `core://Gesture` handlers other than the tap, by name.
    gestures: BTreeMap<String, Value>,
    /// A text field's placeholder.
    hint: Option<String>,
}

#[derive(Clone)]
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
    /// A sideways-scrolling row (a chip row): its children move left by the
    /// offset at `slide:<id>` (points), up to its content's overflow.
    slides: Option<String>,
    /// A `core://SwipeCard` (by semantic id) and its threshold (points): it
    /// moves right by the offset at `card:<id>`.
    card: Option<(String, f32)>,
    /// A text field's text.
    field: Option<FieldText>,
}

/// What a text field's text element knows of its field.
#[derive(Clone)]
struct FieldText {
    field: String,
    max_length: Option<usize>,
    secret: bool,
}

/// The semantic node type of a secret text field (`core://TextField`
/// `secret`): screen readers treat it as a password.
pub const SECRET_FIELD_TYPE: &str = "core://TextField/secret";

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
    /// Inside a scrolling view's content, where heights follow the content:
    /// an inline layout is as tall as its content, and another child that
    /// fills height takes the room short content leaves and is as tall as
    /// its content otherwise ([`placed`]).
    in_scroll: bool,
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

/// The earlier of two optional minutes.
fn earliest(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Lays out frames. Holds the fonts and the measurement cache, so reuse one
/// builder for many frames.
#[derive(Default)]
pub struct SceneBuilder {
    pub text: TextEngine,
    /// The clock that `format` dates and `date` conditionals read (this
    /// device's, unless set).
    pub clock: crate::dates::Clock,
    /// Draws the root at the whole width even when it doesn't adapt to the
    /// window ([`adapts_to_window`]): for a bare node tree, not a layout
    /// ([`SceneBuilder::build_from_spec`]).
    pub whole_width: bool,
}

struct Build<'t, 'i> {
    today: i64,
    /// [`SceneBuilder::whole_width`].
    whole_width: bool,
    /// Now on the clock, in local minutes since the epoch.
    now: i64,
    /// The zone the clock reads in (`None`: this device's).
    zone: Option<jiff::tz::TimeZone>,
    /// The next minute at which a `date` conditional built so far changes
    /// its answer.
    changes_at: Option<i64>,
    /// The scrolling views with `follow_end`, by semantic id.
    follow_end: Vec<String>,
    /// The inline layouts that open panels beside them, by the semantic id
    /// of their content.
    panel_hosts: Vec<String>,
    tree: TaffyTree<Leaf>,
    elems: Vec<Elem>,
    text: &'t mut TextEngine,
    theme: Theme,
    input: &'t Input<'i>,
    /// The reads of the list items being built now ([`Build::list_item`]).
    log: &'t ReadLog,
    /// The frame this one is built over, when it was built against the same
    /// [`Env`]: its list items whose data hasn't changed are reused.
    prev: Option<Prev>,
    /// This frame's list items, for the next frame to reuse.
    parts: HashMap<PartKey, Part>,
    /// The elements (in `elems`) at the top of each part taken over from
    /// `prev`.
    adopted: HashSet<usize>,
    reuse: Reuse,
    /// Each list item's element and its entity (scroll anchors).
    items: HashMap<usize, String>,
    /// How deep the node being built sits ([`MAX_BUILD_DEPTH`]).
    depth: usize,
}

/// What a frame was built against besides its entities and queries. A
/// frame built against other values reuses nothing of the one before.
#[derive(Clone, PartialEq)]
struct Env {
    root: UiNode,
    layouts: BTreeMap<String, UiNode>,
    form: BTreeMap<String, String>,
    route: BTreeMap<String, String>,
    viewport: Viewport,
    dark: bool,
    item: Option<String>,
    insets: [f32; 4],
    focused: Option<String>,
    panels: Panels,
    today: i64,
    /// The theme's entity (`local://theme`): every colour follows it.
    theme: Option<EntityRecord>,
    /// The carousels' pages (`page:<id>` of the scroll offsets: their
    /// badges show them); other offsets only place the frame.
    pages: Vec<(String, f32)>,
}

impl Env {
    fn of(input: &Input, today: i64) -> Env {
        Env {
            root: input.root.clone(),
            layouts: input.layouts.clone(),
            form: input.form.clone(),
            route: input.route.clone(),
            viewport: input.viewport,
            dark: input.dark,
            item: input.item.map(str::to_owned),
            insets: input.insets,
            focused: input.focused.map(str::to_owned),
            panels: input.panels.clone(),
            today,
            theme: input.data.entities.get("local://theme").cloned(),
            pages: input
                .scroll
                .iter()
                .filter(|(k, _)| k.starts_with("page:"))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        }
    }
}

/// A list item: its path (the semantic id it is built at) and its entity.
type PartKey = (String, String);

/// A built list item, kept for the next frame: its element (`None`: it
/// draws nothing), what it was built in, the data it read (each value as
/// it was), and the `follow_end` views in it.
struct Part {
    root: Option<usize>,
    axis: Axis,
    color: Option<Rgba>,
    depth: usize,
    entities: Vec<(String, Option<EntityRecord>)>,
    queries: Vec<(String, Option<Vec<String>>)>,
    follow_end: Vec<String>,
    /// When a `date` conditional in it next changes its answer.
    changes_at: Option<i64>,
}

impl Part {
    /// Whether everything it read is still the same.
    fn holds(&self, data: &DataBundle) -> bool {
        self.entities
            .iter()
            .all(|(u, e)| data.entities.get(u) == e.as_ref())
            && self
                .queries
                .iter()
                .all(|(k, q)| data.queries.get(k) == q.as_ref())
    }

    fn reads(&self) -> Reads {
        Reads {
            entities: self.entities.iter().map(|(u, _)| u.clone()).collect(),
            queries: self.queries.iter().map(|(k, _)| k.clone()).collect(),
        }
    }
}

/// The frame before, taken apart for reuse.
struct Prev {
    /// Its elements; each one taken over is taken out.
    elems: Vec<Option<Elem>>,
    parts: HashMap<PartKey, Part>,
}

/// How many list items a frame built and how many it took over from the
/// frame before ([`Laid::reuse`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reuse {
    pub built: usize,
    pub reused: usize,
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
        SceneBuilder {
            whole_width: true,
            ..SceneBuilder::new()
        }
        .build(&Input {
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
            panels: &NO_PANELS,
        })
    }

    /// Lays out one frame, to place with [`Laid::emit`] (again on scroll).
    pub fn layout(&mut self, input: &Input) -> Result<Laid, RenderError> {
        self.relayout(input, None)
    }

    /// [`SceneBuilder::layout`] over the frame before (`prev`, laid out for
    /// the same screen): when it was built against the same layout, form,
    /// viewport and theme ([`Env`]), its list items whose entity and every
    /// other entity or query they read are unchanged are taken over as
    /// they are (their nodes, bindings and measured layout), so a change to
    /// one row of a long list builds and measures that row only.
    pub fn relayout(&mut self, input: &Input, prev: Option<Laid>) -> Result<Laid, RenderError> {
        if input.root.depth() > MAX_BUILD_DEPTH {
            return Err(RenderError(format!(
                "layout nested more than {MAX_BUILD_DEPTH} deep"
            )));
        }
        let now = self.clock.minute();
        let today = now.div_euclid(1_440);
        let env = Env::of(input, today);
        let (tree, prev) = match prev {
            Some(p) if p.env == env && p.changes_at.is_none_or(|c| c > now) => (
                p.tree,
                Some(Prev {
                    elems: p.elems.into_iter().map(Some).collect(),
                    parts: p.parts,
                }),
            ),
            _ => (TaffyTree::new(), None),
        };
        let theme = Theme::new(input.dark, &input.data.entities);
        let log = ReadLog::default();
        let mut b = Build {
            today,
            whole_width: self.whole_width,
            now,
            zone: self.clock.zone.clone(),
            changes_at: None,
            follow_end: Vec::new(),
            panel_hosts: Vec::new(),
            tree,
            elems: Vec::new(),
            text: &mut self.text,
            theme: theme.clone(),
            input,
            log: &log,
            prev,
            parts: HashMap::new(),
            adopted: HashSet::new(),
            reuse: Reuse::default(),
            items: HashMap::new(),
            depth: 0,
        };
        let ctx = Ctx {
            entities: &input.data.entities,
            queries: &input.data.queries,
            form: input.form,
            route: input.route,
            theme: &theme,
            item: None,
            item_uri: None,
            reads: Some(&log),
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
            in_scroll: false,
        };
        let content = b.node(input.root, &state);
        if let Some(c) = content
            && !b.whole_width
        {
            b.phone_width(c, input.root, true);
        }
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
        b.drop_prev();
        b.compute(root, vp.scale)?;
        let mut laid = Laid {
            tree: b.tree,
            elems: b.elems,
            root,
            theme: b.theme,
            viewport: vp,
            scroll_max: BTreeMap::new(),
            pagers: BTreeMap::new(),
            slides: BTreeMap::new(),
            cards: BTreeMap::new(),
            follow_end: b.follow_end,
            panel_hosts: b.panel_hosts,
            changes_at: b.changes_at,
            env,
            parts: b.parts,
            reuse: b.reuse,
            items: b.items,
        };
        laid.scroll_max = laid.scroll_extents();
        laid.slides = laid
            .elems
            .iter()
            .filter_map(|e| Some((e.slides.clone()?, laid.slide_extent(e)?)))
            .collect();
        laid.pagers = laid
            .elems
            .iter()
            .filter_map(|e| {
                let id = e.pager.clone()?;
                let w = laid.tree.layout(e.node).ok()?.size.width;
                Some((id, (w, e.kids.len())))
            })
            .collect();
        laid.cards = laid
            .elems
            .iter()
            .filter_map(|e| {
                let (id, threshold) = e.card.clone()?;
                let w = laid.tree.layout(e.node).ok()?.size.width;
                Some((id, (threshold, w)))
            })
            .collect();
        Ok(laid)
    }

    /// Lays out and places one frame.
    pub fn build(&mut self, input: &Input) -> Result<Scene, RenderError> {
        Ok(self.layout(input)?.emit(input.scroll))
    }
}

/// Space between a Markdown text's blocks, between a list's items, and
/// above a heading that follows another block (points).
const MD_GAP: u16 = 8;
const MD_ITEM_GAP: u16 = 4;
const MD_HEADING_SPACE: f32 = 6.0;

/// A Markdown text's style: it may shrink below its longest word and wrap.
fn md_shrinks() -> Style {
    Style {
        min_size: Size {
            width: length(0.0),
            height: auto(),
        },
        ..Default::default()
    }
}

/// A regular `core://Button`: padding across and down, and its height
/// (decision `one-button-node`). Its corner is half the height.
const BUTTON_REGULAR: (u16, u16, f32) = (24, 10, 40.0);
/// A small `core://Button`.
const BUTTON_SMALL: (u16, u16, f32) = (16, 6, 32.0);

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
        | UiNode::Bars { .. }
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
        | UiNode::Dock { .. } => true,
        UiNode::Image { size_dp, .. } => size_dp.is_none(),
        // A bar takes the width; a ring is its own size.
        UiNode::Progress { value, shape, .. } => {
            value.is_some() && *shape != Some(protocol::ProgressShape::Ring)
        }
        // Markdown's blocks (a code card, a table) span the width.
        UiNode::Text {
            markdown: true,
            max_lines: None,
            ..
        } => true,
        UiNode::Box {
            fill,
            width_dp,
            content,
            ..
        } => *fill || (width_dp.is_none() && fills_width(content)),
        UiNode::Tappable { content, .. }
        | UiNode::Gesture { content, .. }
        | UiNode::SwipeCard { content, .. }
        | UiNode::Conditional { content, .. }
        | UiNode::Sticky { content } => fills_width(content),
        _ => false,
    }
}

/// Whether a node grows to the height its column leaves.
/// In a scrolling view's content (`in_scroll`) an inline layout is as tall
/// as its content and never grows.
fn fills_height_in(n: &UiNode, in_scroll: bool) -> bool {
    let fills = |c: &UiNode| fills_height_in(c, in_scroll);
    match n {
        UiNode::Scroll { .. }
        | UiNode::Dock { .. }
        | UiNode::Workspace { .. }
        | UiNode::Page { .. } => true,
        // A sized box keeps its height; an unsized one passes its content's on.
        UiNode::Box {
            fill,
            height_dp,
            content,
            ..
        } => *fill || (height_dp.is_none() && fills(content)),
        UiNode::Layout {
            height_dp: None, ..
        } => !in_scroll,
        UiNode::Tappable { content, .. }
        | UiNode::Gesture { content, .. }
        | UiNode::SwipeCard { content, .. }
        | UiNode::Conditional { content, .. } => fills(content),
        // A column placing its children down it (`justify`) takes the
        // height it is given.
        UiNode::Column {
            justify, children, ..
        } => *justify != Justify::Start || children.iter().any(fills),
        UiNode::Row { children, .. } => children.iter().any(fills),
        _ => false,
    }
}

/// A row child that shrinks and wraps (Compose `RowPart.Flexible`). A
/// container of controls only (chips, buttons, sized boxes), either way
/// round, is a control: it keeps its width, so a text beside it that wraps
/// can't squeeze it and leave its chips drawn past it.
fn row_flexible(n: &UiNode) -> bool {
    match n {
        UiNode::Text { .. } | UiNode::Column { .. } => true,
        UiNode::Container { children, .. } => children.iter().any(row_flexible),
        UiNode::Conditional { content, .. }
        | UiNode::Tappable { content, .. }
        | UiNode::Gesture { content, .. }
        | UiNode::SwipeCard { content, .. } => row_flexible(content),
        _ => false,
    }
}

/// A row of controls only (no text, nothing that fills, no unsized Spacer)
/// inside a row: it keeps its content's width like a control, rather than
/// sharing what the others leave and shrinking under its chips.
fn controls_row(n: &UiNode) -> bool {
    match n {
        UiNode::Row { children, .. } => !children.iter().any(|c| {
            row_flexible(c) || fills_width(c) || matches!(c, UiNode::Spacer { size_dp: None })
        }),
        UiNode::Box {
            fill: false,
            width_dp: None,
            content,
            ..
        }
        | UiNode::Tappable { content, .. }
        | UiNode::Gesture { content, .. }
        | UiNode::SwipeCard { content, .. }
        | UiNode::Conditional { content, .. }
        | UiNode::Sticky { content } => controls_row(content),
        _ => false,
    }
}

/// `style` adjusted for where the node sits: its parent's axis, and whether
/// that is inside a scrolling view's content.
fn placed(mut style: Style, n: &UiNode, axis: Axis, in_scroll: bool) -> Style {
    let wide = fills_width(n);
    let tall = fills_height_in(n, in_scroll);
    match axis {
        Axis::Column => {
            style.flex_shrink = 0.0;
            if wide {
                style.align_self = Some(AlignSelf::STRETCH);
            }
            if tall && in_scroll {
                // No height to share: at least the room left, else its
                // content's (a filling box).
                style.flex_grow = 1.0;
            } else if tall {
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
                // Text and columns wrap down to nothing; a container keeps
                // its content's minimum (its widest control, its longest
                // word), or a chip in it would draw past its bounds when a
                // text beside it wraps.
                if !matches!(n, UiNode::Container { .. }) {
                    style.min_size.width = length(0.0);
                }
            } else if wide
                && !controls_row(n)
                && !matches!(n, UiNode::Column { .. } | UiNode::Container { .. })
            {
                style.flex_grow = 1.0;
                style.flex_shrink = 1.0;
                style.flex_basis = length(0.0);
                style.min_size.width = length(0.0);
            } else {
                style.flex_shrink = 0.0;
            }
            // A tall child (a scrolling view, a filling box) takes the
            // row's height, whatever the row's alignment, and no more.
            if tall {
                style.align_self = Some(AlignSelf::STRETCH);
                style.min_size.height = length(0.0);
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
            slides: None,
            card: None,
            field: None,
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
        self.rich_leaf(style, text, Vec::new(), ty, color, sem)
    }

    /// A `core://Text` `markdown` (decision `text-markdown`): its blocks
    /// down a column, in the text's type and colour; with `max_lines`, its
    /// inline styles only, on that many lines. Screen readers get it as one
    /// text without its markup.
    fn markdown(&mut self, source: &str, ty: Type, color: Rgba, b: &B, n: &UiNode) -> usize {
        let blocks = markdown::parse(source);
        let flat = markdown::flatten(&blocks);
        let sem = self.sem(b, n, Some(flat.text.clone()), None);
        if ty.max_lines > 0 || blocks.is_empty() {
            let spans = self.rich_spans(&flat.spans);
            return self.rich_leaf(Style::default(), flat.text, spans, ty, color, Some(sem));
        }
        let ty = Type {
            strike: false,
            ..ty
        };
        let kids = self.md_blocks(&blocks, ty, color, 0);
        self.push(
            column(MD_GAP, pad(0, 0), AlignItems::STRETCH),
            kids,
            Paint::None,
            Some(sem),
            false,
        )
    }

    /// Markdown's inline marks as the text engine's spans: links in the
    /// theme's accent.
    fn rich_spans(&self, spans: &[markdown::Span]) -> Vec<RichSpan> {
        let link = self.theme.token("accent").unwrap_or(self.theme.primary);
        spans
            .iter()
            .map(|s| RichSpan {
                start: s.start,
                end: s.end,
                bold: s.mark.has(Mark::BOLD),
                italic: s.mark.has(Mark::ITALIC),
                mono: s.mark.has(Mark::CODE),
                strike: s.mark.has(Mark::STRIKE),
                color: s.mark.has(Mark::LINK).then(|| link.to_u8()),
            })
            .collect()
    }

    fn md_text(&mut self, text: &Inline, ty: Type, color: Rgba) -> usize {
        let spans = self.rich_spans(&text.spans);
        self.rich_leaf(md_shrinks(), text.text.clone(), spans, ty, color, None)
    }

    fn md_blocks(&mut self, blocks: &[Block], ty: Type, color: Rgba, lists: usize) -> Vec<usize> {
        let mut out = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            let k = self.md_block(block, ty, color, lists);
            // A heading stands a little apart from what comes before it.
            if i > 0 && matches!(block, Block::Heading { level, .. } if *level <= 3) {
                let node = self.elems[k].node;
                let mut st = self.tree.style(node).cloned().unwrap_or_default();
                st.margin.top = length(MD_HEADING_SPACE);
                let _ = self.tree.set_style(node, st);
            }
            out.push(k);
        }
        out
    }

    /// `lists`: how many lists it is in (a nested list's bullets are rings).
    fn md_block(&mut self, block: &Block, ty: Type, color: Rgba, lists: usize) -> usize {
        let theme = self.theme.clone();
        let tint = theme
            .token("muted_surface")
            .unwrap_or(theme.surface_variant);
        let border = theme.token("card_border").unwrap_or(theme.outline);
        match block {
            Block::Heading { level, text } => {
                let (scale, weight) = match level {
                    1 => (1.43, protocol::TextWeight::Bold),
                    2 => (1.25, protocol::TextWeight::Bold),
                    3 => (1.1, protocol::TextWeight::SemiBold),
                    _ => (1.0, protocol::TextWeight::SemiBold),
                };
                let size = (ty.size * scale).round();
                let h = Type {
                    face: ty.face.with_weight(weight),
                    size,
                    line_height: (size * 1.3).round().max(ty.line_height),
                    ..ty
                };
                self.md_text(text, h, color)
            }
            Block::Paragraph(text) => self.md_text(text, ty, color),
            Block::List { start, items } => {
                let mut rows = Vec::new();
                // Numbers line up: each marker as wide as the widest.
                let widest = markdown::marker(*start, items.len().saturating_sub(1), None)
                    .chars()
                    .count();
                for (i, item) in items.iter().enumerate() {
                    let m = match (item.task, start) {
                        (None, None) => self.md_bullet(ty, lists),
                        _ => {
                            let mark = markdown::marker(*start, i, item.task);
                            let mark_color = match item.task {
                                Some(true) => theme.token("accent").unwrap_or(theme.primary),
                                _ => theme.muted_text,
                            };
                            let mst = Style {
                                flex_shrink: 0.0,
                                min_size: Size {
                                    width: length(match item.task {
                                        None => ty.size * 0.6 * widest as f32,
                                        Some(_) => ty.size,
                                    }),
                                    height: auto(),
                                },
                                ..Default::default()
                            };
                            self.text_leaf(mst, mark, ty, mark_color, None)
                        }
                    };
                    // A done task's text recedes.
                    let item_color = if item.task == Some(true) {
                        theme.muted_text
                    } else {
                        color
                    };
                    let inner = self.md_blocks(&item.blocks, ty, item_color, lists + 1);
                    let mut body = column(MD_ITEM_GAP, pad(0, 0), AlignItems::STRETCH);
                    body.flex_grow = 1.0;
                    body.flex_shrink = 1.0;
                    body.flex_basis = length(0.0);
                    body.min_size.width = length(0.0);
                    let body = self.push(body, inner, Paint::None, None, false);
                    rows.push(self.push(
                        row(6, pad(0, 0), AlignItems::FLEX_START),
                        vec![m, body],
                        Paint::None,
                        None,
                        false,
                    ));
                }
                self.push(
                    column(MD_ITEM_GAP, pad(0, 0), AlignItems::STRETCH),
                    rows,
                    Paint::None,
                    None,
                    false,
                )
            }
            Block::Quote(inner) => {
                let bar = Style {
                    size: Size {
                        width: length(3.0),
                        height: auto(),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                };
                let accent = theme.token("accent").unwrap_or(theme.primary);
                let bar = self.push(
                    bar,
                    Vec::new(),
                    Paint::Quad {
                        bg: accent.with_alpha(0.45),
                        border: None,
                        dashed: false,
                        radius: 1.5,
                    },
                    None,
                    false,
                );
                let kids = self.md_blocks(inner, ty, theme.muted_text, lists);
                let mut body = column(MD_GAP, pad(0, 2), AlignItems::STRETCH);
                body.flex_grow = 1.0;
                body.flex_basis = length(0.0);
                body.min_size.width = length(0.0);
                let body = self.push(body, kids, Paint::None, None, false);
                self.push(
                    row(10, pad(0, 0), AlignItems::STRETCH),
                    vec![bar, body],
                    Paint::None,
                    None,
                    false,
                )
            }
            Block::Code(code) => {
                let spans = vec![RichSpan {
                    start: 0,
                    end: code.len(),
                    bold: false,
                    italic: false,
                    mono: true,
                    strike: false,
                    color: None,
                }];
                let t = self.rich_leaf(md_shrinks(), code.clone(), spans, ty, color, None);
                self.push(
                    column(0, pad(12, 10), AlignItems::STRETCH),
                    vec![t],
                    Paint::Quad {
                        bg: tint,
                        border: Some(border),
                        dashed: false,
                        radius: 8.0,
                    },
                    None,
                    false,
                )
            }
            Block::Table { head, rows } => {
                let bold = Type {
                    face: ty.face.with_weight(protocol::TextWeight::SemiBold),
                    ..ty
                };
                let mut lines = Vec::new();
                for (r, cells) in std::iter::once(head).chain(rows).enumerate() {
                    if r > 0 {
                        lines.push(self.md_rule(border));
                    }
                    let mut kids = Vec::new();
                    for cell in cells {
                        let t = self.md_text(cell, if r == 0 { bold } else { ty }, color);
                        let mut c = column(0, pad(10, 6), AlignItems::STRETCH);
                        c.flex_grow = 1.0;
                        c.flex_shrink = 1.0;
                        c.flex_basis = length(0.0);
                        c.min_size.width = length(0.0);
                        kids.push(self.push(c, vec![t], Paint::None, None, false));
                    }
                    let paint = if r == 0 {
                        Paint::Quad {
                            bg: tint,
                            border: None,
                            dashed: false,
                            radius: 0.0,
                        }
                    } else {
                        Paint::None
                    };
                    lines.push(self.push(
                        row(0, pad(0, 0), AlignItems::FLEX_START),
                        kids,
                        paint,
                        None,
                        false,
                    ));
                }
                self.push(
                    column(0, pad(0, 0), AlignItems::STRETCH),
                    lines,
                    Paint::Quad {
                        bg: Rgba::TRANSPARENT,
                        border: Some(border),
                        dashed: false,
                        radius: 8.0,
                    },
                    None,
                    true,
                )
            }
            Block::Rule => self.md_rule(border),
        }
    }

    /// A bullet: a dot centred on the first line, a ring in a nested list.
    fn md_bullet(&mut self, ty: Type, lists: usize) -> usize {
        let muted = self.theme.muted_text;
        let d = (ty.size * 0.36).round().max(4.0);
        let dot = Style {
            size: Size {
                width: length(d),
                height: length(d),
            },
            ..Default::default()
        };
        let paint = if lists.is_multiple_of(2) {
            Paint::Quad {
                bg: muted,
                border: None,
                dashed: false,
                radius: d / 2.0,
            }
        } else {
            Paint::Quad {
                bg: Rgba::TRANSPARENT,
                border: Some(muted),
                dashed: false,
                radius: d / 2.0,
            }
        };
        let dot = self.push(dot, Vec::new(), paint, None, false);
        let mut cell = row(0, pad(0, 0), AlignItems::CENTER);
        cell.justify_content = JustifyContent::CENTER;
        cell.size = Size {
            width: length(ty.size),
            height: length(ty.line_height),
        };
        cell.flex_shrink = 0.0;
        self.push(cell, vec![dot], Paint::None, None, false)
    }

    fn md_rule(&mut self, color: Rgba) -> usize {
        let st = Style {
            size: Size {
                width: auto(),
                height: length(1.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        self.push(
            st,
            Vec::new(),
            Paint::Quad {
                bg: color,
                border: None,
                dashed: false,
                radius: 0.0,
            },
            None,
            false,
        )
    }

    /// A text with styled ranges ([`RichSpan`]).
    fn rich_leaf(
        &mut self,
        style: Style,
        text: String,
        spans: Vec<RichSpan>,
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
                    spans: spans.clone(),
                },
            )
            .expect("taffy leaf");
        self.elems.push(Elem {
            node,
            paint: Paint::Text {
                text,
                ty,
                color,
                spans,
            },
            sem,
            kids: Vec::new(),
            clip: false,
            scrolls: false,
            sticky: false,
            pager: None,
            slides: None,
            card: None,
            field: None,
        });
        self.elems.len() - 1
    }

    /// A list item (`n`, the template, for the entity `uri`): taken over
    /// from the frame before when nothing it read has changed, else built
    /// (and what it read recorded for the next frame).
    fn list_item<'c>(&mut self, n: &'c UiNode, b: &B<'c>, uri: &str) -> Option<usize> {
        let key = (b.path.clone(), uri.to_owned());
        let data = &self.input.data;
        let hit = self.prev.as_mut().and_then(|p| {
            let part = p.parts.remove(&key)?;
            (part.axis == b.axis && part.color == b.color && part.depth == b.depth)
                .then_some(part)
                .filter(|part| part.holds(data))
        });
        if let Some(part) = hit {
            let root = part.root.map(|r| self.adopt(r));
            if let Some(r) = root {
                self.adopted.insert(r);
            }
            // An item being built around this one read what this one did.
            let reads = part.reads();
            for r in self.log.borrow_mut().iter_mut() {
                r.entities.extend(reads.entities.iter().cloned());
                r.queries.extend(reads.queries.iter().cloned());
            }
            self.follow_end.extend(part.follow_end.iter().cloned());
            self.changes_at = earliest(self.changes_at, part.changes_at);
            self.reuse.reused += 1;
            self.parts.insert(key, Part { root, ..part });
            return root;
        }
        self.log.borrow_mut().push(Reads::default());
        let follows = self.follow_end.len();
        let changes_before = self.changes_at.take();
        let root = self.node(n, b);
        let changes_at = self.changes_at;
        self.changes_at = earliest(changes_before, changes_at);
        let mut reads = self.log.borrow_mut().pop().unwrap_or_default();
        reads.entities.insert(uri.to_owned());
        for r in self.log.borrow_mut().iter_mut() {
            r.entities.insert(uri.to_owned());
        }
        let data = &self.input.data;
        let part = Part {
            root,
            axis: b.axis,
            color: b.color,
            depth: b.depth,
            entities: reads
                .entities
                .into_iter()
                .map(|u| {
                    let e = data.entities.get(&u).cloned();
                    (u, e)
                })
                .collect(),
            queries: reads
                .queries
                .into_iter()
                .map(|k| {
                    let q = data.queries.get(&k).cloned();
                    (k, q)
                })
                .collect(),
            follow_end: self.follow_end[follows..].to_vec(),
            changes_at,
        };
        self.reuse.built += 1;
        self.parts.insert(key, part);
        root
    }

    /// Takes element `old` of the frame before, and everything in it, into
    /// this frame (same taffy nodes, so their measured layout stays).
    fn adopt(&mut self, old: usize) -> usize {
        let e = self
            .prev
            .as_mut()
            .and_then(|p| p.elems.get_mut(old)?.take())
            .expect("an element of the frame before, taken over once");
        let kids = e.kids.iter().map(|k| self.adopt(*k)).collect();
        self.elems.push(Elem { kids, ..e });
        self.elems.len() - 1
    }

    /// Removes the frame before's nodes that weren't taken over, and hangs
    /// the ones that were under their new parents.
    fn drop_prev(&mut self) {
        let Some(prev) = self.prev.take() else {
            return;
        };
        // What was taken over was taken out.
        for e in prev.elems.iter().flatten() {
            let _ = self.tree.remove(e.node);
        }
        if self.adopted.is_empty() {
            return;
        }
        for e in &self.elems {
            if e.kids.iter().any(|k| self.adopted.contains(k)) {
                let ids: Vec<NodeId> = e.kids.iter().map(|k| self.elems[*k].node).collect();
                let _ = self.tree.set_children(e.node, &ids);
            }
        }
    }

    /// Sets a node's style when it differs (setting it clears the node's
    /// measured layout).
    fn restyle(&mut self, node: NodeId, style: Style) {
        if self.tree.style(node).ok() != Some(&style) {
            let _ = self.tree.set_style(node, style);
        }
    }

    /// Draws layout element `i` at most [`PHONE_WIDTH_DP`] wide, unless the
    /// layout `l` adapts to the window itself: centred on a whole screen,
    /// at the start (beside what is left of it) in an embed or a panel.
    /// Whether it was capped.
    fn phone_width(&mut self, i: usize, l: &UiNode, centred: bool) -> bool {
        if adapts_to_window(l) {
            return false;
        }
        let node = self.elems[i].node;
        let mut s = self.tree.style(node).cloned().unwrap_or_default();
        s.max_size.width = length(PHONE_WIDTH_DP);
        s.size.width = percent(1.0);
        if centred {
            s.margin.left = auto();
        }
        s.margin.right = auto();
        let _ = self.tree.set_style(node, s);
        true
    }

    /// An inline layout's content (`host`) with the panels open beside it
    /// ([`Panels`]): a row of the last [`PANEL_COLUMNS`] columns, a
    /// hairline between each two, each column as wide as the others.
    fn with_panels(
        &mut self,
        host: &str,
        content: Option<usize>,
        capped: bool,
        b: &B,
        ctx: Ctx,
    ) -> Option<usize> {
        let input = self.input;
        let open = input.panels.get(host).map_or(&[][..], Vec::as_slice);
        // Each column is a node with its semantic id, so a tap inside
        // (on a tagged row too) finds the column it came from.
        let column_sem = |id: String| Sem {
            id,
            node_type: "panel".into(),
            text: None,
            tap: None,
            selected: false,
            gestures: BTreeMap::new(),
            hint: None,
        };
        let content = content.map(|c| {
            let node = self.elems[c].node;
            let mut cs = self.tree.style(node).cloned().unwrap_or_default();
            cs.flex_grow = 1.0;
            let _ = self.tree.set_style(node, cs);
            let mut style = column(0, pad(0, 0), AlignItems::STRETCH);
            style.flex_grow = 1.0;
            self.push(
                style,
                vec![c],
                Paint::None,
                Some(column_sem(host.to_owned())),
                false,
            )
        });
        if open.is_empty() {
            return content;
        }
        // Column 0 is the embed's own content, column k + 1 panel k.
        let first = (open.len() + 1).saturating_sub(PANEL_COLUMNS);
        // Each column, and whether its layout is drawn at phone width.
        let mut cols: Vec<(usize, bool)> = Vec::new();
        if first == 0 {
            cols.extend(content.map(|c| (c, capped)));
        }
        for (k, p) in open.iter().enumerate().skip(first.saturating_sub(1)) {
            let pb = B {
                ctx: Ctx {
                    route: &p.route,
                    ..ctx
                },
                axis: Axis::Column,
                color: b.color,
                path: panel_id(host, k),
                depth: b.depth + 1,
                in_scroll: false,
            };
            let mut capped = false;
            let col = match input.layouts.get(&p.layout) {
                Some(l) => {
                    let c = self.node(l, &pb);
                    if let Some(c) = c {
                        capped = self.phone_width(c, l, false);
                    }
                    c
                }
                None => Some(self.placeholder(&format!("Layout {}", p.layout))),
            };
            if let Some(c) = col {
                let node = self.elems[c].node;
                let mut cs = self.tree.style(node).cloned().unwrap_or_default();
                cs.flex_grow = 1.0;
                let _ = self.tree.set_style(node, cs);
                let style = column(0, pad(0, 0), AlignItems::STRETCH);
                let sem = column_sem(panel_id(host, k));
                cols.push((
                    self.push(style, vec![c], Paint::None, Some(sem), false),
                    capped,
                ));
            }
        }
        let mut kids = Vec::new();
        for (i, (c, capped)) in cols.into_iter().enumerate() {
            if i > 0 {
                let line = Style {
                    size: Size {
                        width: length(1.0),
                        height: auto(),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                };
                kids.push(self.push(
                    line,
                    Vec::new(),
                    Paint::Quad {
                        bg: self.theme.outline,
                        border: None,
                        dashed: false,
                        radius: 0.0,
                    },
                    None,
                    false,
                ));
            }
            // Each column takes the whole height. A phone-width layout's
            // column is phone wide, next to the column before it, so the
            // columns build up from the left; the others share what is
            // left equally (decision `phone-width-layouts`). Short of room,
            // the phone-wide ones shrink too.
            let node = self.elems[c].node;
            let mut cs = self.tree.style(node).cloned().unwrap_or_default();
            if capped {
                cs.flex_grow = 0.0;
                cs.flex_shrink = 1.0;
                cs.flex_basis = length(PHONE_WIDTH_DP);
            } else {
                cs.flex_grow = 1.0;
                cs.flex_basis = length(0.0);
            }
            cs.min_size.width = length(0.0);
            let _ = self.tree.set_style(node, cs);
            kids.push(c);
        }
        let mut style = row(0, pad(0, 0), AlignItems::STRETCH);
        style.flex_grow = 1.0;
        Some(self.push(style, kids, Paint::None, None, false))
    }

    fn sem(&self, b: &B, n: &UiNode, text: Option<String>, tap: Option<Tap>) -> Sem {
        Sem {
            id: b.path.clone(),
            node_type: node_type(n),
            text,
            tap,
            selected: false,
            gestures: BTreeMap::new(),
            hint: None,
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
                        Some(Leaf::Text { text: s, ty, spans }) => {
                            let max = known.width.or(match avail.width {
                                AvailableSpace::Definite(w) => Some(w),
                                AvailableSpace::MinContent => Some(0.0),
                                AvailableSpace::MaxContent => None,
                            });
                            let m = text.measure_rich(s, spans, *ty, max, scale);
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
        // One level of recursion per nested node, and `build_node` is one
        // large frame: components nest boxes in boxes, so a page 20 or 30
        // deep must not depend on how big its thread's stack happens to be.
        if self.depth >= MAX_BUILD_DEPTH {
            return None;
        }
        self.depth += 1;
        let out = stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH, || self.build_node(n, b));
        self.depth -= 1;
        let out = out?;
        let node = self.elems[out].node;
        let style = self.tree.style(node).cloned().unwrap_or_default();
        let _ = self
            .tree
            .set_style(node, placed(style, n, b.axis, b.in_scroll));
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
                    hint: None,
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
            UiNode::Scroll {
                content,
                follow_end,
            } => {
                let cb = B {
                    in_scroll: true,
                    ..b.child(0, Axis::Column)
                };
                let kid = self.node(content, &cb);
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
                if *follow_end && let Some(s) = &self.elems[i].sem {
                    self.follow_end.push(s.id.clone());
                }
                i
            }
            UiNode::Column {
                children,
                spacing_dp,
                padding_h_dp,
                padding_v_dp,
                align,
                justify,
                max_width_dp,
            } => {
                let kids = self.children(children, b, Axis::Column);
                self.justify_children(&kids, *justify, Axis::Column);
                let mut style = column(
                    *spacing_dp,
                    pad(*padding_h_dp, *padding_v_dp),
                    align_items(*align),
                );
                if *max_width_dp > 0 {
                    // At most this wide, centred: auto margins on both sides
                    // take what it leaves (decision `screen-sizes`). Down a
                    // column it is as wide as it may be (auto margins would
                    // otherwise shrink it to its content).
                    style.max_size.width = length(*max_width_dp as f32);
                    if b.axis == Axis::Column {
                        style.size.width = percent(1.0);
                    }
                    style.margin.left = auto();
                    style.margin.right = auto();
                }
                self.push(style, kids, Paint::None, None, false)
            }
            UiNode::Row {
                children,
                spacing_dp,
                padding_h_dp,
                padding_v_dp,
                align,
                justify,
            } => {
                let kids = self.children(children, b, Axis::Row);
                self.justify_children(&kids, *justify, Axis::Row);
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
                // Children 8 points apart, as the shared UI spaces them; a
                // horizontal one wraps its children onto more lines (`FlowRow`).
                let p = pad(*padding_dp, *padding_dp);
                match orientation {
                    Orientation::Vertical => {
                        let kids = self.children(children, b, Axis::Column);
                        self.push(
                            column(8, p, AlignItems::FLEX_START),
                            kids,
                            Paint::None,
                            None,
                            false,
                        )
                    }
                    Orientation::Horizontal => {
                        let kids = self.children(children, b, Axis::Row);
                        let mut style = row(8, p, AlignItems::CENTER);
                        style.flex_wrap = FlexWrap::Wrap;
                        style.gap.height = length(8.0);
                        self.push(style, kids, Paint::None, None, false)
                    }
                }
            }
            UiNode::Text {
                value_expr,
                style,
                count,
                format,
                weight,
                caps,
                max_lines,
                strike,
                markdown,
            } => {
                let mut text = match format {
                    Some(f) => self.dated(value_expr, f, &ctx),
                    None => ctx.text(value_expr),
                };
                if let Some(c) = count {
                    let n = self.count(c, &ctx);
                    text = match (n, &c.zero_text) {
                        (Some(0), Some(z)) => ctx.text(z),
                        (Some(n), _) => {
                            // `$sum_money` before `$sum`, which it starts with.
                            if let Some(field) = &c.sum {
                                let total = self.sum_matching(c, field, &ctx);
                                text = text
                                    .replace("$sum_money", &money(total))
                                    .replace("$sum", &plain_number(total));
                            }
                            text.replace("$count", &n.to_string())
                        }
                        (None, _) => String::new(),
                    };
                }
                let mut text = cap_text(text);
                let mut ty = type_for(*style);
                if let Some(w) = weight {
                    ty.face = ty.face.with_weight(*w);
                }
                ty.max_lines = max_lines.unwrap_or(0).min(protocol::MAX_TEXT_LINES);
                ty.strike = *strike;
                let color = if *style == TextStyle::Secondary && b.color.is_none() {
                    theme.muted_text
                } else {
                    self.fg(b)
                };
                if *markdown {
                    return Some(self.markdown(&text, ty, color, b, n));
                }
                // Screen readers get the text as written, and whole.
                let sem = self.sem(b, n, Some(text.clone()), None);
                if *caps {
                    ty.tracking = protocol::CAPS_TRACKING;
                    text = text.to_uppercase();
                }
                self.text_leaf(Style::default(), text, ty, color, Some(sem))
            }
            UiNode::Button {
                label,
                on_click,
                style: face,
                size,
            } => {
                // One face per style and size (decision `one-button-node`):
                // a layout picks a style, never a height or a corner.
                let (bg, fg, border) = match face {
                    ButtonStyle::Primary => (theme.primary, theme.on_primary, None),
                    ButtonStyle::Secondary => (theme.surface, self.fg(b), Some(theme.outline)),
                    ButtonStyle::Tonal => (
                        theme
                            .token("accent_surface")
                            .unwrap_or(theme.surface_variant),
                        theme.primary,
                        None,
                    ),
                    ButtonStyle::Text => (Rgba::TRANSPARENT, theme.primary, None),
                };
                let (h, v, height) = match size {
                    ButtonSize::Regular => BUTTON_REGULAR,
                    ButtonSize::Small => BUTTON_SMALL,
                };
                let label = ctx.text(label);
                let l = self.text_leaf(Style::default(), label.clone(), LABEL_LARGE, fg, None);
                let tap = Tap::Intent {
                    intent: bind_intent(on_click, &ctx),
                };
                let mut style = row(0, pad(h, v), AlignItems::CENTER);
                style.justify_content = JustifyContent::CENTER;
                style.min_size.height = length(height);
                let sem = self.sem(b, n, Some(label), Some(tap));
                self.push(
                    style,
                    vec![l],
                    Paint::Quad {
                        bg,
                        border,
                        dashed: false,
                        radius: height / 2.0,
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
                        dashed: false,
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
                // An icon-only badge is just its icon: no gap and no empty
                // text beside it, so it centres where it is put (#325).
                let kids = if label.is_empty() {
                    vec![i]
                } else {
                    let t = self.text_leaf(
                        Style::default(),
                        label.clone(),
                        type_for(TextStyle::Secondary),
                        fg,
                        None,
                    );
                    vec![i, t]
                };
                // Decorative alone: screen readers skip a badge with no text.
                let sem = self.sem(b, n, Some(label).filter(|l| !l.is_empty()), None);
                self.push(
                    row(4, pad(0, 0), AlignItems::CENTER),
                    kids,
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
                        dashed: false,
                        radius: 0.0,
                    },
                    None,
                    false,
                )
            }
            UiNode::Conditional {
                content,
                not,
                count: Some(c),
                ..
            } => {
                // Not loaded yet: nothing, either way.
                let n = self.count_matching(&c.query_key, &c.where_, &ctx)?;
                if (n > 0) == *not {
                    return None;
                }
                return self.node(content, b);
            }
            UiNode::Conditional {
                value,
                content,
                equals,
                date: Some(test),
                time,
                not,
                ..
            } => {
                let time = time.as_deref().map(|t| ctx.text(t)).unwrap_or_default();
                // A value that isn't a date fails the test.
                let shows = match crate::dates::moment(&ctx.text(value), &time, self.zone.as_ref())
                {
                    Some(at) => {
                        let (shows, next) = crate::dates::passes(*test, at, self.now);
                        self.changes_at = earliest(self.changes_at, next);
                        shows && equals.as_deref().is_none_or(|e| ctx.shows(value, Some(e)))
                    }
                    None => false,
                };
                if shows == *not {
                    return None;
                }
                return self.node(content, b);
            }
            UiNode::Conditional {
                value,
                content,
                equals,
                not,
                ..
            } => {
                if ctx.shows(value, equals.as_deref()) == *not {
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
            UiNode::SwipeCard {
                content,
                on_swipe_left,
                on_swipe_right,
                threshold_dp,
                content_description,
            } => {
                let kid = self.node(content, &b.child(0, Axis::Column));
                let mut sem = self.sem(
                    b,
                    n,
                    content_description.as_deref().map(|d| ctx.text(d)),
                    None,
                );
                // A swipe that isn't a drag (a screen reader's, the
                // simulator's `swipe-left`) runs these as a gesture's.
                for (name, intent) in [
                    ("swipe_left", on_swipe_left),
                    ("swipe_right", on_swipe_right),
                ] {
                    if let Some(i) = intent {
                        sem.gestures.insert(name.to_owned(), bind_intent(i, &ctx));
                    }
                }
                let id = sem.id.clone();
                let i = self.push(
                    column(0, pad(0, 0), AlignItems::FLEX_START),
                    kid.into_iter().collect(),
                    Paint::None,
                    Some(sem),
                    false,
                );
                let threshold = threshold_dp.map_or(CARD_THRESHOLD, f32::from);
                self.elems[i].card = Some((id, threshold));
                i
            }
            UiNode::Box {
                content,
                background,
                content_color,
                border_color,
                border_style,
                corner_dp,
                padding_h_dp,
                padding_v_dp,
                width_dp,
                height_dp,
                align,
                fill,
            } => {
                let mut inner = b.child(0, Axis::Column);
                inner.in_scroll &= height_dp.is_none();
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
                        dashed: *border_style == BorderStyle::Dashed,
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
                max_length,
                secret,
            } => {
                let value = ctx.form.get(field).cloned().unwrap_or_default();
                let (shown, color) = if value.is_empty() {
                    (ctx.text(placeholder), theme.muted_text)
                } else if *secret {
                    (crate::text::mask(&value), self.fg(b))
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
                let text = self.text_leaf(tstyle, shown, type_for(TextStyle::Body), color, None);
                // The session draws the caret, selection and handles over it.
                self.elems[text].field = Some(FieldText {
                    field: field.clone(),
                    max_length: max_length.map(|m| m as usize),
                    secret: *secret,
                });
                let mut kids = vec![text];
                if !value.is_empty() {
                    // Empties the field and keeps typing in it.
                    let glyph = self.icon("close", theme.muted_text, 20.0);
                    let mut cstyle = column(0, pad(0, 0), AlignItems::CENTER);
                    cstyle.justify_content = JustifyContent::CENTER;
                    cstyle.size = Size {
                        width: length(40.0),
                        height: length(40.0),
                    };
                    cstyle.flex_shrink = 0.0;
                    // Into the field's padding: the glyph sits 10 from its edge.
                    cstyle.margin.right = length(-6.0);
                    let tap = Tap::Intent {
                        intent: json!({ "intent": "form.clear", "field": field, "focus": true }),
                    };
                    let mut csem = self.sem(b, n, Some("Clear".into()), Some(tap));
                    csem.id = format!("clear:{field}");
                    kids.push(self.push(cstyle, vec![glyph], Paint::None, Some(csem), false));
                }
                let blank = value.trim().is_empty();
                let focused = self.input.focused == Some(field.as_str());
                let mut style = row(8, pad(16, 0), AlignItems::CENTER);
                let extra = lines.map_or(0.0, |l| (l.saturating_sub(1)).min(4) as f32 * 20.0);
                style.min_size.height = length(56.0 + extra);
                // Room for a short placeholder ("Qty") on one line.
                style.min_size.width = length(72.0);
                // A secret's text is never in the semantics (screen readers,
                // the simulator's tree): only its dots.
                let said = if *secret {
                    crate::text::mask(&value)
                } else {
                    value
                };
                let mut sem = self.sem(
                    b,
                    n,
                    Some(said),
                    Some(Tap::Focus {
                        field: field.clone(),
                    }),
                );
                sem.id = format!("field:{field}");
                if *secret {
                    sem.node_type = SECRET_FIELD_TYPE.into();
                }
                sem.hint = Some(ctx.text(placeholder)).filter(|h| !h.is_empty());
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
                        dashed: false,
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
                            dashed: false,
                            radius: 8.0,
                        }
                    } else {
                        Paint::Quad {
                            bg: Rgba::TRANSPARENT,
                            border: Some(theme.outline),
                            dashed: false,
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
                        hint: None,
                    };
                    kids.push(self.push(style, vec![t], paint, Some(sem), false));
                }
                let mut style = row(8, pad(*padding_h_dp, 0), AlignItems::CENTER);
                // As wide as its place, whatever its chips; they scroll sideways.
                style.overflow.x = taffy::Overflow::Hidden;
                let mut sem = self.sem(b, n, None, None);
                sem.id = format!("chips:{field}");
                let id = sem.id.clone();
                let i = self.push(style, kids, Paint::None, Some(sem), true);
                self.elems[i].slides = Some(id);
                i
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
                stack,
                min_column_dp,
                trailing,
                merge_query_keys,
                sort_by,
            } => {
                let Some(first) = ctx.query(query_key) else {
                    // Not loaded yet: nothing, as the app shows while loading.
                    return Some(self.push(Style::default(), Vec::new(), Paint::None, None, false));
                };
                // Feature `list_merge`: the other queries' entities after
                // these, each once (a query not loaded yet adds nothing).
                let mut all: Vec<&String> = first.iter().collect();
                for k in merge_query_keys {
                    for u in ctx.query(k).into_iter().flatten() {
                        if !all.contains(&u) {
                            all.push(u);
                        }
                    }
                }
                // Each condition's value once (it binds the form, not the
                // item); a blank one matches everything.
                let wants = Where::of(where_.as_ref().map_or(&[][..], |w| w.conditions()), &ctx);
                let (now, zone) = (self.now, self.zone.clone());
                let mut next = None;
                let mut uris: Vec<&String> = all
                    .into_iter()
                    .filter(|u| {
                        ctx.entity(u).is_some_and(|e| {
                            wants
                                .iter()
                                .all(|w| w.matches(e, now, zone.as_ref(), &mut next))
                        })
                    })
                    .collect();
                self.changes_at = earliest(self.changes_at, next);
                if let Some(field) = sort_by {
                    // Stable: equal or missing values keep the queries' order.
                    let key = |u: &&String| {
                        ctx.entity(u)
                            .and_then(|e| e.get(field))
                            .map(render)
                            .unwrap_or_default()
                    };
                    uris.sort_by_cached_key(key);
                }
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
                // A deck draws its top card and the one behind it.
                uris.truncate(if *stack { 2 } else { MAX_LIST_ITEMS });
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
                            hint: None,
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
                // Automatic columns (decision `screen-sizes`): a CSS grid of
                // as many columns as fit, so cells go into it one by one.
                let auto_cols = *min_column_dp > 0 && !*stack;
                let mut spans = Vec::new();
                if auto_cols {
                    spans.extend(kids.iter().copied());
                }
                let cols = if auto_cols {
                    1
                } else {
                    (*columns).max(1) as usize
                };
                let mut last_group: Option<String> = None;
                // With headers_only, one header per distinct value wherever it
                // comes (docs/layouts.md), not one per run of equal values.
                let mut seen_groups: std::collections::HashSet<String> = Default::default();
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
                // A cell goes into the row being filled (a grid) or the list.
                let place = |this: &mut Self,
                             cell: usize,
                             cells: &mut Vec<usize>,
                             kids: &mut Vec<usize>| {
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
                        let w = this.push(wrap, vec![cell], Paint::None, None, false);
                        cells.push(w);
                        if cells.len() == cols {
                            flush(this, cells, kids);
                        }
                    } else {
                        kids.push(cell);
                    }
                };
                // Every cell fills its column, as the app's list does.
                let fill = |this: &mut Self, cell: usize| {
                    let node = this.elems[cell].node;
                    let mut s = this.tree.style(node).cloned().unwrap_or_default();
                    s.align_self = Some(AlignSelf::STRETCH);
                    s.flex_shrink = 0.0;
                    this.restyle(node, s);
                };
                for (i, uri) in uris.iter().enumerate() {
                    let item = ctx.entity(uri).expect("filtered");
                    let ib = B {
                        ctx: ctx.with_item(uri, item),
                        ..b.child(format!("i{i}"), Axis::Column)
                    };
                    if let Some(g) = group_by {
                        let v = item.get(&g.field).map(render).unwrap_or_default();
                        let new_group = if g.headers_only {
                            seen_groups.insert(v.clone())
                        } else {
                            last_group.as_deref() != Some(v.as_str())
                        };
                        if new_group {
                            last_group = Some(v);
                            flush(self, &mut row_cells, &mut kids);
                            let hb = B {
                                path: format!("{}/h{i}", b.path),
                                ..ib.clone()
                            };
                            let header = self.node(&g.header, &hb);
                            if auto_cols {
                                spans.extend(header);
                            }
                            kids.extend(header);
                        }
                        if g.headers_only {
                            continue;
                        }
                    }
                    let Some(mut cell) = self.list_item(item_template, &ib, uri) else {
                        continue;
                    };
                    self.items.insert(cell, (*uri).clone());
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
                                .and_then(|c| ctx.color(c))
                                .unwrap_or(theme.surface_variant),
                            border: None,
                            dashed: false,
                            radius: 0.0,
                        },
                        None => Paint::None,
                    };
                    fill(self, cell);
                    if indent.is_some() || focused {
                        cell = self.push(style, vec![cell], paint, None, false);
                    }
                    place(self, cell, &mut row_cells, &mut kids);
                }
                // The trailing cell (feature `list_trailing`): the next cell
                // after the items, bound like the list, never a deck's.
                if let Some(t) = trailing.as_deref().filter(|_| !*stack)
                    && let Some(cell) = self.node(t, &b.child("trailing", Axis::Column))
                {
                    fill(self, cell);
                    place(self, cell, &mut row_cells, &mut kids);
                }
                flush(self, &mut row_cells, &mut kids);
                let mut style = column(*spacing_dp, pad(*padding_h_dp, 0), AlignItems::STRETCH);
                if auto_cols {
                    style.display = Display::Grid;
                    style.grid_template_columns = vec![repeat(
                        RepetitionCount::AutoFill,
                        vec![minmax(length(*min_column_dp as f32), fr(1.0))],
                    )];
                    style.gap.width = length(*spacing_dp as f32);
                    // Group headers and the empty text span every column.
                    for &k in &spans {
                        let node = self.elems[k].node;
                        let mut s = self.tree.style(node).cloned().unwrap_or_default();
                        s.grid_column = Line {
                            start: line(1),
                            end: line(-1),
                        };
                        self.restyle(node, s);
                    }
                }
                if *stack && let [top, behind] = kids[..] {
                    // Side by side in a row, the top card pulled back over
                    // the next (drawn first, so under it): the row is as
                    // tall as the taller, and the next card, a little
                    // narrower, stretches to peek out below the top one.
                    let restyle = |this: &mut Self, i: usize, f: &dyn Fn(&mut Style)| {
                        let node = this.elems[i].node;
                        let mut s = this.tree.style(node).cloned().unwrap_or_default();
                        f(&mut s);
                        this.restyle(node, s);
                    };
                    restyle(self, behind, &|s| {
                        s.flex_basis = percent(1.0);
                        s.flex_shrink = 1.0;
                        s.min_size.width = length(0.0);
                        s.align_self = Some(AlignSelf::STRETCH);
                        s.margin = Rect {
                            left: length(STACK_INSET),
                            right: length(STACK_INSET),
                            top: length(STACK_PEEK),
                            bottom: zero(),
                        };
                    });
                    restyle(self, top, &|s| {
                        s.flex_basis = percent(1.0);
                        s.flex_shrink = 0.0;
                        s.align_self = Some(AlignSelf::FLEX_START);
                        s.margin = Rect {
                            left: percent(-1.0),
                            right: zero(),
                            top: zero(),
                            bottom: length(STACK_PEEK),
                        };
                    });
                    // Its card (the first thing down it that paints) fills
                    // that height.
                    let mut k = behind;
                    while let [c] = self.elems[k].kids[..] {
                        restyle(self, c, &|s| s.flex_grow = 1.0);
                        if !matches!(self.elems[c].paint, Paint::None) {
                            break;
                        }
                        k = c;
                    }
                    let mut deck = row(0, pad(0, 0), AlignItems::STRETCH);
                    deck.align_self = Some(AlignSelf::STRETCH);
                    deck.flex_shrink = 0.0;
                    kids = vec![self.push(deck, vec![behind, top], Paint::None, None, false)];
                }
                let mut sem = self.sem(b, n, None, None);
                sem.id = format!("list:{query_key}");
                self.push(style, kids, Paint::None, Some(sem), false)
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
                        hint: None,
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
                            dashed: false,
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
                            dashed: false,
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
                    hint: None,
                };
                let dots = self.push(ds, dots, Paint::None, Some(sem), false);
                self.push(outer, vec![frame, dots], Paint::None, None, false)
            }
            UiNode::Bars {
                query_key,
                bar,
                label,
                color,
                value,
                hatched,
                where_,
                height_dp,
                scale_min,
                field,
                on_click,
                merge,
                row,
                row_label,
            } => self.bars(
                n,
                b,
                BarsSpec {
                    row: row.as_deref(),
                    row_label: row_label.as_deref(),
                    row_height: height_dp.unwrap_or(GRID_ROW_DP) as f32,
                    query_key,
                    bar,
                    label,
                    color,
                    value: value.as_deref(),
                    hatched: hatched.as_deref(),
                    conds: where_.as_ref().map_or(&[][..], |w| w.conditions()),
                    height: height_dp.unwrap_or(BARS_HEIGHT_DP) as f32,
                    scale_min: *scale_min,
                    field: field.as_deref(),
                    on_click: on_click.as_ref(),
                    merge: merge.as_deref(),
                },
            ),
            UiNode::Progress {
                size_dp,
                value: Some(value),
                max,
                color,
                shape,
                label,
            } => {
                // A determinate bar (feature `progress_value`): the track as
                // wide as it is given, the fill value / max of it.
                let num = |v: &serde_json::Value| match v {
                    serde_json::Value::Number(n) => n.as_f64(),
                    serde_json::Value::String(s) => ctx.text(s).trim().parse::<f64>().ok(),
                    _ => None,
                };
                let max = max.as_ref().and_then(num).unwrap_or(1.0);
                let frac = match num(value) {
                    Some(v) if max > 0.0 && v.is_finite() => (v / max).clamp(0.0, 1.0) as f32,
                    _ => 0.0,
                };
                let fill_color = color
                    .as_deref()
                    .and_then(|c| ctx.color(c))
                    .unwrap_or(theme.primary);
                if *shape == Some(protocol::ProgressShape::Ring) {
                    // A ring (feature `progress_ring`), its label centred.
                    let d = size_dp.map_or(56.0, f32::from);
                    let kids = match label.as_deref().map(|l| ctx.text(l)) {
                        Some(t) if !t.is_empty() => {
                            let ty = if d >= 48.0 {
                                type_for(TextStyle::Label)
                            } else {
                                type_for(TextStyle::Caption)
                            };
                            vec![self.text_leaf(Style::default(), t, ty, self.fg(b), None)]
                        }
                        _ => Vec::new(),
                    };
                    let mut style = fixed(d, d);
                    style.align_items = AlignItems::CENTER;
                    style.justify_content = JustifyContent::CENTER;
                    let i = self.push(
                        style,
                        kids,
                        Paint::Ring {
                            frac,
                            color: fill_color,
                            track: theme.surface_variant,
                            thick: (d / 9.0).clamp(3.0, 10.0),
                        },
                        None,
                        false,
                    );
                    let said = match label.as_deref().map(|l| ctx.text(l)) {
                        Some(t) if !t.is_empty() => format!("{t}, {}%", (frac * 100.0).round()),
                        _ => format!("{}%", (frac * 100.0).round()),
                    };
                    self.elems[i].sem = Some(self.sem(b, n, Some(said), None));
                    return Some(i);
                }
                let h = size_dp.map_or(6.0, f32::from);
                let fill = self.push(
                    Style {
                        size: Size {
                            width: percent(frac),
                            height: percent(1.0),
                        },
                        ..Default::default()
                    },
                    Vec::new(),
                    Paint::Quad {
                        bg: fill_color,
                        border: None,
                        dashed: false,
                        radius: h / 2.0,
                    },
                    None,
                    false,
                );
                let i = self.push(
                    Style {
                        size: Size {
                            width: percent(1.0),
                            height: length(h),
                        },
                        flex_shrink: 0.0,
                        ..Default::default()
                    },
                    vec![fill],
                    Paint::Quad {
                        bg: theme.surface_variant,
                        border: None,
                        dashed: false,
                        radius: h / 2.0,
                    },
                    None,
                    true,
                );
                self.elems[i].sem =
                    Some(self.sem(b, n, Some(format!("{}%", (frac * 100.0).round())), None));
                i
            }
            UiNode::Progress { size_dp, .. } => {
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
            UiNode::MediaPicker {
                field,
                label,
                max_items,
            } => {
                // The photos staged so far (their ids in the form), each with
                // a remove button, then the add tile while there is room;
                // wrapping like the shared UI's FlowRow.
                let ids: Vec<String> = ctx
                    .form
                    .get(field)
                    .and_then(|v| serde_json::from_str(v).ok())
                    .unwrap_or_default();
                let mut tiles = Vec::new();
                for (i, id) in ids.iter().enumerate() {
                    let url = ctx
                        .entities
                        .get(&format!("construct://attachments/{id}"))
                        .and_then(|e| e.get_str("preview_url").or_else(|| e.get_str("url")))
                        .unwrap_or_default()
                        .to_owned();
                    let mut sem = self.sem(
                        b,
                        n,
                        Some(format!("Photo {} of {}", i + 1, ids.len())),
                        None,
                    );
                    sem.id = format!("media:{field}/{i}");
                    let img = self.push(
                        fixed(80.0, 80.0),
                        Vec::new(),
                        Paint::Image {
                            url,
                            placeholder: theme.surface_variant,
                            radius: 8.0,
                        },
                        Some(sem),
                        false,
                    );
                    let x = self.icon("close", Rgba::rgb(255, 255, 255), 16.0);
                    let mut rs = column(0, pad(0, 0), AlignItems::CENTER);
                    rs.justify_content = JustifyContent::CENTER;
                    rs.size = Size {
                        width: length(24.0),
                        height: length(24.0),
                    };
                    rs.position = Position::Absolute;
                    // The tile's top right corner (a right inset resolves
                    // against the row, not the tile).
                    rs.inset = Rect {
                        left: length(80.0 - 24.0 - 4.0),
                        right: auto(),
                        top: length(4.0),
                        bottom: auto(),
                    };
                    let mut rsem = self.sem(
                        b,
                        n,
                        Some(format!("Remove photo {}", i + 1)),
                        Some(Tap::Intent {
                            intent: json!({ "intent": "media.remove", "field": field, "id": id }),
                        }),
                    );
                    rsem.id = format!("media-remove:{id}");
                    let rm = self.push(
                        rs,
                        vec![x],
                        Paint::Quad {
                            bg: Rgba([0.0, 0.0, 0.0, 0.6]),
                            border: None,
                            dashed: false,
                            radius: 12.0,
                        },
                        Some(rsem),
                        false,
                    );
                    tiles.push(self.push(
                        fixed(80.0, 80.0),
                        vec![img, rm],
                        Paint::None,
                        None,
                        false,
                    ));
                }
                if (ids.len() as u32) < (*max_items).max(1) {
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
                        Some(
                            label
                                .as_deref()
                                .map(|l| ctx.text(l))
                                .filter(|l| !l.trim().is_empty())
                                .unwrap_or_else(|| "Add photo".to_owned()),
                        ),
                        // The platform's photo picker (`Effect::PickMedia`).
                        Some(Tap::Intent {
                            intent: json!({ "intent": "media.pick", "field": field, "max": max_items }),
                        }),
                    );
                    sem.id = format!("media:{field}");
                    tiles.push(self.push(
                        tile,
                        vec![plus],
                        Paint::Quad {
                            bg: Rgba::TRANSPARENT,
                            border: Some(theme.outline),
                            dashed: false,
                            radius: 8.0,
                        },
                        Some(sem),
                        false,
                    ));
                }
                let mut style = row(8, pad(0, 0), AlignItems::CENTER);
                style.flex_wrap = FlexWrap::Wrap;
                style.gap.height = length(8.0);
                self.push(style, tiles, Paint::None, None, false)
            }
            UiNode::Layout {
                layout,
                show_when,
                equals,
                presentation,
                height_dp,
                params,
                panels,
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
                // `params` bind as `$route.<name>` in it, as when it is
                // opened (bound here, against the embedding layout).
                let route: BTreeMap<String, String>;
                let lctx = if params.is_empty() {
                    ctx
                } else {
                    route = params
                        .iter()
                        .map(|(k, v)| (k.clone(), ctx.text(v)))
                        .collect();
                    Ctx {
                        route: &route,
                        ..ctx
                    }
                };
                let host = format!("{}/layout:{id}", b.path);
                let mut capped = false;
                let kid = match inner {
                    Some(l) => {
                        let lb = B {
                            ctx: lctx,
                            depth: b.depth + 1,
                            in_scroll: b.in_scroll && height_dp.is_none(),
                            ..b.child(format!("layout:{id}"), Axis::Column)
                        };
                        let k = self.node(l, &lb);
                        if let Some(k) = k {
                            capped = self.phone_width(k, l, false);
                        }
                        k
                    }
                    None => Some(self.placeholder(&format!("Layout {id}"))),
                };
                let wide = crate::interact::size_class(self.input.viewport.width) == "expanded";
                let kid = if *panels && wide && b.depth < MAX_EMBED_DEPTH {
                    self.panel_hosts.push(host.clone());
                    self.with_panels(&host, kid, capped, b, ctx)
                } else {
                    kid
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
                    && let Some(list) = ctx.query(q)
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
                        hint: None,
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
                        dashed: false,
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
            // Removed vocabulary and nodes this build doesn't know: named,
            // as the shared UI names them.
            UiNode::Drawer { .. } => self.unsupported("core://Drawer", b, n),
            UiNode::AppFrame { .. } => self.unsupported("core://AppFrame", b, n),
            UiNode::Unknown(raw) => {
                let t = raw.get("type").and_then(Value::as_str).unwrap_or("");
                self.unsupported(t, b, n)
            }
        };
        Some(out)
    }

    /// Places a row's or column's children along its axis (`justify`) with
    /// automatic margins, which take the room left and are nothing when
    /// there is none: children that don't fit start at the padding rather
    /// than hang off both ends, as a centred flexbox's would.
    fn justify_children(&mut self, kids: &[usize], justify: Justify, axis: Axis) {
        let (Some(&first), Some(&last)) = (kids.first(), kids.last()) else {
            return;
        };
        let mut auto = |k: usize, before: bool| {
            let node = self.elems[k].node;
            let mut s = self.tree.style(node).cloned().unwrap_or_default();
            let m = match (axis, before) {
                (Axis::Row, true) => &mut s.margin.left,
                (Axis::Row, false) => &mut s.margin.right,
                (Axis::Column, true) => &mut s.margin.top,
                (Axis::Column, false) => &mut s.margin.bottom,
            };
            *m = LengthPercentageAuto::auto();
            let _ = self.tree.set_style(node, s);
        };
        match justify {
            Justify::Start => {}
            Justify::Center => {
                auto(first, true);
                auto(last, false);
            }
            Justify::End => auto(first, true),
            Justify::SpaceBetween => kids[1..].iter().for_each(|&k| auto(k, true)),
            Justify::SpaceEvenly => {
                kids.iter().for_each(|&k| auto(k, true));
                auto(last, false);
            }
        }
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

    /// A node this build doesn't draw, as the shared UI shows it:
    /// "Unsupported component: <type>" in small grey text.
    fn unsupported(&mut self, node_type: &str, b: &B, n: &UiNode) -> usize {
        let name = if node_type.is_empty() {
            "<missing type>"
        } else {
            node_type
        };
        let label = format!("Unsupported component: {name}");
        let sem = self.sem(b, n, Some(label.clone()), None);
        let t = self.text_leaf(
            Style::default(),
            label,
            crate::style::LABEL_SMALL,
            Rgba::hex("#888888").unwrap_or(self.theme.muted_text),
            Some(sem),
        );
        self.push(
            column(0, pad(4, 4), AlignItems::FLEX_START),
            vec![t],
            Paint::None,
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
                dashed: false,
                radius: 8.0,
            },
            None,
            false,
        )
    }

    /// A `core://Text` with `format`: every bound `YYYY-MM-DD` value drawn
    /// as a date; `past` instead when one is before today.
    fn dated(&self, expr: &str, f: &protocol::TextFormat, ctx: &Ctx) -> String {
        let today = self.today;
        let mut past = false;
        let text = ctx.text_with(
            expr,
            &mut |v| match v.as_str().and_then(crate::dates::iso_date) {
                Some(day) => {
                    past |= day < today;
                    crate::dates::format_date(day, today, f.date)
                }
                None => crate::bind::render(v),
            },
        );
        match &f.past {
            Some(p) if past => ctx.text(p),
            _ => text,
        }
    }

    fn count(&mut self, c: &protocol::TextCount, ctx: &Ctx) -> Option<usize> {
        self.count_matching(&c.query_key, &c.where_, ctx)
    }

    /// The total of `field` over the entities a count counts (feature
    /// `count_sum`); values that aren't numbers count as 0.
    fn sum_matching(&mut self, c: &protocol::TextCount, field: &str, ctx: &Ctx) -> f64 {
        let Some(list) = ctx.query(&c.query_key) else {
            return 0.0;
        };
        let wants = Where::of(&c.where_, ctx);
        let (now, zone) = (self.now, self.zone.clone());
        let mut next = None;
        list.iter()
            .filter_map(|u| ctx.entity(u))
            .filter(|e| {
                wants
                    .iter()
                    .all(|w| w.matches(e, now, zone.as_ref(), &mut next))
            })
            .filter_map(|e| match e.get(field)? {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s
                    .trim()
                    .trim_start_matches('$')
                    .replace(',', "")
                    .parse()
                    .ok(),
                _ => None,
            })
            .filter(|v: &f64| v.is_finite())
            .sum()
    }

    /// How many entities of `queries[query_key]` match every condition
    /// (`None` while the query is not loaded).
    fn count_matching(
        &mut self,
        query_key: &str,
        conds: &[protocol::ListWhere],
        ctx: &Ctx,
    ) -> Option<usize> {
        let list = ctx.query(query_key)?;
        let wants = Where::of(conds, ctx);
        let (now, zone) = (self.now, self.zone.clone());
        let mut next = None;
        let n = list
            .iter()
            .filter_map(|u| ctx.entity(u))
            .filter(|e| {
                wants
                    .iter()
                    .all(|w| w.matches(e, now, zone.as_ref(), &mut next))
            })
            .count();
        self.changes_at = earliest(self.changes_at, next);
        Some(n)
    }
}

/// A `core://Bars` node's fields, as [`Build::bars`] reads them.
struct BarsSpec<'n> {
    query_key: &'n str,
    bar: &'n str,
    label: &'n str,
    color: &'n str,
    value: Option<&'n str>,
    hatched: Option<&'n str>,
    conds: &'n [protocol::ListWhere],
    height: f32,
    scale_min: u16,
    field: Option<&'n str>,
    on_click: Option<&'n ActionIntent>,
    merge: Option<&'n str>,
    /// Feature `bars_rows`: the field whose values make a grid's rows.
    row: Option<&'n str>,
    row_label: Option<&'n str>,
    row_height: f32,
}

/// A `core://Bars` grid row's height without `height_dp`.
const GRID_ROW_DP: u16 = 18;
/// The most rows a grid draws (the first ones).
const MAX_ROWS: usize = 200;
/// The widest a grid's row labels get; longer ones wrap.
const GRID_LABEL_MAX_DP: f32 = 84.0;

/// A `core://Bars` chart's height without `height_dp`.
const BARS_HEIGHT_DP: u16 = 140;
/// The most bars a chart draws (the first ones).
const MAX_BARS: usize = 120;

impl<'i> Build<'_, 'i> {
    /// A `core://Bars` chart (feature `bars`): the axis's numbers on
    /// the left, then one column per bar, its segments stacked from the
    /// bottom over its label; a tap on a column runs `on_click` with
    /// `$item` the bar's first item.
    fn bars<'c>(&mut self, n: &UiNode, b: &B<'c>, spec: BarsSpec) -> usize {
        if let Some(row_field) = spec.row {
            return self.bar_grid(n, b, &spec, row_field);
        }
        let ctx = b.ctx;
        let theme = self.theme.clone();
        let Some(all) = ctx.query(spec.query_key) else {
            return self.push(Style::default(), Vec::new(), Paint::None, None, false);
        };
        let wants = Where::of(spec.conds, &ctx);
        let (now, zone) = (self.now, self.zone.clone());
        let mut next = None;
        // Bars in the order their first item comes: (key, items).
        let mut bars: Vec<(String, Vec<&String>)> = Vec::new();
        for uri in all {
            let Some(e) = ctx.entity(uri) else { continue };
            if !wants
                .iter()
                .all(|w| w.matches(e, now, zone.as_ref(), &mut next))
            {
                continue;
            }
            let key = e.get(spec.bar).map(render).unwrap_or_default();
            if let Some(i) = bars.iter().position(|(k, _)| *k == key) {
                bars[i].1.push(uri);
            } else if bars.len() < MAX_BARS {
                bars.push((key, vec![uri]));
            }
        }
        self.changes_at = earliest(self.changes_at, next);
        let size_of = |e: &protocol::EntityRecord| -> f32 {
            spec.value
                .map_or(1.0, |f| match e.get(f) {
                    Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) as f32,
                    Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
                    _ => 0.0,
                })
                .max(0.0)
        };
        let totals: Vec<f32> = bars
            .iter()
            .map(|(_, items)| {
                items
                    .iter()
                    .filter_map(|u| ctx.entity(u))
                    .map(size_of)
                    .sum()
            })
            .collect();
        let top = totals
            .iter()
            .fold(f32::from(spec.scale_min).max(1.0), |m, t| m.max(*t))
            .ceil();
        // The picked bar: the form value, else the last bar.
        let chosen = spec.field.map(|f| {
            ctx.form
                .get(f)
                .filter(|v| !v.is_empty())
                .cloned()
                .or_else(|| bars.last().map(|(k, _)| k.clone()))
                .unwrap_or_default()
        });
        let caption = type_for(TextStyle::Caption);
        // The axis: its top and 0, beside the plot, over a label's height.
        let axis_text = |this: &mut Self, t: String| {
            let style = Style {
                flex_shrink: 0.0,
                ..Default::default()
            };
            this.text_leaf(style, t, caption, theme.muted_text, None)
        };
        let hi = axis_text(self, format!("{top}"));
        let lo = axis_text(self, "0".into());
        let mut plot_axis = column(0, pad(0, 0), AlignItems::FLEX_END);
        plot_axis.justify_content = JustifyContent::SPACE_BETWEEN;
        plot_axis.size.height = length(spec.height);
        let plot_axis = self.push(plot_axis, vec![hi, lo], Paint::None, None, false);
        let mut axis = column(4, pad(0, 0), AlignItems::FLEX_END);
        axis.flex_shrink = 0.0;
        let gap_label = self.text_leaf(
            Style::default(),
            " ".into(),
            caption,
            theme.muted_text,
            None,
        );
        let axis = self.push(axis, vec![plot_axis, gap_label], Paint::None, None, false);

        let mut columns = Vec::new();
        for (i, (key, items)) in bars.iter().enumerate() {
            let first = items[0];
            let item = ctx.entity(first).expect("filtered");
            let item_ctx = ctx.with_item(first, item);
            // Segments, bottom first: a column that ends at the bottom.
            // `(value, colour, hatched, merge key)`, neighbours with the
            // same merge key and hatching drawn as one (feature
            // `bars_merge`).
            let mut runs: Vec<(f32, Rgba, bool, Option<String>)> = Vec::new();
            for uri in items.iter() {
                let Some(e) = ctx.entity(uri) else { continue };
                let v = size_of(e);
                if v <= 0.0 {
                    continue;
                }
                let striped = spec.hatched.is_some_and(|f| {
                    matches!(e.get(f), Some(Value::Bool(true)))
                        || e.get(f)
                            .is_some_and(|v| matches!(render(v).as_str(), "true" | "1"))
                });
                let key = spec.merge.map(|f| e.get(f).map(render).unwrap_or_default());
                if let Some(last) = runs.last_mut()
                    && key.is_some()
                    && last.3 == key
                    && last.2 == striped
                {
                    last.0 += v;
                    continue;
                }
                let ectx = ctx.with_item(uri, e);
                let c = ectx.color(&ectx.text(spec.color)).unwrap_or(theme.outline);
                runs.push((v, c, striped, key));
            }
            let mut segs = Vec::new();
            for (v, c, striped, _) in runs {
                let h = (v / top * spec.height - 2.0).max(2.0);
                let style = Style {
                    size: Size {
                        width: percent(1.0),
                        height: length(h),
                    },
                    max_size: Size {
                        width: length(22.0),
                        height: auto(),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                };
                let paint = Paint::Quad {
                    bg: if striped { Rgba::TRANSPARENT } else { c },
                    border: striped.then_some(c),
                    dashed: striped,
                    radius: 3.0,
                };
                segs.push(self.push(style, Vec::new(), paint, None, false));
            }
            // The first item at the bottom: drawn top to bottom.
            segs.reverse();
            let mut plot = column(2, pad(0, 0), AlignItems::CENTER);
            plot.justify_content = JustifyContent::FLEX_END;
            plot.size.height = length(spec.height);
            plot.size.width = percent(1.0);
            let plot = self.push(plot, segs, Paint::None, None, false);
            let text = item_ctx.text(spec.label);
            // A label keeps its words whole: at least as wide as it is on
            // one line (a narrow column would break "Now" in two).
            let one_line = self.text.measure(&text, caption, None, 1.0).width;
            let label = self.text_leaf(
                Style {
                    min_size: Size {
                        width: length(one_line.ceil() + 1.0),
                        height: auto(),
                    },
                    ..Default::default()
                },
                if text.is_empty() {
                    " ".into()
                } else {
                    text.clone()
                },
                caption,
                theme.muted_text,
                None,
            );
            let mut style = column(4, pad(0, 0), AlignItems::CENTER);
            // Equal columns, each at least as wide as its label's word.
            style.flex_grow = 1.0;
            style.flex_basis = length(0.0);
            let on = chosen.as_deref() == Some(key.as_str());
            let paint = if on {
                Paint::Quad {
                    bg: theme.surface_variant,
                    border: None,
                    dashed: false,
                    radius: 6.0,
                }
            } else {
                Paint::None
            };
            let tap = match (spec.field, spec.on_click) {
                (Some(f), _) => Some(Tap::SetForm {
                    field: f.to_owned(),
                    value: key.clone(),
                }),
                (None, Some(i)) => Some(Tap::Intent {
                    intent: bind_intent(i, &item_ctx),
                }),
                (None, None) => None,
            };
            let mut sem = self.sem(b, n, Some(text), tap);
            sem.id = format!("{}/bar{i}", b.path);
            sem.selected = on;
            columns.push(self.push(style, vec![plot, label], paint, Some(sem), false));
        }
        let mut plots = row(4, pad(0, 0), AlignItems::FLEX_START);
        plots.flex_grow = 1.0;
        plots.min_size.width = length(0.0);
        let plots = self.push(plots, columns, Paint::None, None, false);
        let mut style = row(8, pad(0, 0), AlignItems::FLEX_START);
        style.align_self = Some(AlignSelf::STRETCH);
        let sem = self.sem(b, n, None, None);
        self.push(style, vec![axis, plots], Paint::None, Some(sem), false)
    }
}

impl<'i> Build<'_, 'i> {
    /// A `core://Bars` grid (feature `bars_rows`): a row per `row` value,
    /// its label on the left, then a cell per bar, the cell's items side by
    /// side from the left, each `value` wide out of `scale_min` (or the
    /// cell's total, if more); the bars' labels under the last row. A tap
    /// on a cell sets `$form.<field>` to `<row>/<bar>`, or runs `on_click`
    /// with `$item` the cell's first item.
    fn bar_grid<'c>(&mut self, n: &UiNode, b: &B<'c>, spec: &BarsSpec, row_field: &str) -> usize {
        let ctx = b.ctx;
        let theme = self.theme.clone();
        let Some(all) = ctx.query(spec.query_key) else {
            return self.push(Style::default(), Vec::new(), Paint::None, None, false);
        };
        let wants = Where::of(spec.conds, &ctx);
        let (now, zone) = (self.now, self.zone.clone());
        let mut next = None;
        // Rows in the order their first item comes, and bars in the order
        // they first come in any row: (key, items).
        let mut rows: Vec<(String, Vec<&String>)> = Vec::new();
        let mut bars: Vec<(String, &String)> = Vec::new();
        for uri in all {
            let Some(e) = ctx.entity(uri) else { continue };
            if !wants
                .iter()
                .all(|w| w.matches(e, now, zone.as_ref(), &mut next))
            {
                continue;
            }
            let bar = e.get(spec.bar).map(render).unwrap_or_default();
            if !bars.iter().any(|(k, _)| *k == bar) {
                if bars.len() >= MAX_BARS {
                    continue;
                }
                bars.push((bar, uri));
            }
            let key = e.get(row_field).map(render).unwrap_or_default();
            if let Some(i) = rows.iter().position(|(k, _)| *k == key) {
                rows[i].1.push(uri);
            } else if rows.len() < MAX_ROWS {
                rows.push((key, vec![uri]));
            }
        }
        self.changes_at = earliest(self.changes_at, next);
        let size_of = |e: &protocol::EntityRecord| -> f32 {
            spec.value
                .map_or(1.0, |f| match e.get(f) {
                    Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) as f32,
                    Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
                    _ => 0.0,
                })
                .max(0.0)
        };
        let scale = f32::from(spec.scale_min).max(1.0);
        let chosen = spec
            .field
            .and_then(|f| ctx.form.get(f).filter(|v| !v.is_empty()).cloned());
        let caption = type_for(TextStyle::Caption);
        let bar_labels: Vec<String> = bars
            .iter()
            .map(|(_, uri)| {
                let e = ctx.entity(uri).expect("filtered");
                ctx.with_item(uri, e).text(spec.label)
            })
            .collect();
        // Every column at least as wide as the widest bar label, so the
        // cells line up with the labels under them.
        let col_min = bar_labels
            .iter()
            .map(|t| self.text.measure(t, caption, None, 1.0).width.ceil() + 1.0)
            .fold(0.0f32, f32::max);
        let row_labels: Vec<String> = rows
            .iter()
            .map(|(key, items)| {
                let first = items[0];
                let e = ctx.entity(first).expect("filtered");
                spec.row_label
                    .map_or_else(|| key.clone(), |l| ctx.with_item(first, e).text(l))
            })
            .collect();
        let label_w = row_labels
            .iter()
            .map(|t| self.text.measure(t, caption, None, 1.0).width.ceil() + 1.0)
            .fold(0.0f32, f32::max)
            .min(GRID_LABEL_MAX_DP);
        let cell_style = |h: f32| {
            let mut st = row(1, pad(0, 0), AlignItems::STRETCH);
            st.flex_grow = 1.0;
            st.flex_basis = length(0.0);
            st.min_size.width = length(col_min);
            st.size.height = length(h);
            st.padding = Rect {
                left: length(1.5),
                right: length(1.5),
                top: length(1.5),
                bottom: length(1.5),
            };
            st
        };
        let label_style = || Style {
            size: Size {
                width: length(label_w),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };

        let mut lines = Vec::new();
        for (r, ((key, items), text)) in rows.iter().zip(&row_labels).enumerate() {
            let label = self.text_leaf(
                label_style(),
                text.clone(),
                caption,
                theme.on_background,
                None,
            );
            let mut cells = vec![label];
            for (c, (bar, _)) in bars.iter().enumerate() {
                let here: Vec<&String> = items
                    .iter()
                    .copied()
                    .filter(|u| {
                        ctx.entity(u).is_some_and(|e| {
                            e.get(spec.bar).map(render).unwrap_or_default() == *bar
                        })
                    })
                    .collect();
                let total: f32 = here.iter().filter_map(|u| ctx.entity(u)).map(size_of).sum();
                let full = total.max(scale);
                let mut segs = Vec::new();
                for uri in &here {
                    let Some(e) = ctx.entity(uri) else { continue };
                    let v = size_of(e);
                    if v <= 0.0 {
                        continue;
                    }
                    let striped = spec.hatched.is_some_and(|f| {
                        e.get(f)
                            .is_some_and(|v| matches!(render(v).as_str(), "true" | "1"))
                    });
                    let ectx = ctx.with_item(uri, e);
                    let col = ectx.color(&ectx.text(spec.color)).unwrap_or(theme.outline);
                    let style = Style {
                        flex_grow: v / full,
                        flex_basis: length(0.0),
                        min_size: Size {
                            width: length(2.0),
                            height: auto(),
                        },
                        ..Default::default()
                    };
                    let paint = Paint::Quad {
                        bg: if striped { Rgba::TRANSPARENT } else { col },
                        border: striped.then_some(col),
                        dashed: striped,
                        radius: 3.0,
                    };
                    segs.push(self.push(style, Vec::new(), paint, None, false));
                }
                if full > total {
                    let style = Style {
                        flex_grow: (full - total) / full,
                        flex_basis: length(0.0),
                        ..Default::default()
                    };
                    segs.push(self.push(style, Vec::new(), Paint::None, None, false));
                }
                let value = format!("{key}/{bar}");
                let on = chosen.as_deref() == Some(value.as_str());
                let paint = if on {
                    Paint::Quad {
                        bg: theme.surface_variant,
                        border: Some(theme.on_background),
                        dashed: false,
                        radius: 4.0,
                    }
                } else {
                    Paint::None
                };
                let tap = match (spec.field, spec.on_click, here.first()) {
                    (Some(f), _, _) => Some(Tap::SetForm {
                        field: f.to_owned(),
                        value,
                    }),
                    (None, Some(i), Some(first)) => {
                        let e = ctx.entity(first).expect("filtered");
                        Some(Tap::Intent {
                            intent: bind_intent(i, &ctx.with_item(first, e)),
                        })
                    }
                    _ => None,
                };
                let mut sem = self.sem(b, n, Some(format!("{text} {}", bar_labels[c])), tap);
                sem.id = format!("{}/cell{r}_{c}", b.path);
                sem.selected = on;
                cells.push(self.push(cell_style(spec.row_height), segs, paint, Some(sem), false));
            }
            let mut line = row(2, pad(0, 0), AlignItems::CENTER);
            line.align_self = Some(AlignSelf::STRETCH);
            lines.push(self.push(line, cells, Paint::None, None, false));
        }
        // The bars' labels under the grid, a column each.
        if !rows.is_empty() {
            let gap = self.push(label_style(), Vec::new(), Paint::None, None, false);
            let mut labels = vec![gap];
            for t in &bar_labels {
                let mut st = column(0, pad(0, 0), AlignItems::CENTER);
                st.flex_grow = 1.0;
                st.flex_basis = length(0.0);
                st.min_size.width = length(col_min);
                let text = self.text_leaf(
                    Style::default(),
                    if t.is_empty() { " ".into() } else { t.clone() },
                    caption,
                    theme.muted_text,
                    None,
                );
                labels.push(self.push(st, vec![text], Paint::None, None, false));
            }
            let mut line = row(2, pad(0, 0), AlignItems::FLEX_START);
            line.align_self = Some(AlignSelf::STRETCH);
            lines.push(self.push(line, labels, Paint::None, None, false));
        }
        let mut style = column(2, pad(0, 0), AlignItems::STRETCH);
        style.align_self = Some(AlignSelf::STRETCH);
        let sem = self.sem(b, n, None, None);
        self.push(style, lines, Paint::None, Some(sem), false)
    }
}

/// One `where` condition as a frame tests it: its `equals` read once (it
/// binds the form, not the item), and its age (feature `list_where_age`).
struct Where<'a> {
    field: &'a str,
    want: String,
    /// `Some`: an `older_than` condition, in seconds (`None` inside: a
    /// duration that doesn't read, which matches nothing).
    older: Option<Option<i64>>,
    not: bool,
}

impl<'a> Where<'a> {
    /// The conditions that test anything (a blank `equals` matches
    /// everything, `not` or not).
    fn of(conds: &'a [protocol::ListWhere], ctx: &Ctx) -> Vec<Self> {
        conds
            .iter()
            .map(|c| Where {
                field: &c.field,
                want: ctx.text(&c.equals),
                older: c.older_than.as_deref().map(protocol::ast::duration_secs),
                not: c.not,
            })
            .filter(|w| w.older.is_some() || !w.want.trim().is_empty())
            .collect()
    }

    /// Whether entity `e` passes at `now` (local minutes); an age
    /// condition notes in `next` the minute its answer changes.
    fn matches(
        &self,
        e: &protocol::EntityRecord,
        now: i64,
        zone: Option<&jiff::tz::TimeZone>,
        next: &mut Option<i64>,
    ) -> bool {
        let v = e.get(self.field);
        let hit = match self.older {
            None => v.is_some_and(|v| matches_text(v, &self.want)),
            Some(None) => false,
            Some(Some(secs)) => {
                let at = v
                    .and_then(Value::as_str)
                    .and_then(|t| crate::dates::moment(t, "", zone))
                    .map(|m| m.day * 1_440 + m.minute.unwrap_or(0));
                match at {
                    Some(at) => {
                        let from = at + (secs + 59) / 60;
                        if from > now {
                            *next = earliest(*next, Some(from));
                        }
                        from <= now
                    }
                    None => false,
                }
            }
        };
        hit != self.not
    }
}

/// Whether a value reads as `want` (as [`render`] would write it), without
/// writing it out for the common kinds.
/// A number as a price (feature `count_sum`): `$98`, `$28.50`,
/// `$1,299.95`, `-$4.20`, as the collections' `money()` writes it.
fn money(v: f64) -> String {
    let cents = (v.abs() * 100.0).round() as u64;
    let (whole, part) = (cents / 100, cents % 100);
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let sign = if v < 0.0 && cents > 0 { "-" } else { "" };
    if part == 0 {
        format!("{sign}${grouped}")
    } else {
        format!("{sign}${grouped}.{part:02}")
    }
}

/// A number without a trailing `.0`, at most two decimals.
fn plain_number(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r.fract() == 0.0 {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

fn matches_text(v: &Value, want: &str) -> bool {
    match v {
        Value::String(s) => s == want,
        Value::Bool(b) => want == if *b { "true" } else { "false" },
        _ => render(v) == want,
    }
}

/// Stretches what a scrolling view drew from `from` (draws, semantics,
/// fields) by `scale` along its axis, away from `anchor`, inside its `clip`
/// (Android's stretch at an end, `scroll::Stretch`). Boxes grow; text,
/// icons and images move with them but keep their size. An image (and its
/// placeholder, drawn just before it in its box) keeps its size because it
/// is cut to its exact pixel size: a new size each frame would cut it again
/// each frame, showing the placeholder meanwhile (`ImageStore::prepare`).
fn stretch_content(
    scene: &mut Scene,
    from: (usize, usize, usize),
    anchor: f32,
    scale: f32,
    clip: [f32; 4],
) {
    let grow = |r: [f32; 4]| [r[0], anchor + (r[1] - anchor) * scale, r[2], r[3] * scale];
    let shift = |r: [f32; 4]| {
        let mid = r[1] + r[3] / 2.0;
        [
            r[0],
            anchor + (mid - anchor) * scale - r[3] / 2.0,
            r[2],
            r[3],
        ]
    };
    let clipped = |c: [f32; 4]| intersect(grow(c), clip);
    let draws = &mut scene.draws[from.0..];
    for k in 0..draws.len() {
        let placeholder = match (&draws[k], draws.get(k + 1)) {
            (Draw::Quad(q), Some(Draw::Image(i))) => q.rect == i.rect,
            _ => false,
        };
        match &mut draws[k] {
            Draw::Quad(q) => {
                q.rect = if placeholder {
                    shift(q.rect)
                } else {
                    grow(q.rect)
                };
                q.clip = clipped(q.clip);
            }
            Draw::Image(i) => {
                i.rect = shift(i.rect);
                i.clip = clipped(i.clip);
            }
            Draw::Text(t) => {
                t.rect = shift(t.rect);
                t.clip = clipped(t.clip);
            }
            Draw::Icon(i) => {
                i.rect = shift(i.rect);
                i.clip = clipped(i.clip);
            }
        }
    }
    for n in &mut scene.semantics[from.1..] {
        n.bounds = intersect(grow(n.bounds), clip);
    }
    for f in &mut scene.fields[from.2..] {
        f.rect = shift(f.rect);
        f.clip = clipped(f.clip);
    }
}

/// A scrolling view's thumb: its width, its gap from the view's right edge
/// and ends, its shortest length (points), and how much of the page's text
/// colour it is drawn in.
const THUMB_WIDTH: f32 = 3.0;
const THUMB_INSET: f32 = 2.0;
const THUMB_MIN: f32 = 24.0;
const THUMB_ALPHA: f32 = 0.28;

/// The scroll indicator of a view whose content is taller than it: a thin
/// rounded bar along its right edge (`view`, its visible box), as long as
/// the share of the content showing (`height` of `content`, at least
/// `THUMB_MIN`), and as far down the track as the view is scrolled
/// (`offset`). Nothing when the content fits.
fn scroll_thumb(
    scene: &mut Scene,
    view: [f32; 4],
    height: f32,
    content: f32,
    offset: f32,
    ink: Rgba,
) {
    let max = content - height;
    let track = view[3] - 2.0 * THUMB_INSET;
    if max < 0.5 || track < THUMB_MIN || view[2] < THUMB_WIDTH + THUMB_INSET {
        return;
    }
    let len = (track * height / content).clamp(THUMB_MIN, track);
    let top = view[1] + THUMB_INSET + (track - len) * (offset / max).clamp(0.0, 1.0);
    let x = view[0] + view[2] - THUMB_INSET - THUMB_WIDTH;
    let mut q = QuadInstance::fill(
        [x, top, THUMB_WIDTH, len],
        ink.with_alpha(THUMB_ALPHA),
        THUMB_WIDTH / 2.0,
    );
    q.clip = view;
    scene.draws.push(Draw::Quad(q));
}

/// How far content fades in from a scrolling view's ends, in points.
const EDGE_FADE: f32 = 24.0;
/// Strips per fade: a gradient of quads, so every renderer draws it as it is.
const EDGE_FADE_STEPS: usize = 24;

/// Fades a scrolling view's top and bottom (`view`, its visible box) into
/// `backdrop`: content scrolled `above` points past its top fades there, and
/// `below` points still to come fades at its bottom. Each fade grows in over
/// its first `EDGE_FADE` points of scrolling, so it shows only when there is
/// content beyond that end.
fn edge_fades(scene: &mut Scene, view: [f32; 4], backdrop: Rgba, above: f32, below: f32) {
    let [x, y, w, h] = view;
    let len = EDGE_FADE.min(h / 4.0);
    if w <= 0.0 || len <= 0.0 {
        return;
    }
    let step = len / EDGE_FADE_STEPS as f32;
    for (scrolled, top) in [(above, true), (below, false)] {
        let strength = (scrolled / EDGE_FADE).clamp(0.0, 1.0);
        if strength <= 0.0 {
            continue;
        }
        for k in 0..EDGE_FADE_STEPS {
            // Opaque at the edge, easing out to nothing.
            let t = 1.0 - (k as f32 + 0.5) / EDGE_FADE_STEPS as f32;
            let a = strength * t * t * (3.0 - 2.0 * t);
            let sy = if top {
                y + k as f32 * step
            } else {
                y + h - (k as f32 + 1.0) * step
            };
            let mut c = backdrop;
            c.0[3] *= a;
            let mut q = QuadInstance::fill([x, sy, w, step], c, 0.0);
            q.clip = view;
            scene.draws.push(Draw::Quad(q));
        }
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

/// The `core://Tappable` a finger (or the mouse button) is down on, or the
/// mouse rests over (`hover`): its semantic id and the point, so a node
/// elsewhere with the same id (a repeated tag) is not drawn pressed.
#[derive(Debug, Clone, PartialEq)]
pub struct Pressed {
    pub id: String,
    pub at: (f32, f32),
    /// Only hovered (a mouse over it, no button down): a lighter wash.
    pub hover: bool,
}

/// How much of the text colour a pressed row is washed with: it darkens a
/// light page and lightens a dark one, as Android's ripple does.
const PRESSED_ALPHA: f32 = 0.10;
/// How much a hovered row is washed with: half a press.
const HOVER_ALPHA: f32 = 0.05;

/// The corner radii (top left, top right, bottom right, bottom left) of a
/// box inside a rounded box `outer` (radius `r`): each of its corners
/// within `r` of a corner of `outer` is rounded to follow it (concentric:
/// `r` less how far in it is), at least `own`.
fn shared_corners(rect: [f32; 4], outer: [f32; 4], r: f32, own: [f32; 4]) -> [f32; 4] {
    let left = rect[0] - outer[0];
    let right = outer[0] + outer[2] - (rect[0] + rect[2]);
    let top = rect[1] - outer[1];
    let bottom = outer[1] + outer[3] - (rect[1] + rect[3]);
    let max = rect[2].min(rect[3]) / 2.0;
    let corner = |dx: f32, dy: f32| {
        let inset = dx.max(dy);
        if dx < -0.5 || dy < -0.5 || inset >= r {
            0.0
        } else {
            (r - inset.max(0.0)).min(max)
        }
    };
    let follow = [
        corner(left, top),
        corner(right, top),
        corner(right, bottom),
        corner(left, bottom),
    ];
    std::array::from_fn(|k| own[k].max(follow[k]))
}

/// A dashed border's dashes and gaps (points), before they are stretched
/// to fit the box (`core://Box` `border_style: Dashed`).
const BORDER_DASH: f32 = 4.0;
const BORDER_GAP: f32 = 3.0;

/// How far the card behind a deck's top one (`core://List` `stack`) peeks
/// out below it, and how much narrower it is each side (points).
const STACK_PEEK: f32 = 10.0;
const STACK_INSET: f32 = 10.0;

/// A swipe card's threshold when the layout gives none (points).
pub const CARD_THRESHOLD: f32 = 96.0;
/// How much of its side's colour a swipe card is washed with at its
/// threshold, and past it (the release would commit).
const CARD_TINT: f32 = 0.10;
const CARD_TINT_PAST: f32 = 0.22;

/// A sticky node pinned at the top of its scrolling view: drawn after the
/// view's other content, over the background behind it.
struct Pinned {
    elem: usize,
    origin: (f32, f32),
    clip: [f32; 4],
    parent_sem: Option<usize>,
    /// The top of the view it is pinned in (the band above it is filled).
    view_top: f32,
    /// What fills that band: the background behind the node.
    backdrop: Rgba,
    /// How far it is moved down from where it lies in the content (how
    /// far the content has scrolled under it).
    shift: f32,
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
    /// The nearest opaque background around it (a page's `core://Box`
    /// `background`, or the theme's): what shows behind it.
    backdrop: Rgba,
    /// The nearest rounded box around it and its corner radius (a card
    /// grouping rows): a pressed row in its corner is rounded as it is.
    rounded: Option<([f32; 4], f32)>,
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
    /// How far each sideways-scrolling row can scroll (its content past its
    /// width).
    pub slides: BTreeMap<String, f32>,
    /// Each swipe card (by semantic id): its threshold and width (points).
    pub cards: BTreeMap<String, (f32, f32)>,
    /// The scrolling views that follow their content to the end after an
    /// action from outside them (`core://Scroll` `follow_end`).
    pub follow_end: Vec<String>,
    /// The inline layouts drawn with panels beside them (`panels: true`, on
    /// an expanded screen), by the semantic id of their content: a layout
    /// opened from inside one opens as a panel ([`Panels`]).
    pub panel_hosts: Vec<String>,
    /// The next minute (local, since the epoch) at which a `date`
    /// conditional of this frame changes its answer: the frame is stale
    /// from then on.
    pub changes_at: Option<i64>,
    env: Env,
    parts: HashMap<PartKey, Part>,
    /// How many list items were built and how many taken over.
    pub reuse: Reuse,
    /// Each list item's element and its entity (scroll anchors).
    items: HashMap<usize, String>,
}

// SAFETY: taffy's `CompactLength` holds a raw pointer only for CSS `calc()`
// values, which this crate never builds (every length is `length`,
// `percent` or `auto`). The tree owns everything else it holds, so moving a
// laid-out frame to another thread (the pod keeps previews behind a mutex)
// shares nothing.
unsafe impl Send for Laid {}

impl Laid {
    /// Where each inline layout is drawn in this frame: by its chain of
    /// embedded layouts (`layout:a/layout:b`), the paths of the embeds
    /// (`root/…/layout:a/…/layout:b`) whose content carries a semantic id.
    /// A view's state (scroll offsets, panels) is keyed by paths, so when
    /// the embedding layout changes around an embed this is how it follows.
    pub fn embeds(&self) -> BTreeMap<String, std::collections::BTreeSet<String>> {
        let mut out: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for s in self.elems.iter().filter_map(|e| e.sem.as_ref()) {
            for (chain, path) in embed_prefixes(&s.id) {
                out.entry(chain).or_default().insert(path);
            }
        }
        out
    }

    /// The list items in scrolling view `id`, in order: each one's entity,
    /// its top in the view's content and its height (points). Items of a
    /// view scrolling inside it are its own (decision
    /// `content-addressed-construct`, scroll anchors).
    pub fn scroll_items(&self, id: &str) -> Vec<(String, f32, f32)> {
        let Some(view) = self
            .elems
            .iter()
            .find(|e| e.scrolls && e.sem.as_ref().is_some_and(|s| s.id == id))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack: Vec<(usize, f32)> = view.kids.iter().rev().map(|k| (*k, 0.0)).collect();
        while let Some((i, top)) = stack.pop() {
            let e = &self.elems[i];
            let Ok(l) = self.tree.layout(e.node) else {
                continue;
            };
            let y = top + l.location.y;
            if let Some(uri) = self.items.get(&i) {
                out.push((uri.clone(), y, l.size.height));
                continue;
            }
            if e.scrolls || e.sticky {
                continue;
            }
            stack.extend(e.kids.iter().rev().map(|k| (*k, y)));
        }
        out
    }

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
        self.emit_into(&mut scene, scroll, (0.0, 0.0), NO_CLIP, None);
        scene
    }

    /// Places the frame into `scene` at `offset`, drawing nothing outside
    /// `clip` (a sheet or panel over another layout), with `pressed` drawn
    /// pressed.
    pub fn emit_into(
        &self,
        scene: &mut Scene,
        scroll: &BTreeMap<String, f32>,
        offset: (f32, f32),
        clip: [f32; 4],
        pressed: Option<&Pressed>,
    ) {
        let vp = self.viewport;
        let at = At {
            scroll_top: None,
            scrolled: 0.0,
            parent: [offset.0, offset.1, vp.width, vp.height],
            backdrop: self.theme.background,
            rounded: None,
        };
        let mut pinned = Vec::new();
        self.emit_at(
            self.root,
            offset,
            clip,
            None,
            at,
            scroll,
            pressed,
            &mut pinned,
            scene,
        );
    }

    /// The theme the frame was laid out with.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The top of the screen's content: below the window's inset there
    /// (the status bar).
    fn content_top(&self) -> f32 {
        self.tree
            .layout(self.elems[self.root].node)
            .map_or(0.0, |root| root.padding.top)
    }

    /// The page colour at each edge of the content (top, right, bottom,
    /// left), for the window's insets around it (the system bars): the
    /// opaque, square box drawn last (on top) that spans that whole edge,
    /// such as a page's `core://Box` with `fill` and `background`, or a
    /// coloured header across the top of it. Nothing inside a
    /// scrolling view, carousel, sideways row or swipe card counts (it
    /// moves). `None` where no box spans the edge: the theme's page shows.
    pub fn edge_colors(&self) -> [Option<Rgba>; 4] {
        let Ok(root) = self.tree.layout(self.elems[self.root].node) else {
            return [None; 4];
        };
        let p = root.padding;
        let content = [
            p.left,
            p.top,
            root.size.width - p.left - p.right,
            root.size.height - p.top - p.bottom,
        ];
        let mut out = [None; 4];
        let mut stack: Vec<(usize, (f32, f32))> = self.elems[self.root]
            .kids
            .iter()
            .rev()
            .map(|&k| (k, (0.0, 0.0)))
            .collect();
        // In drawing order (depth first, parents before children): a later
        // box is drawn over an earlier one.
        while let Some((i, origin)) = stack.pop() {
            let e = &self.elems[i];
            let Ok(l) = self.tree.layout(e.node) else {
                continue;
            };
            let r = [
                origin.0 + l.location.x,
                origin.1 + l.location.y,
                l.size.width,
                l.size.height,
            ];
            if let Paint::Quad { bg, radius, .. } = &e.paint
                && bg.0[3] >= 0.999
                && *radius < 0.5
            {
                const E: f32 = 0.5;
                let spans_w = r[0] <= content[0] + E && r[0] + r[2] >= content[0] + content[2] - E;
                let spans_h = r[1] <= content[1] + E && r[1] + r[3] >= content[1] + content[3] - E;
                let at_top = spans_w && r[1] <= content[1] + E;
                let at_bottom = spans_w && r[1] + r[3] >= content[1] + content[3] - E;
                let at_left = spans_h && r[0] <= content[0] + E;
                let at_right = spans_h && r[0] + r[2] >= content[0] + content[2] - E;
                for (k, at) in [at_top, at_right, at_bottom, at_left]
                    .into_iter()
                    .enumerate()
                {
                    if at {
                        out[k] = Some(*bg);
                    }
                }
            }
            let moves = e.scrolls || e.pager.is_some() || e.slides.is_some() || e.card.is_some();
            if !moves {
                stack.extend(e.kids.iter().rev().map(|&k| (k, (r[0], r[1]))));
            }
        }
        out
    }

    /// How tall the root's content is (a bar laid out on its own).
    pub fn used_height(&self) -> f32 {
        self.content_height(&self.elems[self.root])
    }

    /// How far a sideways-scrolling row's content reaches past its width.
    fn slide_extent(&self, e: &Elem) -> Option<f32> {
        let l = self.tree.layout(e.node).ok()?;
        let right = e
            .kids
            .iter()
            .filter_map(|k| self.tree.layout(self.elems[*k].node).ok())
            .map(|kl| kl.location.x + kl.size.width)
            .fold(0.0, f32::max);
        Some((right + l.padding.right - l.size.width).max(0.0))
    }

    fn content_height(&self, e: &Elem) -> f32 {
        e.kids
            .iter()
            .filter_map(|k| self.tree.layout(self.elems[*k].node).ok())
            .map(|kl| kl.location.y + kl.size.height)
            .fold(0.0, f32::max)
    }

    /// [`Self::emit_node`], with room on the stack however deep the tree is.
    #[allow(clippy::too_many_arguments)]
    fn emit_at(
        &self,
        i: usize,
        origin: (f32, f32),
        clip: [f32; 4],
        parent_sem: Option<usize>,
        at: At,
        scroll: &BTreeMap<String, f32>,
        pressed: Option<&Pressed>,
        pinned: &mut Vec<Pinned>,
        scene: &mut Scene,
    ) {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH, || {
            self.emit_node(
                i, origin, clip, parent_sem, at, scroll, pressed, pinned, scene,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_node(
        &self,
        i: usize,
        origin: (f32, f32),
        clip: [f32; 4],
        parent_sem: Option<usize>,
        at: At,
        scroll: &BTreeMap<String, f32>,
        pressed: Option<&Pressed>,
        pinned: &mut Vec<Pinned>,
        scene: &mut Scene,
    ) {
        let e = &self.elems[i];
        let l = self.tree.layout(e.node).expect("laid out");
        // A swipe card is drawn where the finger has moved it.
        let card_dx = e
            .card
            .as_ref()
            .and_then(|(id, _)| scroll.get(&format!("card:{id}")).copied())
            .unwrap_or(0.0);
        let x = origin.0 + l.location.x + card_dx;
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
                    backdrop: at.backdrop,
                    shift: to - y,
                });
                return;
            }
        }
        let rect = [x, y, w, h];
        let visible = intersects(rect, clip);
        if visible {
            self.paint(e, rect, l, clip, at.rounded, scene);
            if let (Some(f), Paint::Text { ty, .. }) = (&e.field, &e.paint) {
                scene.fields.push(crate::scene::FieldBox {
                    field: f.field.clone(),
                    rect,
                    clip,
                    face: ty.face,
                    size: ty.size,
                    line_height: ty.line_height,
                    max_length: f.max_length,
                    secret: f.secret,
                });
            }
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
                hint: s.hint.clone(),
            });
            sem_index = Some(scene.semantics.len() - 1);
        }
        let inner_clip = if e.clip { intersect(clip, rect) } else { clip };
        let mut oy = 0.0;
        let mut kid_at = At {
            scroll_top: at.scroll_top,
            scrolled: at.scrolled,
            parent: rect,
            backdrop: match &e.paint {
                Paint::Quad { bg, .. } if bg.0[3] >= 0.999 => *bg,
                _ => at.backdrop,
            },
            rounded: match &e.paint {
                Paint::Quad { radius, .. } | Paint::Image { radius, .. } if *radius > 0.0 => {
                    Some((rect, radius.min(w.min(h) / 2.0)))
                }
                _ => at.rounded,
            },
        };
        let mut own_pins = Vec::new();
        let scrolls = e.scrolls && e.sem.is_some();
        let mut stretch = 0.0;
        if scrolls && let Some(s) = &e.sem {
            let max = (self.content_height(e) - h).max(0.0);
            oy = scroll.get(&s.id).copied().unwrap_or(0.0).clamp(0.0, max);
            kid_at.scroll_top = Some(y);
            kid_at.scrolled = oy;
            stretch = scroll
                .get(&format!("stretch:{}", s.id))
                .copied()
                .unwrap_or(0.0);
        }
        let drawn = (scene.draws.len(), scene.semantics.len(), scene.fields.len());
        let ox = e
            .pager
            .as_ref()
            .and_then(|id| scroll.get(&format!("pager:{id}")).copied())
            .or_else(|| {
                let id = e.slides.as_ref()?;
                let max = self.slide_extent(e)?;
                Some(scroll.get(&format!("slide:{id}"))?.clamp(0.0, max))
            })
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
                pressed,
                pins,
                scene,
            );
        }
        if stretch != 0.0 {
            // Pulled past an end: the content stretches from that end.
            let anchor = if stretch < 0.0 { y } else { y + h };
            stretch_content(scene, drawn, anchor, 1.0 + stretch.abs(), inner_clip);
        }
        if scrolls {
            // Content passing under what covers the view's ends fades out
            // into what is behind it, as much as there is left to scroll
            // that way: at the top, under a header above the view (its top
            // short of the screen's) or a sticky node pinned there, starting
            // below that node; at the bottom always, over a bottom bar or
            // the phone's own gesture bar.
            let max = (self.content_height(e) - h).max(0.0);
            let shown = intersect(rect, inner_clip);
            let pinned_bottom = own_pins
                .iter()
                .filter_map(|p| {
                    let pl = self.tree.layout(self.elems[p.elem].node).ok()?;
                    Some((p.origin.1 + pl.location.y + pl.size.height, p.shift))
                })
                .max_by(|a, b| a.0.total_cmp(&b.0));
            let screen_top = self.content_top();
            let (from, above) = match pinned_bottom {
                Some((bottom, shift)) => (bottom, shift),
                None if shown[1] > screen_top + 0.5 => (shown[1], oy),
                None => (shown[1], 0.0),
            };
            let under = [shown[0], from, shown[2], shown[1] + shown[3] - from];
            edge_fades(scene, under, kid_at.backdrop, above, 0.0);
            edge_fades(scene, shown, kid_at.backdrop, 0.0, max - oy);
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
                let mut q = QuadInstance::fill(band, p.backdrop, 0.0);
                q.clip = p.clip;
                scene.draws.push(Draw::Quad(q));
            }
            let free = At {
                scroll_top: None,
                scrolled: 0.0,
                parent: rect,
                backdrop: p.backdrop,
                rounded: None,
            };
            let mut none = Vec::new();
            self.emit_at(
                p.elem,
                p.origin,
                p.clip,
                p.parent_sem,
                free,
                scroll,
                pressed,
                &mut none,
                scene,
            );
        }
        if scrolls && visible {
            // Over everything in the view, sticky nodes included.
            let content = self.content_height(e);
            scroll_thumb(
                scene,
                intersect(rect, inner_clip),
                h,
                content,
                oy,
                self.theme.on_background,
            );
        }
        if visible && let Some((_, threshold)) = &e.card {
            self.paint_card_tint(e, rect, clip, card_dx, *threshold, scene);
        }
        if visible && let Some(p) = pressed {
            self.paint_pressed(e, rect, clip, at.rounded, p, scene);
        }
    }

    /// A swipe card moved aside: a wash of the side it would go to (right:
    /// `success`, left: `error`), deepening up to the threshold and
    /// strongest past it, in its box (rounded as its one child is).
    fn paint_card_tint(
        &self,
        e: &Elem,
        rect: [f32; 4],
        clip: [f32; 4],
        dx: f32,
        threshold: f32,
        scene: &mut Scene,
    ) {
        if dx == 0.0 {
            return;
        }
        let token = if dx > 0.0 { "success" } else { "error" };
        let Some(color) = self.theme.token(token) else {
            return;
        };
        let f = (dx.abs() / threshold.max(1.0)).min(1.0);
        let alpha = if f >= 1.0 {
            CARD_TINT_PAST
        } else {
            CARD_TINT * f
        };
        let mut q = QuadInstance::fill(rect, color.with_alpha(alpha), self.one_child_radius(e));
        q.clip = clip;
        scene.draws.push(Draw::Quad(q));
    }

    /// The corner radius of an element's one child (a card's), else 0.
    fn one_child_radius(&self, e: &Elem) -> f32 {
        match e.kids.as_slice() {
            [k] => match &self.elems[*k].paint {
                Paint::Quad { radius, .. } | Paint::Image { radius, .. } => *radius,
                _ => 0.0,
            },
            _ => 0.0,
        }
    }

    /// A pressed `core://Tappable`: a wash over what it drew, in its box
    /// (rounded as its one child is: a card's corners; and, where a corner
    /// of it is a corner of the rounded box around it, as that box is: the
    /// first and last rows of a card of rows), inside its clip.
    fn paint_pressed(
        &self,
        e: &Elem,
        rect: [f32; 4],
        clip: [f32; 4],
        around: Option<([f32; 4], f32)>,
        p: &Pressed,
        scene: &mut Scene,
    ) {
        let Some(s) = &e.sem else { return };
        let [x, y, w, h] = rect;
        let inside = p.at.0 >= x && p.at.0 < x + w && p.at.1 >= y && p.at.1 < y + h;
        if s.id != p.id || s.node_type != "core://Tappable" || !inside {
            return;
        }
        let mut q = QuadInstance::fill(
            rect,
            self.theme
                .on_background
                .with_alpha(if p.hover { HOVER_ALPHA } else { PRESSED_ALPHA }),
            self.one_child_radius(e),
        );
        if let Some((b, r)) = around {
            q.radii = shared_corners(rect, b, r, q.radii);
        }
        q.clip = clip;
        scene.draws.push(Draw::Quad(q));
    }

    /// Draws what one element paints. `around` is the nearest rounded box
    /// around it: a filled box in one of its corners (a selected row's
    /// background in a card of rows) is rounded as that box is, so it
    /// doesn't poke out past the card's corners.
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        e: &Elem,
        rect: [f32; 4],
        l: &taffy::Layout,
        clip: [f32; 4],
        around: Option<([f32; 4], f32)>,
        scene: &mut Scene,
    ) {
        let [x, y, w, h] = rect;
        match &e.paint {
            Paint::None => {}
            Paint::Quad {
                bg,
                border,
                dashed,
                radius,
            } => {
                let r = radius.min(w.min(h) / 2.0);
                let mut q = QuadInstance::fill(rect, *bg, r);
                if let Some((b, br)) = around {
                    q.radii = shared_corners(rect, b, br, q.radii);
                }
                if let Some(b) = border {
                    q = q.border(*b, 1.0);
                    if *dashed {
                        q = q.dashed(BORDER_DASH, BORDER_GAP);
                    }
                }
                q.clip = clip;
                scene.draws.push(Draw::Quad(q));
            }
            Paint::Text {
                text,
                ty,
                color,
                spans,
            } => {
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
                    tracking: ty.tracking,
                    max_lines: ty.max_lines,
                    strike: ty.strike,
                    spans: spans.clone(),
                    color: *color,
                    rect: [px, py, pw, ph],
                    clip,
                }));
            }
            Paint::Icon { name, color, .. } if crate::icons::drawn(name) => {
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
                // `file://`: a staged photo (a media picker's thumbnail).
                if url.starts_with("http://")
                    || url.starts_with("https://")
                    || url.starts_with("file://")
                {
                    scene.draws.push(Draw::Image(ImageRun {
                        url: url.clone(),
                        rect,
                        radius: r,
                        clip,
                    }));
                }
            }
            Paint::Spinner { color } => {
                // An arc with a fading tail, turning with `scene.spin`: dots
                // close enough to overlap, as quads can't draw an arc.
                const DOTS: usize = 64;
                const ARC: f32 = 0.8; // of a turn
                let d = (w / 8.0).max(1.5);
                let r = (w - d) / 2.0;
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                let head = scene.spin * std::f32::consts::TAU;
                for i in 0..DOTS {
                    let t = i as f32 / (DOTS - 1) as f32;
                    let a = head - t * ARC * std::f32::consts::TAU;
                    let (px, py) = (cx + r * a.sin(), cy - r * a.cos());
                    let mut c = *color;
                    c.0[3] *= (1.0 - t).powi(3);
                    let mut q = QuadInstance::fill([px - d / 2.0, py - d / 2.0, d, d], c, d / 2.0);
                    q.clip = clip;
                    scene.draws.push(Draw::Quad(q));
                }
                let visible = x < clip[0] + clip[2]
                    && x + w > clip[0]
                    && y < clip[1] + clip[3]
                    && y + h > clip[1];
                scene.spinning |= visible;
            }
            Paint::Ring {
                frac,
                color,
                track,
                thick,
            } => {
                // The track is a bordered circle; the arc is dots close
                // enough to overlap, as for the spinner (quads can't draw an
                // arc), with a round cap at each end.
                let d = w.min(h);
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                let mut t = QuadInstance::fill(
                    [cx - d / 2.0, cy - d / 2.0, d, d],
                    Rgba::TRANSPARENT,
                    d / 2.0,
                );
                t = t.border(*track, *thick);
                t.clip = clip;
                scene.draws.push(Draw::Quad(t));
                if *frac > 0.0 {
                    let r = (d - thick) / 2.0;
                    let around = std::f32::consts::TAU * r * frac;
                    let dots = ((around / (thick / 3.0)).ceil() as usize).clamp(2, 720);
                    for i in 0..dots {
                        let a = (i as f32 / (dots - 1) as f32) * frac * std::f32::consts::TAU;
                        let (px, py) = (cx + r * a.sin(), cy - r * a.cos());
                        let mut q = QuadInstance::fill(
                            [px - thick / 2.0, py - thick / 2.0, *thick, *thick],
                            *color,
                            thick / 2.0,
                        );
                        q.clip = clip;
                        scene.draws.push(Draw::Quad(q));
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{ImageRun, NO_CLIP};

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let long = "é".repeat(MAX_TEXT_CHARS + 50);
        let cut = cap_text(long);
        assert_eq!(cut.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(cut.ends_with('…'));
        let short = "a".repeat(MAX_TEXT_CHARS);
        assert_eq!(cap_text(short.clone()), short);
    }

    fn quad(rect: [f32; 4]) -> Draw {
        Draw::Quad(QuadInstance::fill(rect, Rgba::rgb(128, 128, 128), 0.0))
    }

    fn rect(d: &Draw) -> [f32; 4] {
        match d {
            Draw::Quad(q) => q.rect,
            Draw::Image(i) => i.rect,
            Draw::Text(t) => t.rect,
            Draw::Icon(i) => i.rect,
        }
    }

    /// A stretch must not resize images: each new size would be cut again,
    /// and the placeholder would flash while it is.
    #[test]
    fn a_stretch_moves_images_and_their_placeholders_without_resizing_them() {
        let photo = [0.0, 100.0, 300.0, 200.0];
        let mut scene = Scene {
            draws: vec![
                quad([0.0, 0.0, 300.0, 400.0]), // a card
                quad(photo),                    // the photo's placeholder
                Draw::Image(ImageRun {
                    url: "https://img.example/a.jpg".into(),
                    rect: photo,
                    radius: 0.0,
                    clip: NO_CLIP,
                }),
            ],
            ..Scene::default()
        };
        stretch_content(&mut scene, (0, 0, 0), 0.0, 1.1, NO_CLIP);
        // The card grows from the top.
        assert_eq!(rect(&scene.draws[0]), [0.0, 0.0, 300.0, 440.0]);
        // The photo and its placeholder keep their size, centred where the
        // stretch takes their middle (200 → 220).
        for d in &scene.draws[1..] {
            let r = rect(d);
            assert_eq!((r[2], r[3]), (300.0, 200.0));
            assert!((r[1] + r[3] / 2.0 - 220.0).abs() < 1e-3, "{r:?}");
        }
    }
}
