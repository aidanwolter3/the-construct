//! What a laid-out layout draws: rounded SDF quads, text runs and the
//! semantic tree (ids, bounds, text, what a tap does) agents and tests read.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::style::{Face, Rgba, Type};

/// One instanced quad: a rounded rectangle with an optional border and drop
/// shadow, in logical points. The GPU vertex buffer holds these as they are
/// (`quad.wgsl`); the CPU rasterizer evaluates the same distance function.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Default)]
#[cfg_attr(feature = "gpu", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct QuadInstance {
    /// x, y, width, height.
    pub rect: [f32; 4],
    pub bg_color: [f32; 4],
    pub border_color: [f32; 4],
    /// Top-left, top-right, bottom-right, bottom-left.
    pub radii: [f32; 4],
    pub border_width: f32,
    pub shadow_offset: [f32; 2],
    /// Blur, spread.
    pub shadow_params: [f32; 2],
    pub shadow_color: [f32; 4],
    /// x, y, width, height: nothing outside it is drawn (scrolling views).
    pub clip: [f32; 4],
    /// Dash length and gap along the border; zero draws it solid.
    pub border_dash: [f32; 2],
}

impl QuadInstance {
    pub fn fill(rect: [f32; 4], color: Rgba, radius: f32) -> QuadInstance {
        QuadInstance {
            rect,
            bg_color: color.0,
            radii: [radius; 4],
            clip: NO_CLIP,
            ..Default::default()
        }
    }

    pub fn border(mut self, color: Rgba, width: f32) -> QuadInstance {
        self.border_color = color.0;
        self.border_width = width;
        self
    }

    /// The border in dashes of `dash` with gaps of `gap` (roughly: both
    /// are stretched so a whole number fits around the box).
    pub fn dashed(mut self, dash: f32, gap: f32) -> QuadInstance {
        self.border_dash = [dash, gap];
        self
    }

    pub fn shadow(mut self, color: Rgba, offset: [f32; 2], blur: f32, spread: f32) -> QuadInstance {
        self.shadow_color = color.0;
        self.shadow_offset = offset;
        self.shadow_params = [blur, spread];
        self
    }

    /// The area the quad can touch: its rect grown by its shadow.
    pub fn extent(&self) -> [f32; 4] {
        let [x, y, w, h] = self.rect;
        if self.shadow_color[3] <= 0.0 {
            return self.rect;
        }
        let m = self.shadow_params[0] * 1.5 + self.shadow_params[1].max(0.0);
        let [ox, oy] = self.shadow_offset;
        let x0 = x.min(x + ox - m);
        let y0 = y.min(y + oy - m);
        let x1 = (x + w).max(x + w + ox + m);
        let y1 = (y + h).max(y + h + oy + m);
        [x0, y0, x1 - x0, y1 - y0]
    }
}

/// A clip that clips nothing.
pub const NO_CLIP: [f32; 4] = [-1.0e6, -1.0e6, 2.0e6, 2.0e6];

/// A run of text placed in a box (logical points); renderers shape it at
/// the frame's scale and wrap it at `rect` width.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextRun {
    pub text: String,
    pub face: Face,
    pub size: f32,
    pub line_height: f32,
    /// Letter spacing in ems (a `core://Text` with `caps`).
    #[serde(skip_serializing_if = "is_zero")]
    pub tracking: f32,
    /// At most this many lines, the last ending in an ellipsis (0: no limit).
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub max_lines: u8,
    /// A line through each of its lines, in its colour (`core://Text`
    /// `strike`).
    #[serde(skip_serializing_if = "is_false")]
    pub strike: bool,
    /// Styled ranges of `text` (a `core://Text` `markdown`'s bold, code…).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<crate::text::RichSpan>,
    #[serde(skip)]
    pub color: Rgba,
    pub rect: [f32; 4],
    #[serde(skip)]
    pub clip: [f32; 4],
}

impl TextRun {
    /// How it is shaped.
    pub fn ty(&self) -> Type {
        Type {
            face: self.face,
            size: self.size,
            line_height: self.line_height,
            tracking: self.tracking,
            max_lines: self.max_lines,
            strike: self.strike,
        }
    }
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// A named icon (`crate::icons`) drawn in a square box, tinted.
#[derive(Debug, Clone, PartialEq)]
pub struct IconRun {
    pub name: String,
    pub rect: [f32; 4],
    pub color: Rgba,
    pub clip: [f32; 4],
}

/// An image from a URL, cropped to fill its box (as Compose's
/// `ContentScale.Crop`), with rounded corners. Drawn once it has loaded
/// ([`crate::images::ImageStore`]); a placeholder quad is drawn under it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRun {
    pub url: String,
    pub rect: [f32; 4],
    pub radius: f32,
    pub clip: [f32; 4],
}

