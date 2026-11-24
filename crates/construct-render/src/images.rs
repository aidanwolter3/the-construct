//! Images by URL for [`crate::scene::ImageRun`]s: downloaded and decoded by
//! whoever sets a fetcher (`crate::remote` on devices), kept here, and cut to
//! the box they are drawn in (cropped to fill it, corners rounded) for both
//! rasterizers. Without a fetcher (the pod's previews) nothing loads and
//! the placeholders stay. Decoded images are kept under a byte budget
//! ([`BUDGET_BYTES`]): the least recently drawn goes first, and loads again
//! if it is asked for again.

use image::RgbaImage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type Hook = Arc<dyn Fn(String) + Send + Sync>;
type CutKey = (String, u32, u32, u32);
type Waker = Arc<dyn Fn() + Send + Sync>;

enum Slot {
    Pending,
    Ready(Arc<RgbaImage>),
    Failed,
}

struct Inner {
    /// By url: the slot and when it was last asked for.
    slots: HashMap<String, (Slot, u64)>,
    /// Bytes of the ready images in `slots`.
    used: usize,
    budget: usize,
    /// Cut versions: (url, width, height, radius px) → pixels and when
    /// last asked for (the least recent goes first when full).
    cut: HashMap<CutKey, (Arc<RgbaImage>, u64)>,
    /// Cuts being made on a background thread ([`ImageStore::prepare`]).
    cutting: std::collections::HashSet<CutKey>,
    tick: u64,
    /// Stable ids for the GPU's custom glyphs: (url, radius px) ↔ id.
    ids: HashMap<(String, u32), u16>,
    by_id: Vec<(String, u32)>,
    fetch: Option<Hook>,
    waker: Option<Waker>,
}

impl Default for Inner {
    fn default() -> Inner {
        Inner {
            slots: HashMap::new(),
            used: 0,
            budget: BUDGET_BYTES,
            cut: HashMap::new(),
            cutting: Default::default(),
            tick: 0,
            ids: HashMap::new(),
            by_id: Vec::new(),
            fetch: None,
            waker: None,
        }
    }
}

impl Inner {
    /// The ready image at `url`, marked as just used.
    fn ready(&mut self, url: &str) -> Option<Arc<RgbaImage>> {
        self.tick += 1;
        let tick = self.tick;
        match self.slots.get_mut(url) {
            Some((Slot::Ready(i), seen)) => {
                *seen = tick;
                Some(i.clone())
            }
            _ => None,
        }
    }

    /// Drops the least recently used images (never `keep`) until the ready
    /// ones fit the budget, and failed urls past [`MAX_FAILED`] (they are
    /// tried again if asked for again).
    fn evict(&mut self, keep: &str) {
        while self.used > self.budget {
            let Some(oldest) = self
                .slots
                .iter()
                .filter(|(u, (s, _))| matches!(s, Slot::Ready(_)) && u.as_str() != keep)
                .min_by_key(|(_, (_, seen))| *seen)
                .map(|(u, _)| u.clone())
            else {
                break;
            };
            self.remove(&oldest);
        }
        let failed = || {
            self.slots
                .values()
                .filter(|(s, _)| matches!(s, Slot::Failed))
        };
        if failed().count() > MAX_FAILED {
            let mut old: Vec<(u64, String)> = self
                .slots
                .iter()
                .filter(|(_, (s, _))| matches!(s, Slot::Failed))
                .map(|(u, (_, seen))| (*seen, u.clone()))
                .collect();
            old.sort();
            for (_, u) in old.iter().take(old.len() - MAX_FAILED) {
                self.slots.remove(u);
            }
        }
    }

    fn remove(&mut self, url: &str) {
        if let Some((Slot::Ready(i), _)) = self.slots.remove(url) {
            self.used = self.used.saturating_sub(bytes_of(&i));
        }
    }
}

fn bytes_of(i: &RgbaImage) -> usize {
    i.as_raw().len()
}

/// Shared and cheap to clone.
#[derive(Clone, Default)]
pub struct ImageStore {
    inner: Arc<Mutex<Inner>>,
}

/// Custom glyph ids below this are icons (`crate::icons::id`).
pub const FIRST_GLYPH_ID: u16 = 256;
/// Marks a glyph key's radius as a stand-in's ([`ImageStore::stand_in`]).
const STAND_IN: u32 = 1 << 31;
/// Most cut versions kept (the least recently used goes first).
const MAX_CUT: usize = 64;
/// Most bytes of decoded images kept at once (about sixteen of the largest,
/// `crate::remote`'s 1600 px side): the least recently drawn goes first.
pub const BUDGET_BYTES: usize = 160 << 20;
/// Most failed urls remembered (so they aren't fetched again every frame).
const MAX_FAILED: usize = 1024;

impl ImageStore {
    pub fn new() -> ImageStore {
        ImageStore::default()
    }

    /// A store that keeps at most `bytes` of decoded images.
    pub fn with_budget(bytes: usize) -> ImageStore {
        let store = ImageStore::default();
        store.lock().budget = bytes;
        store
    }

