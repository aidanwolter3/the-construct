//! A layout with its state (form values, scroll offsets, a back stack),
//! drawn frame by frame and driven by virtual taps and scrolls: what the
//! desktop window, the Android surface and the pod's MCP previews run.
//!
//! Taps change device state as the app does: a chip sets its form value,
//! `action.local.set` changes the local copy of an entity, opening a layout
//! pushes it. Writes (`action.mutation.optimistic` and `.create`, marking
//! notifications read) change the local copy at once and go to the pod
//! through the [`Loader`]: kept in its outbox until the pod has them where
//! the loader has one (the engine, [`Loader::take`]), else sent in the
//! background and undone if the pod refuses them; without a loader that
//! sends (the pod's previews) they are reported as not sent. What the loader
//! hears of later (another device, an agent) comes in through
//! [`Loader::changes`] on the next frame.

use crate::time::{Duration, Instant};
use image::RgbaImage;
use protocol::{DataBundle, PageScreen, UiNode};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::cpu::CpuRenderer;
use crate::layout::{
    Input, Laid, NO_PANELS, Panel, Panels, Pressed, RenderError, SceneBuilder, Viewport, panel_id,
};
use crate::scene::{COVER, Draw, QuadInstance, Scene, SemanticNode, Tap};
use crate::style::Rgba;

mod gallery;
mod media;
pub use gallery::{GestureEnd, PAGE_WAIT, POINT_REACH, SELECT_HOLD, ScatterGesture, bind_vars};
pub use media::{StageDone, Staged, attachment_uri, pick_files_here};

/// A scrolling view under a point ([`Session::scroll_view_at`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollView {
    pub id: String,
    /// Points scrolled, of `0..=max`.
    pub offset: f32,
    pub max: f32,
    /// Its height on screen (points).
    pub height: f32,
}

/// What a tap or scroll aims at.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A semantic node id (`filter=started`, a `tag`, a path).
    Id(String),
    /// The node showing this text.
    Text(String),
    /// A point in logical points.
    Point(f32, f32),
}

/// What an interaction did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    FormSet {
        field: String,
        value: String,
    },
    Focused {
        field: String,
    },
    LocalSet {
        entity_uri: String,
        fields: BTreeMap<String, Value>,
    },
    Opened {
        layout: String,
    },
    OpenedScreen {
        screen: String,
    },
    /// An `action.device` command ran on the device.
    Device {
        command: String,
    },
    /// A layout or a screen's data couldn't be loaded.
    LoadFailed {
        what: String,
        error: String,
    },
    Back,
    Scrolled {
        id: String,
        offset: f32,
    },
    /// A write went to the pod (its answer comes later).
    Sent {
        path: String,
    },
    /// The platform should open this address in the browser.
    OpenLink {
        url: String,
    },
    /// A pod session opened (`action.auth.session`): the platform saves it
    /// and opens the pod; `code` is the one to offer next time.
    SignedIn {
        token: String,
        code: String,
    },
    /// `leave_pod`: the platform forgets the session and shows sign-in.
    LeftPod,
    /// `set_server`: the platform opens this server instead (its saved
    /// session, else its sign-in screen).
    SwitchServer {
        url: String,
    },
    /// Paste was chosen: the platform reads its clipboard and
    /// [`Session::paste`]s it into this field.
    Paste {
        field: String,
    },
    /// Copy or Cut was chosen: the platform puts this on its clipboard.
    Copy {
        text: String,
    },
    /// A media picker was tapped: the platform opens its photo picker for
    /// up to `max` photos (what the field still takes) and hands what is
    /// picked to [`Session::picked`] (the desktop: [`Session::pick_here`]).
    PickMedia {
        field: String,
        max: u32,
    },
    /// Would go to the pod or the platform; a preview doesn't send it.
    NotSent {
        intent: Value,
    },
    Nothing,
}

impl Effect {
    /// The effect's name (its `effect` tag), for logs: never its values,
    /// which can be what was typed (a pod code) or a session token.
    pub fn name(&self) -> &'static str {
        match self {
            Effect::FormSet { .. } => "form_set",
            Effect::Focused { .. } => "focused",
            Effect::LocalSet { .. } => "local_set",
            Effect::Opened { .. } => "opened",
            Effect::OpenedScreen { .. } => "opened_screen",
            Effect::Device { .. } => "device",
            Effect::LoadFailed { .. } => "load_failed",
            Effect::Back => "back",
            Effect::Scrolled { .. } => "scrolled",
            Effect::Sent { .. } => "sent",
            Effect::OpenLink { .. } => "open_link",
            Effect::SignedIn { .. } => "signed_in",
            Effect::LeftPod => "left_pod",
            Effect::SwitchServer { .. } => "switch_server",
            Effect::Paste { .. } => "paste",
            Effect::Copy { .. } => "copy",
            Effect::PickMedia { .. } => "pick_media",
            Effect::NotSent { .. } => "not_sent",
            Effect::Nothing => "nothing",
        }
    }

    /// The effect for a log, with what says where it went (a layout, a
    /// screen, a path, a field's name) but, as [`Effect::name`], never a
    /// value that can be what was typed or a token: no form value, copied
    /// text, link, session or intent payload.
    pub fn summary(&self) -> String {
        let name = self.name();
        match self {
            Effect::FormSet { field, .. }
            | Effect::Focused { field }
            | Effect::Paste { field }
            | Effect::PickMedia { field, .. } => format!("{name} {field}"),
            Effect::LocalSet { entity_uri, .. } => format!("{name} {entity_uri}"),
            Effect::Opened { layout } => format!("{name} {layout}"),
            Effect::OpenedScreen { screen } => format!("{name} {screen}"),
            Effect::Device { command } => format!("{name} {command}"),
            Effect::LoadFailed { what, error } => format!("{name} {what}: {error}"),
            Effect::Sent { path } => format!("{name} {path}"),
            Effect::NotSent { intent } => {
                let mut s = format!("{name} {}", intent["intent"].as_str().unwrap_or("?"));
                for k in ["layout", "target_uri", "path", "endpoint"] {
                    if let Some(v) = intent[k].as_str() {
                        s.push_str(&format!(" {k}={v}"));
                    }
                }
                // `action.navigation.open` with only a `target_uri` is not
                // sent when no screen of the layouts shown matches it.
                if intent["intent"] == "action.navigation.open"
                    && intent["layout"].is_null()
                    && intent["target_uri"].is_string()
                {
                    s.push_str(" (no screen here matches it)");
                }
                s
            }
            _ => name.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub node: Option<SemanticNode>,
    pub effects: Vec<Effect>,
}

/// The size class of a window this many points wide (feature
/// `device_size`): `compact` under 600, `medium` under 840, else
/// `expanded`, the common breakpoints of phone, tablet and desktop layouts.
pub fn size_class(width: f32) -> &'static str {
    if width < 600.0 {
        "compact"
    } else if width < 840.0 {
        "medium"
    } else {
        "expanded"
    }
}

/// Where an entry is drawn (decision `edge-overlays-and-gestures`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// The whole screen: the root, or a pushed layout.
    Full,
    /// A sheet, panel or overlay from this edge, over a scrim.
    Edge(protocol::Edge),
    /// An `action.confirm` dialog, centred over a scrim.
    Dialog,
}

/// How long a layout or screen takes to slide in or out (the shared UI's
/// `FRAME_SLIDE_MS`).
const SLIDE: Duration = Duration::from_millis(280);
/// One turn of a spinner.
const SPIN: Duration = Duration::from_millis(1000);
/// A sheet's share of the screen's height, and a panel's of its width (at
/// most [`PANEL_MAX_WIDTH`]), as in the shared UI.
const SHEET_HEIGHT_FRACTION: f32 = 0.85;
const PANEL_WIDTH_FRACTION: f32 = 0.86;
const PANEL_MAX_WIDTH: f32 = 340.0;
/// A sheet's handle strip.
const HANDLE_STRIP: f32 = 16.0;
/// Where a drag closes a sheet from its handle (points from its inner edge).
const HANDLE_GRAB: f32 = 32.0;
/// A dragged sheet or panel closes past this share of its size, or flung
/// faster than [`CLOSE_FLING`] points per second toward its edge (the
/// shared UI's thresholds).
const CLOSE_FRACTION: f32 = 0.25;
const CLOSE_FLING: f32 = 500.0;
/// How long a released sheet takes to settle back.
const SETTLE: Duration = Duration::from_millis(180);
/// A carousel's slide to a page, and the release speed that turns the page
/// (points per second).
const PAGE_SLIDE: Duration = Duration::from_millis(260);
const PAGE_FLING: f32 = 300.0;
/// A swipe card flung faster than this (points per second) goes that way
/// from half its threshold; how long it takes to fly off, and to spring
/// back.
const CARD_FLING: f32 = 800.0;
const CARD_FLY: Duration = Duration::from_millis(200);
const CARD_SPRING: Duration = Duration::from_millis(320);

/// A released swipe card on its way: off the screen (and then its intent
/// runs) or back to where it was.
struct CardAnim {
    since: Instant,
    from: f32,
    to: f32,
    /// The card and the intent that runs once it is off.
    commit: Option<(SemanticNode, Value)>,
}

/// Where [`Session::move_cursor`] moves the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMove {
    Left,
    Right,
    Home,
    End,
}

/// Which way a drag moves a sheet or panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragAxis {
    Vertical,
    Horizontal,
}

/// Speeding up as it goes (a card thrown off).
fn ease_in(t: f32) -> f32 {
    t * t
}

/// Past the end a little and back (a card springing into place).
fn ease_out_back(t: f32) -> f32 {
    let (c1, u) = (1.4f32, t - 1.0);
    1.0 + (c1 + 1.0) * u.powi(3) + c1 * u.powi(2)
}

/// Material's standard easing, `cubic-bezier(0.4, 0, 0.2, 1)`
/// (`FastOutSlowInEasing`).
fn fast_out_slow_in(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let (x1, y1, x2, y2) = (0.4f32, 0.0f32, 0.2f32, 1.0f32);
    let bez = |a: f32, b: f32, u: f32| {
        3.0 * a * u * (1.0 - u).powi(2) + 3.0 * b * u * u * (1.0 - u) + u.powi(3)
    };
    // Solve x(u) = t by bisection, then y(u).
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if bez(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(y1, y2, (lo + hi) / 2.0)
}

struct Entry {
    id: String,
    root: UiNode,
    route: BTreeMap<String, String>,
    /// A page screen's `item` (an entity URI).
    item: Option<String>,
    /// The screens of the layout this entry belongs to.
    screens: Vec<PageScreen>,
    /// The last load of this entry's data failed (`local://screen#error`).
    error: Option<String>,
    /// The data path being loaded for it (`local://screen#loading`).
    loading: Option<String>,
    /// Its screen's data path, if it has one.
    data: Option<String>,
    /// Scroll offsets of this entry's scrolling views (by semantic id).
    scroll: BTreeMap<String, f32>,
    frame: Frame,
    /// A sheet's height or a panel's width (`size_dp`).
    size: Option<f32>,
    /// A screen opened inside its layout's sheet or panel.
    in_frame: bool,
    opened: Instant,
    closing: Option<Instant>,
    /// Laid out at this size (points, with these insets).
    laid: Option<(Laid, [f32; 2])>,
    /// The frame laid out before the last change, which the next layout
    /// reuses what it can of ([`crate::layout::SceneBuilder::relayout`]).
    stale: Option<Laid>,
    /// A page screen (opened by a `target_uri`), not the layout itself.
    screen: bool,
    /// The page screen's id, for frame diagnostics (empty for a layout).
    screen_id: String,
    /// An action from outside its `follow_end` scrolling views ran: they
    /// follow their content to the end once it is laid out.
    follow: bool,
    /// The layouts open as panels beside its panel hosts (decision
    /// `wide-screen-panels`).
    panels: Panels,
    /// Its panel hosts as last laid out ([`Laid::panel_hosts`]).
    panel_hosts: Vec<String>,
}

impl Entry {
    /// Lays the entry out again on the next frame, reusing what it can of
    /// this frame.
    fn relay(&mut self) {
        if let Some((laid, _)) = self.laid.take() {
            self.stale = Some(laid);
        }
    }

    fn layout(id: &str, root: UiNode, route: BTreeMap<String, String>) -> Entry {
        let screens = match &root {
            UiNode::Page { screens, .. } => screens.clone(),
            _ => Vec::new(),
        };
        Entry {
            id: id.to_owned(),
            root,
            route,
            item: None,
            screens,
            error: None,
            loading: None,
            data: None,
            scroll: BTreeMap::new(),
            frame: Frame::Full,
            size: None,
            in_frame: false,
            opened: Instant::now(),
            closing: None,
            laid: None,
            stale: None,
            screen: false,
            screen_id: String::new(),
            follow: false,
            panels: Panels::new(),
            panel_hosts: Vec::new(),
        }
    }
}

/// A write to the pod: POST `body` to `path`, then load the layouts in
/// `refresh` again (their data replaces what the device has).
#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    pub path: String,
    pub body: Value,
    pub refresh: Vec<String>,
}

/// What a [`Write`] came back with.
pub struct Written {
    pub result: Result<Value, String>,
    pub refreshed: Vec<DataBundle>,
}

/// Called once with a write's answer, on any thread.
pub type Done = Box<dyn FnOnce(Written) + Send>;

/// Called once with a data path's answer, on any thread.
pub type Loaded = Box<dyn FnOnce(Result<DataBundle, String>) + Send>;

/// What a loader heard of since it was last asked ([`Loader::changes`]):
/// data that replaces the device's, and layouts' new roots.
#[derive(Debug, Default, Clone)]
pub struct Changes {
    pub data: DataBundle,
    pub layouts: Vec<(String, UiNode)>,
}

/// Where a scrolling view was, by its items (decision
/// `content-addressed-construct`): the first item showing, how far the view
/// was scrolled past its top, and the items in order with their tops, for
/// when that one is gone. An entity can be drawn more than once in a view (a
/// preview list above the full one): an item is its entity and which time
/// it is drawn there (`nth`, from 0).
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub uri: String,
    pub nth: usize,
    pub offset_dp: f32,
    pub items: Vec<(String, usize, f32)>,
}

/// Items (entity, top, height) as (entity, which time it is drawn, top).
fn numbered(items: &[(String, f32, f32)]) -> Vec<(String, usize, f32)> {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    items
        .iter()
        .map(|(u, top, _)| {
            let n = seen.entry(u.as_str()).or_default();
            let nth = *n;
            *n += 1;
            (u.clone(), nth, *top)
        })
        .collect()
}

impl Anchor {
    /// The anchor of a view scrolled to `offset` over `items` (entity, top,
    /// height): the last row starting at or above the view's top (the row
    /// under it, or the one just past when the top is in a gap or a heading
    /// after it). `None` without items, or while the view's top is above
    /// every row: what is shown there (a search box, a heading) isn't a row,
    /// and rows appearing under it (results while typing) must not scroll
    /// it away.
    pub fn at(offset: f32, items: &[(String, f32, f32)]) -> Option<Anchor> {
        let i = items.iter().rposition(|(_, top, _)| *top <= offset)?;
        let numbered = numbered(items);
        let (uri, nth, top) = numbered[i].clone();
        Some(Anchor {
            uri,
            nth,
            offset_dp: offset - top,
            items: numbered,
        })
    }

    /// The offset that shows the anchored item where it was, over the
    /// view's new `items`; if it is drawn fewer times now, its last time
    /// before that; if it is gone, the nearest one still there (the next,
    /// else the one before) where that one was; `None` if none is.
    pub fn offset_in(&self, items: &[(String, f32, f32)]) -> Option<f32> {
        let numbered = numbered(items);
        let now = |uri: &str, nth: usize| {
            numbered
                .iter()
                .find(|(u, n, _)| u == uri && *n == nth)
                .map(|i| i.2)
        };
        if let Some(top) = now(&self.uri, self.nth) {
            return Some(top + self.offset_dp);
        }
        if let Some(top) = (0..self.nth).rev().find_map(|n| now(&self.uri, n)) {
            return Some(top + self.offset_dp);
        }
        let at = self
            .items
            .iter()
            .position(|(u, n, _)| *u == self.uri && *n == self.nth)?;
        let scrolled = self.items[at].2 + self.offset_dp;
        let after = self.items[at + 1..].iter();
        let before = self.items[..at].iter().rev();
        after
            .chain(before)
            .find_map(|(u, n, old)| now(u, *n).map(|top| top + scrolled - old))
    }
}

/// The anchors of an entry's vertical scrolls laid out in `laid` (its
/// scrolling views, not the `stretch:`, `pager:` … offsets beside them).
fn anchors_of(laid: &Laid, scroll: &BTreeMap<String, f32>) -> BTreeMap<String, Anchor> {
    scroll
        .iter()
        .filter(|(id, _)| laid.scroll_max.contains_key(*id))
        .filter_map(|(id, &offset)| Some((id.clone(), Anchor::at(offset, &laid.scroll_items(id))?)))
        .collect()
}

/// The inline embeds that moved between two frames ([`Laid::embeds`]):
/// each one's old path and new path, for a chain of layouts drawn once in
/// both (the embedding layout changed around it; a layout drawn twice
/// can't be told apart, and keeps nothing).
fn moved_embeds(
    before: &BTreeMap<String, std::collections::BTreeSet<String>>,
    after: &BTreeMap<String, std::collections::BTreeSet<String>>,
) -> Vec<(String, String)> {
    before
        .iter()
        .filter_map(|(chain, olds)| {
            let news = after.get(chain)?;
            if olds.len() != 1 || news.len() != 1 {
                return None;
            }
            let (old, new) = (olds.first()?, news.first()?);
            (old != new).then(|| (old.clone(), new.clone()))
        })
        .collect()
}

/// A state key (a path, or `stretch:`, `pager:` … and a path) under a moved
/// embed, at its new path; the innermost embed that moved decides.
fn moved_key(key: &str, moves: &[(String, String)]) -> Option<String> {
    let (pre, path) = if key.starts_with("root") {
        ("", key)
    } else {
        let i = key.find(':')?;
        key.split_at(i + 1)
    };
    let (old, new) = moves
        .iter()
        .filter(|(old, _)| {
            path.strip_prefix(old.as_str())
                .is_some_and(|r| r.is_empty() || r.starts_with('/') || r.starts_with('~'))
        })
        .max_by_key(|(old, _)| old.len())?;
    Some(format!("{pre}{new}{}", &path[old.len()..]))
}

/// Moves the state kept under moved embeds to their new paths.
fn move_keys<V>(map: &mut BTreeMap<String, V>, moves: &[(String, String)]) {
    let keys: Vec<(String, String)> = map
        .keys()
        .filter_map(|k| Some((k.clone(), moved_key(k, moves)?)))
        .collect();
    let moved: Vec<(String, V)> = keys
        .into_iter()
        .filter_map(|(old, new)| Some((new, map.remove(&old)?)))
        .collect();
    map.extend(moved);
}

/// The construct's edge graph (decision `content-addressed-construct`):
/// each layout key and the keys it reaches ([`Loader::edges`]), where the
/// loader follows the construct (the engine). A stack entry the one below it no longer reaches is
/// closed.
pub type Edges = BTreeMap<String, std::collections::BTreeSet<String>>;

impl Changes {
    /// Later changes over these: newer entities, queries and layout roots
    /// win.
    pub fn absorb(&mut self, later: Changes) {
        self.data.entities.extend(later.data.entities);
        self.data.queries.extend(later.data.queries);
        for (id, root) in later.layouts {
            self.layouts.retain(|(i, _)| *i != id);
            self.layouts.push((id, root));
        }
    }

    /// The new roots, taken out (what waits for a touch or the keyboard to
    /// finish).
    fn take_roots(&mut self) -> Option<Changes> {
        if self.layouts.is_empty() {
            return None;
        }
        Some(Changes {
            data: DataBundle::default(),
            layouts: std::mem::take(&mut self.layouts),
        })
    }
}

/// Something the pod answered in the background.
enum Answer {
    Write(Undo, Written),
    Data {
        path: String,
        result: Result<DataBundle, String>,
    },
    /// A picked photo the engine staged (or why it couldn't) for a media
    /// picker's field holding at most `max`.
    Staged {
        field: String,
        max: u32,
        result: Result<Staged, String>,
    },
    /// The desktop's file dialog closed with these files.
    Picked(Vec<Result<std::path::PathBuf, String>>),
}

/// What undoes a write's local change when the pod refuses it.
enum Undo {
    /// Fields of an entity, with the values to put back (`Null`: none).
    Fields {
        uri: String,
        before: BTreeMap<String, Value>,
    },
    /// A created entity and the list it was added to.
    Created {
        uri: String,
        query_key: String,
    },
    Nothing,
}

/// Where a live session gets what it opens: another layout of the pod, and a
/// data path's answer (`crate::remote` on devices). Calls block, except
/// [`Loader::send`].
pub trait Loader: Send {
    /// A layout's plumbing root and its data, by key (from the construct:
    /// its element materialized, and its data).
    fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String>;
    /// A data path's answer (a screen's `data`, `action.data.load`).
    fn data(&self, path: &str) -> Result<DataBundle, String>;
    /// A device command that changes something on the pod (`toggle_channel`)
    /// or loads more of the device data (`load_older_notifications`), with
    /// its arguments (`layout`, `id`, `enabled`, `before`): the device data
    /// to merge after it; `None` when this loader doesn't run it.
    fn device_command(&self, _command: &str, _args: &Value) -> Option<Result<DataBundle, String>> {
        None
    }
    /// Sends a write, off the caller's thread where it can, and calls
    /// `done` with the answer; false when this loader sends nothing.
    fn send(&self, _write: Write, _done: Done) -> bool {
        false
    }
    /// [`Loader::data`] off the caller's thread; false when this loader
    /// can't (the session then loads it in place).
    fn fetch(&self, _path: &str, _done: Loaded) -> bool {
        false
    }
    /// POSTs a sign-in (`action.auth.session`) and answers the session
    /// token; `None` when this loader can't.
    fn open_session(&self, _path: &str, _payload: &Value) -> Option<Result<String, String>> {
        None
    }
    /// Carries out a write itself, where the loader keeps writes until the
    /// pod has them (the engine's outbox): `action.mutation.optimistic`,
    /// `action.mutation.create`, or `action.device` `mark_read` and
    /// `mark_all_read`. True when taken: the session's own change stands
    /// until [`Loader::changes`] brings the loader's data. False: the session
    /// sends it with [`Loader::send`].
    fn take(&self, _intent: &Value) -> bool {
        false
    }
    /// Keeps a device-only change (`action.local.set`, already made in the
    /// session) where the loader holds its own copy of the data (the
    /// engine), so its next [`Loader::changes`] carry the new values instead
    /// of putting the old ones back.
    fn local_set(&self, _intent: &Value) {}
    /// Keeps a field of a `local://` entity across starts of the app
    /// (`action.local.set` with `keep`, feature `local_keep`), where the
    /// loader has somewhere to keep it (the engine's store).
    fn keep_local(&self, _entity_uri: &str, _field: &str, _value: &Value) {}
    /// The fields kept so far, `(entity uri, field, value)`: the session
    /// puts them back before its first frame.
    fn kept_locals(&self) -> Vec<(String, String, Value)> {
        Vec::new()
    }
    /// Starts a sign-in to an upstream or signs out of it, where the loader
    /// runs the engine's sign-in (`crate::signin`): `action.auth.login`, or
    /// `action.device` `sign_in` and `sign_out`. True when taken; its
    /// progress shows in `local://session`.
    fn auth(&self, _intent: &Value) -> bool {
        false
    }
    /// Whether [`Loader::auth`] can start a sign-in at all (so a tap that
    /// can't isn't asked about first).
    fn signs_in(&self) -> bool {
        false
    }
    /// One of the platform's own servers (`set_server`, `local:servers`)
    /// by id: its url, and whether it is the one open now. `None` when the
    /// platform offers no such server.
    fn server(&self, _id: &str) -> Option<(String, bool)> {
        None
    }
    /// Hands a link that opened the app to a sign-in waiting for its
    /// redirect; false when it isn't one, or nothing waits for it.
    fn redirect(&self, _url: &str) -> bool {
        false
    }
    /// Stages a picked photo for a create's `attachments` (D49), off the
    /// caller's thread, and calls `done` with it; false when this loader
    /// can't.
    fn stage(&self, _file: &std::path::Path, _done: StageDone) -> bool {
        false
    }
    /// Forgets a staged photo taken off its picker before it was sent.
    fn discard(&self, _id: &str) {}
    /// What changed since the last call (another device, an agent, a write
    /// the pod answered); `None` when nothing did, or the loader doesn't
    /// hear of changes. Called every frame: it must not block.
    fn changes(&self) -> Option<Changes> {
        None
    }
    /// The construct's edge graph when it changed since the last call
    /// (decision `content-addressed-construct`); `None`: unchanged, or the
    /// loader keeps no graph. Called every frame: it must not block.
    fn edges(&self) -> Option<Edges> {
        None
    }
    /// What is on screen now, topmost first, whenever it changes: where
    /// the loader reloads changed data (the engine, decision
    /// `scoped-data-events`), it reloads this first. Must not block.
    fn visible(&self, _shown: &[Shown]) {}
    /// The app came back to the foreground: where the loader keeps an
    /// events stream (the engine), a stream that went down meanwhile
    /// reconnects now and catches up on the changes it missed. Must not
    /// block.
    fn foregrounded(&self) {}
    /// How the owner uses their screens (decision `usage-trails`): a screen
    /// opened, a tap, a Back, with names from the layout only (never bound
    /// text, values or ids). Only the engine records them, for the pod's
    /// `usage` jack; a preview's, a walk's or the simulator's loader leaves
    /// this to its default and records nothing. Must not block.
    fn usage(&self, _event: &protocol::usage::UsageEvent) {}
}

