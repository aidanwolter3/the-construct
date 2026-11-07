//! Images by URL for [`crate::scene::ImageRun`]s: downloaded and decoded by
//! whoever sets a fetcher (`crate::remote` on devices), kept here, and cut to
//! the box they are drawn in (cropped to fill it, corners rounded) for both
//! rasterizers. Without a fetcher (the pod's previews) nothing loads and
//! the placeholders stay.

use image::RgbaImage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type Hook = Arc<dyn Fn(String) + Send + Sync>;
type Waker = Arc<dyn Fn() + Send + Sync>;

enum Slot {
    Pending,
    Ready(Arc<RgbaImage>),
    Failed,
}

#[derive(Default)]
struct Inner {
    slots: HashMap<String, Slot>,
    /// Cut versions: (url, width, height, radius px) → pixels.
    cut: HashMap<(String, u32, u32, u32), Arc<RgbaImage>>,
    /// Stable ids for the GPU's custom glyphs: (url, radius px) ↔ id.
    ids: HashMap<(String, u32), u16>,
    by_id: Vec<(String, u32)>,
    fetch: Option<Hook>,
    waker: Option<Waker>,
}

/// Shared and cheap to clone.
#[derive(Clone, Default)]
pub struct ImageStore {
    inner: Arc<Mutex<Inner>>,
}

/// Custom glyph ids below this are icons (`crate::icons::id`).
pub const FIRST_GLYPH_ID: u16 = 256;
/// Most cut versions kept (oldest dropped first is not tracked: the cache
/// is simply cleared when full).
const MAX_CUT: usize = 128;

impl ImageStore {
    pub fn new() -> ImageStore {
        ImageStore::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// What starts a download (it must call [`ImageStore::insert`] later).
    pub fn set_fetcher(&self, fetch: impl Fn(String) + Send + Sync + 'static) {
        self.lock().fetch = Some(Arc::new(fetch));
    }

    /// Called whenever an image arrives (a window asks for a redraw).
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.lock().waker = Some(Arc::new(wake));
    }

    /// Starts loading `url` unless it is loaded, loading or has failed.
    pub fn request(&self, url: &str) {
        let fetch = {
            let mut g = self.lock();
            if g.slots.contains_key(url) {
                return;
            }
            let Some(f) = g.fetch.clone() else {
                return;
            };
            g.slots.insert(url.to_owned(), Slot::Pending);
            f
        };
        fetch(url.to_owned());
    }

    /// A download's result (`None`: it failed; the placeholder stays).
    pub fn insert(&self, url: &str, img: Option<RgbaImage>) {
        let waker = {
            let mut g = self.lock();
            g.slots.insert(
                url.to_owned(),
                match img {
                    Some(i) => Slot::Ready(Arc::new(i)),
                    None => Slot::Failed,
                },
            );
            g.waker.clone()
        };
        if let Some(w) = waker {
            w();
        }
    }

    pub fn is_ready(&self, url: &str) -> bool {
        matches!(self.lock().slots.get(url), Some(Slot::Ready(_)))
    }

    /// The image cut to `w` × `h` pixels with corners of `radius` pixels,
    /// once it has loaded.
    pub fn cut(&self, url: &str, w: u32, h: u32, radius: f32) -> Option<Arc<RgbaImage>> {
        let r = radius.max(0.0).round() as u32;
        let key = (url.to_owned(), w, h, r);
        let src = {
            let g = self.lock();
            if let Some(c) = g.cut.get(&key) {
                return Some(c.clone());
            }
            match g.slots.get(url) {
                Some(Slot::Ready(i)) => i.clone(),
                _ => return None,
            }
        };
        let out = Arc::new(cut(&src, w.max(1), h.max(1), r as f32));
        let mut g = self.lock();
        if g.cut.len() >= MAX_CUT {
            g.cut.clear();
        }
        g.cut.insert(key, out.clone());
        Some(out)
    }

