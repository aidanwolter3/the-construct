//! Photos paged on taps and scatter plots (decision `scatter-and-tap-paging`).
//!
//! A `core://Carousel` with `tap_paging` turns its pages on taps: the left
//! third shows the previous page, the right third the next (wrapping round).
//! A page whose images aren't loaded and cut yet waits, with the current page
//! showing, until they are (at most [`PAGE_WAIT`]), so a tap never shows a
//! blank. Every page's images are asked for as soon as the carousel is laid
//! out ([`Session::prefetch`]).
//!
//! A `core://Scatter` keeps its view (zoom and pan) and the box being
//! selected among its screen's offsets (`scatter:<id>:*`, `scattersel:<id>:*`),
//! so moving it draws again without laying anything out.

use serde_json::Value;

use super::{Outcome, Session};
use crate::layout::{ScatterView, scatter_hit};
use crate::scene::Draw;
use crate::time::{Duration, Instant};

/// The longest a tap waits for the next page's photo before showing it
/// anyway (its placeholder, then the photo as it arrives).
pub const PAGE_WAIT: Duration = Duration::from_millis(1500);
/// How near a tap must be to a scatter point to pick it (points).
pub const POINT_REACH: f32 = 18.0;

impl Session {
    /// The tap-paged carousel under a point and which third of it the point
    /// is in (-1 left, 0 middle, 1 right).
    pub fn tap_pager_at(&mut self, x: f32, y: f32) -> Option<(String, i8)> {
        // Laid out again first: a page just turned drops the layout, and a
        // second tap before the next frame must still find the carousel.
        self.scene().ok()?;
        let ti = self.top_index();
        let pagers = self.stack[ti]
            .laid
            .as_ref()?
            .0
            .tap_pagers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        if pagers.is_empty() {
            return None;
        }
        let scene = self.scene().ok()?;
        let n = scene.semantics.iter().rev().find(|n| {
            let [bx, by, bw, bh] = n.bounds;
            pagers.contains(&n.id) && x >= bx && x < bx + bw && y >= by && y < by + bh
        })?;
        let f = (x - n.bounds[0]) / n.bounds[2].max(1.0);
        let third = if f < 1.0 / 3.0 {
            -1
        } else if f > 2.0 / 3.0 {
            1
        } else {
            0
        };
        Some((n.id.clone(), third))
    }

    /// The page a tap-paged carousel shows (or is about to).
    pub fn tap_page(&self, id: &str) -> usize {
        if let Some((p, _)) = self.tap_pages.get(id) {
            return *p;
        }
        let ti = self.top_index();
        self.stack[ti]
            .scroll
            .get(&format!("page:{id}"))
            .copied()
            .unwrap_or(0.0)
            .max(0.0) as usize
    }

    /// Turns a tap-paged carousel one page (`step` -1 or 1, wrapping): at
    /// once when the page's images are ready, else once they are (or after
    /// [`PAGE_WAIT`]). Whether there was another page.
    pub fn turn_page(&mut self, id: &str, step: i32) -> bool {
        let ti = self.top_index();
        let Some(count) = self.stack[ti]
            .laid
            .as_ref()
            .and_then(|(l, _)| l.tap_pagers.get(id))
            .map(Vec::len)
        else {
            return false;
        };
        if count < 2 {
            return false;
        }
        let page = (self.tap_page(id) as i32 + step).rem_euclid(count as i32) as usize;
        self.tap_pages.insert(id.to_owned(), (page, Instant::now()));
        self.step_tap_pages(Instant::now());
        true
    }

    /// Whether every image of page `page` of carousel `id` is loaded and cut
    /// at its size (a page without images is ready).
    fn page_ready(&self, id: &str, page: usize) -> bool {
        let ti = self.top_index();
        let Some((laid, _)) = &self.stack[ti].laid else {
            return true;
        };
        let Some(on_page) = laid.tap_pagers.get(id).and_then(|p| p.get(page)) else {
            return true;
        };
        let s = self.viewport.scale.max(1.0);
        on_page.iter().all(|&i| {
            let p = &laid.prefetch[i];
            self.images.status(&p.url) == "failed"
                || self.images.prepare(
                    &p.url,
                    (p.w * s).round() as u32,
                    (p.h * s).round() as u32,
                    p.radius * s,
                )
        })
    }

