//! Text shaping and measurement with cosmic-text over the bundled fonts
//! only (no system fonts), so a layout measures and draws the same on every
//! machine: the pod, CI and each client.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight, fontdb};
use std::collections::{BTreeMap, HashMap};

use crate::style::{Face, Type};

/// Each face at the four weights a `core://Text` `weight` names (400, 500,
/// 600, 700): static cuts of the families' variable fonts, so no weight is
/// synthesised (decision `text-weight-caps-lines`).
/// Their files in `fonts/`, in the same order: a web build fetches them by
/// name instead of carrying them (decision `web-client`).
pub const FACE_FILES: [&str; 8] = [
    "InstrumentSans-Regular.ttf",
    "InstrumentSans-Medium.ttf",
    "InstrumentSans-SemiBold.ttf",
    "InstrumentSans-Bold.ttf",
    "Lora-Regular.ttf",
    "Lora-Medium.ttf",
    "Lora-SemiBold.ttf",
    "Lora-Bold.ttf",
];
/// The fallbacks' files (below), which a web build loads after the first
/// frame.
pub const FALLBACK_FILES: [&str; 3] = [
    "DejaVuSans-Symbols.ttf",
    "DejaVuSans-Bold-Fractions.ttf",
    "NotoEmoji-Regular.ttf",
];
#[cfg(not(target_arch = "wasm32"))]
const FACES: [&[u8]; 8] = [
    include_bytes!("../fonts/InstrumentSans-Regular.ttf"),
    include_bytes!("../fonts/InstrumentSans-Medium.ttf"),
    include_bytes!("../fonts/InstrumentSans-SemiBold.ttf"),
    include_bytes!("../fonts/InstrumentSans-Bold.ttf"),
    include_bytes!("../fonts/Lora-Regular.ttf"),
    include_bytes!("../fonts/Lora-Medium.ttf"),
    include_bytes!("../fonts/Lora-SemiBold.ttf"),
    include_bytes!("../fonts/Lora-Bold.ttf"),
];
/// Symbols the faces above lack (♥, ✓, ★, arrows, fractions such as ⅓):
/// cosmic-text falls back to any loaded font that has a glyph, so these fill
/// those gaps. The bold cut holds only fractions, superscripts and
/// subscripts, so a bold amount ("⅓ cup") keeps a bold fraction.
#[cfg(not(target_arch = "wasm32"))]
const SYMBOLS: &[u8] = include_bytes!("../fonts/DejaVuSans-Symbols.ttf");
#[cfg(not(target_arch = "wasm32"))]
const SYMBOLS_BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold-Fractions.ttf");
/// Emoji and symbols DejaVu lacks (⏰, 📅), drawn as monochrome outlines.
#[cfg(not(target_arch = "wasm32"))]
const EMOJI: &[u8] = include_bytes!("../fonts/NotoEmoji-Regular.ttf");

/// Fonts added at run time, for every text engine: a web build's fetched
/// faces before its first frame, then its fallbacks (decision
/// `web-client`). An engine made earlier loads the ones it lacks before it
/// next shapes, and measures again.
static ADDED: std::sync::Mutex<Vec<std::sync::Arc<Vec<u8>>>> = std::sync::Mutex::new(Vec::new());

/// Adds a font (a file's bytes) to every text engine, made now or later.
pub fn add_font(bytes: Vec<u8>) {
    ADDED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(std::sync::Arc::new(bytes));
}

/// The fonts added from `from` on.
fn added_since(from: usize) -> Vec<std::sync::Arc<Vec<u8>>> {
    let added = ADDED.lock().unwrap_or_else(|p| p.into_inner());
    added.get(from..).map(<[_]>::to_vec).unwrap_or_default()
}

/// A measured text: its size in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub width: f32,
    pub height: f32,
}

/// A place a caret can stand: a byte offset in the text and where it is
/// drawn (points from the text's top left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretStop {
    pub byte: usize,
    pub x: f32,
    pub top: f32,
    pub height: f32,
}

