//! Screen sizes: the simulator's presets, in points (dp).

use construct_render::Viewport;

#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub label: String,
    pub width: u32,
    pub height: u32,
    /// Pixels per point unless `--density` says otherwise.
    pub density: f32,
}

impl Screen {
    /// `phone` (390 × 844 @3), `tablet` (820 × 1180 @2), `desktop`
    /// (1280 × 800 @2), or `WxH` in points (@2).
    pub fn parse(spec: &str) -> Result<Screen, String> {
        let s = spec.trim().to_lowercase();
        let preset = |label: &str, width, height, density| Screen {
            label: label.to_owned(),
            width,
            height,
            density,
        };
        match s.as_str() {
            "phone" => return Ok(preset("phone", 390, 844, 3.0)),
            "tablet" => return Ok(preset("tablet", 820, 1180, 2.0)),
            "desktop" => return Ok(preset("desktop", 1280, 800, 2.0)),
            _ => {}
        }
        let bad = || {
            format!(
                "bad screen '{spec}': expected WxH in points (e.g. 390x844) or phone, tablet, desktop"
            )
        };
        let (w, h) = s.split_once('x').ok_or_else(bad)?;
        let (w, h): (u32, u32) = (
            w.trim().parse().map_err(|_| bad())?,
            h.trim().parse().map_err(|_| bad())?,
        );
        if !(1..=10_000).contains(&w) || !(1..=10_000).contains(&h) {
            return Err(format!(
                "bad screen '{spec}': width and height must be 1..10000"
            ));
        }
        Ok(preset(&format!("{w}x{h}"), w, h, 2.0))
    }

    pub fn viewport(&self, density: Option<f32>) -> Viewport {
        Viewport {
            width: self.width as f32,
            height: self.height as f32,
            scale: density.unwrap_or(self.density),
        }
    }
}