/// Something a frame draws ([`Loader::visible`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    /// A layout, by id (the home layout too).
    Layout(String),
    /// A screen's data path.
    Data(String),
}

/// `uri` against a route pattern (`collections://tasks/{id}`): the captured
/// parameters, or `None`.
pub fn match_route(pattern: &str, uri: &str) -> Option<BTreeMap<String, String>> {
    let p: Vec<&str> = pattern.split('/').collect();
    let u: Vec<&str> = uri.split('/').collect();
    if p.len() != u.len() {
        return None;
    }
    let mut out = BTreeMap::new();
    for (a, b) in p.iter().zip(&u) {
        if let Some(name) = a.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            let ok = !b.is_empty()
                && b.len() <= 64
                && b.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if !ok {
                return None;
            }
            out.insert(name.to_owned(), (*b).to_owned());
        } else if a != b {
            return None;
        }
    }
    Some(out)
}

fn merge(into: &mut DataBundle, from: DataBundle) {
    into.entities.extend(from.entities);
    into.queries.extend(from.queries);
}

/// A push (`action.navigation.open` of a layout, no screen, not as a sheet,
/// panel or overlay) or a Back: what a panel host turns into panels.
fn panel_intent(intent: &Value) -> bool {
    match intent["intent"].as_str() {
        Some("action.navigation.back") => intent["frame"] != true,
        Some("action.navigation.open") => {
            intent["layout"].as_str().is_some_and(|l| !l.is_empty())
                && intent["target_uri"].as_str().is_none_or(str::is_empty)
                && intent["presentation"].as_str().is_none_or(|p| p == "Push")
        }
        _ => false,
    }
}

/// Whether an intent only opens or closes layouts and screens
/// (`action.navigation.open`, `action.navigation.back`): it changes nothing
/// already laid out, unless it merges data in.
fn navigates_only(intent: &Value) -> bool {
    matches!(
        intent["intent"].as_str(),
        Some(
            "action.navigation.open"
                | "action.navigation.back"
                | "action.navigation.pop_to"
                | "action.navigation.pop_to_root"
        )
    )
}

/// [`merge`] that keeps the entities and queries `into` already has as they
/// are: true when any of them is new or different. A loader's news carry
/// all its data each time (the engine's snapshots), most of it unchanged.
fn merge_changed(into: &mut DataBundle, from: DataBundle) -> bool {
    let mut changed = false;
    for (uri, e) in from.entities {
        if into.entities.get(&uri) != Some(&e) {
            into.entities.insert(uri, e);
            changed = true;
        }
    }
    for (key, q) in from.queries {
        if into.queries.get(&key) != Some(&q) {
            into.queries.insert(key, q);
            changed = true;
        }
    }
    changed
}

/// `$route.<name>` replaced in every string of a node (a screen's tree).
fn bind_route(v: &mut Value, route: &BTreeMap<String, String>) {
    match v {
        Value::String(s) if s.contains("$route.") => {
            for (k, val) in route {
                *s = s.replace(&format!("$route.{k}"), val);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| bind_route(x, route)),
        Value::Object(m) => m.values_mut().for_each(|x| bind_route(x, route)),
        _ => {}
    }
}

/// A layout or screen a pressed row opens, laid out before the tap: the
/// open's intent (bound), the entries it pushes and the effects it had.
struct Prepared {
    intent: Value,
    entries: Vec<Entry>,
    effects: Vec<Effect>,
    /// It merged data in (a screen's data loaded in place).
    merged: bool,
}

/// A live layout.
pub struct Session {
    stack: Vec<Entry>,
    /// Whether the intent being run came from the user's own gesture on the
    /// edit menu (or the platform keyboard), not from a layout: only then may
    /// it read or write the clipboard (`edit.paste`, `edit.copy`, `edit.cut`).
    clipboard_ok: bool,
    /// The device command waiting for the user's OK in the app's own
    /// dialog ([`Session::ask_device`]), with the nonce its OK carries.
    asked: Option<(String, Asked)>,
    /// What building frames cost since the window last asked.
    work: Work,
    pub data: DataBundle,
    pub form: BTreeMap<String, String>,
    pub layouts: BTreeMap<String, UiNode>,
    pub viewport: Viewport,
    pub dark: bool,
    builder: SceneBuilder,
    cpu: CpuRenderer,
    scene: Option<Scene>,
    /// Slide layouts in and out (a window); off, every frame is settled
    /// (tests, previews).
    pub animate: bool,
    /// The text field being typed into (drawn with a caret).
    pub focused: Option<String>,
    /// Loads what taps open (a live session); `None`: only what is given.
    pub loader: Option<Box<dyn Loader>>,
    /// The images frames show, loaded by the store's fetcher.
    pub images: crate::images::ImageStore,
    /// Space the platform covers (status bar, camera cutout, navigation
    /// bar), in points: top, right, bottom, left.
    pub insets: [f32; 4],
    /// Writes the pod has answered, waiting to be applied (by [`Session::poll`]).
    answered: Arc<Mutex<Vec<Answer>>>,
    /// Whether the kept `local://` fields were put back ([`Loader::kept_locals`]).
    kept_restored: bool,
    /// Writes sent and not answered yet.
    pub pending: usize,
    /// The last write the pod refused, and why.
    pub last_error: Option<String>,
    /// A one-line notice over the layouts (a refused write or device
    /// command), until dismissed, as the shared UI's notice bar.
    pub notice: Option<String>,
    /// How far the top sheet or panel is dragged toward its edge (points).
    frame_offset: f32,
    /// A released drag settling back: since when, from what offset.
    frame_settle: Option<(Instant, f32)>,
    /// Carousels sliding to a page after a swipe: since when, from and to
    /// what offset, and the page.
    pager_anims: BTreeMap<String, (Instant, f32, f32, usize)>,
    /// Tap-paged carousels tapped to a page whose images aren't ready yet:
    /// the page and since when (`gallery`).
    tap_pages: BTreeMap<String, (usize, Instant)>,
    /// Swipe cards let go, flying off or springing back (by semantic id).
    card_anims: BTreeMap<String, CardAnim>,
    /// Cards that flew off, whose intents run at [`Session::take_swiped`].
    swiped: Vec<(String, SemanticNode, Value)>,
    /// The text being edited: cursor, selection, handles and menu.
    edit: Option<Edit>,
    /// Whether the caret was drawn in the frame kept (it blinks).
    caret_shown: Option<bool>,
    /// The `core://Tappable` held down, drawn pressed until it is let go.
    pressed: Option<Pressed>,
    /// The `core://Tappable` the mouse rests over (a pointer that hovers:
    /// desktop and web), drawn with a light wash.
    hovered: Option<Pressed>,
    /// What the pressed row opens, opened out of sight and laid out while
    /// the finger is down ([`Session::prepare_at`]).
    prepared: Option<Prepared>,
    /// Data merged in so far (a count), to tell whether an open brought
    /// any: one that didn't leaves what is laid out as it is.
    merges: u64,
    /// Scenes built so far ([`Session::scenes_built`]).
    scenes_built: u64,
    /// Spinners turn from here.
    spin_from: Instant,
    /// Whether the frame kept draws a spinner (it turns).
    spinning: bool,
    /// Where the edit handles are, to grab them (from the last frame).
    handles: Vec<(Grab, [f32; 4])>,
    /// A handle being dragged, and how far the finger is below the text
    /// point it moves.
    grab: Option<(Grab, f32)>,
    /// The media picker the platform's picker is open for: its field and
    /// most photos.
    picking: Option<(String, u32)>,
    /// Photos being picked or staged (a script waits for them).
    pub staging: usize,
    /// What the loader was last told is on screen ([`Loader::visible`]).
    shown: Vec<Shown>,
    /// The loader's changes and the pod's answers that came while a screen
    /// slid in or out, applied on the first frame after the slide ends
    /// (decision `hold-changes-during-transitions`).
    held: Option<Changes>,
    held_answers: Vec<Answer>,
    /// New roots that came while a finger was down or a field was being
    /// typed in, swapped in once that ends (decision
    /// `content-addressed-construct`). Not a reason to keep drawing: the
    /// touch's end or the keyboard closing draws the next frame.
    held_roots: Option<Changes>,
    /// The construct's edge graph, as last applied ([`Loader::edges`]), and
    /// one waiting to be (with the roots it came with).
    edges: Option<Edges>,
    held_edges: Option<Edges>,
}

/// How long the caret shows, then hides (Android's 500 ms).
const BLINK: Duration = Duration::from_millis(500);
/// Two taps this close in time are a double tap (Android's 300 ms).
const DOUBLE_TAP: Duration = Duration::from_millis(300);
/// The edit handles' size, and how much further a finger may land.
const HANDLE: f32 = 22.0;
const HANDLE_SLOP: f32 = 12.0;

/// The text field being typed into: where the cursor is and what is
/// selected (byte offsets in its value; `anchor` is the fixed end), and what
/// shows around it.
#[derive(Debug, Clone)]
struct Edit {
    field: String,
    cursor: usize,
    anchor: usize,
    /// The handles show (the caret's after a tap, a selection's always).
    handles: bool,
    /// The Cut / Copy / Paste / Select all bar shows.
    menu: bool,
    /// The caret shows from here, blinking.
    blink: Instant,
    /// The last tap on the field, for a double tap.
    last_tap: Option<(Instant, f32, f32)>,
    /// The word the keyboard is composing (byte range), underlined.
    composing: Option<(usize, usize)>,
}

impl Edit {
    fn range(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }
    fn selected(&self) -> bool {
        self.cursor != self.anchor
    }
}

/// An edit handle a finger can drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// The caret's handle.
    Caret,
    /// The start and end of a selection.
    Start,
    End,
}