/// The dot a secret text field draws for each character it holds.
pub const MASK: char = '\u{25CF}';

/// What a secret text field shows for `value`: one dot per character.
pub fn mask(value: &str) -> String {
    value.chars().map(|_| MASK).collect()
}

/// Caret stops measured on [`mask`]`(value)`, moved onto `value`'s bytes
/// (the k-th dot's boundary is the k-th character's).
pub fn unmask_stops(value: &str, stops: Vec<CaretStop>) -> Vec<CaretStop> {
    let mut starts: Vec<usize> = value.char_indices().map(|(i, _)| i).collect();
    starts.push(value.len());
    stops
        .into_iter()
        .map(|mut s| {
            let k = s.byte / MASK.len_utf8();
            s.byte = starts.get(k).copied().unwrap_or(value.len());
            s
        })
        .collect()
}

/// The stop nearest a point (points from the text's top left): on the line
/// the point is on (the first or last when outside), the closest x.
pub fn nearest_stop(stops: &[CaretStop], x: f32, y: f32) -> Option<CaretStop> {
    let line_top = stops.iter().map(|s| s.top).fold(None::<f32>, |best, top| {
        let s = stops.iter().find(|s| s.top == top).expect("from stops");
        let inside = y >= top && y < top + s.height;
        match best {
            _ if inside => Some(top),
            Some(b) => {
                let d = |t: f32| (y - (t + s.height / 2.0)).abs();
                Some(if d(top) < d(b) { top } else { b })
            }
            None => Some(top),
        }
    })?;
    stops
        .iter()
        .filter(|s| s.top == line_top)
        .min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
        .copied()
}

/// What a measurement depends on: the text, its face, size, wrap width,
/// scale, letter spacing and line limit.
type MeasureKey = (String, Face, u32, u32, u32, u32, u8);

/// Fonts, shaping and a measurement cache. Measuring happens at the frame's
/// scale (device pixels), so line breaks match what is drawn.
pub struct TextEngine {
    fonts: FontSystem,
    sans: String,
    serif: String,
    cache: HashMap<MeasureKey, Measured>,
    /// How many of the fonts added at run time ([`add_font`]) are loaded.
    added: usize,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    pub fn new() -> TextEngine {
        let mut db = fontdb::Database::new();
        #[cfg(not(target_arch = "wasm32"))]
        for data in FACES {
            db.load_font_data(data.to_vec());
        }
        // A web build's faces are the ones added before its first engine.
        let added = added_since(0);
        for data in &added {
            db.load_font_data(data.to_vec());
        }
        let family = |sans: bool| {
            db.faces()
                .find(|f| f.weight.0 == 400 && f.families[0].0.contains("Sans") == sans)
                .map(|f| f.families[0].0.clone())
                .unwrap_or_default()
        };
        let sans = family(true);
        let serif = family(false);
        // The fallbacks, loaded after the faces are named.
        #[cfg(not(target_arch = "wasm32"))]
        for data in [SYMBOLS, SYMBOLS_BOLD, EMOJI] {
            db.load_font_data(data.to_vec());
        }
        TextEngine {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            sans,
            serif,
            cache: HashMap::new(),
            added: added.len(),
        }
    }

    pub fn font_system(&mut self) -> &mut FontSystem {
        self.catch_up();
        &mut self.fonts
    }

    /// Loads the fonts added since this engine last looked ([`add_font`]);
    /// a measurement made without them is measured again.
    fn catch_up(&mut self) {
        let new = added_since(self.added);
        if new.is_empty() {
            return;
        }
        self.added += new.len();
        let db = self.fonts.db_mut();
        for data in &new {
            db.load_font_data(data.to_vec());
        }
        if self.sans.is_empty() || self.serif.is_empty() {
            let family = |sans: bool| {
                db.faces()
                    .find(|f| f.weight.0 == 400 && f.families[0].0.contains("Sans") == sans)
                    .map(|f| f.families[0].0.clone())
                    .unwrap_or_default()
            };
            self.sans = family(true);
            self.serif = family(false);
        }
        self.cache.clear();
    }

