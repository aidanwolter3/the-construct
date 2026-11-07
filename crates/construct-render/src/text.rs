//! Text shaping and measurement with cosmic-text over the bundled fonts
//! only (no system fonts), so a layout measures and draws the same on every
//! machine: the pod, CI and each client.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight, fontdb};
use std::collections::HashMap;

use crate::style::{Face, Type};

const SANS: &[u8] = include_bytes!("../fonts/InstrumentSans-Regular.ttf");
const SANS_BOLD: &[u8] = include_bytes!("../fonts/InstrumentSans-Bold.ttf");
const SERIF: &[u8] = include_bytes!("../fonts/Lora-Regular.ttf");

/// A measured text: its size in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub width: f32,
    pub height: f32,
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
        let family = |weight: u16, sans: bool| {
            db.faces()
                .find(|f| f.weight.0 == weight && (f.families[0].0.contains("Sans") == sans))
                .map(|f| f.families[0].0.clone())
                .unwrap_or_default()
        };
        let sans = family(400, true);
        let serif = family(400, false);
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
    fn bold_is_wider_than_regular() {
        let mut t = TextEngine::new();
        let regular = t.measure("Cancel", type_for(TextStyle::Body), None, 1.0);
        let bold = t.measure("Cancel", LABEL_LARGE, None, 1.0);
        assert!(bold.width >= regular.width);
    }
}