    /// Bytes of decoded images kept now.
    pub fn bytes_used(&self) -> usize {
        self.lock().used
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
            g.tick += 1;
            let tick = g.tick;
            g.slots.insert(url.to_owned(), (Slot::Pending, tick));
            f
        };
        fetch(url.to_owned());
    }

    /// A download's result (`None`: it failed; the placeholder stays).
    pub fn insert(&self, url: &str, img: Option<RgbaImage>) {
        let waker = {
            let mut g = self.lock();
            g.remove(url);
            g.tick += 1;
            let tick = g.tick;
            let slot = match img {
                Some(i) => {
                    g.used += bytes_of(&i);
                    Slot::Ready(Arc::new(i))
                }
                None => Slot::Failed,
            };
            g.slots.insert(url.to_owned(), (slot, tick));
            g.evict(url);
            g.waker.clone()
        };
        if let Some(w) = waker {
            w();
        }
    }

    /// Asks the window for a redraw (something else arrived: a write's answer).
    pub fn wake(&self) {
        let waker = self.lock().waker.clone();
        if let Some(w) = waker {
            w();
        }
    }

    /// `ready`, `loading`, `failed` or `none` (never asked for).
    pub fn status(&self, url: &str) -> &'static str {
        match self.lock().slots.get(url) {
            Some((Slot::Ready(_), _)) => "ready",
            Some((Slot::Pending, _)) => "loading",
            Some((Slot::Failed, _)) => "failed",
            None => "none",
        }
    }

    pub fn is_ready(&self, url: &str) -> bool {
        matches!(self.lock().slots.get(url), Some((Slot::Ready(_), _)))
    }

    /// The image cut to `w` × `h` pixels with corners of `radius` pixels,
    /// once it has loaded.
    pub fn cut(&self, url: &str, w: u32, h: u32, radius: f32) -> Option<Arc<RgbaImage>> {
        let r = radius.max(0.0).round() as u32;
        let key = (url.to_owned(), w, h, r);
        let src = {
            let mut g = self.lock();
            g.tick += 1;
            let tick = g.tick;
            if let Some(c) = g.cut.get_mut(&key) {
                c.1 = tick;
                return Some(c.0.clone());
            }
            g.ready(url)?
        };
        let out = Arc::new(cut(&src, w.max(1), h.max(1), r as f32));
        self.keep(key, out.clone());
        Some(out)
    }

    fn keep(&self, key: CutKey, img: Arc<RgbaImage>) {
        let mut g = self.lock();
        while g.cut.len() >= MAX_CUT {
            let Some(oldest) = g
                .cut
                .iter()
                .min_by_key(|(_, v)| v.1)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            g.cut.remove(&oldest);
        }
        g.tick += 1;
        let tick = g.tick;
        g.cut.insert(key, (img, tick));
    }

    /// Whether the image is ready to draw at `w` × `h` pixels with corners
    /// of `radius` pixels. If it has loaded but isn't cut to that size yet,
    /// it is cut on a background thread (the window is woken when done), so
    /// a frame never waits for it: its placeholder shows meanwhile.
    pub fn prepare(&self, url: &str, w: u32, h: u32, radius: f32) -> bool {
        let r = radius.max(0.0).round() as u32;
        let key = (url.to_owned(), w, h, r);
        let src = {
            let mut g = self.lock();
            g.tick += 1;
            let tick = g.tick;
            if let Some(c) = g.cut.get_mut(&key) {
                c.1 = tick;
                return true;
            }
            if g.cutting.contains(&key) {
                return false;
            }
            match g.ready(url) {
                Some(i) => i,
                None => return false,
            }
        };
        self.lock().cutting.insert(key.clone());
        let store = self.clone();
        let work = move || {
            let out = Arc::new(cut(&src, key.1.max(1), key.2.max(1), key.3 as f32));
            store.lock().cutting.remove(&key);
            store.keep(key, out);
            store.wake();
        };
        // A browser has no threads (decision `web-client`): it cuts the
        // image now, and draws it from the next frame.
        #[cfg(target_arch = "wasm32")]
        work();
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(work);
        false
    }

    /// Whether some cut of `url` is ready (at any size), to stand in for
    /// one that isn't cut yet ([`ImageStore::stand_in_glyph_id`]).
    pub fn has_cut(&self, url: &str) -> bool {
        self.lock().cut.keys().any(|k| k.0 == url)
    }

    /// The cut of `url` nearest `w` × `h` pixels (preferring `radius`
    /// corners), scaled to `w` × `h` quickly (nearest pixel): what draws
    /// for a frame or two while the exact cut is made on another thread,
    /// so an image that has loaded never shows as a blank box when the
    /// size it is drawn at changes (a deck's next card taking the top).
    pub fn stand_in(&self, url: &str, w: u32, h: u32, radius: u32) -> Option<RgbaImage> {
        let near = {
            let g = self.lock();
            g.cut
                .iter()
                .filter(|(k, _)| k.0 == url)
                .min_by_key(|(k, _)| {
                    (
                        k.3 != radius,
                        k.1.abs_diff(w) as u64 + k.2.abs_diff(h) as u64,
                    )
                })
                .map(|(_, v)| v.0.clone())?
        };
        Some(image::imageops::resize(
            near.as_ref(),
            w.max(1),
            h.max(1),
            image::imageops::FilterType::Nearest,
        ))
    }

    /// A stable custom-glyph id for `url` drawn with `radius` pixel corners.
    pub fn glyph_id(&self, url: &str, radius: f32) -> u16 {
        self.id_for(url, radius.max(0.0).round() as u32)
    }

    /// A stable custom-glyph id for a stand-in of `url` ([`Self::stand_in`]):
    /// apart from [`Self::glyph_id`]'s, so the atlas never keeps a stand-in
    /// in place of the exact cut.
    pub fn stand_in_glyph_id(&self, url: &str, radius: f32) -> u16 {
        self.id_for(url, radius.max(0.0).round() as u32 | STAND_IN)
    }

    fn id_for(&self, url: &str, radius: u32) -> u16 {
        let key = (url.to_owned(), radius);
        let mut g = self.lock();
        if let Some(id) = g.ids.get(&key) {
            return *id;
        }
        let id = FIRST_GLYPH_ID.saturating_add(g.by_id.len().min(u16::MAX as usize - 257) as u16);
        g.by_id.push(key.clone());
        g.ids.insert(key, id);
        id
    }

    /// The url and corner radius (pixels) of a glyph id, and whether it is
    /// a stand-in's ([`Self::stand_in_glyph_id`]).
    pub fn glyph(&self, id: u16) -> Option<(String, u32, bool)> {
        let g = self.lock();
        g.by_id
            .get(id.checked_sub(FIRST_GLYPH_ID)? as usize)
            .map(|(u, r)| (u.clone(), r & !STAND_IN, r & STAND_IN != 0))
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
            Some(("https://img.example/a.jpg".into(), 2, false))
        );
        assert_eq!(store.glyph_id("https://img.example/a.jpg", 2.0), id);
        // A stand-in's id is another, for the same image and corners.
        let stand = store.stand_in_glyph_id("https://img.example/a.jpg", 2.0);
        assert_ne!(stand, id);
        assert_eq!(
            store.glyph(stand),
            Some(("https://img.example/a.jpg".into(), 2, true))
        );
    }

    /// A cut at another size stands in for one not made yet, at the size
    /// asked for; an image never cut has none.
    #[test]
    fn a_cut_at_another_size_stands_in() {
        let store = ImageStore::new();
        let url = "https://img.example/a.jpg";
        store.insert(url, Some(RgbaImage::new(80, 100)));
        assert!(!store.has_cut(url));
        assert!(store.stand_in(url, 30, 40, 0).is_none());
        store.cut(url, 20, 25, 0.0).unwrap();
        assert!(store.has_cut(url));
        let s = store.stand_in(url, 30, 40, 0).unwrap();
        assert_eq!((s.width(), s.height()), (30, 40));
        assert!(!store.has_cut("https://img.example/b.jpg"));
    }

    /// Decoded images stay under the budget: the least recently drawn goes
    /// first, and loads again when asked for.
    #[test]
    fn the_store_stays_under_its_budget() {
        let one = 100 * 100 * 4;
        let store = ImageStore::with_budget(3 * one);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let a = asked.clone();
        store.set_fetcher(move |u| a.lock().unwrap().push(u));
        let url = |i: usize| format!("https://img.example/{i}.jpg");
        for i in 0..3 {
            store.request(&url(i));
            store.insert(&url(i), Some(RgbaImage::new(100, 100)));
        }
        assert_eq!(store.bytes_used(), 3 * one);
        // Drawing 0 makes 1 the least recently used.
        assert!(store.cut(&url(0), 10, 10, 0.0).is_some());
        for i in 3..300 {
            store.request(&url(i));
            store.insert(&url(i), Some(RgbaImage::new(100, 100)));
            assert!(store.bytes_used() <= 3 * one, "{i}");
        }
        assert!(store.is_ready(&url(299)));
        assert_eq!(store.status(&url(1)), "none");
        // An image larger than the whole budget is kept alone, while drawn.
        store.insert(&url(1000), Some(RgbaImage::new(200, 200)));
        assert!(store.is_ready(&url(1000)));
        assert_eq!(store.bytes_used(), 200 * 200 * 4);
        // An evicted image loads again when asked for.
        let before = asked.lock().unwrap().len();
        store.request(&url(1));
        assert_eq!(asked.lock().unwrap().len(), before + 1);
        // Failed urls are remembered only up to a point.
        for i in 0..(MAX_FAILED + 10) {
            store.insert(&format!("https://bad.example/{i}"), None);
        }
        let failed = store
            .lock()
            .slots
            .values()
            .filter(|(s, _)| matches!(s, Slot::Failed))
            .count();
        assert_eq!(failed, MAX_FAILED);
    }
}