    fn attrs(&self, ty: Type) -> Attrs<'_> {
        let family = if ty.face.serif() {
            &self.serif
        } else {
            &self.sans
        };
        let attrs = Attrs::new()
            .family(Family::Name(family))
            .weight(Weight(ty.face.weight()));
        if ty.tracking == 0.0 {
            attrs
        } else {
            attrs.letter_spacing(ty.tracking)
        }
    }

    /// `text` shaped as it is: every line, wrapping at `max_width` points.
    fn shape(&mut self, text: &str, ty: Type, max_width: Option<f32>, scale: f32) -> Buffer {
        self.catch_up();
        let metrics = Metrics::new(ty.size * scale, ty.line_height * scale);
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(max_width.map(|w| w * scale), None);
        let attrs = self.attrs(ty);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }

    /// A shaped buffer for `text` at `scale` (device pixels per point),
    /// wrapping at `max_width` points when set. With `ty.max_lines`, a text
    /// that takes more lines is cut so that it ends, in an ellipsis, on the
    /// last of them ([`TextEngine::fitted`]).
    pub fn buffer(&mut self, text: &str, ty: Type, max_width: Option<f32>, scale: f32) -> Buffer {
        let buffer = self.shape(text, ty, max_width, scale);
        let lines = usize::from(ty.max_lines);
        if lines == 0 || buffer.layout_runs().count() <= lines {
            return buffer;
        }
        let fitted = self.fitted(text, ty, max_width, scale, &buffer);
        self.shape(&fitted, ty, max_width, scale)
    }

    /// `text` cut to `ty.max_lines` lines (it takes more in `buffer`): the
    /// longest start of it that, followed by an ellipsis, still fits.
    fn fitted(
        &mut self,
        text: &str,
        ty: Type,
        max_width: Option<f32>,
        scale: f32,
        buffer: &Buffer,
    ) -> String {
        let lines = usize::from(ty.max_lines);
        // Where the last line that may be kept ends: no longer start fits.
        let mut para = vec![0usize];
        para.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let end = buffer
            .layout_runs()
            .take(lines)
            .map(|run| {
                let base = para.get(run.line_i).copied().unwrap_or(0);
                base + run.glyphs.iter().map(|g| g.end).max().unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
            .min(text.len());
        let cuts: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .filter(|i| *i <= end)
            .chain(text.is_char_boundary(end).then_some(end))
            .collect();
        let with_ellipsis = |n: usize| format!("{}\u{2026}", text[..n].trim_end());
        // The longest cut that fits, by bisection (a longer start never
        // takes fewer lines).
        let (mut lo, mut hi) = (0usize, cuts.len());
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            let shaped = self.shape(&with_ellipsis(cuts[mid]), ty, max_width, scale);
            if shaped.layout_runs().count() <= lines {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        with_ellipsis(cuts.get(lo).copied().unwrap_or(0))
    }

    /// Where a caret can stand in `text` laid out as it is drawn (wrapped
    /// at `max_width` points): one stop per character boundary, in byte
    /// order, with its x, its line's top and height (points, from the text's
    /// top left). A boundary where a line wraps stands at the start of the
    /// next line, as editors show it.
    pub fn caret_stops(
        &mut self,
        text: &str,
        ty: Type,
        max_width: Option<f32>,
        scale: f32,
    ) -> Vec<CaretStop> {
        let buffer = self.buffer(text, ty, max_width, scale);
        // Byte offset of each paragraph (`\n`-separated line) in `text`.
        let mut para = vec![0usize];
        for (i, c) in text.char_indices() {
            if c == '\n' {
                para.push(i + 1);
            }
        }
        let mut at: BTreeMap<usize, CaretStop> = BTreeMap::new();
        let put = |at: &mut BTreeMap<usize, CaretStop>,
                   byte: usize,
                   x: f32,
                   top: f32,
                   h: f32,
                   start_of_line: bool| {
            let stop = CaretStop {
                byte,
                x: x / scale,
                top: top / scale,
                height: h / scale,
            };
            // A line's start wins over the previous line's end.
            if start_of_line || !at.contains_key(&byte) {
                at.insert(byte, stop);
            }
        };
        let mut last_line = (0.0, ty.line_height * scale);
        for run in buffer.layout_runs() {
            let base = para.get(run.line_i).copied().unwrap_or(0);
            last_line = (run.line_top, run.line_height);
            let mut first = true;
            for g in run.glyphs {
                put(
                    &mut at,
                    base + g.start,
                    g.x,
                    run.line_top,
                    run.line_height,
                    first,
                );
                first = false;
                put(
                    &mut at,
                    base + g.end,
                    g.x + g.w,
                    run.line_top,
                    run.line_height,
                    false,
                );
            }
            if run.glyphs.is_empty() {
                put(&mut at, base, 0.0, run.line_top, run.line_height, true);
            }
        }
        if at.is_empty() {
            put(&mut at, 0, 0.0, last_line.0, last_line.1, true);
        }
        // Every character boundary, standing where the nearest laid-out one
        // before it does (inside a ligature or cluster).
        let mut out = Vec::new();
        let mut prev = *at.values().next().expect("one stop at least");
        let bounds = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()));
        for b in bounds {
            if let Some(s) = at.get(&b) {
                prev = *s;
            }
            out.push(CaretStop { byte: b, ..prev });
        }
        out
    }

    /// The size `text` takes, wrapped at `max_width` points (none: one line
    /// per paragraph). Widths are rounded up to whole points so a box laid
    /// out at the measured width wraps the same way when drawn.
    pub fn measure(
        &mut self,
        text: &str,
        ty: Type,
        max_width: Option<f32>,
        scale: f32,
    ) -> Measured {
        self.catch_up();
        let key = (
            text.to_owned(),
            ty.face,
            ty.size.to_bits(),
            max_width.map_or(u32::MAX, |w| w.max(0.0).floor() as u32),
            scale.to_bits(),
            ty.tracking.to_bits(),
            ty.max_lines,
        );
        if let Some(m) = self.cache.get(&key) {
            return *m;
        }
        let buffer = self.buffer(text, ty, max_width.map(|w| w.max(0.0).floor()), scale);
        let mut width: f32 = 0.0;
        let mut lines = 0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        let m = Measured {
            width: (width / scale).ceil(),
            height: lines.max(1) as f32 * ty.line_height,
        };
        self.cache.insert(key, m);
        m
    }
}