    /// Shows the pages tapped to whose images are ready (or waited long
    /// enough): the track moves there and the bars follow.
    pub(super) fn step_tap_pages(&mut self, now: Instant) {
        if self.tap_pages.is_empty() {
            return;
        }
        let ti = self.top_index();
        let mut done = Vec::new();
        for (id, (page, since)) in &self.tap_pages {
            if self.page_ready(id, *page)
                || now.duration_since(*since) >= PAGE_WAIT
                || !self.animate
            {
                done.push((id.clone(), *page));
            }
        }
        if done.is_empty() {
            return;
        }
        for (id, page) in &done {
            self.tap_pages.remove(id);
            let w = self.stack[ti]
                .laid
                .as_ref()
                .and_then(|(l, _)| l.pagers.get(id))
                .map_or(0.0, |(w, _)| *w);
            let e = &mut self.stack[ti];
            e.scroll.insert(format!("pager:{id}"), *page as f32 * w);
            e.scroll.insert(format!("page:{id}"), *page as f32);
        }
        // The bars show the new page.
        self.stack[ti].relay();
        self.scene = None;
    }

    /// Asks for every image the top layout has laid out but may not draw
    /// yet (other pages of its carousels), and cuts those that have loaded
    /// at the size they'll be drawn, so they show the moment they are paged
    /// to. Called before the drawn images are asked for, which go first.
    pub(super) fn prefetch(&self) {
        let ti = self.top_index();
        let Some((laid, _)) = &self.stack[ti].laid else {
            return;
        };
        let s = self.viewport.scale.max(1.0);
        for p in &laid.prefetch {
            if self.images.is_ready(&p.url) {
                self.images.prepare(
                    &p.url,
                    (p.w * s).round() as u32,
                    (p.h * s).round() as u32,
                    p.radius * s,
                );
            } else {
                self.images.request(&p.url);
            }
        }
    }

    /// The urls the top layout prefetches (tests, the simulator).
    pub fn prefetched(&self) -> Vec<String> {
        let ti = self.top_index();
        self.stack[ti]
            .laid
            .as_ref()
            .map(|(l, _)| l.prefetch.iter().map(|p| p.url.clone()).collect())
            .unwrap_or_default()
    }

    /// The scatter under a point.
    pub fn scatter_at(&mut self, x: f32, y: f32) -> Option<String> {
        self.scene().ok()?;
        let ti = self.top_index();
        let ids: Vec<String> = self.stack[ti]
            .laid
            .as_ref()?
            .0
            .scatters
            .keys()
            .cloned()
            .collect();
        if ids.is_empty() {
            return None;
        }
        let scene = self.scene().ok()?;
        scene
            .semantics
            .iter()
            .rev()
            .find(|n| {
                let [bx, by, bw, bh] = n.bounds;
                ids.contains(&n.id) && x >= bx && x < bx + bw && y >= by && y < by + bh
            })
            .map(|n| n.id.clone())
    }

    /// A scatter's box on screen.
    fn scatter_rect(&mut self, id: &str) -> Option<[f32; 4]> {
        let scene = self.scene().ok()?;
        scene.find_node(id).map(|n| n.bounds)
    }

    /// A scatter's view now.
    pub fn scatter_view(&self, id: &str) -> ScatterView {
        let ti = self.top_index();
        ScatterView::of(&self.stack[ti].scroll, id)
    }

    fn set_scatter_view(&mut self, id: &str, v: ScatterView) {
        let ti = self.top_index();
        let s = &mut self.stack[ti].scroll;
        s.insert(format!("scatter:{id}:z"), v.zoom);
        s.insert(format!("scatter:{id}:x"), v.x);
        s.insert(format!("scatter:{id}:y"), v.y);
        // A pan or pinch step changes only the plot's view: patch it into
        // the scene drawn now rather than building the screen again (once
        // per pointer move, which made the taste map lag).
        let patched = self.scene.as_mut().is_some_and(|scene| {
            let mut found = false;
            for d in scene.draws.iter_mut() {
                if let Draw::Points(run) = d
                    && run.id == id
                {
                    run.view = [v.zoom, v.x, v.y];
                    found = true;
                }
            }
            found
        });
        if !patched {
            self.scene = None;
        }
    }

    /// Whether a drag on the scatter selects a box (its `select` is on).
    pub fn scatter_selects(&self, id: &str) -> bool {
        let ti = self.top_index();
        self.stack[ti]
            .laid
            .as_ref()
            .and_then(|(l, _)| l.scatters.get(id))
            .is_some_and(|p| p.selecting)
    }

    /// Moves a scatter's content with a drag of (`dx`, `dy`) points.
    pub fn scatter_pan(&mut self, id: &str, dx: f32, dy: f32) {
        let Some(rect) = self.scatter_rect(id) else {
            return;
        };
        let v = self
            .scatter_view(id)
            .panned(ScatterView::area(rect), dx, dy);
        self.set_scatter_view(id, v);
    }