/// The word around byte `at` of `text` (letters, digits and `_`), or the
/// single other character there.
fn word_at(text: &str, at: usize) -> (usize, usize) {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let at = at.min(text.len());
    let after = text[at..].chars().next();
    let before = text[..at].chars().next_back();
    let inside = after.is_some_and(word) || before.is_some_and(word);
    if !inside {
        let end = after.map_or(at, |c| at + c.len_utf8());
        return (at, end);
    }
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| word(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let end = text[at..]
        .char_indices()
        .take_while(|(_, c)| word(*c))
        .last()
        .map_or(at, |(i, c)| at + i + c.len_utf8());
    (start, end)
}

/// The character boundary before and after byte `at`.
fn prev_char(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}
fn next_char(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

/// What building frames cost (frame diagnostics, [`crate::perf`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Work {
    /// Layouts or screens laid out again, and the time it took.
    pub laid_out: u16,
    pub layout: Duration,
    /// The rest of `Session::scene`, by part: taking the loader's changes
    /// (`poll`); the clock, pagers, cards and closed entries (`tidy`);
    /// telling the loader what is shown (`shown`); and building the frame
    /// from what is laid out (`build`).
    pub poll: Duration,
    pub tidy: Duration,
    pub shown: Duration,
    pub build: Duration,
    /// Data snapshots that changed what is drawn.
    pub data_changes: u16,
}

impl Session {
    /// What building frames cost since last asked.
    pub fn take_work(&mut self) -> Work {
        std::mem::take(&mut self.work)
    }

    /// What the top of the screen shows, for frame diagnostics: the
    /// layout's id, and the page screen's after a slash.
    pub fn showing(&self) -> String {
        let e = &self.stack[self.top_index()];
        if e.screen_id.is_empty() {
            e.id.clone()
        } else {
            format!("{}/{}", e.id, e.screen_id)
        }
    }

    pub fn new(id: &str, root: UiNode, data: DataBundle, viewport: Viewport) -> Session {
        Session {
            stack: vec![Entry::layout(id, root, BTreeMap::new())],
            asked: None,
            work: Work::default(),
            data,
            form: BTreeMap::new(),
            layouts: BTreeMap::new(),
            viewport,
            dark: false,
            builder: SceneBuilder::new(),
            cpu: CpuRenderer::new(),
            scene: None,
            animate: false,
            focused: None,
            loader: None,
            images: crate::images::ImageStore::new(),
            insets: [0.0; 4],
            answered: Arc::new(Mutex::new(Vec::new())),
            kept_restored: false,
            pending: 0,
            last_error: None,
            notice: None,
            frame_offset: 0.0,
            frame_settle: None,
            pager_anims: BTreeMap::new(),
            tap_pages: BTreeMap::new(),
            card_anims: BTreeMap::new(),
            swiped: Vec::new(),
            edit: None,
            caret_shown: None,
            pressed: None,
            hovered: None,
            prepared: None,
            clipboard_ok: false,
            merges: 0,
            scenes_built: 0,
            spin_from: Instant::now(),
            spinning: false,
            handles: Vec::new(),
            grab: None,
            picking: None,
            staging: 0,
            shown: Vec::new(),
            held: None,
            held_answers: Vec::new(),
            held_roots: None,
            edges: None,
            held_edges: None,
        }
    }

    /// The edit of the focused field (started at the end of its value if
    /// it has none), its value clamped to the value as it is now.
    fn edit_now(&mut self) -> Option<&mut Edit> {
        let field = self.focused.clone()?;
        let value = self.form.get(&field).cloned().unwrap_or_default();
        if self.edit.as_ref().is_none_or(|e| e.field != field) {
            self.edit = Some(Edit {
                field: field.clone(),
                cursor: value.len(),
                anchor: value.len(),
                handles: false,
                menu: false,
                blink: Instant::now(),
                last_tap: None,
                composing: None,
            });
        }
        let e = self.edit.as_mut()?;
        let fix = |i: usize| {
            let mut i = i.min(value.len());
            while !value.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        e.cursor = fix(e.cursor);
        e.anchor = fix(e.anchor);
        Some(e)
    }

    /// The focused field's value and its edit (after [`Session::edit_now`]).
    fn editing(&mut self) -> Option<(String, Edit)> {
        let e = self.edit_now()?.clone();
        let v = self.form.get(&e.field).cloned().unwrap_or_default();
        Some((v, e))
    }

    /// Whether `field` is a secret text field (in the frame).
    fn is_secret(&mut self, field: &str) -> bool {
        if self.scene().is_err() {
            return false;
        }
        self.scene
            .as_ref()
            .and_then(|s| s.fields.iter().find(|f| f.field == field))
            .is_some_and(|f| f.secret)
    }

    /// The field's maximum length (characters), from the frame.
    fn max_length(&mut self, field: &str) -> Option<usize> {
        self.scene().ok()?;
        self.scene
            .as_ref()?
            .fields
            .iter()
            .find(|f| f.field == field)?
            .max_length
    }

    /// Sets the edited value and where the cursor and selection are.
    fn set_edit(&mut self, value: String, cursor: usize, anchor: usize) -> Outcome {
        let Some(field) = self.edit.as_ref().map(|e| e.field.clone()) else {
            return Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            };
        };
        if let Some(e) = self.edit.as_mut() {
            e.cursor = cursor;
            e.anchor = anchor;
            e.blink = Instant::now();
            e.composing = None;
        }
        self.form.insert(field.clone(), value.clone());
        self.invalidate();
        Outcome {
            node: None,
            effects: vec![Effect::FormSet { field, value }],
        }
    }

    /// Typed or pasted text, in place of the selection (no further than
    /// the field's maximum length). Handles and the menu go, as on Android.
    pub fn insert_text(&mut self, text: &str) -> Outcome {
        let Some((value, e)) = self.editing() else {
            return Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            };
        };
        let (lo, hi) = e.range();
        let text: String = text.chars().filter(|c| *c != '\r').collect();
        let mut v = format!("{}{}{}", &value[..lo], text, &value[hi..]);
        // As the shared UI does: the value is cut to the field's length.
        if let Some(max) = self.max_length(&e.field) {
            v = v.chars().take(max).collect();
        }
        if let Some(e) = self.edit.as_mut() {
            e.handles = false;
            e.menu = false;
        }
        let at = (lo + text.len()).min(v.len());
        self.set_edit(v, at, at)
    }

    /// Backspace: the selection, else the character before the cursor.
    pub fn backspace(&mut self) -> Outcome {
        let Some((value, e)) = self.editing() else {
            return Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            };
        };
        let (lo, hi) = e.range();
        let lo = if lo == hi { prev_char(&value, lo) } else { lo };
        if let Some(e) = self.edit.as_mut() {
            e.handles = false;
            e.menu = false;
        }
        self.set_edit(format!("{}{}", &value[..lo], &value[hi..]), lo, lo)
    }

    /// Delete: the selection, else the character after the cursor.
    pub fn delete_forward(&mut self) -> Outcome {
        let Some((value, e)) = self.editing() else {
            return Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            };
        };
        let (lo, hi) = e.range();
        let hi = if lo == hi { next_char(&value, hi) } else { hi };
        self.set_edit(format!("{}{}", &value[..lo], &value[hi..]), lo, lo)
    }

    /// Moves the cursor one character left or right, or to the start or
    /// end; with `extend`, the selection grows with it.
    pub fn move_cursor(&mut self, to: CursorMove, extend: bool) {
        let Some((value, e)) = self.editing() else {
            return;
        };
        let (lo, hi) = e.range();
        let c = match to {
            CursorMove::Left if !extend && e.selected() => lo,
            CursorMove::Right if !extend && e.selected() => hi,
            CursorMove::Left => prev_char(&value, e.cursor),
            CursorMove::Right => next_char(&value, e.cursor),
            CursorMove::Home => 0,
            CursorMove::End => value.len(),
        };
        let anchor = if extend { e.anchor } else { c };
        if let Some(e) = self.edit.as_mut() {
            e.cursor = c;
            e.anchor = anchor;
            e.blink = Instant::now();
            e.menu = false;
            e.handles = false;
        }
        self.scene = None;
    }

    /// Selects the whole value (Select all, Ctrl+A).
    pub fn select_all(&mut self) {
        let Some((value, _)) = self.editing() else {
            return;
        };
        if let Some(e) = self.edit.as_mut() {
            e.anchor = 0;
            e.cursor = value.len();
            e.handles = !value.is_empty();
            e.menu = true;
        }
        self.scene = None;
    }

    /// The selected text (Copy, Ctrl+C); with `cut`, it is removed too.
    pub fn copy(&mut self, cut: bool) -> Option<String> {
        let (value, e) = self.editing()?;
        // A secret never leaves its field.
        if self.is_secret(&e.field) {
            return None;
        }
        let (lo, hi) = e.range();
        if lo == hi {
            return None;
        }
        let text = value[lo..hi].to_owned();
        if let Some(e) = self.edit.as_mut() {
            e.menu = false;
            e.handles = false;
        }
        if cut {
            self.set_edit(format!("{}{}", &value[..lo], &value[hi..]), lo, lo);
        } else {
            if let Some(e) = self.edit.as_mut() {
                e.anchor = e.cursor;
            }
            self.scene = None;
        }
        Some(text)
    }

    /// The platform's clipboard text pasted into a field, in place of its
    /// selection ([`Effect::Paste`]).
    pub fn paste(&mut self, field: &str, text: &str) -> Outcome {
        self.focused = Some(field.to_owned());
        self.insert_text(text.trim_end_matches(['\n', '\r']))
    }

    /// The focused field as the platform's keyboard sees it (`crate::ime`):
    /// its value, selection and the word being composed.
    pub fn text_state(&mut self) -> Option<crate::ime::TextState> {
        let (value, e) = self.editing()?;
        let field = e.field.clone();
        let composing = e
            .composing
            .filter(|&(a, b)| a < b && b <= value.len())
            .filter(|&(a, b)| value.is_char_boundary(a) && value.is_char_boundary(b));
        Some(crate::ime::TextState {
            field: e.field,
            value,
            cursor: e.cursor,
            anchor: e.anchor,
            composing,
            secret: self.is_secret(&field),
        })
    }

    /// The focused field as the keyboard left it (it typed, corrected,
    /// composed or moved the cursor), no longer than the field's maximum
    /// length. Handles and the menu go, as on Android.
    pub fn set_text_state(&mut self, s: &crate::ime::TextState) -> Outcome {
        if self.focused.as_ref() != Some(&s.field) || self.edit_now().is_none() {
            return Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            };
        }
        let mut value = s.value.replace('\r', "");
        if let Some(max) = self.max_length(&s.field) {
            value = value.chars().take(max).collect();
        }
        let fix = |i: usize| {
            let mut i = i.min(value.len());
            while !value.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let (cursor, anchor) = (fix(s.cursor), fix(s.anchor));
        let composing = s
            .composing
            .map(|(a, b)| (fix(a), fix(b)))
            .filter(|(a, b)| a < b);
        let changed = self.form.get(&s.field) != Some(&value);
        if let Some(e) = self.edit.as_mut() {
            e.handles = false;
            e.menu = false;
        }
        let out = if changed {
            self.set_edit(value, cursor, anchor)
        } else {
            if let Some(e) = self.edit.as_mut() {
                e.cursor = cursor;
                e.anchor = anchor;
                e.blink = Instant::now();
            }
            self.scene = None;
            Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            }
        };
        if let Some(e) = self.edit.as_mut() {
            e.composing = composing;
        }
        out
    }

    /// The caret stops of `value` drawn in field box `fb`: on its dots
    /// when the field is secret, with the value's own bytes.
    fn field_stops(
        &mut self,
        fb: &crate::scene::FieldBox,
        value: &str,
        ty: crate::style::Type,
    ) -> Vec<crate::text::CaretStop> {
        let width = Some(fb.rect[2] + 0.5);
        if fb.secret {
            let shown = crate::text::mask(value);
            let stops = self
                .builder
                .text
                .caret_stops(&shown, ty, width, self.viewport.scale);
            return crate::text::unmask_stops(value, stops);
        }
        self.builder
            .text
            .caret_stops(value, ty, width, self.viewport.scale)
    }

    /// The edit's caret stops in the frame, with the field's text box.
    fn caret_stops(
        &mut self,
        field: &str,
    ) -> Option<(crate::scene::FieldBox, Vec<crate::text::CaretStop>)> {
        self.scene().ok()?;
        let fb = self
            .scene
            .as_ref()?
            .fields
            .iter()
            .find(|f| f.field == field)?
            .clone();
        let value = self.form.get(field).cloned().unwrap_or_default();
        let ty = crate::style::Type::new(fb.face, fb.size, fb.line_height);
        let stops = self.field_stops(&fb, &value, ty);
        Some((fb, stops))
    }

    /// The byte of `field`'s value nearest a point of the screen.
    fn byte_at(&mut self, field: &str, x: f32, y: f32) -> Option<usize> {
        let (fb, stops) = self.caret_stops(field)?;
        crate::text::nearest_stop(&stops, x - fb.rect[0], y - fb.rect[1]).map(|s| s.byte)
    }

    /// A tap on a text field at a point: it is focused with the cursor
    /// there; on the field being typed into, a tap on the caret shows the
    /// menu, and a double tap selects the word.
    fn focus_at(&mut self, field: &str, x: f32, y: f32, again: bool) {
        self.focused = Some(field.to_owned());
        let at = self.byte_at(field, x, y);
        let now = Instant::now();
        let Some(e) = self.edit_now() else {
            return;
        };
        let double = again
            && e.last_tap.is_some_and(|(t, tx, ty)| {
                now.duration_since(t) < DOUBLE_TAP && (tx - x).abs() < 24.0 && (ty - y).abs() < 24.0
            });
        let on_caret = again && !e.selected() && at == Some(e.cursor);
        e.last_tap = Some((now, x, y));
        e.blink = now;
        if double {
            e.last_tap = None;
            self.select_word(field, x, y);
            return;
        }
        let Some(e) = self.edit.as_mut() else {
            return;
        };
        if on_caret {
            e.handles = true;
            e.menu = !e.menu;
            return;
        }
        let at = at.unwrap_or(e.cursor);
        e.cursor = at;
        e.anchor = at;
        e.handles = again;
        e.menu = false;
    }

    /// Selects the word at a point of a field, with handles and the menu
    /// (an empty field: the menu, to paste).
    fn select_word(&mut self, field: &str, x: f32, y: f32) {
        self.focused = Some(field.to_owned());
        let at = self.byte_at(field, x, y);
        let value = self.form.get(field).cloned().unwrap_or_default();
        let Some(e) = self.edit_now() else {
            return;
        };
        let (lo, hi) = word_at(&value, at.unwrap_or(e.cursor));
        e.anchor = lo;
        e.cursor = hi;
        e.handles = lo != hi;
        e.menu = true;
        e.blink = Instant::now();
        // Laid out again: the field is drawn focused.
        self.invalidate();
    }

    /// A finger (or the mouse button) went down at a point: a
    /// `core://Tappable` there is drawn pressed until [`Session::release_press`],
    /// so the touch shows it registered. Returns whether anything is drawn
    /// pressed (the window draws again).
    pub fn press(&mut self, x: f32, y: f32) -> bool {
        let pressed = self
            .find(&Target::Point(x, y))
            .ok()
            .flatten()
            .filter(|n| n.node_type == "core://Tappable")
            .map(|n| Pressed {
                id: n.id,
                at: (x, y),
                hover: false,
            });
        // A new touch: what an earlier one prepared is dropped.
        self.prepared = None;
        let changed = pressed != self.pressed;
        if changed {
            self.pressed = pressed;
            self.scene = None;
        }
        self.pressed.is_some()
    }

    /// While a finger rests still at a point whose tap opens a layout or a
    /// screen (`action.navigation.open`: a row, a button, an icon button),
    /// opens it out of sight and lays it out, so when the finger lifts the
    /// slide starts with nothing left to lay out. Between the press and the
    /// lift (typically 50–100 ms) nothing moves, so the time it takes
    /// doesn't show. A release that isn't a tap, another touch or new data
    /// throws it away. A screen whose data loads in the background isn't
    /// prepared (its answer is matched to the stack). True when one was
    /// prepared.
    pub fn prepare_at(&mut self, x: f32, y: f32) -> bool {
        if self.prepared.is_some() || !self.animate {
            return false;
        }
        let Some(node) = self.find(&Target::Point(x, y)).ok().flatten() else {
            return false;
        };
        let Some(Tap::Intent { intent }) = node.tap.clone() else {
            return false;
        };
        let Some(bound) = self.bind_form(&intent) else {
            return false;
        };
        if bound["intent"] != "action.navigation.open"
            || self.loads_screen_data(&bound)
            || (panel_intent(&bound) && self.panel_slot_of(&node).is_some())
        {
            return false;
        }
        let before = self.stack.len();
        let merges = self.merges;
        let scene = self.scene.take();
        let mut effects = Vec::new();
        self.run(&bound, &mut effects);
        let mut ok = self.stack.len() > before;
        for i in before..self.stack.len() {
            ok &= self.layout_entry(i).is_ok();
        }
        let entries = self.stack.split_off(before.min(self.stack.len()));
        // A screen whose data loads in the background is answered on the
        // stack: it opens on the tap, as before.
        ok &= entries.iter().all(|e| e.loading.is_none());
        // What is on screen didn't change.
        self.scene = scene;
        if ok {
            self.prepared = Some(Prepared {
                intent: bound,
                entries,
                effects,
                merged: self.merges != merges,
            });
        }
        ok
    }

    /// Whether an open goes to a screen (`target_uri`) that loads data:
    /// not worth preparing. A layout opened with a screen of its own is
    /// counted as one (its screens aren't known before it opens).
    fn loads_screen_data(&self, open: &Value) -> bool {
        let Some(uri) = open["target_uri"].as_str().filter(|t| !t.is_empty()) else {
            return false;
        };
        if open["layout"].as_str().is_some_and(|l| !l.is_empty()) {
            return true;
        }
        let top = &self.stack[self.top_index()];
        top.screens
            .iter()
            .find(|s| match_route(&s.route, uri).is_some())
            .is_none_or(|s| s.data.is_some())
    }

    /// The finger lifted, or the touch became a scroll or a swipe: nothing
    /// is drawn pressed. Returns whether something was.
    pub fn release_press(&mut self) -> bool {
        self.prepared = None;
        let was = self.pressed.take().is_some();
        if was {
            self.scene = None;
        }
        was
    }

    /// The mouse moved to a point (a pointer that hovers; touch screens
    /// never call it): the `core://Tappable` under it is drawn with a light
    /// wash. Returns whether that changed (the window draws again).
    pub fn hover(&mut self, x: f32, y: f32) -> bool {
        let hovered = self
            .find(&Target::Point(x, y))
            .ok()
            .flatten()
            .filter(|n| n.node_type == "core://Tappable")
            .map(|n| Pressed {
                id: n.id,
                at: (x, y),
                hover: true,
            });
        let changed = hovered.as_ref().map(|h| &h.id) != self.hovered.as_ref().map(|h| &h.id);
        self.hovered = hovered;
        if changed {
            self.scene = None;
        }
        changed
    }

    /// The mouse left the window: nothing is hovered. Returns whether
    /// something was.
    pub fn hover_end(&mut self) -> bool {
        let was = self.hovered.take().is_some();
        if was {
            self.scene = None;
        }
        was
    }

    /// Whether the mouse rests over something tappable (the pointer shows a
    /// hand).
    pub fn hovering(&self) -> bool {
        self.hovered.is_some()
    }

    /// What is drawn pressed (tests and the simulator read it).
    pub fn pressed(&self) -> Option<&Pressed> {
        self.pressed.as_ref()
    }

    /// A long press at a point: on a text field, the word there is selected
    /// (Cut, Copy, Paste and Select all show); elsewhere the
    /// `core://Gesture` there runs its `long_press`.
    pub fn long_press(&mut self, x: f32, y: f32) -> Result<Outcome, String> {
        let hit = self.find(&Target::Point(x, y))?;
        if let Some(n) = &hit
            && let Some(Tap::Focus { field }) = &n.tap
        {
            let field = field.clone();
            self.select_word(&field, x, y);
            return Ok(Outcome {
                node: hit.clone(),
                effects: vec![Effect::Focused { field }],
            });
        }
        self.gesture(x, y, "long_press")
    }

    /// The edit handle under a point, if any: dragging it moves the caret
    /// or that end of the selection.
    pub fn grab_at(&mut self, x: f32, y: f32) -> Option<Grab> {
        let _ = self.scene().ok()?;
        let (g, r) = self.handles.iter().rev().find(|(_, r)| {
            x >= r[0] - HANDLE_SLOP
                && x < r[0] + r[2] + HANDLE_SLOP
                && y >= r[1] - HANDLE_SLOP
                && y < r[1] + r[3] + HANDLE_SLOP
        })?;
        let (g, r) = (*g, *r);
        // The finger stays this far below the text point it moves.
        self.grab = Some((g, (y - r[1]) + HANDLE / 2.0 + 10.0));
        if let Some(e) = self.edit.as_mut() {
            e.menu = false;
        }
        Some(g)
    }

    /// Starts dragging the end of the selection a long press just made
    /// (dragging on selects more, as on Android).
    pub fn grab_selection_end(&mut self) -> bool {
        if self.edit.as_ref().is_some_and(Edit::selected) {
            self.grab = Some((Grab::End, 0.0));
            true
        } else {
            false
        }
    }

    /// The grabbed handle follows the finger.
    pub fn drag_grab(&mut self, x: f32, y: f32) {
        let Some((g, dy)) = self.grab else {
            return;
        };
        let Some(field) = self.edit.as_ref().map(|e| e.field.clone()) else {
            return;
        };
        let Some(at) = self.byte_at(&field, x, y - dy) else {
            return;
        };
        let Some(e) = self.edit.as_mut() else {
            return;
        };
        let (lo, hi) = e.range();
        match g {
            Grab::Caret => {
                e.cursor = at;
                e.anchor = at;
            }
            // A selection keeps at least one character.
            Grab::Start => {
                if at < hi {
                    e.anchor = hi;
                    e.cursor = at;
                }
            }
            Grab::End => {
                if at > lo {
                    e.anchor = lo;
                    e.cursor = at;
                }
            }
        }
        e.blink = Instant::now();
        self.scene = None;
    }

    /// The handle is let go: a selection shows the menu again.
    pub fn release_grab(&mut self) {
        self.grab = None;
        if let Some(e) = self.edit.as_mut() {
            e.menu = e.selected();
            e.handles = true;
        }
        self.scene = None;
    }

    /// When the caret next blinks, or a `date` conditional on screen next
    /// changes its answer, for a window to draw then.
    pub fn next_wake(&self) -> Option<Instant> {
        let clock = self.changes_at().and_then(|m| self.builder.clock.until(m));
        let clock = clock.map(|d| Instant::now() + d);
        let caret = self.edit.as_ref().and_then(|e| {
            if self.focused.as_ref() != Some(&e.field) || e.selected() {
                return None;
            }
            let since = e.blink.elapsed().as_millis() / BLINK.as_millis();
            Some(e.blink + BLINK * (since as u32 + 1))
        });
        match (clock, caret) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The clock frames read (`format` dates, `date` conditionals): this
    /// device's unless set, e.g. fixed for a simulator run or in a pod's
    /// zone for its previews.
    pub fn set_clock(&mut self, clock: crate::dates::Clock) {
        self.builder.clock = clock;
        for e in &mut self.stack {
            e.laid = None;
            e.stale = None;
        }
        self.scene = None;
    }

    /// The first minute at which a laid-out layout's `date` conditional
    /// changes its answer.
    fn changes_at(&self) -> Option<i64> {
        self.stack
            .iter()
            .filter_map(|e| e.laid.as_ref()?.0.changes_at)
            .min()
    }

    /// Lays out again the layouts whose `date` conditionals changed their
    /// answer since they were laid out.
    fn follow_clock(&mut self) {
        if self.changes_at().is_none() {
            return;
        }
        let now = self.builder.clock.minute();
        for e in &mut self.stack {
            if e.laid
                .as_ref()
                .is_some_and(|(l, _)| l.changes_at.is_some_and(|c| c <= now))
            {
                e.relay();
                self.scene = None;
            }
        }
    }

    /// Whether the caret shows now (it blinks).
    fn caret_on(&self) -> Option<bool> {
        let e = self.edit.as_ref()?;
        if self.focused.as_ref() != Some(&e.field) || e.selected() {
            return None;
        }
        Some((e.blink.elapsed().as_millis() / BLINK.as_millis()).is_multiple_of(2))
    }

    /// The carousel under a point, if it has more than one page.
    pub fn pager_at(&mut self, x: f32, y: f32) -> Option<String> {
        let scene = self.scene().ok()?;
        let id = scene
            .semantics
            .iter()
            .rev()
            .find(|n| {
                let [bx, by, bw, bh] = n.bounds;
                n.node_type == "core://Carousel" && x >= bx && x < bx + bw && y >= by && y < by + bh
            })?
            .id
            .clone();
        let (_, count) = self.pager(&id)?;
        // A tap-paged carousel leaves drags to what is around it.
        let ti = self.top_index();
        if self.stack[ti]
            .laid
            .as_ref()
            .is_some_and(|(l, _)| l.tap_pagers.contains_key(&id))
        {
            return None;
        }
        (count > 1).then_some(id)
    }

    /// A carousel's page width and count, in the top layout.
    fn pager(&self, id: &str) -> Option<(f32, usize)> {
        let top = &self.stack[self.top_index()];
        top.laid.as_ref()?.0.pagers.get(id).copied()
    }

    /// Where a carousel's track is (points scrolled left).
    pub fn pager_offset(&self, id: &str) -> f32 {
        let top = &self.stack[self.top_index()];
        top.scroll
            .get(&format!("pager:{id}"))
            .copied()
            .unwrap_or(0.0)
    }

    /// Drags a carousel's track to `offset` (points from its first page).
    pub fn drag_pager(&mut self, id: &str, offset: f32) {
        let Some((w, count)) = self.pager(id) else {
            return;
        };
        self.pager_anims.remove(id);
        let max = w * (count.saturating_sub(1)) as f32;
        let ti = self.top_index();
        self.stack[ti]
            .scroll
            .insert(format!("pager:{id}"), offset.clamp(0.0, max));
        self.scene = None;
    }

    /// Ends a carousel drag with its release velocity (points per second,
    /// positive: the finger moved right): it slides to the next or previous
    /// page on a fling, else to the nearest.
    pub fn release_pager(&mut self, id: &str, vx: f32) {
        let Some((w, count)) = self.pager(id) else {
            return;
        };
        let w = w.max(1.0);
        let at = self.pager_offset(id);
        let f = at / w;
        let page = if vx < -PAGE_FLING {
            f.ceil()
        } else if vx > PAGE_FLING {
            f.floor()
        } else {
            f.round()
        };
        let page = (page.max(0.0) as usize).min(count - 1);
        self.pager_anims
            .insert(id.to_owned(), (Instant::now(), at, page as f32 * w, page));
        self.scene = None;
        if !self.animate {
            self.step_pagers(Instant::now() + PAGE_SLIDE);
        }
    }

    /// The `core://SwipeCard` under a point (the innermost).
    pub fn card_at(&mut self, x: f32, y: f32) -> Option<String> {
        let scene = self.scene().ok()?;
        let id = scene
            .semantics
            .iter()
            .rev()
            .find(|n| {
                let [bx, by, bw, bh] = n.bounds;
                n.node_type == "core://SwipeCard"
                    && x >= bx
                    && x < bx + bw
                    && y >= by
                    && y < by + bh
            })?
            .id
            .clone();
        Some(id)
    }

    /// How far a swipe card is moved right of its place (points).
    pub fn card_offset(&self, id: &str) -> f32 {
        let top = &self.stack[self.top_index()];
        top.scroll
            .get(&format!("card:{id}"))
            .copied()
            .unwrap_or(0.0)
    }

    /// Moves a swipe card to `offset` (points right of its place): it
    /// follows the finger, and stops on its way off or back.
    pub fn drag_card(&mut self, id: &str, offset: f32) {
        self.card_anims.remove(id);
        let ti = self.top_index();
        self.stack[ti].scroll.insert(format!("card:{id}"), offset);
        self.scene = None;
    }

    /// Lets go of a swipe card with its release velocity (points per
    /// second, positive: right). Past its threshold, or flung that way past
    /// half of it, toward a side it has an intent for, it flies off and the
    /// intent runs; else it springs back. Without animations it is there at
    /// once and the outcome is returned; with them it comes from
    /// [`Session::take_swiped`] once the card is off.
    pub fn release_card(&mut self, id: &str, vx: f32) -> Option<Outcome> {
        let at = self.card_offset(id);
        let top = &self.stack[self.top_index()];
        let (threshold, width) = top
            .laid
            .as_ref()
            .and_then(|(l, _)| l.cards.get(id).copied())
            .unwrap_or((crate::layout::CARD_THRESHOLD, self.viewport.width));
        let past = at.abs() >= threshold
            || (at.abs() >= threshold / 2.0 && vx.abs() > CARD_FLING && vx.signum() == at.signum());
        let kind = if at > 0.0 {
            "swipe_right"
        } else {
            "swipe_left"
        };
        let commit = past
            .then(|| self.scene().ok())
            .flatten()
            .and_then(|s| s.semantics.iter().find(|n| n.id == id).cloned())
            .and_then(|n| {
                let intent = n.gestures.get(kind)?.clone();
                Some((n, intent))
            });
        // Off past the screen's edge, however far along the card is.
        let to = match commit {
            Some(_) => at.signum() * (self.viewport.width + width),
            None => 0.0,
        };
        let since = Instant::now();
        self.card_anims.insert(
            id.to_owned(),
            CardAnim {
                since,
                from: at,
                to,
                commit,
            },
        );
        self.scene = None;
        if self.animate {
            return None;
        }
        self.step_cards(since + CARD_SPRING.max(CARD_FLY));
        self.take_swiped().pop()
    }

    /// Runs the intents of the cards that flew off since last asked: what
    /// they did, as a swipe's [`Session::gesture`] says.
    pub fn take_swiped(&mut self) -> Vec<Outcome> {
        let swiped = std::mem::take(&mut self.swiped);
        swiped
            .into_iter()
            .map(|(id, node, intent)| {
                // Back in its place, where the next card of a deck shows.
                let ti = self.top_index();
                self.stack[ti].scroll.remove(&format!("card:{id}"));
                self.clipboard_ok = false;
                let mut effects = Vec::new();
                match self.bind_form(&intent) {
                    Some(bound) => self.run(&bound, &mut effects),
                    None => effects.push(Effect::Nothing),
                }
                self.invalidate();
                Outcome {
                    node: Some(node),
                    effects,
                }
            })
            .collect()
    }

    /// Moves released swipe cards on. One that is off is put back in its
    /// place (the card after it in a deck shows there) and its intent is
    /// due.
    fn step_cards(&mut self, now: Instant) {
        if self.card_anims.is_empty() {
            return;
        }
        let ti = self.top_index();
        let mut done = Vec::new();
        for (id, a) in &self.card_anims {
            let (length, ease): (Duration, fn(f32) -> f32) = match a.commit {
                Some(_) => (CARD_FLY, ease_in),
                None => (CARD_SPRING, ease_out_back),
            };
            let t = (now.duration_since(a.since).as_secs_f32() / length.as_secs_f32()).min(1.0);
            let key = format!("card:{id}");
            let e = &mut self.stack[ti];
            if t >= 1.0 && a.commit.is_none() {
                e.scroll.remove(&key);
            } else {
                // One that flew off stays off until its intent has run.
                e.scroll.insert(key, a.from + (a.to - a.from) * ease(t));
            }
            if t >= 1.0 {
                done.push(id.clone());
            }
        }
        for id in &done {
            if let Some(CardAnim {
                commit: Some(c), ..
            }) = self.card_anims.remove(id)
            {
                self.swiped.push((id.clone(), c.0, c.1));
            }
        }
        self.scene = None;
    }

    /// The sideways-scrolling row (a chip row) under a point, if its
    /// content overflows it.
    pub fn slide_at(&mut self, x: f32, y: f32) -> Option<String> {
        let under: Vec<String> = self
            .scene()
            .ok()?
            .semantics
            .iter()
            .rev()
            .filter(|n| {
                let [bx, by, bw, bh] = n.bounds;
                x >= bx && x < bx + bw && y >= by && y < by + bh
            })
            .map(|n| n.id.clone())
            .collect();
        let top = &self.stack[self.top_index()];
        let slides = &top.laid.as_ref()?.0.slides;
        under
            .into_iter()
            .find(|id| slides.get(id).is_some_and(|max| *max > 0.0))
    }

    /// Where a sideways-scrolling row is (points scrolled left; within its
    /// last layout's extent).
    pub fn slide_offset(&self, id: &str) -> f32 {
        let top = &self.stack[self.top_index()];
        let max = top
            .laid
            .as_ref()
            .and_then(|(l, _)| l.slides.get(id).copied())
            .unwrap_or(f32::MAX);
        top.scroll
            .get(&format!("slide:{id}"))
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, max)
    }

    /// Scrolls a sideways-scrolling row to `offset` (points from its start);
    /// true if it moved.
    pub fn drag_slide(&mut self, id: &str, offset: f32) -> bool {
        // Laid out first: a tap may have left the layout to be rebuilt.
        if self.scene().is_err() {
            return false;
        }
        let before = self.slide_offset(id);
        let ti = self.top_index();
        let Some(max) = self.stack[ti]
            .laid
            .as_ref()
            .and_then(|(l, _)| l.slides.get(id).copied())
        else {
            return false;
        };
        let to = offset.clamp(0.0, max);
        if to == before {
            return false;
        }
        self.stack[ti].scroll.insert(format!("slide:{id}"), to);
        self.scene = None;
        true
    }

    /// Moves sliding carousels on; one that arrives shows its page number.
    fn step_pagers(&mut self, now: Instant) {
        if self.pager_anims.is_empty() {
            return;
        }
        let ti = self.top_index();
        let mut done = Vec::new();
        for (id, (at, from, to, page)) in &self.pager_anims {
            let t = now.duration_since(*at).as_secs_f32() / PAGE_SLIDE.as_secs_f32();
            let x = from + (to - from) * fast_out_slow_in(t);
            let e = &mut self.stack[ti];
            e.scroll.insert(format!("pager:{id}"), x);
            if t >= 1.0 {
                e.scroll.insert(format!("page:{id}"), *page as f32);
                done.push(id.clone());
            }
        }
        for id in &done {
            self.pager_anims.remove(id);
        }
        if !done.is_empty() {
            // The badge and dots show the new page.
            self.stack[ti].relay();
        }
        self.scene = None;
    }

    /// The top sheet or panel a drag starting at a point would move: a
    /// sheet by its handle strip, a panel anywhere on it.
    pub fn frame_drag_at(&self, x: f32, y: f32) -> Option<DragAxis> {
        let top = self.top_index();
        if top == 0 {
            return None;
        }
        let g = self.group_start(top);
        let Frame::Edge(edge) = self.stack[g].frame else {
            return None;
        };
        let (f, _, _) = self.frame_rects(g);
        let inside_x = x >= f[0] && x < f[0] + f[2];
        match edge {
            protocol::Edge::Bottom => {
                (inside_x && y >= f[1] && y < f[1] + HANDLE_GRAB).then_some(DragAxis::Vertical)
            }
            protocol::Edge::Top => (inside_x && y < f[1] + f[3] && y >= f[1] + f[3] - HANDLE_GRAB)
                .then_some(DragAxis::Vertical),
            protocol::Edge::Start | protocol::Edge::End => {
                (inside_x && y >= f[1] && y < f[1] + f[3]).then_some(DragAxis::Horizontal)
            }
        }
    }

    /// The top sheet or panel's edge and its size along the drag.
    fn top_frame(&self) -> Option<(protocol::Edge, f32)> {
        let top = self.top_index();
        let g = self.group_start(top);
        match self.stack[g].frame {
            Frame::Edge(edge) if top > 0 => {
                let (f, _, _) = self.frame_rects(g);
                let size = match edge {
                    protocol::Edge::Bottom | protocol::Edge::Top => f[3],
                    _ => f[2],
                };
                Some((edge, size.max(1.0)))
            }
            _ => None,
        }
    }

    /// Moves the top sheet or panel: `dx`, `dy` since the drag started.
    pub fn drag_frame(&mut self, dx: f32, dy: f32) {
        let Some((edge, size)) = self.top_frame() else {
            return;
        };
        let along = match edge {
            protocol::Edge::Bottom => dy,
            protocol::Edge::Top => -dy,
            protocol::Edge::Start => -dx,
            protocol::Edge::End => dx,
        };
        self.frame_offset = along.clamp(0.0, size);
        self.frame_settle = None;
        self.scene = None;
    }

    /// Ends a drag of the top sheet or panel, with the release velocity
    /// (points per second, `vx`, `vy`): it closes past a quarter of its size
    /// or on a fling toward its edge, else settles back. True if it closed.
    pub fn release_frame(&mut self, vx: f32, vy: f32) -> bool {
        let Some((edge, size)) = self.top_frame() else {
            self.frame_offset = 0.0;
            return false;
        };
        let v = match edge {
            protocol::Edge::Bottom => vy,
            protocol::Edge::Top => -vy,
            protocol::Edge::Start => -vx,
            protocol::Edge::End => vx,
        };
        let off = self.frame_offset;
        self.scene = None;
        if off > size * CLOSE_FRACTION || (off > 0.0 && v > CLOSE_FLING) {
            let top = self.top_index();
            let g = self.group_start(top);
            self.frame_offset = 0.0;
            self.focused = None;
            if !self.animate {
                self.stack.truncate(g.max(1));
                return true;
            }
            // Close from where it is: start the slide out as far along as
            // the drag already took it.
            let done = off / size;
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..24 {
                let mid = (lo + hi) / 2.0;
                if fast_out_slow_in(mid) < done {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let now = Instant::now();
            let started = now.checked_sub(SLIDE.mul_f32(lo)).unwrap_or(now);
            self.stack.truncate(top + 1);
            for e in &mut self.stack[g..] {
                e.closing = Some(started);
            }
            return true;
        }
        if off > 0.0 {
            self.frame_settle = Some((Instant::now(), off));
        }
        self.frame_offset = 0.0;
        false
    }

    /// The top sheet or panel's drag offset now (settling back after a
    /// release).
    fn frame_offset_now(&self, now: Instant) -> f32 {
        match self.frame_settle {
            Some((at, from)) => {
                let t = now.duration_since(at).as_secs_f32() / SETTLE.as_secs_f32();
                if t >= 1.0 {
                    0.0
                } else {
                    from * (1.0 - fast_out_slow_in(t))
                }
            }
            None => self.frame_offset,
        }
    }

    /// Applies what the loader heard of ([`Loader::changes`]) and the
    /// answers of writes that came back: the pod's fresh data, or the local
    /// change undone. True when anything changed.
    ///
    /// While a screen slides in or out nothing on it changes: what comes
    /// in is kept aside and applied, and laid out, on the first frame after
    /// the slide ends (decision `hold-changes-during-transitions`; laying
    /// every screen out again mid-slide drops frames on a phone). Only a
    /// picked photo, the user's own choice, applies at once, like the
    /// user's taps and local writes, which never wait for a poll.
    pub fn poll(&mut self) -> bool {
        self.restore_kept();
        let news = self.loader.as_ref().and_then(|l| l.changes());
        if let Some(g) = self.loader.as_ref().and_then(|l| l.edges()) {
            self.held_edges = Some(g);
        }
        let mut arrived: Vec<Answer> = match self.answered.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(p) => std::mem::take(&mut *p.into_inner()),
        };
        let mut changes = self.held.take();
        if let Some(c) = news {
            match changes.as_mut() {
                Some(h) => h.absorb(c),
                None => changes = Some(c),
            }
        }
        if self.sliding(Instant::now()) {
            self.held = changes.take();
            let (picked, rest): (Vec<Answer>, Vec<Answer>) = arrived
                .into_iter()
                .partition(|a| matches!(a, Answer::Picked(_)));
            self.held_answers.extend(rest);
            if picked.is_empty() {
                return false;
            }
            arrived = picked;
        } else if !self.held_answers.is_empty() {
            let mut done = std::mem::take(&mut self.held_answers);
            done.append(&mut arrived);
            arrived = done;
        }
        // A new root waits for a touch or the keyboard to finish; data
        // doesn't (decision `content-addressed-construct`).
        if self.touching() {
            if let Some(roots) = changes.as_mut().and_then(Changes::take_roots) {
                match self.held_roots.as_mut() {
                    Some(h) => h.absorb(roots),
                    None => self.held_roots = Some(roots),
                }
            }
        } else if let Some(roots) = self.held_roots.take() {
            match changes.as_mut() {
                Some(c) => {
                    let mut all = roots;
                    all.absorb(std::mem::take(c));
                    *c = all;
                }
                None => changes = Some(roots),
            }
        }
        let mut changed = changes.is_some();
        if let Some(c) = changes {
            self.apply_changes(c);
        }
        // The edge graph goes with the roots: after them, never mid-slide or
        // mid-touch.
        if !self.touching()
            && !self.sliding(Instant::now())
            && self.held_roots.is_none()
            && let Some(g) = self.held_edges.take()
        {
            let before = self.edges.replace(g);
            self.cut_back(before.as_ref());
            self.invalidate();
            changed = true;
        }
        let done = arrived;
        if done.is_empty() {
            return changed;
        }
        for answer in done {
            let (undo, w) = match answer {
                Answer::Write(undo, w) => (undo, w),
                Answer::Staged { field, max, result } => {
                    self.staged(field, max, result);
                    continue;
                }
                Answer::Picked(files) => {
                    self.dialog_closed(files);
                    continue;
                }
                Answer::Data { path, result } => {
                    let error = result.as_ref().err().cloned();
                    if let Ok(d) = result {
                        self.merge_data(d);
                    }
                    for e in &mut self.stack {
                        if e.loading.as_deref() == Some(path.as_str()) {
                            e.loading = None;
                            e.error = error.clone();
                        }
                    }
                    continue;
                }
            };
            self.pending = self.pending.saturating_sub(1);
            match w.result {
                Ok(_) => {
                    for d in w.refreshed {
                        self.merge_data(d);
                    }
                }
                Err(e) => {
                    eprintln!("construct-render: write refused: {e}");
                    self.notice = Some(notice_text(&e));
                    self.last_error = Some(e);
                    match undo {
                        Undo::Fields { uri, before } => {
                            let entity = self.data.entities.entry(uri).or_default();
                            for (f, v) in before {
                                entity.set(&f, v);
                            }
                        }
                        Undo::Created { uri, query_key } => {
                            if let Some(q) = self.data.queries.get_mut(&query_key) {
                                q.retain(|u| *u != uri);
                            }
                            self.data.entities.remove(&uri);
                        }
                        Undo::Nothing => {}
                    }
                }
            }
        }
        self.invalidate();
        true
    }

    /// The loader's news: its data replaces the device's, and a layout with
    /// a new root shows it where it is open (the base of the stack, and every
    /// entry still showing its old root; its screens stay as they are).
    fn apply_changes(&mut self, c: Changes) {
        let changed = merge_changed(&mut self.data, c.data);
        if !changed && c.layouts.is_empty() {
            // Nothing drawn can differ: the frame stays as it is.
            return;
        }
        for (id, root) in c.layouts {
            let old = self.layouts.insert(id.clone(), root.clone());
            for (i, e) in self.stack.iter_mut().enumerate() {
                let showing = i == 0 || old.as_ref() == Some(&e.root);
                if e.id == id && showing && e.root != root {
                    let fresh = Entry::layout(&id, root.clone(), BTreeMap::new());
                    e.root = fresh.root;
                    e.screens = fresh.screens;
                }
            }
        }
        self.work.data_changes = self.work.data_changes.saturating_add(1);
        self.invalidate();
    }

    /// Whether a finger is down or a field is being typed in.
    fn touching(&self) -> bool {
        self.pressed.is_some() || self.grab.is_some() || self.focused.is_some()
    }

    /// After a new edge graph: at the first entry the one below it reached
    /// before and doesn't now, the stack is cut back, each entry above
    /// closing with its own transition (decision
    /// `content-addressed-construct`). Entries opened some other way (a
    /// notification) are left alone.
    fn cut_back(&mut self, before: Option<&Edges>) {
        let (Some(now), Some(before)) = (self.edges.as_ref(), before) else {
            return;
        };
        let top = self.top_index();
        let mut presenter = self.stack[0].id.clone();
        for i in 1..=top {
            let e = &self.stack[i];
            if e.closing.is_some() || e.screen {
                continue;
            }
            let reaches = |g: &Edges| g.get(&presenter).is_some_and(|r| r.contains(&e.id));
            if reaches(before) && !reaches(now) {
                self.close_from(i);
                return;
            }
            presenter = e.id.clone();
        }
    }

    /// Close every entry from `from` up (Back's transitions; at once
    /// without animation).
    fn close_from(&mut self, from: usize) {
        let top = self.top_index();
        if from == 0 || from > top {
            return;
        }
        let now = Instant::now();
        self.stack.truncate(top + 1);
        if self.animate {
            for e in &mut self.stack[from..] {
                e.closing.get_or_insert(now);
            }
        } else {
            self.stack.truncate(from.max(1));
        }
        self.focused = None;
    }

    /// Whether writes or loads are still out (a script waits for them).
    pub fn busy(&self) -> bool {
        self.pending > 0 || self.staging > 0 || self.stack.iter().any(|e| e.loading.is_some())
    }

    /// Waits until nothing is out, or `timeout` (scripts and tests).
    pub fn settle(&mut self, timeout: Duration) {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            self.poll();
            if !self.busy() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sends a write through the loader; its answer is applied by [`Session::poll`].
    fn send(&mut self, write: Write, undo: Undo, out: &mut Vec<Effect>) {
        let path = write.path.clone();
        let Some(loader) = self.loader.as_ref() else {
            out.push(Effect::NotSent {
                intent: json!({ "path": write.path, "body": write.body }),
            });
            return;
        };
        let answered = self.answered.clone();
        let images = self.images.clone();
        let body = write.body.clone();
        let done: Done = Box::new(move |w: Written| {
            if let Ok(mut g) = answered.lock() {
                g.push(Answer::Write(undo, w));
            }
            images.wake();
        });
        if loader.send(write, done) {
            self.pending += 1;
            out.push(Effect::Sent { path });
        } else {
            out.push(Effect::NotSent {
                intent: json!({ "path": path, "body": body }),
            });
        }
    }

    /// Hands a write to a loader that keeps it until the pod has it
    /// ([`Loader::take`]); true when it took it.
    fn taken(&mut self, intent: &Value, out: &mut Vec<Effect>) -> bool {
        if !self.loader.as_ref().is_some_and(|l| l.take(intent)) {
            return false;
        }
        let path = intent["endpoint"]
            .as_str()
            .unwrap_or("/v1/notifications/read")
            .to_owned();
        out.push(Effect::Sent { path });
        true
    }

    /// The ids of the layouts on the stack (each once), to load again after
    /// a write that adds to their lists.
    fn stack_layouts(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for e in &self.stack {
            if e.closing.is_none() && !ids.contains(&e.id) {
                ids.push(e.id.clone());
            }
        }
        ids
    }

    /// `$form.<f>`, `$form?.<f>` and `$nonce` bound in every string of an
    /// intent but its `value_expr`s, as the shared UI's `bindForm`: one nonce
    /// for the whole tap; `None` (nothing runs) when a `$form.<f>` is blank.
    /// A string that is exactly a token of a list value (`["a","b"]`)
    /// becomes that list.
    pub fn bind_form(&self, intent: &Value) -> Option<Value> {
        let text = intent.to_string();
        if !text.contains("$form.") && !text.contains("$form?.") && !text.contains("$nonce") {
            return Some(intent.clone());
        }
        let nonce = new_nonce();
        let mut missing = false;
        let bound = bind_form_value(intent, None, &self.form, &nonce, &mut missing);
        (!missing).then_some(bound)
    }

    /// The id of the layout on top of the stack.
    pub fn current(&self) -> &str {
        &self.stack[self.top_index()].id
    }

    /// The `$route` of the layout or screen taps go to.
    pub fn top_route(&self) -> &BTreeMap<String, String> {
        &self.stack[self.top_index()].route
    }

    /// The entry taps go to: the last one not closing.
    fn top_index(&self) -> usize {
        self.stack
            .iter()
            .rposition(|e| e.closing.is_none())
            .unwrap_or(0)
    }

    /// How many times system Back would close something before it reaches
    /// home: the entries over it and the panels open beside them. The web
    /// client keeps one browser history entry for each, so the browser's
    /// Back steps through them before it leaves the page.
    pub fn back_steps(&self) -> usize {
        let top = self.top_index();
        let panels: usize = self.stack[..=top]
            .iter()
            .filter(|e| e.closing.is_none())
            .map(|e| {
                e.panel_hosts
                    .iter()
                    .filter_map(|h| e.panels.get(h))
                    .map(Vec::len)
                    .sum::<usize>()
            })
            .sum();
        top + panels
    }

    /// The first entry of the sheet or panel `i` is drawn in (`i` itself
    /// unless it is a screen opened inside one).
    fn group_start(&self, mut i: usize) -> usize {
        while i > 0 && self.stack[i].in_frame {
            i -= 1;
        }
        i
    }

    /// A frame laid layouts out for `took` (a screen opening lays itself
    /// out on its first frame): slides under way start that much later, so
    /// the next frame goes on from where this one draws them instead of
    /// jumping ahead by the layout's time.
    fn hold_slides(&mut self, now: Instant, took: Duration) {
        if !self.animate {
            return;
        }
        for e in self.stack.iter_mut().skip(1) {
            let stamp = e.closing.as_mut().unwrap_or(&mut e.opened);
            if now.duration_since(*stamp) < SLIDE {
                *stamp += took;
            }
        }
    }

    /// How far entry `i` has slid in (0 to 1), or out when closing.
    fn progress(&self, i: usize, now: Instant) -> f32 {
        if !self.animate || i == 0 {
            return if self.stack[i].closing.is_some() {
                0.0
            } else {
                1.0
            };
        }
        let e = &self.stack[i];
        let t = |since: Instant| now.duration_since(since).as_secs_f32() / SLIDE.as_secs_f32();
        match e.closing {
            Some(c) => 1.0 - fast_out_slow_in(t(c)),
            None => fast_out_slow_in(t(e.opened)),
        }
    }

    /// Whether a layout or screen is sliding in or out at `now` (what
    /// [`Session::poll`] holds changes for).
    fn sliding(&self, now: Instant) -> bool {
        self.animate
            && self
                .stack
                .iter()
                .skip(1)
                // A closing entry until a frame has taken it off: else the
                // frame drawn just before its slide ended stays on screen.
                .any(|e| e.closing.is_some() || now.duration_since(e.opened) < SLIDE)
    }

    /// Whether a slide is still moving, or changes held during one wait
    /// for the frame after it (draw again next frame).
    pub fn animating(&self) -> bool {
        if !self.animate {
            return false;
        }
        let now = Instant::now();
        if self.spinning
            || self.held.is_some()
            || !self.held_answers.is_empty()
            || !self.pager_anims.is_empty()
            || !self.tap_pages.is_empty()
            || !self.card_anims.is_empty()
            || !self.swiped.is_empty()
            || self
                .frame_settle
                .is_some_and(|(at, _)| now.duration_since(at) < SETTLE)
        {
            return true;
        }
        self.sliding(now)
    }

    /// The box a sheet or panel group draws in, and the box and insets its
    /// layout gets (points, settled: no slide).
    fn frame_rects(&self, i: usize) -> ([f32; 4], [f32; 4], [f32; 4]) {
        let g = self.group_start(i);
        let (w, h) = (self.viewport.width, self.viewport.height);
        let [it, _, ib, _] = self.insets;
        let e = &self.stack[g];
        match e.frame {
            Frame::Full | Frame::Dialog => ([0.0, 0.0, w, h], [0.0, 0.0, w, h], self.insets),
            Frame::Edge(edge) => match edge {
                protocol::Edge::Bottom => {
                    let sh = e.size.unwrap_or(h * SHEET_HEIGHT_FRACTION).min(h);
                    let f = [0.0, h - sh, w, sh];
                    (
                        f,
                        [0.0, h - sh + HANDLE_STRIP, w, sh - HANDLE_STRIP],
                        [0.0, 0.0, ib, 0.0],
                    )
                }
                protocol::Edge::Top => {
                    let sh = e.size.unwrap_or(h * SHEET_HEIGHT_FRACTION).min(h);
                    (
                        [0.0, 0.0, w, sh],
                        [0.0, 0.0, w, sh - HANDLE_STRIP],
                        [it, 0.0, 0.0, 0.0],
                    )
                }
                protocol::Edge::Start | protocol::Edge::End => {
                    let pw = e
                        .size
                        .unwrap_or((w * PANEL_WIDTH_FRACTION).min(PANEL_MAX_WIDTH))
                        .min(w);
                    let x = if edge == protocol::Edge::Start {
                        0.0
                    } else {
                        w - pw
                    };
                    let f = [x, 0.0, pw, h];
                    (f, f, [it, 0.0, ib, 0.0])
                }
            },
        }
    }

    /// Lays out entry `i` for its box, with its device data.
    fn layout_entry(&mut self, i: usize) -> Result<(), RenderError> {
        let (_, content, insets) = self.frame_rects(i);
        let size = [content[2], content[3]];
        if self.stack[i].laid.as_ref().is_some_and(|(_, s)| *s == size) {
            return Ok(());
        }
        self.stack[i].relay();
        let e = &self.stack[i];
        let group = &self.stack[self.group_start(i)];
        let presentation = if i == 0 {
            "root"
        } else if e.in_frame {
            "screen"
        } else {
            match e.frame {
                Frame::Full => "push",
                Frame::Dialog => "dialog",
                Frame::Edge(protocol::Edge::Bottom | protocol::Edge::Top) => "sheet",
                Frame::Edge(_) => "panel",
            }
        };
        let mut nav = protocol::EntityRecord::new();
        nav.set("can_go_back", i > 0);
        nav.set(
            "pushed",
            presentation == "push" || (presentation == "screen" && group.frame == Frame::Full),
        );
        nav.set("inline", false);
        nav.set("in_frame", group.frame != Frame::Full);
        nav.set("presentation", presentation);
        self.data.entities.insert("local://nav".into(), nav);
        let mut screen = protocol::EntityRecord::new();
        screen.set("loading", e.loading.is_some());
        screen.set("error", e.error.clone().unwrap_or_default());
        self.data.entities.insert("local://screen".into(), screen);
        self.set_device_size();
        let vp = Viewport {
            width: size[0],
            height: size[1],
            scale: self.viewport.scale,
        };
        let mut prev = self.stack[i].stale.take();
        // On the construct's edge graph each scroll keeps its first visible
        // item where it was (decision `content-addressed-construct`).
        let mut anchors = match (&prev, self.edges.is_some()) {
            (Some(p), true) => anchors_of(p, &self.stack[i].scroll),
            _ => BTreeMap::new(),
        };
        // Where the inline layouts were drawn: a view's state is kept by
        // its path, which an edit of the layout embedding it can move.
        let mut embeds = prev.as_ref().map(Laid::embeds);
        let started = Instant::now();
        let laid = loop {
            let e = &self.stack[i];
            let laid = self.builder.relayout(
                &Input {
                    root: &e.root,
                    data: &self.data,
                    form: &self.form,
                    route: &e.route,
                    layouts: &self.layouts,
                    scroll: &e.scroll,
                    viewport: vp,
                    dark: self.dark,
                    item: e.item.as_deref(),
                    insets,
                    focused: self.focused.as_deref(),
                    panels: &e.panels,
                },
                prev.take(),
            )?;
            self.work.laid_out = self.work.laid_out.saturating_add(1);
            let moves = embeds
                .take()
                .map(|b| moved_embeds(&b, &laid.embeds()))
                .unwrap_or_default();
            if moves.is_empty() {
                break laid;
            }
            // An embed moved: its scroll offsets, anchors and panels follow
            // it, and it is laid out once more with them.
            let e = &mut self.stack[i];
            move_keys(&mut e.scroll, &moves);
            move_keys(&mut e.panels, &moves);
            move_keys(&mut anchors, &moves);
            prev = Some(laid);
        };
        self.work.layout += started.elapsed();
        let e = &mut self.stack[i];
        for (id, anchor) in &anchors {
            let max = laid.scroll_max.get(id).copied().unwrap_or(0.0);
            if let Some(offset) = anchor.offset_in(&laid.scroll_items(id)) {
                e.scroll.insert(id.clone(), offset.clamp(0.0, max));
            }
        }
        // After an action from outside it, a `follow_end` view goes to
        // its end once it has something to scroll.
        if e.follow {
            let mut moved = false;
            for id in &laid.follow_end {
                if let Some(&max) = laid.scroll_max.get(id)
                    && max > 0.0
                {
                    e.scroll.insert(id.clone(), max);
                    moved = true;
                }
            }
            e.follow = !moved && !laid.follow_end.is_empty();
        }
        // Panels beside a host no longer drawn (another view picked, a
        // narrower window) close.
        e.panels.retain(|h, _| laid.panel_hosts.contains(h));
        e.panel_hosts.clone_from(&laid.panel_hosts);
        e.laid = Some((laid, size));
        Ok(())
    }

    /// Lays out the current frame (cached until something changes; a
    /// scroll only places the laid-out frames again). Draws the top
    /// full-screen layout, and over it any sheets, panels and overlays with
    /// their scrim; only the top one's nodes can be tapped.
    /// Put back the `local://` fields the device kept (feature
    /// `local_keep`), once, as soon as there is a loader to ask: before the
    /// first frame, so the app opens on the tab it was left on.
    fn restore_kept(&mut self) {
        if self.kept_restored {
            return;
        }
        let Some(loader) = self.loader.as_ref() else {
            return;
        };
        self.kept_restored = true;
        let kept = loader.kept_locals();
        if kept.is_empty() {
            return;
        }
        // Kept before device-owned data was read-only: not put back.
        for (uri, field, value) in kept {
            if !protocol::is_device_owned(&uri) {
                self.data.entities.entry(uri).or_default().set(field, value);
            }
        }
        self.invalidate();
    }

    pub fn scene(&mut self) -> Result<&Scene, RenderError> {
        self.restore_kept();
        let polling = Instant::now();
        self.poll();
        let tidying = Instant::now();
        self.work.poll += tidying - polling;
        self.follow_clock();
        let mut now = Instant::now();
        self.step_pagers(now);
        self.step_tap_pages(now);
        self.step_cards(now);
        if self.caret_on() != self.caret_shown {
            self.scene = None;
        }
        let animate = self.animate;
        let before = self.stack.len();
        let mut k = 0;
        self.stack.retain(|e| {
            k += 1;
            k == 1
                || e.closing
                    .is_none_or(|c| animate && now.duration_since(c) < SLIDE)
        });
        if self.stack.len() != before || self.animating() {
            self.scene = None;
        }
        self.work.tidy += tidying.elapsed();
        // (`if let` returning the borrow here is refused by today's borrow
        // checker: the scene is built below on the other path.)
        #[allow(clippy::unnecessary_unwrap)]
        if self.scene.is_some() {
            return Ok(self.scene.as_ref().expect("checked"));
        }
        let showing = Instant::now();
        self.load_inline_layouts();
        // From the top down to the first settled full-screen layout.
        let mut visible = Vec::new();
        for i in (0..self.stack.len()).rev() {
            visible.push(i);
            let e = &self.stack[i];
            if i == 0 || (e.frame == Frame::Full && !e.in_frame && self.progress(i, now) >= 1.0) {
                break;
            }
        }
        visible.reverse();
        self.report_shown(&visible);
        let laying = Instant::now();
        self.work.shown += laying - showing;
        let laid_before = self.work.laid_out;
        for &i in &visible {
            self.layout_entry(i)?;
        }
        if self.work.laid_out != laid_before {
            let took = laying.elapsed();
            self.hold_slides(now, took);
            // This frame draws where the slides were before they were held.
            now += took;
        }
        let building = Instant::now();
        let (w, h) = (self.viewport.width, self.viewport.height);
        let top = self.top_index();
        let top_group = self.group_start(top);
        let theme = self.stack[visible[0]]
            .laid
            .as_ref()
            .expect("laid out")
            .0
            .theme()
            .clone();
        let mut scene = Scene {
            width: w,
            height: h,
            scale: self.viewport.scale,
            background: theme.background.0,
            // Still when not animating, so previews and tests are stable.
            spin: if animate {
                (now.duration_since(self.spin_from).as_secs_f32() / SPIN.as_secs_f32()).fract()
            } else {
                0.0
            },
            ..Default::default()
        };
        let mut keep_from = 0;
        let insets = self.insets;
        // The navigation bar's edge (top, right, bottom, left): the
        // widest of the others (on its side in landscape, with buttons).
        let nav_edge = if insets[2] >= insets[1].max(insets[3]) {
            2
        } else if insets[1] >= insets[3] {
            1
        } else {
            3
        };
        let mut bars = [theme.background; 2];
        for &i in &visible {
            let p = self.progress(i, now);
            let (frame, content, entry_insets) = self.frame_rects(i);
            let e = &self.stack[i];
            // Only the top layout's nodes can be pressed, as tapped.
            let pressed = self
                .pressed
                .as_ref()
                .or(self.hovered.as_ref())
                .filter(|_| i >= top_group);
            if i == top_group {
                keep_from = scene.semantics.len();
            }
            // Where the entry's frame sits now (sliding in or out).
            let g = self.group_start(i);
            let gp = self.progress(g, now);
            let slide = |frame: Frame, size: [f32; 2], p: f32| -> (f32, f32) {
                let q = 1.0 - p;
                match frame {
                    Frame::Full => (q * w, 0.0),
                    Frame::Dialog => (0.0, 0.0),
                    Frame::Edge(protocol::Edge::Bottom) => (0.0, q * size[1]),
                    Frame::Edge(protocol::Edge::Top) => (0.0, -q * size[1]),
                    Frame::Edge(protocol::Edge::Start) => (-q * size[0], 0.0),
                    Frame::Edge(protocol::Edge::End) => (q * size[0], 0.0),
                }
            };
            let group_frame = self.stack[g].frame;
            let (fx, fy) = if g == 0 {
                (0.0, 0.0)
            } else {
                slide(group_frame, [frame[2], frame[3]], gp)
            };
            // A drag on the top sheet or panel moves it toward its edge.
            let (fx, fy, drag) = match group_frame {
                Frame::Edge(edge) if g == top_group && g > 0 => {
                    let off = self.frame_offset_now(now);
                    let (size, d) = match edge {
                        protocol::Edge::Bottom => (frame[3], (0.0, off)),
                        protocol::Edge::Top => (frame[3], (0.0, -off)),
                        protocol::Edge::Start => (frame[2], (-off, 0.0)),
                        protocol::Edge::End => (frame[2], (off, 0.0)),
                    };
                    (fx + d.0, fy + d.1, off / size.max(1.0))
                }
                _ => (fx, fy, 0.0),
            };
            let frame_now = [frame[0] + fx, frame[1] + fy, frame[2], frame[3]];
            let (laid, _) = e.laid.as_ref().expect("laid out");
            // The layout's page colour at each edge, and what a sheet or
            // panel is drawn on: its page where it meets the handle strip
            // (or the top, for a panel), else the theme's surface.
            let edges = laid.edge_colors();
            let sheet_fill = match e.frame {
                Frame::Edge(protocol::Edge::Top) => edges[2],
                _ => edges[0],
            }
            .or_else(|| edges.iter().flatten().next().copied())
            .unwrap_or(theme.surface);
            let shown = p >= 0.5;
            if !e.in_frame && shown {
                match e.frame {
                    Frame::Full => {}
                    Frame::Dialog => bars = bars.map(|c| c.dimmed(0.32 * p)),
                    Frame::Edge(edge) => {
                        bars = bars.map(|c| c.dimmed(0.32 * p * (1.0 - drag)));
                        let side = match edge {
                            protocol::Edge::Top => 0,
                            protocol::Edge::End => 1,
                            protocol::Edge::Bottom => 2,
                            protocol::Edge::Start => 3,
                        };
                        if side == 0 {
                            bars[0] = edges[0].unwrap_or(sheet_fill);
                        }
                        if side == nav_edge {
                            bars[1] = edges[nav_edge].unwrap_or(sheet_fill);
                        }
                    }
                }
            }
            if e.in_frame {
                // A screen inside its layout's box slides in from the side.
                let dx = (1.0 - p) * frame[2];
                let r = [
                    content[0] + fx + dx,
                    content[1] + fy,
                    content[2],
                    content[3],
                ];
                let mut q = QuadInstance::fill(r, theme.background, 0.0);
                q.clip = frame_now;
                scene.draws.push(Draw::Quad(q));
                // It covers the frame's own content, which stays laid out
                // under it: a tap on the screen must not reach that.
                scene.semantics.push(SemanticNode {
                    id: "frame_cover".into(),
                    node_type: COVER.into(),
                    bounds: r,
                    text_content: None,
                    interactive: false,
                    tap: None,
                    selected: false,
                    parent: None,
                    gestures: BTreeMap::new(),
                    hint: None,
                });
                laid.emit_into(&mut scene, &e.scroll, (r[0], r[1]), frame_now, pressed);
                continue;
            }
            match e.frame {
                Frame::Full => {
                    if i > 0 {
                        // Covers what is under it as it slides in.
                        let mut q = QuadInstance::fill(frame_now, theme.background, 0.0).shadow(
                            Rgba([0.0, 0.0, 0.0, 0.18]),
                            [0.0, 0.0],
                            8.0,
                            0.0,
                        );
                        q.clip = [0.0, 0.0, w, h];
                        scene.draws.push(Draw::Quad(q));
                    }
                    // The page's own colour under the system bars, as each
                    // edge of it is drawn.
                    inset_bands(&mut scene, frame_now, insets, &edges);
                    if shown {
                        bars = [edges[0], edges[nav_edge]].map(|c| c.unwrap_or(theme.background));
                    }
                    laid.emit_into(
                        &mut scene,
                        &e.scroll,
                        (frame_now[0], frame_now[1]),
                        [0.0, 0.0, w, h],
                        pressed,
                    );
                }
                Frame::Dialog => {
                    let scrim =
                        QuadInstance::fill([0.0, 0.0, w, h], Rgba([0.0, 0.0, 0.0, 0.32 * p]), 0.0);
                    scene.draws.push(Draw::Quad(scrim));
                    if i == top_group {
                        // A tap outside cancels, as Back does.
                        scene.semantics.push(SemanticNode {
                            id: "scrim".into(),
                            node_type: "scrim".into(),
                            bounds: [0.0, 0.0, w, h],
                            text_content: None,
                            interactive: true,
                            tap: Some(Tap::Intent {
                                intent: json!({ "intent": "action.navigation.back" }),
                            }),
                            selected: false,
                            parent: None,
                            gestures: BTreeMap::new(),
                            hint: None,
                        });
                    }
                    if p > 0.5 {
                        laid.emit_into(
                            &mut scene,
                            &e.scroll,
                            (0.0, 0.0),
                            [0.0, 0.0, w, h],
                            pressed,
                        );
                    }
                }
                Frame::Edge(edge) => {
                    let scrim = QuadInstance::fill(
                        [0.0, 0.0, w, h],
                        Rgba([0.0, 0.0, 0.0, 0.32 * p * (1.0 - drag)]),
                        0.0,
                    );
                    scene.draws.push(Draw::Quad(scrim));
                    if i == top_group {
                        scene.semantics.push(SemanticNode {
                            id: "scrim".into(),
                            node_type: "scrim".into(),
                            bounds: [0.0, 0.0, w, h],
                            text_content: None,
                            interactive: true,
                            tap: Some(Tap::Intent {
                                intent: json!({ "intent": "action.navigation.back", "frame": true }),
                            }),
                            selected: false,
                            parent: None,
                            gestures: BTreeMap::new(),
                            hint: None,
                        });
                    }
                    let radii = match edge {
                        protocol::Edge::Bottom => [16.0, 16.0, 0.0, 0.0],
                        protocol::Edge::Top => [0.0, 0.0, 16.0, 16.0],
                        protocol::Edge::Start => [0.0, 16.0, 16.0, 0.0],
                        protocol::Edge::End => [16.0, 0.0, 0.0, 16.0],
                    };
                    let mut surface = QuadInstance::fill(frame_now, sheet_fill, 0.0).shadow(
                        Rgba([0.0, 0.0, 0.0, 0.25]),
                        [0.0, 2.0],
                        8.0,
                        0.0,
                    );
                    surface.radii = radii;
                    scene.draws.push(Draw::Quad(surface));
                    if matches!(edge, protocol::Edge::Bottom | protocol::Edge::Top) {
                        // The handle, on the sheet's inner side.
                        let hy = if edge == protocol::Edge::Bottom {
                            frame_now[1] + 8.0
                        } else {
                            frame_now[1] + frame_now[3] - 12.0
                        };
                        // A tint of the sheet's own colour, so it reads on any page.
                        let grip = if sheet_fill.is_light() {
                            Rgba([0.0, 0.0, 0.0, 0.22])
                        } else {
                            Rgba([1.0, 1.0, 1.0, 0.28])
                        };
                        let handle = QuadInstance::fill(
                            [frame_now[0] + frame_now[2] / 2.0 - 16.0, hy, 32.0, 4.0],
                            grip,
                            2.0,
                        );
                        scene.draws.push(Draw::Quad(handle));
                    }
                    let origin = (content[0] + fx, content[1] + fy);
                    // Its page under the system bars too, where it differs
                    // from what the sheet is drawn on.
                    let differing = edges.map(|c| c.filter(|c| *c != sheet_fill));
                    inset_bands(
                        &mut scene,
                        [origin.0, origin.1, content[2], content[3]],
                        entry_insets,
                        &differing,
                    );
                    let mut clip = frame_now;
                    if matches!(edge, protocol::Edge::Bottom | protocol::Edge::Top) {
                        clip = [origin.0, origin.1, content[2], content[3]];
                    }
                    laid.emit_into(&mut scene, &e.scroll, origin, clip, pressed);
                }
            }
        }
        scene.bars = bars.map(|c| c.0);
        // Only the top layout (or sheet, with its scrim) answers taps.
        if keep_from > 0 {
            scene.semantics.drain(..keep_from);
            for n in &mut scene.semantics {
                n.parent = n.parent.and_then(|p| p.checked_sub(keep_from));
            }
        }
        // What may show next first, so what shows now (asked for after)
        // loads first.
        self.prefetch();
        for d in &scene.draws {
            if let Draw::Image(i) = d {
                self.images.request(&i.url);
            }
        }
        if let Some(text) = self.notice.clone() {
            self.notice_bar(&mut scene, &text)?;
        }
        self.caret_shown = self.caret_on();
        self.edit_overlay(&mut scene, &theme);
        self.spinning = scene.spinning;
        self.scene = Some(scene);
        self.scenes_built += 1;
        self.work.build += building.elapsed();
        Ok(self.scene.as_ref().expect("just built"))
    }

    /// What editing draws over the focused field, as Android does: the
    /// selection's highlight, the blinking caret, the handles, and the Cut /
    /// Copy / Paste / Select all bar above the text (below when there is no
    /// room), on top of everything.
    fn edit_overlay(&mut self, scene: &mut Scene, theme: &crate::style::Theme) {
        self.handles.clear();
        let Some(e) = self.edit.clone() else {
            return;
        };
        if self.focused.as_ref() != Some(&e.field) {
            return;
        }
        let Some(fb) = scene.fields.iter().find(|f| f.field == e.field).cloned() else {
            return;
        };
        let value = self.form.get(&e.field).cloned().unwrap_or_default();
        let ty = crate::style::Type::new(fb.face, fb.size, fb.line_height);
        let stops = self.field_stops(&fb, &value, ty);
        let (ox, oy) = (fb.rect[0], fb.rect[1]);
        let stop = |b: usize| {
            stops
                .iter()
                .find(|s| s.byte == b)
                .or(stops.last())
                .copied()
                .expect("one stop at least")
        };
        let clip = fb.clip;
        let accent = theme.primary;
        let mut quad = |r: [f32; 4], c: Rgba, radius: [f32; 4], clip: [f32; 4]| {
            let mut q = QuadInstance::fill(r, c, 0.0);
            q.radii = radius;
            q.clip = clip;
            scene.draws.push(Draw::Quad(q));
        };
        let (lo, hi) = e.range();
        let (a, b) = (stop(lo), stop(hi));
        let all = [0.0, 0.0, self.viewport.width, self.viewport.height];
        if e.selected() {
            // One band per line the selection covers.
            let mut tops: Vec<f32> = stops
                .iter()
                .filter(|s| s.byte >= lo && s.byte <= hi)
                .map(|s| s.top)
                .collect();
            tops.dedup();
            for top in tops {
                let on: Vec<&crate::text::CaretStop> =
                    stops.iter().filter(|s| s.top == top).collect();
                let x0 = if top == a.top { a.x } else { 0.0 };
                let x1 = if top == b.top {
                    b.x
                } else {
                    on.iter().map(|s| s.x).fold(0.0, f32::max).max(fb.rect[2])
                };
                let h = on.first().map_or(fb.line_height, |s| s.height);
                quad(
                    [ox + x0, oy + top, (x1 - x0).max(1.0), h],
                    accent.with_alpha(0.3),
                    [0.0; 4],
                    clip,
                );
            }
        } else if self.caret_on() == Some(true) {
            let c = stop(e.cursor);
            quad(
                [ox + c.x - 1.0, oy + c.top + 1.0, 2.0, c.height - 2.0],
                accent,
                [1.0; 4],
                clip,
            );
        }
        if let Some((cs, ce)) = e.composing.filter(|&(a, b)| a < b && b <= value.len()) {
            // The word being composed is underlined, one line per row.
            let (a, b) = (stop(cs), stop(ce));
            let mut tops: Vec<f32> = stops
                .iter()
                .filter(|s| s.byte >= cs && s.byte <= ce)
                .map(|s| s.top)
                .collect();
            tops.dedup();
            for top in tops {
                let on: Vec<&crate::text::CaretStop> =
                    stops.iter().filter(|s| s.top == top).collect();
                let x0 = if top == a.top { a.x } else { 0.0 };
                let x1 = if top == b.top {
                    b.x
                } else {
                    on.iter().map(|s| s.x).fold(0.0, f32::max)
                };
                let h = on.first().map_or(fb.line_height, |s| s.height);
                quad(
                    [ox + x0, oy + top + h - 2.0, (x1 - x0).max(1.0), 1.5],
                    theme.on_background,
                    [0.0; 4],
                    clip,
                );
            }
        }
        if e.handles {
            let r = HANDLE / 2.0;
            let under = |s: crate::text::CaretStop| (ox + s.x, oy + s.top + s.height);
            if e.selected() {
                let (sx, sy) = under(a);
                let start = [sx - HANDLE, sy, HANDLE, HANDLE];
                quad(start, accent, [r, 0.0, r, r], all);
                self.handles.push((Grab::Start, start));
                let (ex, ey) = under(b);
                let end = [ex, ey, HANDLE, HANDLE];
                quad(end, accent, [0.0, r, r, r], all);
                self.handles.push((Grab::End, end));
            } else {
                let (cx, cy) = under(stop(e.cursor));
                quad([cx - 1.0, cy, 2.0, 4.0], accent, [0.0; 4], all);
                let knob = [cx - r, cy + 3.0, HANDLE, HANDLE];
                quad(knob, accent, [r; 4], all);
                self.handles.push((Grab::Caret, knob));
            }
        }
        if !e.menu {
            return;
        }
        let mut items: Vec<(&str, &str)> = Vec::new();
        if e.selected() && !fb.secret {
            items.push(("Cut", "edit.cut"));
            items.push(("Copy", "edit.copy"));
        }
        items.push(("Paste", "edit.paste"));
        if !value.is_empty() && (lo > 0 || hi < value.len()) {
            items.push(("Select all", "edit.select_all"));
        }
        let label = crate::style::type_for(protocol::TextStyle::Label);
        let widths: Vec<(f32, f32)> = items
            .iter()
            .map(|(t, _)| {
                let m = self
                    .builder
                    .text
                    .measure(t, label, None, self.viewport.scale);
                (m.width, m.height)
            })
            .collect();
        let pad = 16.0;
        let w: f32 = widths.iter().map(|(w, _)| w + 2.0 * pad).sum();
        let h = 44.0;
        let mid = (ox + (a.x + b.x) / 2.0).clamp(0.0, self.viewport.width);
        let x = (mid - w / 2.0).clamp(8.0, (self.viewport.width - w - 8.0).max(8.0));
        let above = oy + a.top - h - 8.0;
        let y = if above >= self.insets[0] + 4.0 {
            above
        } else {
            oy + b.top + b.height + HANDLE + 8.0
        };
        let mut bar = QuadInstance::fill([x, y, w, h], theme.surface, 8.0).shadow(
            Rgba([0.0, 0.0, 0.0, 0.28]),
            [0.0, 2.0],
            8.0,
            0.0,
        );
        bar.clip = all;
        scene.draws.push(Draw::Quad(bar));
        let mut ix = x;
        for ((text, intent), (tw, th)) in items.iter().zip(&widths) {
            let iw = tw + 2.0 * pad;
            scene.draws.push(Draw::Text(crate::scene::TextRun {
                text: (*text).into(),
                face: label.face,
                size: label.size,
                line_height: label.line_height,
                tracking: 0.0,
                max_lines: 0,
                strike: false,
                spans: Vec::new(),
                color: theme.on_background,
                rect: [ix + pad, y + (h - th) / 2.0, *tw, *th],
                clip: all,
            }));
            scene.semantics.push(SemanticNode {
                id: intent.replace('.', ":"),
                node_type: "menu".into(),
                bounds: [ix, y, iw, h],
                text_content: Some((*text).into()),
                interactive: true,
                tap: Some(Tap::Intent {
                    intent: json!({ "intent": intent }),
                }),
                selected: false,
                parent: None,
                gestures: BTreeMap::new(),
                hint: None,
            });
            ix += iw;
        }
    }

    /// Draws the current frame on the CPU.
    pub fn render_cpu(&mut self) -> Result<RgbaImage, RenderError> {
        self.scene()?;
        let scene = self.scene.as_ref().expect("built");
        self.cpu.images = Some(self.images.clone());
        Ok(self.cpu.render(scene, &mut self.builder.text))
    }

    /// How many scenes have been built: a client that rasterizes only a
    /// new scene (the web client, decision `web-client`) compares it after
    /// [`Session::scene`].
    pub fn scenes_built(&self) -> u64 {
        self.scenes_built
    }

    /// The fonts, for a GPU renderer drawing this session's scenes.
    pub fn text_engine(&mut self) -> &mut crate::text::TextEngine {
        &mut self.builder.text
    }

    /// The scene and the fonts together (a GPU frame needs both).
    pub fn scene_and_text(
        &mut self,
    ) -> Result<(&Scene, &mut crate::text::TextEngine), RenderError> {
        self.scene()?;
        Ok((self.scene.as_ref().expect("built"), &mut self.builder.text))
    }

    /// Merges data in (counted in `merges`).
    fn merge_data(&mut self, from: DataBundle) {
        merge(&mut self.data, from);
        self.merges += 1;
    }

    /// Lays the frame out again next time (data, form or size changed).
    pub fn invalidate(&mut self) {
        for e in &mut self.stack {
            e.relay();
        }
        // A prepared open lays out again when the tap brings it in.
        if let Some(p) = &mut self.prepared {
            for e in &mut p.entries {
                e.relay();
            }
        }
        self.scene = None;
    }

    /// New platform insets (points): the next frame lays out again.
    pub fn set_insets(&mut self, insets: [f32; 4]) {
        if insets != self.insets {
            self.insets = insets;
            self.invalidate();
        }
    }

    /// `local://device`'s `width_dp`, `height_dp` and `size_class`: the
    /// window's size (feature `device_size`, decision `screen-sizes`), over
    /// whatever device data the loader gave.
    fn set_device_size(&mut self) {
        let (w, h) = (self.viewport.width, self.viewport.height);
        let device = self
            .data
            .entities
            .entry("local://device".into())
            .or_default();
        device.set("width_dp", w.round() as i64);
        device.set("height_dp", h.round() as i64);
        device.set("size_class", size_class(w));
    }

    /// A new viewport (a window resize): the next frame lays out again.
    pub fn resize(&mut self, viewport: Viewport) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.invalidate();
        }
    }

    pub fn set_form(&mut self, field: &str, value: &str) {
        self.form.insert(field.to_owned(), value.to_owned());
        self.invalidate();
    }

    /// The first layout's `$route` parameters and `$item` entity (a
    /// preview drawing a screen or a row on its own).
    pub fn set_root(&mut self, route: BTreeMap<String, String>, item: Option<String>) {
        self.stack[0].route = route;
        self.stack[0].item = item;
        self.invalidate();
    }

    /// How far each scrolling view of the top layout or screen can scroll
    /// (points), by id.
    pub fn scroll_extents(&mut self) -> Result<BTreeMap<String, f32>, RenderError> {
        self.scene()?;
        let top = &self.stack[self.top_index()];
        Ok(top
            .laid
            .as_ref()
            .map(|(l, _)| l.scroll_max.clone())
            .unwrap_or_default())
    }

    fn find(&mut self, target: &Target) -> Result<Option<SemanticNode>, String> {
        let scene = self.scene().map_err(|e| e.to_string())?;
        let found = match target {
            Target::Id(id) => scene.find_node(id),
            Target::Text(t) => scene.find_text(t),
            Target::Point(x, y) => scene.hit_test(*x, *y),
        };
        let Some(found) = found else {
            return Ok(None);
        };
        // Tapping a label taps the button around it.
        let index = scene
            .semantics
            .iter()
            .position(|n| std::ptr::eq(n, found))
            .expect("from this scene");
        Ok(Some(
            scene
                .interactive_for(index)
                .unwrap_or(&scene.semantics[index])
                .clone(),
        ))
    }

    /// A tap on `target`.
    /// A tap at `target`: what it does, as [`Outcome`]; how the owner uses
    /// their screens goes to the loader ([`Loader::usage`]).
    pub fn tap(&mut self, target: &Target) -> Result<Outcome, String> {
        let before = self.usage_place();
        let out = self.tap_inner(target);
        if let Ok(o) = &out {
            self.report_usage(&before, o, true);
        }
        out
    }

    /// Run an intent from the platform (keyboard, notification, system
    /// Back); usage is reported as for [`Session::tap`].
    pub fn dispatch(&mut self, intent: &Value) -> Result<Outcome, String> {
        let before = self.usage_place();
        let out = self.dispatch_inner(intent);
        if let Ok(o) = &out {
            self.report_usage(&before, o, false);
        }
        out
    }

    /// Where the owner is before an interaction: the top layout, its route
    /// name and when it opened.
    fn usage_place(&self) -> (String, Option<String>, Instant) {
        let e = &self.stack[self.top_index()];
        (e.id.clone(), None, e.opened)
    }

    /// Tell the loader what an interaction did (decision `usage-trails`).
    fn report_usage(&self, before: &(String, Option<String>, Instant), o: &Outcome, tap: bool) {
        use protocol::usage::{UsageEvent, node_identity, now_secs};
        let Some(loader) = self.loader.as_ref() else {
            return;
        };
        let (from, route, opened) = before;
        let at = now_secs();
        if tap && let Some(n) = &o.node {
            let intent = match &n.tap {
                Some(crate::scene::Tap::Intent { intent }) => Some(intent),
                _ => None,
            };
            let effect = if o.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::Opened { .. } | Effect::OpenedScreen { .. } | Effect::Back
                )
            }) {
                "navigated"
            } else if o
                .effects
                .iter()
                .any(|e| matches!(e, Effect::Sent { .. } | Effect::NotSent { .. }))
            {
                "sent"
            } else if o.effects.iter().all(|e| matches!(e, Effect::Nothing)) {
                "none"
            } else {
                "changed"
            };
            loader.usage(&UsageEvent::tap(
                at,
                from,
                &node_identity(&n.id),
                intent.and_then(|i| i["intent"].as_str()),
                intent,
                effect,
            ));
        }
        for e in &o.effects {
            match e {
                Effect::Opened { layout } => {
                    loader.usage(&UsageEvent::screen(at, layout, None, from))
                }
                Effect::OpenedScreen { screen } => {
                    loader.usage(&UsageEvent::screen(at, self.current(), Some(screen), from))
                }
                Effect::Back => loader.usage(&UsageEvent::back(
                    at,
                    from,
                    route.as_deref(),
                    opened.elapsed().as_millis() as u64,
                )),
                _ => {}
            }
        }
    }

    fn tap_inner(&mut self, target: &Target) -> Result<Outcome, String> {
        if let Target::Point(x, y) = *target {
            // A scatter picks the point nearest the tap.
            if let Some(out) = self.scatter_tap(x, y) {
                return out;
            }
            // A tap-paged carousel turns on its outer thirds; its middle
            // third taps as anything does (its `on_tap`).
            if let Some((id, third)) = self.tap_pager_at(x, y)
                && third != 0
                && self.turn_page(&id, i32::from(third))
            {
                self.pressed = None;
                return Ok(Outcome {
                    node: self.scene.as_ref().and_then(|s| s.find_node(&id).cloned()),
                    effects: vec![Effect::Nothing],
                });
            }
        }
        let Some(node) = self.find(target)? else {
            // A tap on bare background (beside a layout drawn at phone
            // width, decision `phone-width-layouts`) still ends typing.
            if matches!(target, Target::Point(..)) && self.focused.take().is_some() {
                self.scene = None;
            }
            return Err(format!("nothing at {target:?}"));
        };
        // A tap anywhere but a field ends typing (the field's tap sets it
        // again); the edit menu's items keep it.
        let again =
            matches!(&node.tap, Some(Tap::Focus { field }) if self.focused.as_ref() == Some(field));
        let (px, py) = match target {
            Target::Point(x, y) => (*x, *y),
            _ => (
                node.bounds[0] + node.bounds[2] / 2.0,
                node.bounds[1] + node.bounds[3] / 2.0,
            ),
        };
        // A tap ends the touch: nothing stays pressed.
        self.pressed = None;
        let prepared = self.prepared.take();
        let (focused, merges) = (self.focused.clone(), self.merges);
        // Only opened or closed a layout or screen: what is laid out stays.
        let mut moved_only = false;
        let menu = node.node_type == "menu";
        self.clipboard_ok = menu;
        if !menu {
            self.focused = None;
        }
        let effects = match node.tap.clone() {
            None => vec![Effect::Nothing],
            Some(Tap::SetForm { field, value }) => {
                self.set_form(&field, &value);
                vec![Effect::FormSet { field, value }]
            }
            Some(Tap::Focus { field }) => {
                self.focus_at(&field, px, py, again);
                vec![Effect::Focused { field }]
            }
            Some(Tap::Intent { intent }) => match self.bind_form(&intent) {
                // A screen of the layout in a panel (its post, its New
                // post): opened from that layout, not the one under it.
                Some(bound) if self.panel_screen(&node, &bound).is_some() => {
                    let mut out = Vec::new();
                    if let Some((layout, uri)) = self.panel_screen(&node, &bound) {
                        self.open_screen_from(&uri, Some(&layout), &mut out);
                    }
                    moved_only = self.merges == merges;
                    out
                }
                Some(bound) if panel_intent(&bound) && self.panel_slot_of(&node).is_some() => {
                    let mut out = Vec::new();
                    self.in_panels(&node, &bound, &mut out);
                    out
                }
                Some(bound) => {
                    self.mark_follow(px, py);
                    match prepared.filter(|p| p.intent == bound) {
                        // Opened while the finger was down: in it comes,
                        // its slide starting now.
                        Some(p) => {
                            let now = Instant::now();
                            for mut e in p.entries {
                                e.opened = now;
                                self.stack.push(e);
                            }
                            moved_only = !p.merged;
                            p.effects
                        }
                        None => {
                            let mut out = Vec::new();
                            self.run(&bound, &mut out);
                            moved_only = navigates_only(&bound) && self.merges == merges;
                            out
                        }
                    }
                }
                None => vec![Effect::Nothing],
            },
        };
        self.relay_unless(moved_only && self.focused == focused);
        Ok(Outcome {
            node: Some(node),
            effects,
        })
    }

    /// After an interaction: what is laid out is laid out again, unless
    /// all it did was open or close layouts and screens (`moved_only`: no
    /// data merged, no field left), which changes nothing already laid
    /// out; home under a screen that opened or closed stays as it was.
    fn relay_unless(&mut self, moved_only: bool) {
        if moved_only {
            self.scene = None;
        } else {
            self.invalidate();
        }
    }

    /// A tap at a point outside the top entry's `follow_end` scrolling
    /// views (a footer's Send, not a like inside the list) makes them
    /// follow their content to the end, as the shared UI does.
    fn mark_follow(&mut self, x: f32, y: f32) {
        let ti = self.top_index();
        let follows = match &self.stack[ti].laid {
            Some((l, _)) if !l.follow_end.is_empty() => l.follow_end.clone(),
            _ => return,
        };
        let inside = self.scene.as_ref().is_some_and(|sc| {
            sc.semantics.iter().any(|n| {
                let [bx, by, bw, bh] = n.bounds;
                follows.contains(&n.id) && x >= bx && x < bx + bw && y >= by && y < by + bh
            })
        });
        if !inside {
            self.stack[ti].follow = true;
        }
    }

    /// The platform's light or dark mode: the next frame draws with it.
    pub fn set_dark(&mut self, dark: bool) {
        if dark != self.dark {
            self.dark = dark;
            self.invalidate();
        }
    }

    /// Runs an intent as a tap would (system Back, a test).
    fn dispatch_inner(&mut self, intent: &Value) -> Result<Outcome, String> {
        let mut effects = Vec::new();
        let (focused, merges) = (self.focused.clone(), self.merges);
        let mut moved_only = false;
        // Callers of `dispatch` are the platform (keyboard, notifications),
        // never a layout's tap.
        self.clipboard_ok = true;
        match self.bind_form(intent) {
            Some(bound) => {
                self.run(&bound, &mut effects);
                moved_only = navigates_only(&bound) && self.merges == merges;
            }
            None => effects.push(Effect::Nothing),
        }
        self.clipboard_ok = false;
        // System Back closing a screen: what is under it stays laid out.
        self.relay_unless(moved_only && self.focused == focused);
        Ok(Outcome {
            node: None,
            effects,
        })
    }

    /// Types into a text field (`field:<name>` or the form field name).
    pub fn type_text(&mut self, field: &str, text: &str) -> Outcome {
        let field = field.strip_prefix("field:").unwrap_or(field);
        self.set_form(field, text);
        // The whole value is replaced: the cursor goes to its end.
        if let Some(e) = self.edit.as_mut().filter(|e| e.field == field) {
            e.cursor = text.len();
            e.anchor = text.len();
        }
        Outcome {
            node: None,
            effects: vec![Effect::FormSet {
                field: field.to_owned(),
                value: text.to_owned(),
            }],
        }
    }

    /// The scrolling view at a point: its id, where it is scrolled to, how
    /// far it can scroll, and how tall it is (for the window's drags, flings
    /// and stretches, `scroll.rs`).
    pub fn scroll_view_at(&mut self, x: f32, y: f32) -> Option<ScrollView> {
        let scene = self.scene().ok()?;
        let (id, height) = scene
            .semantics
            .iter()
            .rev()
            .filter(|n| n.node_type == "core://Scroll")
            .find(|n| {
                let [bx, by, bw, bh] = n.bounds;
                x >= bx && x < bx + bw && y >= by && y < by + bh
            })
            .map(|n| (n.id.clone(), n.bounds[3]))?;
        let top = &self.stack[self.top_index()];
        let max = top
            .laid
            .as_ref()
            .and_then(|(l, _)| l.scroll_max.get(&id).copied())
            .unwrap_or(0.0);
        let offset = top.scroll.get(&id).copied().unwrap_or(0.0).clamp(0.0, max);
        Some(ScrollView {
            id,
            offset,
            max,
            height,
        })
    }

    /// Places a scrolling view of the top screen at `offset`.
    pub fn set_scroll(&mut self, id: &str, offset: f32) {
        let ti = self.top_index();
        self.stack[ti].scroll.insert(id.to_owned(), offset);
        self.scene = None;
    }

    /// Draws a scrolling view of the top screen `stretch` longer along its
    /// axis (`scroll::Stretch::scale`; 0 for not at all).
    pub fn set_stretch(&mut self, id: &str, stretch: f32) {
        let ti = self.top_index();
        let scroll = &mut self.stack[ti].scroll;
        let key = format!("stretch:{id}");
        if stretch == 0.0 {
            scroll.remove(&key);
        } else {
            scroll.insert(key, stretch);
        }
        self.scene = None;
    }

    /// Scrolls the scrolling view at or around `target` by `dy` points.
    pub fn scroll_by(&mut self, target: Option<&Target>, dy: f32) -> Result<Outcome, String> {
        let scene = self.scene().map_err(|e| e.to_string())?;
        let scroll_view = |n: &SemanticNode| n.node_type == "core://Scroll";
        let id = match target {
            Some(Target::Id(id)) => Some(id.clone()),
            Some(Target::Point(x, y)) => scene
                .semantics
                .iter()
                .rev()
                .filter(|n| scroll_view(n))
                .find(|n| {
                    let [bx, by, bw, bh] = n.bounds;
                    *x >= bx && *x < bx + bw && *y >= by && *y < by + bh
                })
                .map(|n| n.id.clone()),
            _ => scene
                .semantics
                .iter()
                .find(|n| scroll_view(n))
                .map(|n| n.id.clone()),
        };
        let Some(id) = id else {
            return Err("no scrolling view there".into());
        };
        let ti = self.top_index();
        let top = &mut self.stack[ti];
        let max = top
            .laid
            .as_ref()
            .and_then(|(l, _)| l.scroll_max.get(&id).copied())
            .unwrap_or(f32::MAX);
        let before = top.scroll.get(&id).copied().unwrap_or(0.0).min(max);
        let offset = (before + dy).clamp(0.0, max);
        if offset == before {
            // At an end: nothing moves (a fling stops, a pull may be a swipe).
            return Ok(Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            });
        }
        top.scroll.insert(id.clone(), offset);
        self.scene = None;
        Ok(Outcome {
            node: None,
            effects: vec![Effect::Scrolled { id, offset }],
        })
    }

    /// A swipe (`swipe_up`, `swipe_down`, `swipe_left`, `swipe_right`) or a
    /// `long_press` at a point: the innermost `core://Gesture` there with that
    /// handler runs it.
    pub fn gesture(&mut self, x: f32, y: f32, kind: &str) -> Result<Outcome, String> {
        let scene = self.scene().map_err(|e| e.to_string())?;
        let found = scene.semantics.iter().rev().find(|n| {
            let [bx, by, bw, bh] = n.bounds;
            x >= bx && x < bx + bw && y >= by && y < by + bh && n.gestures.contains_key(kind)
        });
        let Some(node) = found.cloned() else {
            return Ok(Outcome {
                node: None,
                effects: vec![Effect::Nothing],
            });
        };
        let mut effects = Vec::new();
        self.clipboard_ok = false;
        match self.bind_form(&node.gestures[kind]) {
            Some(bound) => self.run(&bound, &mut effects),
            None => effects.push(Effect::Nothing),
        }
        self.invalidate();
        Ok(Outcome {
            node: Some(node),
            effects,
        })
    }

    fn run(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        match intent["intent"].as_str().unwrap_or("") {
            "action.sequence" => {
                let steps = intent["steps"].as_array().cloned().unwrap_or_default();
                for (i, step) in steps.iter().enumerate() {
                    if step["intent"] == "action.confirm" {
                        // The steps after it wait for the answer too.
                        let mut c = step.clone();
                        let rest = &steps[i + 1..];
                        if !rest.is_empty() {
                            let mut then = vec![step["then"].clone()];
                            then.extend(rest.iter().cloned());
                            c["then"] = json!({ "intent": "action.sequence", "steps": then });
                        }
                        self.run(&c, out);
                        return;
                    }
                    self.run(step, out);
                }
            }
            "action.confirm" => self.confirm(intent, out),
            "action.local.set" => {
                let uri = intent["entity_uri"].as_str().unwrap_or("").to_owned();
                // The device's own data (`local://servers`, `session`,
                // `theme`...) is read-only to layouts (decision
                // `device-owned-local-state`): a layout can't choose the
                // server the app opens or fake a sign-in.
                if protocol::is_device_owned(&uri) {
                    eprintln!("construct-render: local.set {uri}: the device's own data; not set");
                    out.push(Effect::Nothing);
                    return;
                }
                let updates: Vec<(String, String)> = std::iter::once(intent)
                    .chain(intent["also"].as_array().into_iter().flatten())
                    .filter_map(|a| {
                        Some((
                            a["field"].as_str()?.to_owned(),
                            a["value_expr"].as_str()?.to_owned(),
                        ))
                    })
                    .collect();
                let entity = self.data.entities.entry(uri.clone()).or_default();
                let Some(values) = eval_all(&updates, entity) else {
                    out.push(Effect::Nothing);
                    return;
                };
                let mut fields = BTreeMap::new();
                for (f, v) in values {
                    entity.set(&f, v.clone());
                    fields.insert(f, v);
                }
                // The device's own data (`local://`) is the session's alone;
                // with `keep` the loader stores it for the next start.
                if let Some(loader) = self.loader.as_ref() {
                    if !uri.starts_with("local://") {
                        loader.local_set(intent);
                    } else if intent["keep"] == true {
                        for (f, v) in &fields {
                            loader.keep_local(&uri, f, v);
                        }
                    }
                }
                out.push(Effect::LocalSet {
                    entity_uri: uri,
                    fields,
                });
            }
            // The clipboard is the user's: a layout's button can't read it
            // into a field or overwrite it (security audit, #459).
            "edit.paste" | "edit.copy" | "edit.cut" if !self.clipboard_ok => {
                out.push(Effect::Nothing);
            }
            "edit.paste" => {
                if let Some(f) = self.edit.as_ref().map(|e| e.field.clone()) {
                    self.focused = Some(f.clone());
                    if let Some(e) = self.edit.as_mut() {
                        e.menu = false;
                    }
                    out.push(Effect::Paste { field: f });
                }
            }
            "edit.copy" | "edit.cut" => {
                let cut = intent["intent"] == "edit.cut";
                if let Some(text) = self.copy(cut) {
                    out.push(Effect::Copy { text });
                }
            }
            "edit.select_all" => self.select_all(),
            "media.pick" => {
                let field = intent["field"].as_str().unwrap_or("").to_owned();
                let max = self.start_pick(&field, intent["max"].as_u64().unwrap_or(1) as u32);
                out.push(Effect::PickMedia { field, max });
            }
            "media.remove" => {
                let field = intent["field"].as_str().unwrap_or("");
                self.remove_media(field, intent["id"].as_str().unwrap_or(""));
                out.push(Effect::Nothing);
            }
            "form.clear" => {
                // The session's own step after a field's Send, and a field's
                // clear button (`focus`: typing goes on in the emptied field).
                if let Some(f) = intent["field"].as_str() {
                    self.form.remove(f);
                    if intent["focus"] == true {
                        self.focused = Some(f.to_owned());
                        self.edit = None;
                        out.push(Effect::Focused {
                            field: f.to_owned(),
                        });
                    }
                }
            }
            "action.form.clear" => {
                // Feature `form_clear`: the boxes empty after an Add, and
                // typing goes on in `focus` (the keyboard stays up).
                for f in intent["fields"].as_array().into_iter().flatten() {
                    if let Some(f) = f.as_str() {
                        self.form.remove(f);
                    }
                }
                if let Some(f) = intent["focus"].as_str().filter(|f| !f.is_empty()) {
                    self.focused = Some(f.to_owned());
                    self.edit = None;
                    out.push(Effect::Focused {
                        field: f.to_owned(),
                    });
                }
            }
            "action.mutation.optimistic" => self.optimistic(intent, out),
            "action.auth.session" => self.open_session(intent, out),
            "action.auth.login" => self.login(intent, out),
            "action.mutation.create" => self.create(intent, out),
            "action.link.open" => {
                let url = intent["url"].as_str().unwrap_or("").trim();
                if url.starts_with("https://") || url.starts_with("http://") {
                    out.push(Effect::OpenLink {
                        url: url.to_owned(),
                    });
                } else {
                    out.push(Effect::Nothing);
                }
            }
            "action.navigation.open" => {
                let layout = intent["layout"].as_str().filter(|l| !l.is_empty());
                let target = intent["target_uri"].as_str().filter(|t| !t.is_empty());
                if let Some(id) = layout {
                    if let Some(root) = self.layout_root(id, out) {
                        let route = intent["params"]
                            .as_object()
                            .map(|m| {
                                m.iter()
                                    .map(|(k, v)| (k.clone(), crate::bind::render(v)))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut entry = Entry::layout(id, root, route);
                        entry.frame = match intent["presentation"].as_str() {
                            Some("Sheet") => Frame::Edge(protocol::Edge::Bottom),
                            Some("Panel") => Frame::Edge(protocol::Edge::Start),
                            Some("Overlay") => Frame::Edge(
                                serde_json::from_value(intent["edge"].clone()).unwrap_or_default(),
                            ),
                            _ => Frame::Full,
                        };
                        entry.size = intent["size_dp"].as_f64().map(|v| v as f32);
                        self.stack.push(entry);
                        out.push(Effect::Opened {
                            layout: id.to_owned(),
                        });
                        if let Some(t) = target {
                            self.open_screen(t, out);
                        }
                    }
                } else if let Some(t) = target {
                    if !self.open_screen(t, out) {
                        out.push(Effect::NotSent {
                            intent: intent.clone(),
                        });
                    }
                } else {
                    out.push(Effect::NotSent {
                        intent: intent.clone(),
                    });
                }
            }
            "action.device" => {
                let command = intent["command"].as_str().unwrap_or("").to_owned();
                // `app` is the older spelling of `layout`.
                let layout = intent["layout"]
                    .as_str()
                    .or(intent["app"].as_str())
                    .unwrap_or("")
                    .to_owned();
                let id = intent["id"].as_str().unwrap_or("").to_owned();
                match command.as_str() {
                    "open_notification" => return self.open_notification(&id, out),
                    protocol::intent::NOTIFICATION_PERMISSION_COMMAND => {
                        // The platform's prompt (`Platform::ask_notifications`).
                        out.push(Effect::Device { command });
                        return;
                    }
                    "refresh" => {
                        // The platform loads the pod again.
                        out.push(Effect::Device { command });
                        return;
                    }
                    "leave_pod" => {
                        // The platform forgets the session and shows the
                        // pod's sign-in screen (`Effect::LeftPod`).
                        self.stack.truncate(1);
                        self.focused = None;
                        out.push(Effect::LeftPod);
                        return;
                    }
                    "mark_read" => {
                        self.mark_read(Some(&id), out);
                        out.push(Effect::Device { command });
                        return;
                    }
                    "mark_all_read" => {
                        self.mark_read(None, out);
                        out.push(Effect::Device { command });
                        return;
                    }
                    "sign_in" if !intent["value"].as_str().unwrap_or("").trim().is_empty() => {
                        return self.sign_in_elsewhere(intent, out);
                    }
                    "sign_in" | "sign_out"
                        if self.loader.as_ref().is_some_and(|l| l.auth(intent)) =>
                    {
                        out.push(Effect::Device { command });
                        return;
                    }
                    // Retired (`RETIRED_SHELL_COMMANDS`): kept so layout
                    // versions written before still open their layout.
                    "open_settings" | "open_inbox" => {
                        let (layout, presentation) = if command == "open_settings" {
                            ("settings", "Panel")
                        } else {
                            ("inbox", "Sheet")
                        };
                        let open = json!({ "intent": "action.navigation.open",
                            "layout": layout, "presentation": presentation });
                        self.run(&open, out);
                        out.push(Effect::Device { command });
                        return;
                    }
                    // The notice bar's own: Dismiss, and the bar itself
                    // (a tap on it reaches nothing under it).
                    "dismiss_notice" => {
                        self.notice = None;
                        out.push(Effect::Nothing);
                        return;
                    }
                    "notice" => {
                        out.push(Effect::Nothing);
                        return;
                    }
                    "set_server" => return self.set_server(&id, out),
                    ANSWER_ASKED => return self.answer_asked(&id, out),
                    "toggle_channel" => return self.toggle_channel(&layout, &id, out),
                    "load_older_notifications" => return self.load_older(out),
                    _ => {}
                }
                out.push(Effect::NotSent {
                    intent: intent.clone(),
                });
            }
            "action.data.load" => {
                let path = intent["path"].as_str().unwrap_or("").to_owned();
                self.load_data(&path, out);
            }
            "action.navigation.pop_to" | "action.navigation.pop_to_root" => {
                // Back to the nearest entry with that edge key, or to the
                // root (feature `navigate_pop_to`).
                let top = self.top_index();
                let to = if intent["intent"] == "action.navigation.pop_to_root" {
                    Some(0)
                } else {
                    let edge = intent["edge"].as_str().unwrap_or("");
                    (0..=top).rev().find(|&i| {
                        let e = &self.stack[i];
                        e.closing.is_none() && !e.screen && e.id == edge
                    })
                };
                match to {
                    Some(i) if i < top => {
                        self.close_from(i + 1);
                        out.push(Effect::Back);
                    }
                    _ => out.push(Effect::Nothing),
                }
            }
            "action.navigation.back" if intent["frame"] != true && self.close_panel() => {
                out.push(Effect::Back);
            }
            "action.navigation.back" => {
                let top = self.top_index();
                if top == 0 {
                    out.push(Effect::Nothing);
                } else {
                    // A scrim tap closes the whole sheet or panel; Back
                    // closes the top screen.
                    let from = if intent["frame"] == true {
                        self.group_start(top)
                    } else {
                        top
                    };
                    let now = Instant::now();
                    self.stack.truncate(top + 1);
                    if self.animate {
                        for e in &mut self.stack[from..] {
                            e.closing.get_or_insert(now);
                        }
                    } else {
                        self.stack.truncate(from.max(1));
                    }
                    self.focused = None;
                    out.push(Effect::Back);
                }
            }
            _ => out.push(Effect::NotSent {
                intent: intent.clone(),
            }),
        }
    }

    /// `action.mutation.optimistic`: the fields change here at once (every
    /// `value_expr` against the entity as it was), the payload goes to the
    /// pod with `$value` as the new value, and a refusal puts them back.
    fn optimistic(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        let uri = intent["entity_uri"].as_str().unwrap_or("").to_owned();
        let Some(field) = intent["field"].as_str() else {
            out.push(Effect::Nothing);
            return;
        };
        let mut updates = vec![(
            field.to_owned(),
            intent["value_expr"].as_str().unwrap_or("").to_owned(),
        )];
        for a in intent["also"].as_array().into_iter().flatten() {
            if let (Some(f), Some(e)) = (a["field"].as_str(), a["value_expr"].as_str()) {
                updates.push((f.to_owned(), e.to_owned()));
            }
        }
        let entity = self.data.entities.entry(uri.clone()).or_default();
        let pre = entity.clone();
        let Some(values) = eval_all(&updates, &pre) else {
            out.push(Effect::Nothing);
            return;
        };
        let mut before = BTreeMap::new();
        let mut fields = BTreeMap::new();
        for (i, (f, v)) in values.into_iter().enumerate() {
            let old = pre.get(&f).cloned().unwrap_or_else(|| {
                if i == 0 {
                    intent["rollback_value"].clone()
                } else {
                    Value::Null
                }
            });
            before.insert(f.clone(), old);
            entity.set(f.clone(), v.clone());
            fields.insert(f, v);
        }
        let value = fields.get(field).cloned().unwrap_or(Value::Null);
        out.push(Effect::LocalSet {
            entity_uri: uri.clone(),
            fields,
        });
        if self.taken(intent, out) {
            return;
        }
        let mut body = intent["payload"].clone();
        substitute_value(&mut body, &value);
        let write = Write {
            path: intent["endpoint"].as_str().unwrap_or("").to_owned(),
            body,
            refresh: Vec::new(),
        };
        self.send(write, Undo::Fields { uri, before }, out);
    }

    /// `action.mutation.create`: the entity is added to its list here at
    /// once, sent, and the layouts on the stack are loaded again once the
    /// pod has it (a refusal takes it out again).
    fn create(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        let uri = intent["entity_uri"].as_str().unwrap_or("").to_owned();
        let query_key = intent["query_key"].as_str().unwrap_or("").to_owned();
        let mut entity = intent["entity"].clone();
        Session::bind_attachments(intent, &mut entity);
        if let Ok(e) = serde_json::from_value::<protocol::EntityRecord>(entity) {
            self.data.entities.insert(uri.clone(), e);
        }
        let list = self.data.queries.entry(query_key.clone()).or_default();
        list.retain(|u| *u != uri);
        if intent["position"] == "start" {
            list.insert(0, uri.clone());
        } else {
            list.push(uri.clone());
        }
        out.push(Effect::LocalSet {
            entity_uri: uri.clone(),
            fields: BTreeMap::new(),
        });
        if self.taken(intent, out) {
            self.attachments_taken(intent);
            return;
        }
        let write = Write {
            path: intent["endpoint"].as_str().unwrap_or("").to_owned(),
            body: intent["payload"].clone(),
            refresh: self.stack_layouts(),
        };
        self.send(write, Undo::Created { uri, query_key }, out);
    }

    /// Marks one notification read (or all of them) here and on the pod.
    fn mark_read(&mut self, id: Option<&str>, out: &mut Vec<Effect>) {
        let mut changed = 0i64;
        for (uri, e) in self.data.entities.iter_mut() {
            let Some(nid) = uri.strip_prefix("local://notifications/") else {
                continue;
            };
            if id.is_some_and(|id| id != nid) {
                continue;
            }
            if e.get("unread").is_some_and(crate::bind::truthy) {
                changed += 1;
            }
            e.set("unread", false);
            e.set("read", true);
        }
        if let Some(inbox) = self.data.entities.get_mut("local://inbox") {
            let left = if id.is_none() {
                0
            } else {
                (inbox.get("unread").and_then(Value::as_i64).unwrap_or(0) - changed).max(0)
            };
            inbox.set("unread", left);
            inbox.set(
                "label",
                if left == 0 {
                    "All read".to_owned()
                } else {
                    format!("{left} unread")
                },
            );
        }
        let intent = match id {
            Some(id) => json!({ "intent": "action.device", "command": "mark_read", "id": id }),
            None => json!({ "intent": "action.device", "command": "mark_all_read" }),
        };
        if self.taken(&intent, out) {
            return;
        }
        let body = match id {
            Some(id) => json!({ "ids": [id] }),
            None => json!({ "all": true }),
        };
        let write = Write {
            path: "/v1/notifications/read".into(),
            body,
            refresh: Vec::new(),
        };
        self.send(write, Undo::Nothing, out);
    }

    /// `open_notification`: marks it read, closes the inbox (the sheet or
    /// panel on top) and opens the notification's layout and screen, as the
    /// shared UI does.
    ///
    /// `id` is a notification's id (`local:notifications`), or the URI of
    /// any entity shaped like one (a row of the pod's `notifications`
    /// collection, decision `notifications-in-a-collection`), whose `id`
    /// is the one marked read.
    fn open_notification(&mut self, id: &str, out: &mut Vec<Effect>) {
        let uri = if id.contains("://") {
            id.to_owned()
        } else {
            format!("local://notifications/{id}")
        };
        let Some(n) = self.data.entities.get(&uri).cloned() else {
            out.push(Effect::Nothing);
            return;
        };
        let id = n
            .get("id")
            .and_then(Value::as_str)
            .filter(|_| id.contains("://"))
            .map_or_else(|| id.to_owned(), str::to_owned);
        let id = id.as_str();
        let text = |f: &str| {
            n.get(f)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let target = text("layout").or_else(|| text("app_id"));
        let target_uri = text("target_uri");
        self.mark_read(Some(id), out);
        let top = self.top_index();
        if top > 0 && self.stack[self.group_start(top)].frame != Frame::Full {
            self.run(
                &json!({ "intent": "action.navigation.back", "frame": true }),
                out,
            );
        }
        let root = self.stack[0].id.clone();
        match target {
            Some(t) if t != root => {
                let mut open = json!({ "intent": "action.navigation.open", "layout": t, "presentation": "Push" });
                if let Some(u) = &target_uri {
                    open["target_uri"] = json!(u);
                }
                self.run(&open, out);
            }
            _ => {
                if let Some(u) = &target_uri {
                    self.open_screen(u, out);
                }
            }
        }
        out.push(Effect::Device {
            command: "open_notification".into(),
        });
    }

    /// `action.confirm`: the app's own dialog over everything, with the
    /// intent's title, message, confirm label and Cancel; confirm runs
    /// `then`, Cancel, Back or a tap outside do nothing.
    fn confirm(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        let title = intent["title"].as_str().unwrap_or("Are you sure?");
        let label = intent["confirm_label"].as_str().unwrap_or("OK");
        let accept = json!({ "intent": "action.sequence", "steps": [
            { "intent": "action.navigation.back" }, intent["then"].clone() ] });
        let button = |text: &str, tag: &str, on: Value| {
            json!({ "type": "core://Tappable", "tag": tag, "content_description": text, "on_click": on,
                "content": { "type": "core://Box", "padding_h_dp": 12, "padding_v_dp": 10,
                    "content_color": "${local://theme#accent}",
                    "content": { "type": "core://Text", "style": "Label", "value_expr": text } } })
        };
        let mut body =
            vec![json!({ "type": "core://Text", "style": "Title", "value_expr": title })];
        if let Some(m) = intent["message"].as_str().filter(|m| !m.trim().is_empty()) {
            body.push(json!({ "type": "core://Text", "style": "Body", "value_expr": m }));
        }
        body.push(json!({ "type": "core://Row", "spacing_dp": 8, "children": [
            { "type": "core://Spacer" },
            button("Cancel", "confirm:cancel", json!({ "intent": "action.navigation.back" })),
            button(label, "confirm:ok", accept),
        ] }));
        let root = json!({ "type": "core://Box", "fill": true, "align": "Center", "padding_h_dp": 32,
            "content": { "type": "core://Box", "background": "${local://theme#card}", "corner_dp": 28,
                "padding_h_dp": 24, "padding_v_dp": 24,
                "content": { "type": "core://Column", "spacing_dp": 16, "children": body } } });
        let Ok(root) = serde_json::from_value::<UiNode>(root) else {
            // Without a dialog, nothing runs.
            out.push(Effect::Nothing);
            return;
        };
        let mut e = Entry::layout("confirm", root, BTreeMap::new());
        e.frame = Frame::Dialog;
        self.stack.push(e);
        self.focused = None;
        out.push(Effect::Opened {
            layout: "confirm".into(),
        });
    }

    /// A message from the app itself (not a layout), in its own dialog with
    /// an OK button: what the platform reports (a link that opened the app,
    /// photos picked, a message received).
    /// A link that opened the app: true when it was a sign-in's redirect
    /// and went to the sign-in waiting for it ([`Loader::redirect`]).
    pub fn auth_redirect(&mut self, url: &str) -> bool {
        self.loader.as_ref().is_some_and(|l| l.redirect(url))
    }

    pub fn notice(&mut self, title: &str, message: &str) {
        let root = json!({ "type": "core://Box", "fill": true, "align": "Center", "padding_h_dp": 32,
            "content": { "type": "core://Box", "background": "${local://theme#card}", "corner_dp": 28,
                "padding_h_dp": 24, "padding_v_dp": 24,
                "content": { "type": "core://Column", "spacing_dp": 16, "children": [
                    { "type": "core://Text", "style": "Title", "value_expr": title },
                    { "type": "core://Text", "style": "Body", "value_expr": message },
                    { "type": "core://Row", "children": [
                        { "type": "core://Spacer" },
                        { "type": "core://Tappable", "tag": "notice:ok", "content_description": "OK",
                          "on_click": { "intent": "action.navigation.back" },
                          "content": { "type": "core://Box", "padding_h_dp": 12, "padding_v_dp": 10,
                              "content_color": "${local://theme#accent}",
                              "content": { "type": "core://Text", "style": "Label", "value_expr": "OK" } } } ] } ] } } });
        let Ok(root) = serde_json::from_value::<UiNode>(root) else {
            return;
        };
        let mut e = Entry::layout("notice", root, BTreeMap::new());
        e.frame = Frame::Dialog;
        self.stack.push(e);
        self.focused = None;
        self.invalidate();
    }

    /// `action.auth.session`: POSTs the form-bound payload to the pod path
    /// (a pod code to `/v1/pods/open`, or `/v1/pods/temporary`); its session
    /// token becomes the device's (`Effect::SignedIn`, which the platform
    /// saves and opens).
    fn open_session(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        let path = intent["path"].as_str().unwrap_or("");
        let payload = intent["payload"].clone();
        let Some(answer) = self
            .loader
            .as_ref()
            .and_then(|l| l.open_session(path, &payload))
        else {
            out.push(Effect::NotSent {
                intent: intent.clone(),
            });
            return;
        };
        match answer {
            Ok(token) => {
                // The code to remember: the one typed, else the new pod's.
                let code = payload["token"]
                    .as_str()
                    .filter(|c| !c.is_empty())
                    .unwrap_or(&token)
                    .to_owned();
                out.push(Effect::SignedIn { token, code });
            }
            Err(error) => {
                let s = self
                    .data
                    .entities
                    .entry("local://session".into())
                    .or_default();
                s.set("error", error.clone());
                out.push(Effect::LoadFailed {
                    what: path.to_owned(),
                    error,
                });
            }
        }
    }

    /// `set_server`: another of the platform's own servers
    /// ([`Loader::server`], never an entity a layout could have written),
    /// which the platform opens (`Effect::SwitchServer`) once the user says
    /// so in the app's own dialog (decision `device-owned-local-state`).
    fn set_server(&mut self, id: &str, out: &mut Vec<Effect>) {
        let Some((url, current)) = self.loader.as_ref().and_then(|l| l.server(id)) else {
            out.push(Effect::Nothing);
            return;
        };
        let url = url.trim().to_owned();
        if url.is_empty() || current {
            out.push(Effect::Nothing);
            return;
        }
        let message = format!("The app opens {url} from now on, here and at every start.");
        self.ask_device(
            "Switch server?",
            &message,
            "Switch",
            Asked::Switch(url),
            out,
        );
    }

    /// `sign_in` on another server (`value`): only a server the user typed
    /// into a field of this screen (a literal or any other binding is
    /// refused), and only once the user has seen its host in the app's own
    /// dialog.
    fn sign_in_elsewhere(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        let server = intent["value"].as_str().unwrap_or("").trim().to_owned();
        let id = intent["id"].as_str().unwrap_or("");
        let custom = self
            .data
            .entities
            .get(&format!("local://providers/{id}"))
            .and_then(|p| p.get("custom_server"))
            .is_some_and(crate::bind::truthy);
        if !custom {
            // The provider's own server: the value means nothing.
            let mut plain = intent.clone();
            if let Some(m) = plain.as_object_mut() {
                m.remove("value");
            }
            if self.loader.as_ref().is_some_and(|l| l.auth(&plain)) {
                out.push(Effect::Device {
                    command: "sign_in".into(),
                });
            } else {
                out.push(Effect::NotSent { intent: plain });
            }
            return;
        }
        if !self.form.values().any(|v| v.trim() == server) {
            eprintln!("construct-render: sign_in: {server:?} is not a server the user typed");
            out.push(Effect::Nothing);
            return;
        }
        let host = host_of(&server);
        let message = format!("Your browser opens {host} to sign in.");
        self.ask_device(
            &format!("Sign in on {host}?"),
            &message,
            "Continue",
            Asked::SignIn(intent.clone()),
            out,
        );
    }

    /// `action.auth.login`: on the pod's own server for a sign-in (its
    /// provider's, `local://providers`) at once; on any other server only
    /// once the user has seen its host in the app's own dialog, as for
    /// `sign_in` elsewhere, so a layout can't send the browser to a sign-in
    /// page of its choosing by sending this intent itself.
    fn login(&mut self, intent: &Value, out: &mut Vec<Effect>) {
        let server = intent["instance_url"].as_str().unwrap_or("").trim();
        let own = server.is_empty()
            || self
                .data
                .entities
                .range("local://providers/".to_owned()..)
                .take_while(|(uri, _)| uri.starts_with("local://providers/"))
                .filter_map(|(_, p)| p.get_str("server"))
                .any(|s| same_server(s, server));
        if own || !self.loader.as_ref().is_some_and(|l| l.signs_in()) {
            if self.loader.as_ref().is_some_and(|l| l.auth(intent)) {
                out.push(Effect::Device {
                    command: "sign_in".into(),
                });
            } else {
                out.push(Effect::NotSent {
                    intent: intent.clone(),
                });
            }
            return;
        }
        let host = host_of(server);
        let message = format!("Your browser opens {host} to sign in.");
        self.ask_device(
            &format!("Sign in on {host}?"),
            &message,
            "Continue",
            Asked::SignIn(intent.clone()),
            out,
        );
    }

    /// The app's own dialog before a device command a layout can't be
    /// trusted to choose alone: its OK runs [`ANSWER_ASKED`] with a
    /// nonce only this dialog holds, so a layout (an `action.sequence`
    /// after the command) can't answer it.
    fn ask_device(
        &mut self,
        title: &str,
        message: &str,
        label: &str,
        asked: Asked,
        out: &mut Vec<Effect>,
    ) {
        let nonce = new_nonce();
        self.asked = Some((nonce.clone(), asked));
        let confirm = json!({ "intent": "action.confirm", "title": title, "message": message,
            "confirm_label": label,
            "then": { "intent": "action.device", "command": ANSWER_ASKED, "id": nonce } });
        self.confirm(&confirm, out);
    }

    /// The user's OK in [`Session::ask_device`]'s dialog.
    fn answer_asked(&mut self, nonce: &str, out: &mut Vec<Effect>) {
        let asked = match self.asked.take() {
            Some((held, asked)) if !nonce.is_empty() && held == nonce => asked,
            other => {
                self.asked = other;
                out.push(Effect::Nothing);
                return;
            }
        };
        match asked {
            Asked::Switch(url) => out.push(Effect::SwitchServer { url }),
            Asked::SignIn(intent) => {
                if self.loader.as_ref().is_some_and(|l| l.auth(&intent)) {
                    out.push(Effect::Device {
                        command: "sign_in".into(),
                    });
                } else {
                    out.push(Effect::NotSent { intent });
                }
            }
        }
    }

    /// `toggle_channel`: the switch flips at once, then the pod is told; a
    /// refusal flips it back and says why.
    fn toggle_channel(&mut self, layout: &str, id: &str, out: &mut Vec<Effect>) {
        let uri = format!("local://channels/{layout}/{id}");
        let Some(c) = self.data.entities.get_mut(&uri) else {
            out.push(Effect::Nothing);
            return;
        };
        let on = !c.get("enabled").is_some_and(crate::bind::truthy);
        c.set("enabled", on);
        let args = json!({ "layout": layout, "id": id, "enabled": on });
        let answer = self
            .loader
            .as_ref()
            .and_then(|l| l.device_command("toggle_channel", &args));
        if !matches!(answer, Some(Ok(_)))
            && let Some(c) = self.data.entities.get_mut(&uri)
        {
            c.set("enabled", !on);
        }
        self.device_answer("toggle_channel", args, answer, out);
    }

    /// `load_older_notifications`: the next page of the inbox, after what
    /// it shows (`local://inbox` `next_before` is where it starts).
    fn load_older(&mut self, out: &mut Vec<Effect>) {
        let before = self
            .data
            .entities
            .get("local://inbox")
            .and_then(|i| i.get_str("next_before"))
            .unwrap_or("")
            .to_owned();
        if before.is_empty() {
            out.push(Effect::Nothing);
            return;
        }
        let args = json!({ "before": before });
        let answer = self
            .loader
            .as_ref()
            .and_then(|l| l.device_command("load_older_notifications", &args));
        let Some(Ok(mut page)) = answer else {
            return self.device_answer("load_older_notifications", args, answer, out);
        };
        // The older page goes under the newer ones; the inbox keeps its
        // counts and learns where the next page starts.
        let older = page
            .queries
            .remove("local:notifications")
            .unwrap_or_default();
        let list = self
            .data
            .queries
            .entry("local:notifications".into())
            .or_default();
        for u in older {
            if !list.contains(&u) {
                list.push(u);
            }
        }
        if let Some(fields) = page.entities.remove("local://inbox") {
            let inbox = self
                .data
                .entities
                .entry("local://inbox".into())
                .or_default();
            for (k, v) in fields.0 {
                inbox.set(k, v);
            }
        }
        self.merge_data(page);
        out.push(Effect::Device {
            command: "load_older_notifications".into(),
        });
    }

    /// What a device command's answer does: its data merged, or the reason
    /// it failed shown in the notice bar.
    fn device_answer(
        &mut self,
        command: &str,
        args: Value,
        answer: Option<Result<DataBundle, String>>,
        out: &mut Vec<Effect>,
    ) {
        match answer {
            Some(Ok(d)) => {
                self.merge_data(d);
                out.push(Effect::Device {
                    command: command.to_owned(),
                });
            }
            Some(Err(error)) => {
                self.notice = Some(notice_text(&error));
                out.push(Effect::LoadFailed {
                    what: command.to_owned(),
                    error,
                });
            }
            None => {
                let mut intent = args;
                intent["intent"] = json!("action.device");
                intent["command"] = json!(command);
                out.push(Effect::NotSent { intent });
            }
        }
    }

    /// The notice bar at the bottom, as the shared UI's: the text and
    /// Dismiss (tagged `notice` and `notice-dismiss`).
    fn notice_bar(&mut self, scene: &mut Scene, text: &str) -> Result<(), RenderError> {
        let (surface, on_surface, action) = if self.dark {
            ("#E6E1E5", "#313033", "#6750A4")
        } else {
            ("#313033", "#F4EFF4", "#D0BCFF")
        };
        let bar = json!({ "type": "core://Column", "children": [
            { "type": "core://Box", "padding_h_dp": 16, "padding_v_dp": 16,
              "content": { "type": "core://Tappable", "tag": "notice", "content_description": text,
                "on_click": { "intent": "action.device", "command": "notice" },
                "content": { "type": "core://Box", "background": surface, "corner_dp": 12,
                  "content_color": on_surface,
                  "content": { "type": "core://Row", "spacing_dp": 0, "children": [
                    { "type": "core://Spacer", "size_dp": 16 },
                    { "type": "core://Column", "padding_v_dp": 12, "children": [
                      { "type": "core://Text", "style": "Body", "value_expr": text } ] },
                    { "type": "core://Spacer" },
                    { "type": "core://Tappable", "tag": "notice-dismiss", "content_description": "Dismiss",
                      "on_click": { "intent": "action.device", "command": "dismiss_notice" },
                      "content": { "type": "core://Box", "padding_h_dp": 12, "padding_v_dp": 10,
                        "content_color": action,
                        "content": { "type": "core://Text", "style": "Label", "value_expr": "Dismiss" } } },
                    { "type": "core://Spacer", "size_dp": 4 }
                  ] } } } }
        ] });
        let root: UiNode = serde_json::from_value(bar).map_err(|e| RenderError(e.to_string()))?;
        let empty = BTreeMap::new();
        let laid = self.builder.layout(&Input {
            root: &root,
            data: &self.data,
            form: &empty,
            route: &empty,
            layouts: &BTreeMap::new(),
            scroll: &BTreeMap::new(),
            viewport: self.viewport,
            dark: self.dark,
            item: None,
            insets: [0.0; 4],
            focused: None,
            panels: &NO_PANELS,
        })?;
        let (w, h) = (self.viewport.width, self.viewport.height);
        let y = h - self.insets[2] - laid.used_height();
        laid.emit_into(scene, &BTreeMap::new(), (0.0, y), [0.0, 0.0, w, h], None);
        Ok(())
    }

    /// Where a node of the top entry sits among the panel hosts it laid
    /// out: the host (the semantic id of its content) and the column, 0
    /// for the host's own content and k + 1 for panel k. The innermost
    /// host when they nest.
    fn panel_slot(&self, id: &str) -> Option<(String, usize)> {
        let e = &self.stack[self.top_index()];
        let under = |prefix: &str| {
            id.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        };
        e.panel_hosts
            .iter()
            .filter_map(|h| {
                if under(h) {
                    return Some((h.clone(), 0));
                }
                let open = e.panels.get(h).map_or(0, Vec::len);
                (0..open)
                    .find(|&k| under(&panel_id(h, k)))
                    .map(|k| (h.clone(), k + 1))
            })
            .max_by_key(|(h, _)| h.len())
    }

    /// [`Session::panel_slot`] of a node of the current scene: its own id's,
    /// else its nearest ancestor's (a `tag` replaces a node's own id).
    fn panel_slot_of(&self, node: &SemanticNode) -> Option<(String, usize)> {
        let sems = &self.scene.as_ref()?.semantics;
        let mut at = Some(node);
        while let Some(n) = at {
            if let Some(slot) = self.panel_slot(&n.id) {
                return Some(slot);
            }
            at = n.parent.and_then(|i| sems.get(i));
        }
        None
    }

    /// A tapped screen open (`action.navigation.open` of a `target_uri`, no
    /// layout) inside panel k of a host, or inside a layout embedded inline
    /// (`core://Layout`, as home's pane beside its list on a wide window
    /// draws the row picked): that layout and the address, when one of its
    /// screens has the route; the innermost such layout first. `None`
    /// elsewhere (the top entry's screens take it, as before).
    fn panel_screen(&self, node: &SemanticNode, intent: &Value) -> Option<(String, String)> {
        if intent["intent"] != "action.navigation.open"
            || intent["layout"].as_str().is_some_and(|l| !l.is_empty())
        {
            return None;
        }
        let uri = intent["target_uri"].as_str().filter(|t| !t.is_empty())?;
        let has_route = |layout: &str| match self.layouts.get(layout) {
            Some(UiNode::Page { screens, .. }) => {
                screens.iter().any(|s| match_route(&s.route, uri).is_some())
            }
            _ => false,
        };
        if let Some((host, slot)) = self.panel_slot_of(node) {
            let e = &self.stack[self.top_index()];
            if let Some(p) = slot
                .checked_sub(1)
                .and_then(|k| e.panels.get(&host)?.get(k))
                && has_route(&p.layout)
            {
                return Some((p.layout.clone(), uri.to_owned()));
            }
        }
        // The layouts embedded around the node, innermost first: each
        // embed's own node is `layout:<id>` among the semantics.
        let sems = &self.scene.as_ref()?.semantics;
        let mut at = Some(node);
        while let Some(n) = at {
            if let Some(layout) = n.id.strip_prefix("layout:")
                && has_route(layout)
            {
                return Some((layout.to_owned(), uri.to_owned()));
            }
            at = n.parent.and_then(|i| sems.get(i));
        }
        None
    }

    /// A push or Back tapped inside a panel host: the push opens a panel
    /// right of the column it came from (closing those past it), and Back
    /// in a panel closes it and those past it.
    fn in_panels(&mut self, node: &SemanticNode, intent: &Value, out: &mut Vec<Effect>) {
        let Some((host, slot)) = self.panel_slot_of(node) else {
            return;
        };
        let ti = self.top_index();
        if intent["intent"] == "action.navigation.back" {
            if let Some(open) = self.stack[ti].panels.get_mut(&host) {
                open.truncate(slot.saturating_sub(1));
            }
            out.push(Effect::Back);
            return;
        }
        let Some(layout) = intent["layout"].as_str() else {
            return;
        };
        if self.layout_root(layout, out).is_none() {
            return;
        }
        let route = intent["params"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), crate::bind::render(v)))
                    .collect()
            })
            .unwrap_or_default();
        let open = self.stack[ti].panels.entry(host).or_default();
        open.truncate(slot);
        open.push(Panel {
            layout: layout.to_owned(),
            route,
        });
        self.load_inline_layouts();
        out.push(Effect::Opened {
            layout: layout.to_owned(),
        });
    }

    /// System Back with panels open on the top entry: the rightmost panel
    /// of a host drawn now closes. False when none is open.
    fn close_panel(&mut self) -> bool {
        let ti = self.top_index();
        let e = &mut self.stack[ti];
        let host = e
            .panel_hosts
            .iter()
            .rev()
            .find(|h| e.panels.get(*h).is_some_and(|p| !p.is_empty()))
            .cloned();
        match host.and_then(|h| e.panels.get_mut(&h)) {
            Some(open) => {
                open.pop();
                // What is laid out changed, not only what is stacked.
                e.relay();
                true
            }
            None => false,
        }
    }

    /// A layout's root: given, or loaded (its data merged).
    fn layout_root(&mut self, id: &str, out: &mut Vec<Effect>) -> Option<UiNode> {
        if let Some(r) = self.layouts.get(id) {
            return Some(r.clone());
        }
        let loaded = self.loader.as_ref()?.layout(id);
        match loaded {
            Ok((root, data)) => {
                self.merge_data(data);
                self.layouts.insert(id.to_owned(), root.clone());
                Some(root)
            }
            Err(error) => {
                out.push(Effect::LoadFailed {
                    what: format!("layout {id}"),
                    error,
                });
                None
            }
        }
    }

    /// Opens the screen of the top layout whose route matches `uri`, loading
    /// its data; false when no screen matches.
    fn open_screen(&mut self, uri: &str, out: &mut Vec<Effect>) -> bool {
        self.open_screen_from(uri, None, out)
    }

    /// [`Session::open_screen`] from the screens of `from` (a layout shown
    /// in a panel of the top entry) instead of the top entry's: the screen
    /// is pushed over the whole window.
    fn open_screen_from(&mut self, uri: &str, from: Option<&str>, out: &mut Vec<Effect>) -> bool {
        let top = &self.stack[self.top_index()];
        let (id, screens, frame, size) = match from {
            Some(layout) => match self.layouts.get(layout) {
                Some(UiNode::Page { screens, .. }) => {
                    (layout.to_owned(), screens.clone(), Frame::Full, None)
                }
                _ => return false,
            },
            None => (top.id.clone(), top.screens.clone(), top.frame, top.size),
        };
        let Some((screen, route)) = screens
            .iter()
            .find_map(|s| match_route(&s.route, uri).map(|r| (s.clone(), r)))
        else {
            return false;
        };
        let mut root = serde_json::to_value(&screen.root).unwrap_or(Value::Null);
        bind_route(&mut root, &route);
        let Ok(root) = serde_json::from_value::<UiNode>(root) else {
            return false;
        };
        let item = screen.item.as_ref().map(|i| {
            let mut v = Value::String(i.clone());
            bind_route(&mut v, &route);
            crate::bind::render(&v)
        });
        self.stack.push(Entry {
            id,
            root,
            route: route.clone(),
            item,
            screens,
            error: None,
            loading: None,
            data: None,
            scroll: BTreeMap::new(),
            in_frame: frame != Frame::Full,
            frame,
            size,
            opened: Instant::now(),
            closing: None,
            laid: None,
            stale: None,
            screen: true,
            screen_id: screen.id.clone(),
            follow: false,
            panels: Panels::new(),
            panel_hosts: Vec::new(),
        });
        out.push(Effect::OpenedScreen {
            screen: screen.id.clone(),
        });
        if let Some(path) = &screen.data {
            let mut v = Value::String(path.clone());
            bind_route(&mut v, &route);
            let path = crate::bind::render(&v);
            let ti = self.top_index();
            self.stack[ti].data = Some(path.clone());
            self.load_data(&path, out);
        }
        true
    }

    fn load_data(&mut self, path: &str, out: &mut Vec<Effect>) {
        let Some(loader) = self.loader.as_ref() else {
            out.push(Effect::NotSent {
                intent: json!({ "intent": "action.data.load", "path": path }),
            });
            return;
        };
        // In the background where the loader can: the screen slides in at
        // once, showing `local://screen#loading`, and fills when it arrives.
        let answered = self.answered.clone();
        let images = self.images.clone();
        let p = path.to_owned();
        let done: Loaded = Box::new(move |result| {
            if let Ok(mut g) = answered.lock() {
                g.push(Answer::Data { path: p, result });
            }
            images.wake();
        });
        if loader.fetch(path, done) {
            let ti = self.top_index();
            self.stack[ti].loading = Some(path.to_owned());
            self.stack[ti].error = None;
            return;
        }
        let result = loader.data(path);
        let ti = self.top_index();
        let top = &mut self.stack[ti];
        match result {
            Ok(d) => {
                top.error = None;
                self.merge_data(d);
            }
            Err(error) => {
                top.error = Some(error.clone());
                out.push(Effect::LoadFailed {
                    what: path.to_owned(),
                    error,
                });
            }
        }
    }

    /// Tells the loader what the entries `drawn` (bottom first) show, when
    /// it changed: each one's layout and data path, and the layouts drawn
    /// inline in them, topmost first ([`Loader::visible`]).
    fn report_shown(&mut self, drawn: &[usize]) {
        let Some(loader) = self.loader.as_ref() else {
            return;
        };
        let mut shown = Vec::new();
        for &i in drawn.iter().rev() {
            let e = &self.stack[i];
            if let Some(d) = &e.data {
                push_new(&mut shown, Shown::Data(d.clone()));
            }
            push_new(&mut shown, Shown::Layout(e.id.clone()));
            // Inline layouts and panels, and theirs (each once).
            let mut roots = vec![&e.root];
            for p in e.panels.values().flatten() {
                if push_new(&mut shown, Shown::Layout(p.layout.clone()))
                    && let Some(r) = self.layouts.get(&p.layout)
                {
                    roots.push(r);
                }
            }
            while let Some(root) = roots.pop() {
                for n in root.walk() {
                    if let UiNode::Layout {
                        layout,
                        presentation: protocol::EmbedPresentation::Inline,
                        ..
                    } = n
                        && !layout.contains('$')
                        && push_new(&mut shown, Shown::Layout(layout.clone()))
                        && let Some(r) = self.layouts.get(layout)
                    {
                        roots.push(r);
                    }
                }
            }
        }
        if shown != self.shown {
            loader.visible(&shown);
            self.shown = shown;
        }
    }

    /// Loads the layouts inline `core://Layout`s of the top entry draw,
    /// and the ones those draw inline in turn (a tab's layout embedding
    /// its views): each loaded layout's own inline embeds are looked at
    /// too, until nothing new can be loaded.
    fn load_inline_layouts(&mut self) {
        if self.loader.is_none() {
            return;
        }
        let mut ignored = Vec::new();
        loop {
            let missing = self.missing_inline_layouts();
            if missing.is_empty() {
                return;
            }
            let mut loaded = false;
            for id in missing {
                loaded |= self.layout_root(&id, &mut ignored).is_some();
            }
            // A layout that failed to load is asked for again next time,
            // not in a loop here.
            if !loaded {
                return;
            }
        }
    }

    /// The inline layouts the top entry draws, directly or inside the
    /// inline layouts and panels it draws, that aren't loaded yet.
    fn missing_inline_layouts(&self) -> Vec<String> {
        let top = &self.stack[self.top_index()];
        let mut roots: Vec<&UiNode> = std::iter::once(&top.root)
            .chain(
                top.panels
                    .values()
                    .flatten()
                    .filter_map(|p| self.layouts.get(&p.layout)),
            )
            .collect();
        let mut seen = std::collections::HashSet::new();
        let mut missing = Vec::new();
        while let Some(root) = roots.pop() {
            for n in root.walk() {
                let UiNode::Layout {
                    layout,
                    presentation: protocol::EmbedPresentation::Inline,
                    ..
                } = n
                else {
                    continue;
                };
                if layout.contains('$') || !seen.insert(layout.as_str()) {
                    continue;
                }
                match self.layouts.get(layout) {
                    Some(r) => roots.push(r),
                    None => missing.push(layout.clone()),
                }
            }
        }
        missing
    }

    /// The field a tap focused, for platforms that show a keyboard.
    pub fn focused_field(outcome: &Outcome) -> Option<&str> {
        outcome.effects.iter().find_map(|e| match e {
            Effect::Focused { field } => Some(field.as_str()),
            _ => None,
        })
    }

    /// A summary of the state, for agents: the layout shown and the form.
    pub fn state(&self) -> Value {
        json!({ "layout": self.current(), "form": self.form,
                "scroll": self.stack.last().map(|e| e.scroll.clone()).unwrap_or_default(),
                "stack": self.stack.iter().map(|e| e.id.clone()).collect::<Vec<_>>() })
    }
}