/// How high a `core://Text` `strike` line sits above each line's baseline,
/// in ems: through the middle of its lowercase letters.
pub const STRIKE_RISE: f32 = 0.3;

/// The lines a `core://Text` with `strike` draws through `buffer` (shaped
/// at `scale` for `ty`): one per laid-out line that has glyphs, from its
/// first glyph's left to its last glyph's right, 1 dp thick and
/// [`STRIKE_RISE`] above its baseline. Device pixels, whole, from the
/// buffer's origin (x, y, width, height), so the CPU and the GPU renderer
/// draw the same pixels.
pub fn strike_lines(buffer: &Buffer, ty: Type, scale: f32) -> Vec<[i32; 4]> {
    let thick = scale.round().max(1.0) as i32;
    buffer
        .layout_runs()
        .filter_map(|run| {
            let left = run.glyphs.iter().map(|g| g.x).reduce(f32::min)?;
            let right = run.glyphs.iter().map(|g| g.x + g.w).reduce(f32::max)?;
            let x = left.round() as i32;
            let w = (right.round() as i32 - x).max(0);
            let mid = run.line_y.round() - STRIKE_RISE * ty.size * scale;
            let y = (mid - thick as f32 / 2.0).round() as i32;
            (w > 0).then_some([x, y, w, thick])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{LABEL_LARGE, type_for};
    use protocol::TextStyle;

    #[test]
    fn a_font_added_later_is_loaded_and_measured_again() {
        // A copy of a loaded face (so other tests, which share the added
        // fonts, measure the same): the engine loads it on its next
        // measure and drops what it measured before.
        let mut t = TextEngine::new();
        let ty = crate::style::type_for(protocol::TextStyle::Body);
        let _ = t.measure("before", ty, None, 1.0);
        let before = t.added;
        add_font(include_bytes!("../fonts/Lora-Regular.ttf").to_vec());
        let _ = t.measure("after", ty, None, 1.0);
        assert!(t.added > before);
        assert_eq!(t.cache.len(), 1, "measured again after the font came");
    }

    #[test]
    fn longer_text_is_wider_and_wraps() {
        let mut t = TextEngine::new();
        let body = type_for(TextStyle::Body);
        let short = t.measure("Save", body, None, 2.0);
        let long = t.measure("Save everything now", body, None, 2.0);
        assert!(short.width > 10.0 && long.width > short.width * 2.0);
        assert_eq!(short.height, 20.0);
        let wrapped = t.measure("Save everything now", body, Some(long.width / 2.0), 2.0);
        assert!(wrapped.height >= 40.0, "{wrapped:?}");
        assert!(wrapped.width <= long.width / 2.0 + 1.0);
    }

    #[test]
    fn symbols_fall_back_to_a_font_that_has_them() {
        let mut t = TextEngine::new();
        let texts = ["♥ 3", "✓", "★ ☆", "→ ›", "¼ ½ ¾ ⅓ ⅔ ⅛ ⅜ ⅝ ⅞ ⅕ ⅙", "⏰ 📅"];
        for (text, ty) in texts
            .iter()
            .flat_map(|t| [(t, type_for(TextStyle::Body)), (t, LABEL_LARGE)])
        {
            let buffer = t.buffer(text, ty, None, 2.0);
            let glyphs: Vec<u16> = buffer
                .layout_runs()
                .flat_map(|r| r.glyphs.iter().map(|g| g.glyph_id))
                .collect();
            // Glyph 0 is .notdef: the box drawn for a missing character.
            assert!(
                !glyphs.is_empty() && glyphs.iter().all(|g| *g != 0),
                "{text}: {glyphs:?}"
            );
        }
    }

    #[test]
    fn bold_text_keeps_bold_fractions() {
        let mut t = TextEngine::new();
        for text in ["⅓ cup", "½ cup"] {
            let buffer = t.buffer(text, LABEL_LARGE, None, 2.0);
            let fraction = buffer.layout_runs().next().expect("a line").glyphs[0].font_id;
            let face = t.font_system().db().face(fraction).expect("a face");
            assert_eq!(face.weight.0, 700, "{text}: {}", face.families[0].0);
        }
    }

    #[test]
    fn bold_is_wider_than_regular() {
        let mut t = TextEngine::new();
        let regular = t.measure("Cancel", type_for(TextStyle::Body), None, 1.0);
        let bold = t.measure("Cancel", LABEL_LARGE, None, 1.0);
        assert!(bold.width >= regular.width);
    }
}

#[cfg(test)]
mod mask_tests {
    use super::*;

    #[test]
    fn dots_stand_for_characters_and_map_back_to_bytes() {
        let value = "aé€x";
        assert_eq!(mask(value), "\u{25CF}".repeat(4));
        let stop = |byte| CaretStop {
            byte,
            x: 0.0,
            top: 0.0,
            height: 0.0,
        };
        let shown: Vec<CaretStop> = (0..=4).map(|k| stop(k * MASK.len_utf8())).collect();
        let bytes: Vec<usize> = unmask_stops(value, shown).iter().map(|s| s.byte).collect();
        assert_eq!(bytes, [0, 1, 3, 6, 7]);
    }
}