    /// Zooms a scatter by `factor` about screen point (`x`, `y`).
    pub fn scatter_zoom(&mut self, id: &str, factor: f32, x: f32, y: f32) {
        let Some(rect) = self.scatter_rect(id) else {
            return;
        };
        let v = self
            .scatter_view(id)
            .zoomed(ScatterView::area(rect), factor, x, y);
        self.set_scatter_view(id, v);
    }

    /// Back to the whole plot.
    pub fn scatter_reset(&mut self, id: &str) {
        self.set_scatter_view(
            id,
            ScatterView {
                zoom: 1.0,
                x: 0.0,
                y: 0.0,
            },
        );
    }

    /// The box being selected runs from where a drag started to (`x`, `y`)
    /// (screen points): drawn until [`Session::scatter_select_end`].
    pub fn scatter_select(&mut self, id: &str, from: (f32, f32), to: (f32, f32)) {
        let Some(rect) = self.scatter_rect(id) else {
            return;
        };
        let a = ScatterView::area(rect);
        let v = self.scatter_view(id);
        let (x0, y0) = v.to_data(a, from.0, from.1);
        let (x1, y1) = v.to_data(a, to.0, to.1);
        let ti = self.top_index();
        let s = &mut self.stack[ti].scroll;
        for (k, val) in [("x0", x0), ("y0", y0), ("x1", x1), ("y1", y1)] {
            s.insert(format!("scattersel:{id}:{k}"), val.clamp(0.0, 1.0));
        }
        self.scene = None;
    }

    /// The box being selected (data coordinates, sorted), if any.
    pub fn scatter_selection(&self, id: &str) -> Option<[f32; 4]> {
        let ti = self.top_index();
        let s = &self.stack[ti].scroll;
        let g = |k: &str| s.get(&format!("scattersel:{id}:{k}")).copied();
        let (x0, y0, x1, y1) = (g("x0")?, g("y0")?, g("x1")?, g("y1")?);
        Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
    }

    /// Ends a selection: the box goes, and the scatter's `on_select` runs
    /// with it bound, unless the box is too small to mean anything.
    pub fn scatter_select_end(&mut self, id: &str) -> Result<Outcome, String> {
        let sel = self.scatter_selection(id);
        let ti = self.top_index();
        let s = &mut self.stack[ti].scroll;
        for k in ["x0", "y0", "x1", "y1"] {
            s.remove(&format!("scattersel:{id}:{k}"));
        }
        self.scene = None;
        let Some([x0, y0, x1, y1]) = sel else {
            return Err("no box".into());
        };
        let span = 0.6 / self.scatter_view(id).zoom.max(1.0) / 100.0;
        if x1 - x0 < span || y1 - y0 < span {
            return Err("box too small".into());
        }
        let Some(intent) = self.stack[ti]
            .laid
            .as_ref()
            .and_then(|(l, _)| l.scatters.get(id))
            .and_then(|p| p.on_select.clone())
        else {
            return Err("no on_select".into());
        };
        let m = |v: f32| ((v.clamp(0.0, 1.0) * 1_000_000.0).round() as u64).to_string();
        let vars = [
            ("$select.x0", format!("{x0:.6}")),
            ("$select.y0", format!("{y0:.6}")),
            ("$select.x1", format!("{x1:.6}")),
            ("$select.y1", format!("{y1:.6}")),
            (
                "$select.box",
                format!("{}-{}-{}-{}", m(x0), m(y0), m(x1), m(y1)),
            ),
        ];
        self.dispatch(&bind_vars(&intent, &vars))
    }

    /// A tap on a scatter: the point nearest it (within [`POINT_REACH`])
    /// runs `on_tap` with it bound. `None` when the tap isn't on one.
    pub(super) fn scatter_tap(&mut self, x: f32, y: f32) -> Option<Result<Outcome, String>> {
        let id = self.scatter_at(x, y)?;
        let rect = self.scatter_rect(&id)?;
        let view = self.scatter_view(&id);
        let ti = self.top_index();
        let plot = self.stack[ti].laid.as_ref()?.0.scatters.get(&id)?.clone();
        let Some(i) = scatter_hit(&plot, view, rect, x, y, POINT_REACH) else {
            return Some(Err("no point there".into()));
        };
        let Some(intent) = plot.on_tap.clone() else {
            return Some(Err("no on_tap".into()));
        };
        let p = plot.points[i];
        let vars = [
            ("$point.index", i.to_string()),
            ("$point.x", format!("{:.6}", p.x)),
            ("$point.y", format!("{:.6}", p.y)),
        ];
        Some(self.dispatch(&bind_vars(&intent, &vars)))
    }
}