/// Adds `s` unless `list` has it; true when added.
fn push_new(list: &mut Vec<Shown>, s: Shown) -> bool {
    if list.contains(&s) {
        return false;
    }
    list.push(s);
    true
}

/// An error as the notice bar shows it: the pod's own `detail` (or error
/// code) when it answered one, else the error's first line; at most 160
/// characters.
fn notice_text(error: &str) -> String {
    let said = error
        .find('{')
        .and_then(|i| serde_json::from_str::<Value>(&error[i..]).ok())
        .and_then(|v| {
            let s = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty());
            s("detail")
                .or(s("message"))
                .map(str::to_owned)
                .or_else(|| s("error").map(|e| format!("The pod refused it ({e})")))
        });
    let line = said
        .as_deref()
        .unwrap_or_else(|| error.lines().next().unwrap_or(""))
        .trim();
    let mut t: String = line.chars().take(160).collect();
    if line.chars().count() > 160 {
        t.push('…');
    }
    if t.is_empty() {
        "Something went wrong".into()
    } else {
        t
    }
}

/// Every `(field, value_expr)` evaluated against `entity` as it is, with
/// the engine's expression language (`protocol::expr`, as client-core). `None`
/// if one fails: then nothing changes, as in client-core.
fn eval_all(
    updates: &[(String, String)],
    entity: &protocol::EntityRecord,
) -> Option<Vec<(String, Value)>> {
    updates
        .iter()
        .map(|(f, e)| match protocol::expr::eval(e, entity) {
            Ok(v) => Some((f.clone(), v)),
            Err(err) => {
                eprintln!("[construct-render] {f} = {e}: {err}; no change");
                None
            }
        })
        .collect()
}

