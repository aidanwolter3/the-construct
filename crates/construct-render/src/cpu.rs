//! The CPU rasterizer: the same rounded-box distance function as
//! `gpu/quad.wgsl`, evaluated per pixel, and glyphs from cosmic-text's swash
//! cache. Always built: the pod draws MCP previews with it (no GPU there),
//! and it is the reference the GPU frames are compared with.

use cosmic_text::{SwashCache, SwashContent};
use image::RgbaImage;

use crate::scene::{Draw, IconRun, QuadInstance, Scene, TextRun};
use crate::style::Type;
use crate::text::TextEngine;
use std::collections::HashMap;

/// Signed distance from `p` (relative to the box centre) to a rounded box of
/// half size `b`; radii are top-left, top-right, bottom-right, bottom-left
/// (y grows down).
pub fn sd_rounded_box(p: [f32; 2], b: [f32; 2], radii: [f32; 4]) -> f32 {
    let r = match (p[0] >= 0.0, p[1] >= 0.0) {
        (false, false) => radii[0],
        (true, false) => radii[1],
        (true, true) => radii[2],
        (false, true) => radii[3],
    };
    let r = r.min(b[0]).min(b[1]).max(0.0);
    let qx = p[0].abs() - b[0] + r;
    let qy = p[1].abs() - b[1] + r;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    qx.max(qy).min(0.0) + outside - r
}

/// Coverage of a pixel at signed distance `d` (pixels): a one-pixel ramp.
fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The premultiplied colour one quad gives the pixel centred at `p`
/// (physical pixels), shadow included: what the fragment shader computes.
pub fn shade(q: &QuadInstance, p: [f32; 2], scale: f32) -> [f32; 4] {
    let [x, y, w, h] = q.rect.map(|v| v * scale);
    let half = [w / 2.0, h / 2.0];
    let radii = q.radii.map(|r| r * scale);
    let local = [p[0] - (x + half[0]), p[1] - (y + half[1])];
    let d = sd_rounded_box(local, half, radii);
    let outer = coverage(d);
    let bw = q.border_width * scale;
    let inner = if bw > 0.0 { coverage(d + bw) } else { outer };
    let bg = q.bg_color;
    let bc = q.border_color;
    let mut out = [0.0; 4];
    for c in 0..3 {
        out[c] = bg[c] * bg[3] * inner + bc[c] * bc[3] * (outer - inner);
    }
    out[3] = bg[3] * inner + bc[3] * (outer - inner);
    let sc = q.shadow_color;
    if sc[3] > 0.0 {
        let blur = (q.shadow_params[0] * scale).max(0.5);
        let spread = q.shadow_params[1] * scale;
        let off = [q.shadow_offset[0] * scale, q.shadow_offset[1] * scale];
        let sp = [local[0] - off[0], local[1] - off[1]];
        let sh = [half[0] + spread, half[1] + spread];
        let sr = radii.map(|r| r + spread.max(0.0));
        let ds = sd_rounded_box(sp, sh, sr);
        // A box shadow shows only outside the box (as CSS draws it).
        let a = sc[3] * (1.0 - smoothstep(-blur, blur, ds)) * (1.0 - outer);
        for c in 0..3 {
            out[c] += sc[c] * a * (1.0 - out[3]);
        }
        out[3] += a * (1.0 - out[3]);
    }
    out
}

fn to_u8(c: [f32; 4]) -> [u8; 4] {
    // `as u8` saturates.
    c.map(|v| (v * 255.0 + 0.5) as u8)
}

/// `c` (premultiplied, 0..=1) over the pixel `d` (premultiplied 8-bit).
fn over(d: &mut [u8; 4], c: [f32; 4]) {
    let keep = 1.0 - c[3];
    for k in 0..4 {
        d[k] = ((c[k] + d[k] as f32 / 255.0 * keep) * 255.0 + 0.5) as u8;
    }
}