/// `vars` (`$point.index` …) replaced in every string of an intent.
pub fn bind_vars(v: &Value, vars: &[(&str, String)]) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for (k, val) in vars {
                if s.contains(k) {
                    s = s.replace(k, val);
                }
            }
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| bind_vars(x, vars)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), bind_vars(x, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A pointer held this long without moving on a scatter starts a box
/// selection (as `select` does), on touch and mouse alike.
pub const SELECT_HOLD: Duration = Duration::from_millis(450);

/// What a pointer on a scatter does, for every front end (window, browser,
/// simulator): one finger or the mouse pans (or, while selecting, draws a
/// box), two fingers pinch, the wheel zooms; a press that didn't move taps.
#[derive(Debug, Clone)]
pub struct ScatterGesture {
    pub id: String,
    /// The pointers down, by id, and where they are.
    pointers: Vec<(u64, (f32, f32))>,
    start: (f32, f32),
    started: Instant,
    moved: bool,
    selecting: bool,
    /// The fingers' distance and middle at the last move of a pinch.
    pinch: Option<(f32, (f32, f32))>,
}

/// How a pointer leaving a scatter ends.
#[derive(Debug)]
pub enum GestureEnd {
    /// Other pointers are still down.
    Continues,
    /// It didn't move: a tap at that point (the front end taps there).
    Tap(f32, f32),
    /// A box was selected: what its `on_select` did.
    Selected(Box<Result<Outcome, String>>),
    /// It panned or pinched.
    Done,
}

impl ScatterGesture {
    /// A pointer going down: a gesture when it is on a scatter.
    pub fn begin(
        session: &mut Session,
        pointer: u64,
        p: (f32, f32),
        select: bool,
    ) -> Option<ScatterGesture> {
        let id = session.scatter_at(p.0, p.1)?;
        let selecting = select || session.scatter_selects(&id);
        Some(ScatterGesture {
            id,
            pointers: vec![(pointer, p)],
            start: p,
            started: Instant::now(),
            moved: false,
            selecting,
            pinch: None,
        })
    }

    /// Whether the gesture is drawing a selection box.
    pub fn selecting(&self) -> bool {
        self.selecting
    }

    /// Another pointer going down: two make a pinch.
    pub fn add(&mut self, pointer: u64, p: (f32, f32)) {
        self.pointers.retain(|(id, _)| *id != pointer);
        self.pointers.push((pointer, p));
        if self.pointers.len() >= 2 {
            self.moved = true;
            self.pinch = Some(self.span());
        }
    }

    fn span(&self) -> (f32, (f32, f32)) {
        let (a, b) = (self.pointers[0].1, self.pointers[1].1);
        (
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt().max(1.0),
            ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0),
        )
    }

    /// A pointer moved; whether anything changed (draw again).
    pub fn moved(&mut self, session: &mut Session, pointer: u64, p: (f32, f32)) -> bool {
        let Some(at) = self.pointers.iter().position(|(id, _)| *id == pointer) else {
            return false;
        };
        let last = self.pointers[at].1;
        self.pointers[at].1 = p;
        if self.pointers.len() >= 2 {
            let (d, mid) = self.span();
            if let Some((d0, mid0)) = self.pinch {
                session.scatter_zoom(&self.id, d / d0, mid.0, mid.1);
                session.scatter_pan(&self.id, mid.0 - mid0.0, mid.1 - mid0.1);
            }
            self.pinch = Some((d, mid));
            return true;
        }
        if !self.moved {
            let far = (p.0 - self.start.0).abs().max((p.1 - self.start.1).abs())
                > crate::scroll::TOUCH_SLOP;
            if !far {
                return false;
            }
            self.moved = true;
            // Held still before moving: a selection, as `select` makes it.
            if self.started.elapsed() >= SELECT_HOLD {
                self.selecting = true;
            }
        }
        if self.selecting {
            session.scatter_select(&self.id, self.start, p);
        } else {
            session.scatter_pan(&self.id, p.0 - last.0, p.1 - last.1);
        }
        true
    }

    /// A pointer went up (or was cancelled: no tap, no selection).
    pub fn ended(
        &mut self,
        session: &mut Session,
        pointer: u64,
        p: (f32, f32),
        cancelled: bool,
    ) -> GestureEnd {
        self.pointers.retain(|(id, _)| *id != pointer);
        if !self.pointers.is_empty() {
            // Down to one finger: it pans on from where it is.
            self.pinch = None;
            return GestureEnd::Continues;
        }
        if cancelled {
            if self.selecting {
                let _ = session.scatter_select_end_silently(&self.id);
            }
            return GestureEnd::Done;
        }
        if !self.moved {
            return GestureEnd::Tap(p.0, p.1);
        }
        if self.selecting {
            return GestureEnd::Selected(Box::new(session.scatter_select_end(&self.id)));
        }
        GestureEnd::Done
    }
}