    /// A stable custom-glyph id for `url` drawn with `radius` pixel corners.
    pub fn glyph_id(&self, url: &str, radius: f32) -> u16 {
        let key = (url.to_owned(), radius.max(0.0).round() as u32);
        let mut g = self.lock();
        if let Some(id) = g.ids.get(&key) {
            return *id;
        }
        let id = FIRST_GLYPH_ID.saturating_add(g.by_id.len().min(u16::MAX as usize - 257) as u16);
        g.by_id.push(key.clone());
        g.ids.insert(key, id);
        id
    }

    /// The url and corner radius (pixels) of a glyph id.
    pub fn glyph(&self, id: u16) -> Option<(String, u32)> {
        let g = self.lock();
        g.by_id
            .get(id.checked_sub(FIRST_GLYPH_ID)? as usize)
            .cloned()
    }
}

/// `src` scaled to cover `w` × `h` (centre-cropped) with rounded corners, as
/// straight (unpremultiplied) RGBA.
pub fn cut(src: &RgbaImage, w: u32, h: u32, radius: f32) -> RgbaImage {
    let (sw, sh) = (src.width().max(1) as f32, src.height().max(1) as f32);
    let scale = (w as f32 / sw).max(h as f32 / sh);
    let (cw, ch) = (
        (w as f32 / scale).round() as u32,
        (h as f32 / scale).round() as u32,
    );
    let x = ((sw as u32).saturating_sub(cw)) / 2;
    let y = ((sh as u32).saturating_sub(ch)) / 2;
    let crop = image::imageops::crop_imm(src, x, y, cw.max(1), ch.max(1)).to_image();
    let mut out = image::imageops::resize(&crop, w, h, image::imageops::FilterType::Triangle);
    if radius > 0.0 {
        let half = [w as f32 / 2.0, h as f32 / 2.0];
        for (px, py, p) in out.enumerate_pixels_mut() {
            let local = [px as f32 + 0.5 - half[0], py as f32 + 0.5 - half[1]];
            let d = crate::cpu::sd_rounded_box(local, half, [radius; 4]);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            p.0[3] = (p.0[3] as f32 * cover).round() as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_crops_to_fill_and_rounds_corners() {
        // A wide image, left half red and right half blue.
        let src = RgbaImage::from_fn(200, 100, |x, _| {
            if x < 100 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let out = cut(&src, 50, 50, 10.0);
        assert_eq!(out.dimensions(), (50, 50));
        // The centre crop keeps both halves; the corner is transparent.
        assert_eq!(out.get_pixel(5, 25).0[0], 255);
        assert_eq!(out.get_pixel(45, 25).0[2], 255);
        assert_eq!(out.get_pixel(0, 0).0[3], 0);
        assert_eq!(out.get_pixel(25, 25).0[3], 255);
    }

    #[test]
    fn requests_fetch_once_and_wake_on_arrival() {
        let store = ImageStore::new();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let a = asked.clone();
        store.set_fetcher(move |u| a.lock().unwrap().push(u));
        let woke = Arc::new(Mutex::new(0));
        let w = woke.clone();
        store.set_waker(move || *w.lock().unwrap() += 1);
        store.request("https://img.example/a.jpg");
        store.request("https://img.example/a.jpg");
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert!(
            store
                .cut("https://img.example/a.jpg", 10, 10, 0.0)
                .is_none()
        );
        store.insert("https://img.example/a.jpg", Some(RgbaImage::new(4, 4)));
        assert_eq!(*woke.lock().unwrap(), 1);
        assert!(
            store
                .cut("https://img.example/a.jpg", 10, 10, 2.0)
                .is_some()
        );
        let id = store.glyph_id("https://img.example/a.jpg", 2.0);
        assert!(id >= FIRST_GLYPH_ID);
        assert_eq!(
            store.glyph(id),
            Some(("https://img.example/a.jpg".into(), 2))
        );
        assert_eq!(store.glyph_id("https://img.example/a.jpg", 2.0), id);
    }
}