/// Draws scenes to RGBA images on the CPU.
pub struct CpuRenderer {
    swash: SwashCache,
    /// Icon masks by name and pixel size.
    icons: HashMap<(String, u32), Vec<u8>>,
    /// Where images come from; without one, image runs draw nothing.
    pub images: Option<crate::images::ImageStore>,
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuRenderer {
    pub fn new() -> CpuRenderer {
        CpuRenderer {
            swash: SwashCache::new(),
            icons: HashMap::new(),
            images: None,
        }
    }

    /// The frame at the scene's pixel size.
    pub fn render(&mut self, scene: &Scene, text: &mut TextEngine) -> RgbaImage {
        let s = scene.scale;
        let w = (scene.width * s).round().max(1.0) as usize;
        let h = (scene.height * s).round().max(1.0) as usize;
        let bg = scene.background;
        // 8-bit premultiplied, as the GPU's render target: only touched
        // pixels cost anything.
        let mut px = vec![to_u8([bg[0] * bg[3], bg[1] * bg[3], bg[2] * bg[3], bg[3]]); w * h];
        for d in &scene.draws {
            match d {
                Draw::Quad(q) => self.quad(&mut px, w, h, q, s),
                Draw::Text(t) => self.text(&mut px, w, h, t, s, text),
                Draw::Icon(i) => self.icon(&mut px, w, h, i, s),
                Draw::Image(i) => self.image(&mut px, w, h, i, s),
            }
        }
        // Unpremultiplied: opaque pixels (nearly all) are copied as they are.
        for p in px.iter_mut() {
            let a = p[3];
            if a != 255 && a != 0 {
                let k = 255.0 / a as f32;
                for c in p.iter_mut().take(3) {
                    *c = (*c as f32 * k + 0.5) as u8;
                }
            }
        }
        RgbaImage::from_raw(w as u32, h as u32, px.concat()).expect("sized to the frame")
    }

    /// An icon: its mask at the box's pixel size, tinted.
    fn icon(&mut self, px: &mut [[u8; 4]], w: usize, h: usize, i: &IconRun, s: f32) {
        let size = (i.rect[2] * s).round().max(1.0) as u32;
        let Some(mask) = self
            .icons
            .entry((i.name.clone(), size))
            .or_insert_with(|| crate::icons::mask(&i.name, size).unwrap_or_default())
            .get(..size as usize * size as usize)
        else {
            return;
        };
        let ox = (i.rect[0] * s).round() as i32;
        let oy = (i.rect[1] * s).round() as i32;
        let [cx, cy, cw, ch] = i.clip.map(|v| v * s);
        let c = i.color.0;
        for row in 0..size as i32 {
            for col in 0..size as i32 {
                let (x, y) = (ox + col, oy + row);
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    continue;
                }
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                if fx < cx || fy < cy || fx > cx + cw || fy > cy + ch {
                    continue;
                }
                let a = mask[(row as u32 * size + col as u32) as usize] as f32 / 255.0 * c[3];
                if a > 0.0 {
                    over(
                        &mut px[y as usize * w + x as usize],
                        [c[0] * a, c[1] * a, c[2] * a, a],
                    );
                }
            }
        }
    }