/// One draw command, in paint order.
#[derive(Debug, Clone, PartialEq)]
pub enum Draw {
    Quad(QuadInstance),
    Text(TextRun),
    Icon(IconRun),
    Image(ImageRun),
}

/// What a tap on a node does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Tap {
    /// Sets the form value `field` (a `core://ChoiceChips` option).
    SetForm { field: String, value: String },
    /// Focuses a text field editing `field`.
    Focus { field: String },
    /// Dispatches an intent (bound to the item it was drawn for).
    Intent { intent: Value },
}

/// The node type of a surface drawn over content that stays laid out
/// under it (a screen inside its layout's sheet or panel): taps stop there.
pub const COVER: &str = "cover";

/// A node of the semantic tree: what is where, in logical points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticNode {
    pub id: String,
    pub node_type: String,
    /// x, y, width, height in logical points.
    pub bounds: [f32; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    pub interactive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap: Option<Tap>,
    /// Drawn selected or on (a chip option, a switch).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub selected: bool,
    /// Index of the parent in [`Scene::semantics`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// `core://Gesture` handlers besides the tap (`long_press`, `swipe_up`,
    /// `swipe_down`, `swipe_left`, `swipe_right`), bound intents.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub gestures: std::collections::BTreeMap<String, Value>,
    /// What a text field asks for (its placeholder): its name for screen
    /// readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// A laid-out frame: what to draw and what is where.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    /// Logical size.
    pub width: f32,
    pub height: f32,
    /// Device pixels per logical point.
    pub scale: f32,
    pub background: [f32; 4],
    /// What is drawn under the status bar and under the navigation bar
    /// (the window's insets): the top screen's page colour at those
    /// edges, dimmed by a scrim or covered by a sheet. Android picks the
    /// bars' icon colour from it (`Platform::system_bars`).
    pub bars: [[f32; 4]; 2],
    pub draws: Vec<Draw>,
    pub semantics: Vec<SemanticNode>,
    /// Where each visible text field's text is drawn, for editing it.
    pub fields: Vec<FieldBox>,
    /// How far spinners have turned, in turns (0 to 1): set before drawing.
    pub spin: f32,
    /// Whether a spinner was drawn (a window draws again next frame).
    pub spinning: bool,
}

/// A text field's text box: where its value is drawn and how.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldBox {
    pub field: String,
    pub rect: [f32; 4],
    pub clip: [f32; 4],
    pub face: Face,
    pub size: f32,
    pub line_height: f32,
    pub max_length: Option<usize>,
    /// Drawn as dots ([`crate::text::mask`]): carets and selections are
    /// measured on the dots, Copy and Cut are off.
    pub secret: bool,
}

impl Scene {
    pub fn quads(&self) -> impl Iterator<Item = &QuadInstance> {
        self.draws.iter().filter_map(|d| match d {
            Draw::Quad(q) => Some(q),
            _ => None,
        })
    }

    pub fn texts(&self) -> impl Iterator<Item = &TextRun> {
        self.draws.iter().filter_map(|d| match d {
            Draw::Text(t) => Some(t),
            _ => None,
        })
    }

    pub fn find_node(&self, id: &str) -> Option<&SemanticNode> {
        self.semantics.iter().find(|n| n.id == id)
    }

    /// The first node whose text is `text` (else one containing it).
    pub fn find_text(&self, text: &str) -> Option<&SemanticNode> {
        let has = |n: &&SemanticNode, exact: bool| {
            n.text_content
                .as_deref()
                .is_some_and(|t| if exact { t == text } else { t.contains(text) })
        };
        self.semantics
            .iter()
            .find(|n| has(n, true))
            .or_else(|| self.semantics.iter().find(|n| has(n, false)))
    }

    /// The innermost interactive node under a point, else the innermost
    /// node there. A [`COVER`] node hides what was drawn under it: nothing
    /// below one is hit.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<&SemanticNode> {
        let inside = |n: &&SemanticNode| {
            let [bx, by, bw, bh] = n.bounds;
            x >= bx && x < bx + bw && y >= by && y < by + bh
        };
        // Later nodes are drawn on top and nested deeper.
        let mut found = None;
        for n in self.semantics.iter().rev().filter(inside) {
            if n.interactive {
                return Some(n);
            }
            found.get_or_insert(n);
            if n.node_type == COVER {
                break;
            }
        }
        found
    }

    /// The innermost interactive node at or around `id`: the node itself,
    /// else its nearest interactive ancestor (tapping a label taps its
    /// button).
    pub fn interactive_for(&self, index: usize) -> Option<&SemanticNode> {
        let mut i = Some(index);
        while let Some(at) = i {
            let n = &self.semantics[at];
            if n.interactive {
                return Some(n);
            }
            i = n.parent;
        }
        None
    }
}
