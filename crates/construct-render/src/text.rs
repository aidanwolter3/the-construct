//! Text shaping and measurement with cosmic-text over the bundled fonts
//! only (no system fonts), so a layout measures and draws the same on every
//! machine: the pod, CI and each client.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight, fontdb};
use std::collections::{BTreeMap, HashMap};

use crate::style::{Face, Type};

const SANS: &[u8] = include_bytes!("../fonts/InstrumentSans-Regular.ttf");
const SANS_BOLD: &[u8] = include_bytes!("../fonts/InstrumentSans-Bold.ttf");
const SERIF: &[u8] = include_bytes!("../fonts/Lora-Regular.ttf");
/// Symbols the faces above lack (♥, ✓, ★, arrows, fractions such as ⅓):
/// cosmic-text falls back to any loaded font that has a glyph, so these fill
/// those gaps. The bold cut holds only fractions, superscripts and
/// subscripts, so a bold amount ("⅓ cup") keeps a bold fraction.
const SYMBOLS: &[u8] = include_bytes!("../fonts/DejaVuSans-Symbols.ttf");
const SYMBOLS_BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold-Fractions.ttf");
/// Emoji and symbols DejaVu lacks (⏰, 📅), drawn as monochrome outlines.
const EMOJI: &[u8] = include_bytes!("../fonts/NotoEmoji-Regular.ttf");

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

/// Fonts, shaping and a measurement cache. Measuring happens at the frame's
/// scale (device pixels), so line breaks match what is drawn.
pub struct TextEngine {
    fonts: FontSystem,
    sans: String,
    serif: String,
    cache: HashMap<(String, Face, u32, u32, u32), Measured>,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    pub fn new() -> TextEngine {
        let mut db = fontdb::Database::new();
        for data in [SANS, SANS_BOLD, SERIF] {
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
        for data in [SYMBOLS, SYMBOLS_BOLD, EMOJI] {
            db.load_font_data(data.to_vec());
        }
        TextEngine {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            sans,
            serif,
            cache: HashMap::new(),
        }
    }

    pub fn font_system(&mut self) -> &mut FontSystem {
        &mut self.fonts
    }

    fn attrs(&self, face: Face) -> Attrs<'_> {
        match face {
            Face::Sans => Attrs::new().family(Family::Name(&self.sans)),
            Face::SansBold => Attrs::new()
                .family(Family::Name(&self.sans))
                .weight(Weight::BOLD),
            Face::Serif => Attrs::new().family(Family::Name(&self.serif)),
        }
    }

    /// A shaped buffer for `text` at `scale` (device pixels per point),
    /// wrapping at `max_width` points when set.
    pub fn buffer(&mut self, text: &str, ty: Type, max_width: Option<f32>, scale: f32) -> Buffer {
        let metrics = Metrics::new(ty.size * scale, ty.line_height * scale);
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(max_width.map(|w| w * scale), None);
        let attrs = self.attrs(ty.face);
        let attrs = attrs.clone();
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
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
        let key = (
            text.to_owned(),
            ty.face,
            ty.size.to_bits(),
            max_width.map_or(u32::MAX, |w| w.max(0.0).floor() as u32),
            scale.to_bits(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{LABEL_LARGE, type_for};
    use protocol::TextStyle;

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