    /// A loaded image, cut to its box (cropped to fill, corners rounded).
    fn image(
        &mut self,
        px: &mut [[u8; 4]],
        w: usize,
        h: usize,
        i: &crate::scene::ImageRun,
        s: f32,
    ) {
        let Some(store) = &self.images else {
            return;
        };
        let (x0, y0) = (
            (i.rect[0] * s).round() as i32,
            (i.rect[1] * s).round() as i32,
        );
        let (iw, ih) = (
            (i.rect[2] * s).round().max(1.0) as u32,
            (i.rect[3] * s).round().max(1.0) as u32,
        );
        let Some(img) = store.cut(&i.url, iw, ih, i.radius * s) else {
            return;
        };
        let [cx, cy, cw, ch] = i.clip.map(|v| v * s);
        for (col, row, p) in img.enumerate_pixels() {
            let (x, y) = (x0 + col as i32, y0 + row as i32);
            if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                continue;
            }
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if fx < cx || fy < cy || fx > cx + cw || fy > cy + ch {
                continue;
            }
            let a = p.0[3] as f32 / 255.0;
            if a > 0.0 {
                let c = p.0.map(|v| v as f32 / 255.0);
                over(
                    &mut px[y as usize * w + x as usize],
                    [c[0] * a, c[1] * a, c[2] * a, a],
                );
            }
        }
    }

    fn quad(&self, px: &mut [[u8; 4]], w: usize, h: usize, q: &QuadInstance, s: f32) {
        let [ex, ey, ew, eh] = q.extent();
        let [cx, cy, cw, ch] = q.clip;
        let x0 = ((ex.max(cx) * s).floor().max(0.0)) as usize;
        let y0 = ((ey.max(cy) * s).floor().max(0.0)) as usize;
        let x1 = (((ex + ew).min(cx + cw) * s).ceil().max(0.0) as usize).min(w);
        let y1 = (((ey + eh).min(cy + ch) * s).ceil().max(0.0) as usize).min(h);
        // Pixels well inside the straight edges are the plain fill: no
        // distance to evaluate (most of a card's pixels).
        let [rx, ry, rw, rh] = q.rect.map(|v| v * s);
        let inset = q.border_width * s + 1.0;
        let r = q.radii.iter().fold(0.0f32, |m, v| m.max(*v)) * s;
        let bg = q.bg_color;
        let fill = [bg[0] * bg[3], bg[1] * bg[3], bg[2] * bg[3], bg[3]];
        let solid = |fx: f32, fy: f32| {
            let in_x = fx > rx + inset && fx < rx + rw - inset;
            let in_y = fy > ry + inset && fy < ry + rh - inset;
            (in_x && in_y)
                && ((fy > ry + r + inset && fy < ry + rh - r - inset)
                    || (fx > rx + r + inset && fx < rx + rw - r - inset))
        };
        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                let c = if solid(fx, fy) {
                    fill
                } else {
                    shade(q, [fx, fy], s)
                };
                if c[3] <= 0.0 {
                    continue;
                }
                over(&mut px[y * w + x], c);
            }
        }
    }

    fn text(
        &mut self,
        px: &mut [[u8; 4]],
        w: usize,
        h: usize,
        t: &TextRun,
        s: f32,
        engine: &mut TextEngine,
    ) {
        let ty = Type {
            face: t.face,
            size: t.size,
            line_height: t.line_height,
        };
        let buffer = engine.buffer(&t.text, ty, Some(t.rect[2] + 0.5), s);
        let ox = (t.rect[0] * s).round();
        let oy = (t.rect[1] * s).round();
        let [cx, cy, cw, ch] = t.clip.map(|v| v * s);
        let color = t.color.0;
        let mut glyphs = Vec::new();
        // Placed as glyphon places them (the baseline rounded on its own),
        // so CPU and GPU frames agree to the pixel.
        for run in buffer.layout_runs() {
            for g in run.glyphs {
                glyphs.push((g.physical((ox, oy), 1.0), run.line_y.round() as i32));
            }
        }
        for (g, line_y) in glyphs {
            let Some(img) = self
                .swash
                .get_image(engine.font_system(), g.cache_key)
                .as_ref()
            else {
                continue;
            };
            let p = img.placement;
            let gx = g.x + p.left;
            let gy = line_y + g.y - p.top;
            for row in 0..p.height as i32 {
                for col in 0..p.width as i32 {
                    let x = gx + col;
                    let y = gy + row;
                    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                        continue;
                    }
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    if fx < cx || fy < cy || fx > cx + cw || fy > cy + ch {
                        continue;
                    }
                    let i = (row * p.width as i32 + col) as usize;
                    let (src, a) = match img.content {
                        SwashContent::Mask => {
                            let a = img.data[i] as f32 / 255.0 * color[3];
                            ([color[0] * a, color[1] * a, color[2] * a], a)
                        }
                        SwashContent::SubpixelMask => {
                            let a = img.data[i * 4 + 1] as f32 / 255.0 * color[3];
                            ([color[0] * a, color[1] * a, color[2] * a], a)
                        }
                        SwashContent::Color => {
                            let c = &img.data[i * 4..i * 4 + 4];
                            let a = c[3] as f32 / 255.0;
                            (
                                [
                                    c[0] as f32 / 255.0 * a,
                                    c[1] as f32 / 255.0 * a,
                                    c[2] as f32 / 255.0 * a,
                                ],
                                a,
                            )
                        }
                    };
                    if a <= 0.0 {
                        continue;
                    }
                    over(
                        &mut px[y as usize * w + x as usize],
                        [src[0], src[1], src[2], a],
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_zero_on_the_edge_and_negative_inside() {
        let b = [50.0, 20.0];
        assert!((sd_rounded_box([50.0, 0.0], b, [0.0; 4])).abs() < 1e-4);
        assert!(sd_rounded_box([0.0, 0.0], b, [0.0; 4]) < 0.0);
        assert!(sd_rounded_box([60.0, 0.0], b, [0.0; 4]) > 9.9);
        // A rounded corner is cut: the box corner itself is outside.
        assert!(sd_rounded_box([50.0, 20.0], b, [10.0; 4]) > 0.0);
        assert!(sd_rounded_box([50.0, 20.0], b, [0.0; 4]).abs() < 1e-4);
    }

    #[test]
    fn border_paints_the_edge_and_fill_the_middle() {
        let q = QuadInstance::fill(
            [0.0, 0.0, 20.0, 20.0],
            crate::style::Rgba([1.0, 0.0, 0.0, 1.0]),
            0.0,
        )
        .border(crate::style::Rgba([0.0, 0.0, 1.0, 1.0]), 2.0);
        let middle = shade(&q, [10.0, 10.0], 1.0);
        let edge = shade(&q, [0.5, 10.0], 1.0);
        assert_eq!(middle, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(edge, [0.0, 0.0, 1.0, 1.0]);
    }
}