impl Session {
    /// Drops the box being selected without running anything.
    pub fn scatter_select_end_silently(&mut self, id: &str) -> bool {
        let ti = self.top_index();
        let s = &mut self.stack[ti].scroll;
        let had = s.contains_key(&format!("scattersel:{id}:x0"));
        for k in ["x0", "y0", "x1", "y1"] {
            s.remove(&format!("scattersel:{id}:{k}"));
        }
        self.scene = None;
        had
    }

    /// A wheel (or trackpad scroll) of `dy` points over a scatter zooms it
    /// about the pointer; whether it was over one.
    pub fn scatter_wheel(&mut self, x: f32, y: f32, dy: f32) -> bool {
        let Some(id) = self.scatter_at(x, y) else {
            return false;
        };
        self.scatter_zoom(&id, (-dy * 0.004).exp(), x, y);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use image::RgbaImage;
    use protocol::scatter::{Mark, Point, encode};
    use protocol::{DataBundle, EntityRecord, UiNode};
    use serde_json::json;

    use super::super::{Effect, Session, Target, Viewport};

    fn entity(fields: serde_json::Value) -> EntityRecord {
        match fields {
            serde_json::Value::Object(m) => EntityRecord(m),
            _ => unreachable!(),
        }
    }

    /// A piece with three photos, in a deck of two such pieces (the second
    /// card is laid out behind the first), each card a tap-paged carousel.
    fn deck() -> (UiNode, DataBundle) {
        let root: UiNode = serde_json::from_value(json!({
            "type": "core://List", "query_key": "q:deck", "stack": true,
            "item_template": {
                "type": "core://SwipeCard", "on_swipe_left": {"intent": "action.navigation.back"},
                "content": {
                    "type": "core://Carousel", "items": "$item.photos", "tap_paging": true,
                    "aspect_ratio": 0.8,
                    "on_tap": {"intent": "action.local.set", "entity_uri": "x://flags/tap",
                               "field": "middle", "value_expr": "true"},
                    "item_template": {"type": "core://Image", "url": "$item.url"}
                }
            }
        }))
        .unwrap();
        let mut data = DataBundle::default();
        for piece in ["a", "b"] {
            let photos: Vec<String> = (0..3).map(|k| format!("x://photos/{piece}{k}")).collect();
            for (k, uri) in photos.iter().enumerate() {
                data.entities.insert(
                    uri.clone(),
                    entity(json!({"url": format!("https://img.example/{piece}{k}.jpg")})),
                );
            }
            data.entities.insert(
                format!("x://pieces/{piece}"),
                entity(json!({"photos": photos})),
            );
        }
        data.queries.insert(
            "q:deck".into(),
            vec!["x://pieces/a".into(), "x://pieces/b".into()],
        );
        (root, data)
    }

    /// A session whose image store records what it is asked for (nothing
    /// arrives unless a test puts it there).
    fn session(root: UiNode, data: DataBundle) -> (Session, Arc<Mutex<Vec<String>>>) {
        let s = Session::new("home", root, data, Viewport::PHONE);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        s.images.set_fetcher(move |u| log.lock().unwrap().push(u));
        (s, asked)
    }

    /// The top card's carousel: the tap-paged one under the middle of the
    /// screen's top half (the card behind is drawn under it).
    fn pager_id(s: &mut Session) -> String {
        s.scene().unwrap();
        s.tap_pager_at(195.0, 200.0).expect("a carousel there").0
    }

    fn bounds(s: &mut Session, id: &str) -> [f32; 4] {
        s.scene().unwrap().find_node(id).unwrap().bounds
    }

    #[test]
    fn every_photo_of_the_deck_is_asked_for_up_front() {
        let (root, data) = deck();
        let (mut s, asked) = session(root, data);
        s.scene().unwrap();
        let asked = asked.lock().unwrap().clone();
        // The top card's photos and the card behind it, all three pages each.
        for want in ["a0", "a1", "a2", "b0", "b1", "b2"] {
            let url = format!("https://img.example/{want}.jpg");
            assert!(asked.contains(&url), "{url} not asked for: {asked:?}");
        }
        // The page showing is asked for last of the top card's: it is the
        // one drawn, so it goes to the front of the queue.
        assert!(s.prefetched().len() >= 6);
    }

    #[test]
    fn taps_on_the_outer_thirds_turn_the_pages_and_wrap() {
        let (root, data) = deck();
        let (mut s, _) = session(root, data);
        s.animate = false; // no waiting for photos
        let id = pager_id(&mut s);
        let [x, y, w, h] = bounds(&mut s, &id);
        let (left, right, mid) = (x + w * 0.1, x + w * 0.9, x + w * 0.5);
        let cy = y + h / 2.0;
        assert_eq!(s.tap_page(&id), 0);
        s.tap(&Target::Point(right, cy)).unwrap();
        s.scene().unwrap();
        assert_eq!(s.tap_page(&id), 1);
        // Two quick taps, no frame between them: both turn.
        s.tap(&Target::Point(right, cy)).unwrap();
        s.tap(&Target::Point(right, cy)).unwrap();
        assert_eq!(s.tap_page(&id), 0, "wraps after the last photo");
        s.tap(&Target::Point(left, cy)).unwrap();
        s.scene().unwrap();
        assert_eq!(s.tap_page(&id), 2, "back from the first wraps to the last");
        // The middle third runs on_tap and turns nothing.
        let out = s.tap(&Target::Point(mid, cy)).unwrap();
        assert!(
            out.effects
                .iter()
                .any(|e| matches!(e, Effect::LocalSet { .. })),
            "{:?}",
            out.effects
        );
        assert_eq!(s.tap_page(&id), 2);
    }

    #[test]
    fn a_page_waits_for_its_photo_with_the_current_one_showing() {
        let (root, data) = deck();
        let (mut s, _) = session(root, data);
        s.animate = true;
        let id = pager_id(&mut s);
        let shown = |s: &mut Session| {
            let ti = s.top_index();
            s.scene().unwrap();
            s.stack[ti]
                .scroll
                .get(&format!("page:{id}"))
                .copied()
                .unwrap_or(0.0)
        };
        let [x, y, w, h] = bounds(&mut s, &id);
        s.tap(&Target::Point(x + w * 0.9, y + h / 2.0)).unwrap();
        // The second photo hasn't arrived: the first still shows.
        assert_eq!(shown(&mut s), 0.0);
        assert_eq!(s.tap_page(&id), 1, "the turn is under way");
        s.images
            .insert("https://img.example/a1.jpg", Some(RgbaImage::new(8, 10)));
        // It is cut to size on another thread, then shows.
        let mut page = 0.0;
        for _ in 0..200 {
            page = shown(&mut s);
            if page == 1.0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(page, 1.0, "shows once its photo is in");
    }

    fn plot() -> (UiNode, DataBundle) {
        let points = [
            Point {
                x: 0.1,
                y: 0.1,
                value: Some(0.9),
                mark: Mark::Highlight,
            },
            Point {
                x: 0.5,
                y: 0.5,
                value: None,
                mark: Mark::Plain,
            },
            Point {
                x: 0.9,
                y: 0.8,
                value: Some(0.1),
                mark: Mark::Muted,
            },
        ];
        let root: UiNode = serde_json::from_value(json!({
            "type": "core://Scatter", "points": "${x://maps/m#points}", "tag": "plot",
            "select": "${x://maps/m#selecting}",
            "on_tap": {"intent": "action.local.set", "entity_uri": "x://flags/t",
                       "field": "point", "value_expr": "\"$point.index\""},
            "on_select": {"intent": "action.local.set", "entity_uri": "x://flags/t",
                          "field": "box", "value_expr": "\"$select.box\""}
        }))
        .unwrap();
        let mut data = DataBundle::default();
        data.entities.insert(
            "x://maps/m".into(),
            entity(json!({"points": encode(&points), "selecting": true})),
        );
        (root, data)
    }

    fn set_fields(out: &super::super::Outcome) -> BTreeMap<String, serde_json::Value> {
        out.effects
            .iter()
            .find_map(|e| match e {
                Effect::LocalSet { fields, .. } => Some(fields.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_tap_near_a_point_runs_on_tap_with_its_index() {
        let (root, data) = plot();
        let (mut s, _) = session(root, data);
        s.scene().unwrap();
        let ti = s.top_index();
        let id = s.stack[ti]
            .laid
            .as_ref()
            .unwrap()
            .0
            .scatters
            .keys()
            .next()
            .unwrap()
            .clone();
        let rect = bounds(&mut s, &id);
        let a = crate::layout::ScatterView::area(rect);
        let v = s.scatter_view(&id);
        let (px, py) = v.to_screen(a, 0.9, 0.8);
        let out = s.tap(&Target::Point(px + 4.0, py - 3.0)).unwrap();
        assert_eq!(set_fields(&out).get("point"), Some(&json!("2")));
        // Zoomed in on the middle, the same screen spot is another place.
        s.scatter_zoom(&id, 4.0, a[0] + a[2] / 2.0, a[1] + a[3] / 2.0);
        let v = s.scatter_view(&id);
        assert!((v.zoom - 4.0).abs() < 1e-3);
        let (mx, my) = v.to_screen(a, 0.5, 0.5);
        let out = s.tap(&Target::Point(mx, my)).unwrap();
        assert_eq!(set_fields(&out).get("point"), Some(&json!("1")));
    }

    #[test]
    fn a_dragged_box_runs_on_select_with_its_bounds() {
        let (root, data) = plot();
        let (mut s, _) = session(root, data);
        s.scene().unwrap();
        let ti = s.top_index();
        let id = s.stack[ti]
            .laid
            .as_ref()
            .unwrap()
            .0
            .scatters
            .keys()
            .next()
            .unwrap()
            .clone();
        assert!(s.scatter_selects(&id));
        let a = crate::layout::ScatterView::area(bounds(&mut s, &id));
        let v = s.scatter_view(&id);
        let from = v.to_screen(a, 0.25, 0.75);
        let to = v.to_screen(a, 0.5, 0.25);
        s.scatter_select(&id, from, to);
        let sel = s.scatter_selection(&id).unwrap();
        assert!((sel[0] - 0.25).abs() < 1e-3 && (sel[1] - 0.25).abs() < 1e-3);
        assert!((sel[2] - 0.5).abs() < 1e-3 && (sel[3] - 0.75).abs() < 1e-3);
        let out = s.scatter_select_end(&id).unwrap();
        assert_eq!(
            set_fields(&out).get("box"),
            Some(&json!("250000-250000-500000-750000"))
        );
        assert!(
            s.scatter_selection(&id).is_none(),
            "the box goes on release"
        );
        // A box too small to mean anything runs nothing.
        let p = v.to_screen(a, 0.5, 0.5);
        s.scatter_select(&id, p, (p.0 + 0.5, p.1 + 0.5));
        assert!(s.scatter_select_end(&id).is_err());
    }

    /// A plot of `n` points spread over the square, like the taste map.
    fn big_plot(n: usize) -> (UiNode, DataBundle) {
        let mut seed = 7u64;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f32) / (u32::MAX >> 1) as f32
        };
        let points: Vec<Point> = (0..n)
            .map(|i| Point {
                x: rnd(),
                y: rnd(),
                value: Some(rnd()),
                mark: match i % 200 {
                    0 => Mark::Highlight,
                    1..=4 => Mark::Muted,
                    _ => Mark::Plain,
                },
            })
            .collect();
        let root: UiNode = serde_json::from_value(json!({
            "type": "core://Column", "children": [
                {"type": "core://Text", "value_expr": "Your taste map", "style": "Title"},
                {"type": "core://Scatter", "points": "${x://maps/m#points}", "tag": "plot"}
            ]
        }))
        .unwrap();
        let mut data = DataBundle::default();
        data.entities.insert(
            "x://maps/m".into(),
            entity(json!({"points": encode(&points)})),
        );
        (root, data)
    }

    /// A pan or pinch step moves the drawn points without building the
    /// screen again, and the plot is one draw however many points it has.
    #[test]
    fn a_pan_step_patches_the_drawn_plot() {
        let (root, data) = big_plot(2_000);
        let (mut s, _) = session(root, data);
        let points = |s: &mut Session| {
            s.scene()
                .unwrap()
                .draws
                .iter()
                .filter_map(|d| match d {
                    crate::scene::Draw::Points(p) => Some((p.view, p.dots.len())),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(points(&mut s).len(), 1, "one draw for the plot");
        assert_eq!(points(&mut s)[0].1, 2_000);
        let id = "plot".to_string();
        let a = crate::layout::ScatterView::area(bounds(&mut s, &id));
        let _ = s.take_work();
        s.scatter_zoom(&id, 2.0, a[0] + a[2] / 2.0, a[1] + a[3] / 2.0);
        s.scatter_pan(&id, 10.0, 5.0);
        let v = s.scatter_view(&id);
        assert_eq!(
            points(&mut s)[0].0,
            [v.zoom, v.x, v.y],
            "drawn with the new view"
        );
        let w = s.take_work();
        assert_eq!(w.laid_out, 0, "nothing laid out again");
        assert_eq!(
            w.build,
            std::time::Duration::ZERO,
            "the scene wasn't built again"
        );
    }

    /// Timings for the taste map's size (run with `--ignored --nocapture`).
    #[test]
    #[ignore]
    fn time_a_big_plot() {
        let (root, data) = big_plot(32_233);
        let (mut s, _) = session(root, data);
        let t = std::time::Instant::now();
        s.scene().unwrap();
        let first = t.elapsed();
        let id = "plot".to_string();
        let a = crate::layout::ScatterView::area(bounds(&mut s, &id));
        // Zoomed out, every point drawn: a pinch step of 1.01.
        let t = std::time::Instant::now();
        for _ in 0..20 {
            s.scatter_zoom(&id, 1.001, a[0] + a[2] / 2.0, a[1] + a[3] / 2.0);
            s.scene().unwrap();
        }
        let whole = t.elapsed() / 20;
        let whole_draws = s.scene().unwrap().draws.len();
        s.scatter_zoom(&id, 3.0, a[0] + a[2] / 2.0, a[1] + a[3] / 2.0);
        s.scene().unwrap();
        let _ = s.take_work();
        let steps = 60;
        let t = std::time::Instant::now();
        for k in 0..steps {
            s.scatter_pan(&id, if k % 2 == 0 { 3.0 } else { -3.0 }, 1.0);
            s.scene().unwrap();
        }
        let per = t.elapsed() / steps;
        let w = s.take_work();
        eprintln!(
            "big plot: first scene {first:?}; zoomed-out pinch step + scene {whole:?} ({whole_draws} draws); \
             pan step + scene at 3x {per:?}; laid out {} times in {steps} steps; draws {}",
            w.laid_out,
            s.scene().unwrap().draws.len()
        );
    }

    #[test]
    fn the_view_stays_inside_the_square() {
        let (root, data) = plot();
        let (mut s, _) = session(root, data);
        s.scene().unwrap();
        let ti = s.top_index();
        let id = s.stack[ti]
            .laid
            .as_ref()
            .unwrap()
            .0
            .scatters
            .keys()
            .next()
            .unwrap()
            .clone();
        s.scatter_pan(&id, 500.0, 500.0);
        assert_eq!(
            s.scatter_view(&id).x,
            0.0,
            "at zoom 1 there's nowhere to pan"
        );
        s.scatter_zoom(&id, 100.0, 0.0, 0.0);
        assert!(s.scatter_view(&id).zoom <= crate::layout::SCATTER_MAX_ZOOM);
        s.scatter_pan(&id, -1.0e6, -1.0e6);
        let v = s.scatter_view(&id);
        assert!(v.x + 1.0 / v.zoom <= 1.0 + 1e-4 && v.y + 1.0 / v.zoom <= 1.0 + 1e-4);
        s.scatter_reset(&id);
        assert_eq!(s.scatter_view(&id).zoom, 1.0);
    }

    #[test]
    fn the_page_bars_fit_inside_the_photo() {
        // A carousel in a padded column, as on a card: its bars stay inside it.
        let (_, data) = deck();
        for stack in [false, true] {
            let root: UiNode = serde_json::from_value(json!({
                "type": "core://List", "query_key": "q:deck", "stack": stack,
                "item_template": {
                    "type": "core://SwipeCard", "on_swipe_left": {"intent": "action.navigation.back"},
                    "content": {"type": "core://Box", "padding_h_dp": 30, "content": {
                        "type": "core://Carousel", "items": "$item.photos", "tap_paging": true,
                        "aspect_ratio": 0.8,
                        "item_template": {"type": "core://Image", "url": "$item.url"}
                    }}
                }
            }))
            .unwrap();
            let (mut s, _) = session(root, data.clone());
            let scene = s.scene().unwrap();
            // Each carousel's bars (`dots:<its id>`) inside it.
            let mut seen = 0;
            for n in scene.semantics.iter().filter(|n| n.node_type == "dots") {
                let id = n.id.strip_prefix("dots:").unwrap();
                let frame = scene.find_node(id).expect("its carousel").bounds;
                let dots = n.bounds;
                let ok =
                    dots[0] >= frame[0] - 0.5 && dots[0] + dots[2] <= frame[0] + frame[2] + 0.5;
                assert!(ok, "stack {stack}: bars {dots:?} in {frame:?}");
                assert!(
                    dots[1] >= frame[1] && dots[1] < frame[1] + 40.0,
                    "bars at the top: {dots:?}"
                );
                seen += 1;
            }
            assert!(seen >= 1);
        }
    }
}