/// Every string that is exactly `$value` replaced by `value` (client-core's
/// `substitute_value_token`).
fn substitute_value(v: &mut Value, value: &Value) {
    match v {
        Value::String(s) if s == "$value" => *v = value.clone(),
        Value::Array(a) => a.iter_mut().for_each(|x| substitute_value(x, value)),
        Value::Object(m) => m.values_mut().for_each(|x| substitute_value(x, value)),
        _ => {}
    }
}

/// The `action.device` command of the OK in [`Session::ask_device`]'s
/// dialog (`id`: its nonce): the session's own, not in `DEVICE_COMMANDS`,
/// so the pod refuses it in a layout.
const ANSWER_ASKED: &str = "answer_asked";

/// A device command [`Session::ask_device`] holds until the user's OK.
#[derive(Debug, Clone)]
enum Asked {
    /// `set_server`: the platform's server to open.
    Switch(String),
    /// `sign_in` on a server the user typed: the bound intent.
    SignIn(Value),
}

/// The host of a server the user typed (`https://pix.example/x` →
/// `pix.example`), for the dialog before signing in there.
/// Whether two server URLs name one server: ignoring case and a trailing
/// `/`.
fn same_server(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

fn host_of(server: &str) -> String {
    let rest = server.split_once("://").map_or(server, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // Credentials before an `@` are not the host.
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    if host.is_empty() {
        server.to_owned()
    } else {
        host.to_owned()
    }
}

/// 24 hex characters, fresh for each tap (`$nonce`).
fn new_nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(n);
        h.write_u64(i);
        if let Ok(t) = crate::time::SystemTime::now().duration_since(crate::time::UNIX_EPOCH) {
            h.write_u128(t.as_nanos());
        }
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(24);
    out
}

/// A form value that is a list (`["a","b"]`, a media picker's staged ids).
fn form_list(v: Option<&String>) -> Option<Vec<String>> {
    let v = v?.trim();
    if !v.starts_with('[') {
        return None;
    }
    serde_json::from_str(v).ok()
}

fn bind_form_value(
    v: &Value,
    key: Option<&str>,
    form: &BTreeMap<String, String>,
    nonce: &str,
    missing: &mut bool,
) -> Value {
    match v {
        Value::String(s) if key != Some("value_expr") && key != Some("intent") => {
            bind_form_text(s, form, nonce, missing)
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| bind_form_value(x, None, form, nonce, missing))
                .collect(),
        ),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), bind_form_value(x, Some(k), form, nonce, missing)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn bind_form_text(
    s: &str,
    form: &BTreeMap<String, String>,
    nonce: &str,
    missing: &mut bool,
) -> Value {
    let name_len = |t: &str| {
        t.char_indices()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
            .map_or(t.len(), |(j, _)| j)
    };
    // A whole `$form.<f>` of a list value is the list itself.
    for (prefix, optional) in [("$form?.", true), ("$form.", false)] {
        if let Some(name) = s.strip_prefix(prefix)
            && !name.is_empty()
            && name_len(name) == name.len()
            && let Some(list) = form_list(form.get(name))
        {
            if list.is_empty() && !optional {
                *missing = true;
            }
            return json!(list);
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        if let Some(t) = after.strip_prefix("$nonce") {
            out.push_str(nonce);
            rest = t;
            continue;
        }
        let token = after
            .strip_prefix("$form?.")
            .map(|t| (t, true))
            .or_else(|| after.strip_prefix("$form.").map(|t| (t, false)));
        match token {
            Some((t, optional)) if name_len(t) > 0 => {
                let n = name_len(t);
                let raw = form.get(&t[..n]);
                let v = if form_list(raw).is_some_and(|l| l.is_empty()) {
                    String::new()
                } else {
                    raw.map(|r| r.trim().to_owned()).unwrap_or_default()
                };
                if v.is_empty() && !optional {
                    *missing = true;
                }
                out.push_str(&v);
                rest = &t[n..];
            }
            _ => {
                out.push('$');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    Value::String(out)
}

/// Paints a full-screen layout's page colours (`Laid::edge_colors`: top,
/// right, bottom, left) into the window's insets around its frame `at`,
/// where the system bars are drawn over it: they then show the page, not
/// the theme's default. An edge without one keeps what is under it.
fn inset_bands(scene: &mut Scene, at: [f32; 4], insets: [f32; 4], edges: &[Option<Rgba>; 4]) {
    let [x, y, w, h] = at;
    let [it, ir, ib, il] = insets.map(|v| v.max(0.0));
    let bands = [
        [x, y, w, it],
        [x + w - ir, y + it, ir, h - it - ib],
        [x, y + h - ib, w, ib],
        [x, y + it, il, h - it - ib],
    ];
    for (band, color) in bands.into_iter().zip(edges) {
        if let Some(c) = color
            && band[2] > 0.0
            && band[3] > 0.0
        {
            scene
                .draws
                .push(Draw::Quad(QuadInstance::fill(band, *c, 0.0)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_holds_the_slides_under_way() {
        let root: UiNode =
            serde_json::from_value(serde_json::json!({ "type": "core://Spacer" })).unwrap();
        let mut s = Session::new("home", root.clone(), DataBundle::default(), Viewport::PHONE);
        s.animate = true;
        let now = Instant::now();
        let took = Duration::from_millis(15);
        let mut sliding = Entry::layout("a", root.clone(), BTreeMap::new());
        sliding.opened = now - Duration::from_millis(40);
        let mut settled = Entry::layout("b", root.clone(), BTreeMap::new());
        settled.opened = now - Duration::from_secs(2);
        let mut closing = Entry::layout("c", root, BTreeMap::new());
        closing.closing = Some(now - Duration::from_millis(100));
        s.stack.extend([sliding, settled, closing]);
        // The slide at `now + took` is where it was at `now`.
        let before = s.progress(1, now);
        s.hold_slides(now, took);
        assert_eq!(s.progress(1, now + took), before);
        assert_eq!(s.stack[1].opened, now - Duration::from_millis(25));
        // A slide that ended stays ended; one closing is held too.
        assert_eq!(s.stack[2].opened, now - Duration::from_secs(2));
        assert_eq!(s.stack[3].closing, Some(now - Duration::from_millis(85)));
    }

    /// A loader whose changes a test hands it.
    struct News(Arc<Mutex<Option<Changes>>>);

    impl Loader for News {
        fn layout(&self, id: &str) -> Result<(UiNode, DataBundle), String> {
            Err(format!("no layout {id}"))
        }
        fn data(&self, _path: &str) -> Result<DataBundle, String> {
            Ok(DataBundle::default())
        }
        fn changes(&self) -> Option<Changes> {
            self.0.lock().unwrap().take()
        }
    }

    #[test]
    fn changes_wait_for_the_slide_to_end_and_the_users_own_writes_dont() {
        let root: UiNode = serde_json::from_value(serde_json::json!({
            "type": "core://Column", "children": [
                { "type": "core://Text", "style": "Body", "value_expr": "${x://g/a#name}" },
                { "type": "core://Text", "style": "Body", "value_expr": "${x://g/b#name}" }
            ]
        }))
        .unwrap();
        let named = |uri: &str, name: &str| {
            let mut e = protocol::EntityRecord::new();
            e.set("name", name);
            (uri.to_owned(), e)
        };
        let mut data = DataBundle::default();
        data.entities
            .extend([named("x://g/a", "Basil"), named("x://g/b", "Mint")]);
        let news = Arc::new(Mutex::new(None));
        let mut s = Session::new("home", root.clone(), data, Viewport::PHONE);
        s.loader = Some(Box::new(News(news.clone())));
        s.animate = true;
        let texts = |s: &mut Session| -> Vec<String> {
            s.scene()
                .unwrap()
                .semantics
                .iter()
                .filter_map(|n| n.text_content.clone())
                .collect()
        };
        assert!(texts(&mut s).contains(&"Basil".to_owned()));
        s.take_work();

        // A screen slides in; the engine's answer lands mid-slide.
        let mut opening = Entry::layout("calendar", root, BTreeMap::new());
        opening.opened = Instant::now();
        s.stack.push(opening);
        let mut fresh = DataBundle::default();
        fresh.entities.extend([named("x://g/a", "Thyme")]);
        *news.lock().unwrap() = Some(Changes {
            data: fresh,
            layouts: Vec::new(),
        });
        assert!(!s.poll());
        s.scene().unwrap();
        assert_eq!(s.data.entities["x://g/a"].get_str("name"), Some("Basil"));
        assert_eq!(s.take_work().data_changes, 0);
        // The window keeps drawing so the frame after the slide applies it.
        assert!(s.animating());

        // The user's own write during the slide shows at once.
        s.dispatch(&serde_json::json!({
            "intent": "action.local.set", "entity_uri": "x://g/b",
            "field": "name", "value_expr": "\"Sage\""
        }))
        .unwrap();
        assert_eq!(s.data.entities["x://g/b"].get_str("name"), Some("Sage"));
        s.scene().unwrap();
        assert_eq!(s.data.entities["x://g/a"].get_str("name"), Some("Basil"));

        // The slide ends: the first frame after it applies the change.
        let ended = Instant::now() - SLIDE - Duration::from_millis(1);
        s.stack[1].opened = ended;
        let shown = texts(&mut s);
        assert!(shown.contains(&"Thyme".to_owned()), "{shown:?}");
        assert!(shown.contains(&"Sage".to_owned()));
        assert_eq!(s.take_work().data_changes, 1);
        assert!(!s.animating());
    }

    #[test]
    fn the_clipboard_is_the_users_not_a_layouts() {
        let root: UiNode = serde_json::from_value(json!({ "type": "core://Column", "children": [
            { "type": "core://TextField", "field": "f", "placeholder": "F" } ] }))
        .unwrap();
        let mut s = Session::new("home", root, DataBundle::default(), Viewport::PHONE);
        s.focus_at("f", 20.0, 20.0, false);
        let paste = json!({ "intent": "edit.paste" });
        let copy = json!({ "intent": "edit.copy" });
        // Run as a layout's tap or gesture would: nothing is read or written.
        for intent in [&paste, &copy] {
            let mut out = Vec::new();
            s.run(intent, &mut out);
            assert_eq!(out, vec![Effect::Nothing], "{intent}");
        }
        // The edit menu's own tap, or the platform keyboard's, may.
        s.clipboard_ok = true;
        let mut out = Vec::new();
        s.run(&paste, &mut out);
        assert_eq!(out, vec![Effect::Paste { field: "f".into() }]);
    }

    #[test]
    fn an_effects_name_is_its_tag_and_carries_no_value() {
        let effects = [
            Effect::FormSet {
                field: "pod_token".into(),
                value: "secret-code".into(),
            },
            Effect::SignedIn {
                token: "secret-session".into(),
                code: "secret-code".into(),
            },
            Effect::Copy {
                text: "secret-text".into(),
            },
            Effect::LeftPod,
            Effect::Nothing,
        ];
        for e in &effects {
            let tagged = serde_json::to_value(e).unwrap();
            assert_eq!(tagged["effect"], e.name(), "{e:?}");
            assert!(!e.name().contains("secret"));
        }
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::Anchor;

    fn items(list: &[(&str, f32)]) -> Vec<(String, f32, f32)> {
        list.iter()
            .map(|(u, t)| (u.to_string(), *t, 40.0))
            .collect()
    }

    #[test]
    fn an_anchor_keeps_its_item_and_falls_back_to_the_next() {
        let before = items(&[("a", 0.0), ("b", 40.0), ("c", 80.0), ("d", 120.0)]);
        // Scrolled 10 points into b.
        let a = Anchor::at(50.0, &before).unwrap();
        assert_eq!((a.uri.as_str(), a.offset_dp), ("b", 10.0));
        // Two rows above: b moved down 80, so does the offset.
        let above = items(&[
            ("x", 0.0),
            ("y", 40.0),
            ("a", 80.0),
            ("b", 120.0),
            ("c", 160.0),
        ]);
        assert_eq!(a.offset_in(&above), Some(130.0));
        // b is gone: c stays where it was (30 points below the top).
        let gone = items(&[("a", 0.0), ("c", 40.0), ("d", 80.0)]);
        assert_eq!(a.offset_in(&gone), Some(10.0));
        // Only rows before it are left: the one before keeps its place.
        assert_eq!(a.offset_in(&items(&[("a", 0.0)])), Some(50.0));
        assert_eq!(a.offset_in(&items(&[("z", 0.0)])), None);
    }

    #[test]
    fn a_view_whose_top_is_above_every_row_keeps_its_offset() {
        // A search box above a list, the view at its top: rows that appear
        // under the box (results while typing) push the list down, and the
        // view stays on the box instead of following the list.
        let list = items(&[("a", 120.0), ("b", 160.0)]);
        assert_eq!(Anchor::at(0.0, &list), None);
        assert_eq!(Anchor::at(119.0, &list), None);
        // In the gap or a heading just past a row, that row is the anchor.
        let a = Anchor::at(205.0, &list).unwrap();
        assert_eq!((a.uri.as_str(), a.offset_dp), ("b", 45.0));
    }

    #[test]
    fn an_item_drawn_twice_keeps_the_one_it_was_on() {
        // A preview of a and b above the full list a, b, c.
        let both = items(&[
            ("a", 0.0),
            ("b", 40.0),
            ("a", 100.0),
            ("b", 140.0),
            ("c", 180.0),
        ]);
        // Scrolled 10 points into the full list's a, not the preview's.
        let a = Anchor::at(110.0, &both).unwrap();
        assert_eq!((a.uri.as_str(), a.nth, a.offset_dp), ("a", 1, 10.0));
        assert_eq!(a.offset_in(&both), Some(110.0));
        // A row above moves both copies down 30: the full list's a stays.
        let lower: Vec<(String, f32, f32)> = both
            .iter()
            .map(|(u, t, h)| (u.clone(), t + 30.0, *h))
            .collect();
        assert_eq!(a.offset_in(&lower), Some(140.0));
        // The preview went (a drawn once now): that one keeps its place.
        let once = items(&[("a", 20.0), ("b", 60.0), ("c", 100.0)]);
        assert_eq!(a.offset_in(&once), Some(30.0));
    }

    #[test]
    fn state_under_a_moved_embed_follows_it() {
        use super::{move_keys, moved_embeds};
        use crate::layout::embed_prefixes;
        use std::collections::{BTreeMap, BTreeSet};

        assert_eq!(
            embed_prefixes("root/1/tab/layout:c-a/0/layout:c-b/2"),
            vec![
                (
                    "layout:c-a".to_string(),
                    "root/1/tab/layout:c-a".to_string()
                ),
                (
                    "layout:c-a/layout:c-b".to_string(),
                    "root/1/tab/layout:c-a/0/layout:c-b".to_string()
                ),
            ]
        );
        assert!(embed_prefixes("layout:c-a").is_empty());
        assert!(embed_prefixes("field:note").is_empty());

        let embeds = |paths: &[&str]| {
            let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for p in paths {
                for (chain, path) in embed_prefixes(p) {
                    m.entry(chain).or_default().insert(path);
                }
            }
            m
        };
        // A node was put above the embed: c-a went from child 1 to child 2.
        let before = embeds(&["root/1/layout:c-a/0", "root/1/layout:c-a/3/layout:c-b/0"]);
        let after = embeds(&["root/2/layout:c-a/0", "root/2/layout:c-a/3/layout:c-b/0"]);
        let moves = moved_embeds(&before, &after);
        let mut scroll: BTreeMap<String, f32> = [
            ("root/1/layout:c-a/0", 300.0),
            ("stretch:root/1/layout:c-a/0", 4.0),
            ("root/1/layout:c-a/3/layout:c-b/0", 50.0),
            ("root/1/layout:c-ab/0", 9.0),
            ("root/0", 7.0),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        move_keys(&mut scroll, &moves);
        let keys: Vec<(&str, f32)> = scroll.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            keys,
            vec![
                ("root/0", 7.0),
                ("root/1/layout:c-ab/0", 9.0),
                ("root/2/layout:c-a/0", 300.0),
                ("root/2/layout:c-a/3/layout:c-b/0", 50.0),
                ("stretch:root/2/layout:c-a/0", 4.0),
            ]
        );
        // A layout embedded twice can't be told apart: nothing moves.
        let twice = embeds(&["root/2/layout:c-a/0", "root/3/layout:c-a/0"]);
        assert!(
            moved_embeds(&before, &twice)
                .iter()
                .all(|(o, _)| !o.ends_with("layout:c-a"))
        );
        assert!(moved_embeds(&before, &before).is_empty());
    }
}
