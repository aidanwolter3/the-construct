//! The script language (docs/simulator.md): commands separated by `;` or
//! newlines, `#` comments, the first word the command and the rest of the
//! entry its argument (`click Count: 2` clicks "Count: 2").
//!
//! Shared by the simulator (`construct-sim`) and the pod's journey walks
//! (`walk_journey`, decision `journeys-and-dry-runs`), so a journey is
//! written in the commands the simulator runs. [`find`] is how both find the
//! node a target names.

pub mod screen;

use crate::scene::{Scene, SemanticNode};
use screen::Screen;

/// Upper bound for `wait` and every timeout, 10 minutes.
pub const MAX_WAIT_MS: u64 = 600_000;
/// The longest `drag-down`, in points.
pub const MAX_DRAG: u32 = 2000;
/// `wait-images` without a timeout.
pub const DEFAULT_WAIT_IMAGES_MS: u64 = 10_000;

/// Where a layout opened by `open-layout` is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    Push,
    Sheet,
    Panel,
}

impl Presentation {
    /// The `presentation` of `action.navigation.open`.
    pub fn wire(self) -> &'static str {
        match self {
            Presentation::Push => "Push",
            Presentation::Sheet => "Sheet",
            Presentation::Panel => "Panel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Shot(String),
    Click(String),
    /// A finger held down on a node (drawn pressed) until `release` or a
    /// `click`.
    Press(String),
    /// The mouse resting over a node (drawn hovered), until another
    /// `hover` or `click`.
    Hover(String),
    Release,
    Type {
        target: String,
        text: String,
    },
    Pick(Vec<String>),
    /// `swipe-left`, `swipe-right` (a carousel or a gesture), `swipe-down`,
    /// `swipe-up` (a quick flick: a sheet's handle or a gesture).
    Swipe(Direction, String),
    DragDown {
        points: u32,
        target: String,
    },
    /// A swipe card moved sideways by `points` (negative: left) and held
    /// there until `release`.
    DragCard {
        points: i32,
        target: String,
    },
    ScrollTo(String),
    /// `tap-photo left|right <target>`: a tap on the left or right third of
    /// the tap-paged carousel at the target (feature `carousel_tap_paging`).
    TapPhoto {
        right: bool,
        target: String,
    },
    /// `scatter-box`/`scatter-select <x0> <y0> <x1> <y1> <target>`: a box
    /// dragged on a scatter between data points (0..1); `scatter-box` holds
    /// it (drawn until `release`, which ends it), `scatter-select` lets go.
    ScatterBox {
        bounds: [f32; 4],
        target: String,
        release: bool,
    },
    /// `scatter-zoom <factor> <target>`: zoomed about the plot's middle.
    ScatterZoom {
        factor: f32,
        target: String,
    },
    /// `scatter-tap <x> <y> <target>`: a tap at a data point (0..1).
    ScatterTap {
        at: [f32; 2],
        target: String,
    },
    Back,
    Wait(u64),
    Idle,
    AssertText(String),
    AssertNoText(String),
    WaitText {
        ms: u64,
        text: String,
    },
    AssertLink(String),
    WaitImages(u64),
    Screen(Screen),
    /// `mode dark` or `mode light`: the system's mode (light at start).
    Dark(bool),
    /// `insets <top> <right> <bottom> <left>` (points): the space a phone's
    /// system bars, cutout or keyboard cover (none at start).
    Insets([f32; 4]),
    /// `clock YYYY-MM-DDTHH:MM`: the device's wall clock stops there (in
    /// seconds since the epoch).
    Clock(i64),
    Offline,
    Online,
    /// `not-saving <error>` (`not-saving` alone clears it): changes wait
    /// and the pod answers them with `error`.
    NotSaving(Option<String>),
    PublishVersion(String),
    OpenLayout {
        layout: String,
        how: Presentation,
    },
    OpenSettings,
    OpenInbox,
    Notify {
        app: String,
        channel: String,
        title: String,
    },
    ToggleChannel(String),
    /// `expect-write <collection> {field: value, ...}`: a write the journey
    /// made (dry-run, never sent) put a row like that in the collection
    /// (`walk_journey` only, decision `journeys-and-dry-runs`).
    ExpectWrite {
        collection: String,
        fields: serde_json::Map<String, serde_json::Value>,
    },
    /// Fixture mode: the pod's construct moves to the one in this file
    /// (decision `content-addressed-construct`): `{"layouts": {key: page},
    /// "reach": {key: [keys]}, "data"?: {entities, queries}}`.
    Construct(String),
}

impl Step {
    /// The command's name, as written.
    pub fn command(&self) -> &'static str {
        match self {
            Step::Shot(_) => "shot",
            Step::Click(_) => "click",
            Step::Press(_) => "press",
            Step::Hover(_) => "hover",
            Step::Release => "release",
            Step::Type { .. } => "type",
            Step::Pick(_) => "pick",
            Step::Swipe(Direction::Left, _) => "swipe-left",
            Step::Swipe(Direction::Right, _) => "swipe-right",
            Step::Swipe(Direction::Up, _) => "swipe-up",
            Step::Swipe(Direction::Down, _) => "swipe-down",
            Step::DragDown { .. } => "drag-down",
            Step::DragCard { .. } => "drag-card",
            Step::ScrollTo(_) => "scroll-to",
            Step::TapPhoto { .. } => "tap-photo",
            Step::ScatterBox { release: false, .. } => "scatter-box",
            Step::ScatterBox { release: true, .. } => "scatter-select",
            Step::ScatterZoom { .. } => "scatter-zoom",
            Step::ScatterTap { .. } => "scatter-tap",
            Step::Back => "back",
            Step::Wait(_) => "wait",
            Step::Idle => "idle",
            Step::AssertText(_) => "assert-text",
            Step::AssertNoText(_) => "assert-no-text",
            Step::WaitText { .. } => "wait-text",
            Step::AssertLink(_) => "assert-link",
            Step::WaitImages(_) => "wait-images",
            Step::Screen(_) => "screen",
            Step::Dark(_) => "mode",
            Step::Insets(_) => "insets",
            Step::Clock(_) => "clock",
            Step::Offline => "offline",
            Step::Online => "online",
            Step::NotSaving(_) => "not-saving",
            Step::PublishVersion(_) => "publish-version",
            Step::OpenLayout { .. } => "open-layout",
            Step::OpenSettings => "open-settings",
            Step::OpenInbox => "open-inbox",
            Step::Notify { .. } => "notify",
            Step::ToggleChannel(_) => "toggle-channel",
            Step::Construct(_) => "construct",
            Step::ExpectWrite { .. } => "expect-write",
        }
    }

    /// Steps only the fixture pod can run.
    pub fn fixture_only(&self) -> bool {
        matches!(
            self,
            Step::Offline
                | Step::Online
                | Step::NotSaving(_)
                | Step::PublishVersion(_)
                | Step::Notify { .. }
                | Step::Construct(_)
        )
    }
}

/// A step and its source text.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub step: Step,
    pub command: String,
    pub arg: String,
}

const COMMANDS: &str = "shot, click, type, pick, swipe-left, swipe-right, swipe-down, swipe-up, drag-down, \
    drag-card, scroll-to, back, wait, idle, assert-text, assert-no-text, wait-text, assert-link, wait-images, screen, \
    offline, online, publish-version, open-layout, open-settings, open-inbox, notify, toggle-channel, expect-write";

/// Parses a script; the error names the entry and what it needs.
pub fn parse(script: &str) -> Result<Vec<Line>, String> {
    let mut out = Vec::new();
    for (index, raw) in script.split([';', '\n']).enumerate() {
        let entry = raw.trim();
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        let raw_command = entry.split([' ', '\t']).next().unwrap_or(entry);
        let command = raw_command.to_lowercase();
        let arg = entry[raw_command.len()..].trim().to_owned();
        let at = || format!("command #{} '{entry}'", index + 1);
        let need = |what: &str| -> Result<String, String> {
            if arg.is_empty() {
                Err(format!("{}: '{command}' needs {what}", at()))
            } else {
                Ok(arg.clone())
            }
        };
        let none = |step: Step| -> Result<Step, String> {
            if arg.is_empty() {
                Ok(step)
            } else {
                Err(format!("{}: '{command}' takes no argument", at()))
            }
        };
        let ms = |s: &str| s.parse::<u64>().ok().filter(|m| *m <= MAX_WAIT_MS);
        let target = "a target (text, content description or tag:<id>)";
        let step = match command.as_str() {
            "shot" => Step::Shot(need("a name")?),
            "click" => Step::Click(need("the text to click")?),
            "press" => Step::Press(need(target)?),
            "hover" => Step::Hover(need(target)?),
            "type" => {
                let a = need("a target and the text to type")?;
                let (target, text) = split_type_arg(&a).ok_or_else(|| {
                    format!(
                        "{}: 'type' needs '<target> <text>' (target: tag:<id>, one word, or \"quoted text\")",
                        at()
                    )
                })?;
                Step::Type { target, text }
            }
            "pick" => Step::Pick(split_paths(&need("one or more file paths")?).ok_or_else(
                || {
                    format!(
                        "{}: 'pick' needs file paths (quote a path with spaces)",
                        at()
                    )
                },
            )?),
            "swipe-left" => Step::Swipe(Direction::Left, need(target)?),
            "swipe-right" => Step::Swipe(Direction::Right, need(target)?),
            "swipe-up" => Step::Swipe(Direction::Up, need(target)?),
            "swipe-down" => Step::Swipe(Direction::Down, need(target)?),
            "drag-down" => {
                let a = need("a distance and a target")?;
                let (n, t) = a.split_once(' ').unwrap_or((&a, ""));
                let points = n.parse::<u32>().ok().filter(|p| (1..=MAX_DRAG).contains(p));
                match points {
                    Some(points) if !t.trim().is_empty() => Step::DragDown {
                        points,
                        target: t.trim().to_owned(),
                    },
                    _ => {
                        return Err(format!(
                            "{}: 'drag-down' needs '<points in 1..{MAX_DRAG}> <target>'",
                            at()
                        ));
                    }
                }
            }
            "drag-card" => {
                let a = need("a distance and a target")?;
                let (n, t) = a.split_once(' ').unwrap_or((&a, ""));
                let max = MAX_DRAG as i32;
                let points = n.parse::<i32>().ok().filter(|p| (-max..=max).contains(p));
                match points {
                    Some(points) if !t.trim().is_empty() => Step::DragCard {
                        points,
                        target: t.trim().to_owned(),
                    },
                    _ => {
                        return Err(format!(
                            "{}: 'drag-card' needs '<points in -{MAX_DRAG}..{MAX_DRAG}> <target>'",
                            at()
                        ));
                    }
                }
            }
            "scroll-to" => Step::ScrollTo(need(target)?),
            "tap-photo" => {
                let a = need("left or right and a target")?;
                let (side, t) = a.split_once(' ').unwrap_or((&a, ""));
                match side {
                    "left" | "right" if !t.trim().is_empty() => Step::TapPhoto {
                        right: side == "right",
                        target: t.trim().to_owned(),
                    },
                    _ => return Err(format!("{}: 'tap-photo' needs 'left|right <target>'", at())),
                }
            }
            c @ ("scatter-box" | "scatter-select" | "scatter-zoom" | "scatter-tap") => {
                let n = match c {
                    "scatter-zoom" => 1,
                    "scatter-tap" => 2,
                    _ => 4,
                };
                let a = need("numbers and a target")?;
                let mut parts = a.splitn(n + 1, ' ');
                let nums: Vec<f32> = parts
                    .by_ref()
                    .take(n)
                    .filter_map(|v| v.parse().ok())
                    .collect();
                let t = parts.next().unwrap_or("").trim().to_owned();
                let unit = |v: &f32| (0.0..=1.0).contains(v);
                let ok = nums.len() == n
                    && !t.is_empty()
                    && if c == "scatter-zoom" {
                        (0.05..=50.0).contains(&nums[0])
                    } else {
                        nums.iter().all(unit)
                    };
                if !ok {
                    return Err(format!(
                        "{}: '{c}' needs {} and a target",
                        at(),
                        match c {
                            "scatter-zoom" => "a factor (0.05..50)",
                            "scatter-tap" => "'<x> <y>' (0..1)",
                            _ => "'<x0> <y0> <x1> <y1>' (0..1)",
                        }
                    ));
                }
                match c {
                    "scatter-zoom" => Step::ScatterZoom {
                        factor: nums[0],
                        target: t,
                    },
                    "scatter-tap" => Step::ScatterTap {
                        at: [nums[0], nums[1]],
                        target: t,
                    },
                    _ => Step::ScatterBox {
                        bounds: [nums[0], nums[1], nums[2], nums[3]],
                        target: t,
                        release: c == "scatter-select",
                    },
                }
            }
            "back" => none(Step::Back)?,
            "release" => none(Step::Release)?,
            "wait" => Step::Wait(ms(&need("a duration in ms")?).ok_or_else(|| {
                format!(
                    "{}: 'wait' needs a whole number of ms in 0..{MAX_WAIT_MS}",
                    at()
                )
            })?),
            "idle" => none(Step::Idle)?,
            "assert-text" => Step::AssertText(need("the expected text")?),
            "assert-no-text" => Step::AssertNoText(need("the text that must be absent")?),
            "wait-text" => {
                let a = need("a timeout in ms and the text")?;
                let (n, t) = a.split_once(' ').unwrap_or((&a, ""));
                match ms(n) {
                    Some(ms) if !t.trim().is_empty() => Step::WaitText {
                        ms,
                        text: t.trim().to_owned(),
                    },
                    _ => {
                        return Err(format!(
                            "{}: 'wait-text' needs '<ms in 0..{MAX_WAIT_MS}> <text>'",
                            at()
                        ));
                    }
                }
            }
            "assert-link" => Step::AssertLink(need("text the opened link contains")?),
            "wait-images" => Step::WaitImages(if arg.is_empty() {
                DEFAULT_WAIT_IMAGES_MS
            } else {
                ms(&arg).ok_or_else(|| {
                    format!(
                        "{}: 'wait-images' takes an optional timeout in ms in 0..{MAX_WAIT_MS}",
                        at()
                    )
                })?
            }),
            "screen" => Step::Screen(
                Screen::parse(&need("WxH or a preset (phone, tablet, desktop)")?)
                    .map_err(|e| format!("{}: {e}", at()))?,
            ),
            "mode" => match need("dark or light")?.to_lowercase().as_str() {
                "dark" => Step::Dark(true),
                "light" => Step::Dark(false),
                _ => return Err(format!("{}: 'mode' takes dark or light", at())),
            },
            "insets" => {
                let a = need("'<top> <right> <bottom> <left>' (points)")?;
                let v: Vec<f32> = a
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .map_err(|_| format!("{}: 'insets' takes four numbers", at()))?;
                match v.as_slice() {
                    &[t, r, b, l] if v.iter().all(|x| *x >= 0.0) => Step::Insets([t, r, b, l]),
                    _ => return Err(format!("{}: 'insets' takes four numbers, 0 or more", at())),
                }
            }
            "clock" => Step::Clock(
                crate::dates::wall_time(&need("a time YYYY-MM-DDTHH:MM")?)
                    .ok_or_else(|| format!("{}: 'clock' takes a time YYYY-MM-DDTHH:MM", at()))?,
            ),
            "offline" => none(Step::Offline)?,
            "online" => none(Step::Online)?,
            "not-saving" => Step::NotSaving((!arg.is_empty()).then(|| arg.clone())),
            "publish-version" => Step::PublishVersion(need("a note (the changelog line)")?),
            "open-layout" => {
                let a = need("a layout id and push, sheet or panel")?;
                let parts: Vec<&str> = a.split_whitespace().collect();
                let how = match parts.get(1).map(|h| h.to_lowercase()).as_deref() {
                    None | Some("push") => Some(Presentation::Push),
                    Some("sheet") => Some(Presentation::Sheet),
                    Some("panel") => Some(Presentation::Panel),
                    Some(_) => None,
                };
                let Some(how) = how.filter(|_| parts.len() <= 2) else {
                    return Err(format!(
                        "{}: 'open-layout' needs '<layout id> [push|sheet|panel]'",
                        at()
                    ));
                };
                Step::OpenLayout {
                    layout: parts[0].to_owned(),
                    how,
                }
            }
            "expect-write" => {
                let a = need("'<collection> {field: value, ...}'")?;
                let (c, f) = a.split_once(char::is_whitespace).unwrap_or((&a, "{}"));
                let fields = serde_json::from_str::<serde_json::Value>(f.trim())
                    .ok()
                    .and_then(|v| v.as_object().cloned());
                match fields {
                    Some(fields) if !c.is_empty() => Step::ExpectWrite {
                        collection: c.to_owned(),
                        fields,
                    },
                    _ => {
                        return Err(format!(
                            "{}: 'expect-write' needs '<collection> {{\"field\": value, ...}}' (JSON)",
                            at()
                        ));
                    }
                }
            }
            "open-settings" => none(Step::OpenSettings)?,
            "open-inbox" => none(Step::OpenInbox)?,
            "construct" => Step::Construct(need("a construct file (crates/construct-sim/data/…)")?),
            "notify" => {
                let a = need("'<layout> <channel> <title>'")?;
                let mut it = a.splitn(3, char::is_whitespace);
                match (it.next(), it.next(), it.next().map(str::trim)) {
                    (Some(app), Some(channel), Some(title)) if !title.is_empty() => Step::Notify {
                        app: app.to_owned(),
                        channel: channel.to_owned(),
                        title: title.to_owned(),
                    },
                    _ => {
                        return Err(format!(
                            "{}: 'notify' needs '<layout> <channel> <title>'",
                            at()
                        ));
                    }
                }
            }
            "toggle-channel" => {
                let a = need("'<layout>.<channel>' (e.g. pixelfed.likes)")?;
                let ok = a.contains('.')
                    && !a.starts_with('.')
                    && !a.ends_with('.')
                    && !a.contains(char::is_whitespace);
                if !ok {
                    return Err(format!(
                        "{}: 'toggle-channel' needs '<layout>.<channel>' (e.g. pixelfed.likes)",
                        at()
                    ));
                }
                Step::ToggleChannel(a)
            }
            _ => {
                return Err(format!(
                    "{}: unknown command '{command}' (expected one of: {COMMANDS})",
                    at()
                ));
            }
        };
        out.push(Line { step, command, arg });
    }
    Ok(out)
}

/// A `type` argument: a target in double quotes may hold spaces
/// (`"Search posts" cats`), else it is the first word.
pub fn split_type_arg(arg: &str) -> Option<(String, String)> {
    let a = arg.trim();
    let (target, rest) = if let Some(q) = a.strip_prefix('"') {
        let end = q.find('"')?;
        (&q[..end], &q[end + 1..])
    } else {
        let end = a.find(char::is_whitespace)?;
        (&a[..end], &a[end..])
    };
    let text = rest.trim();
    (!target.is_empty() && !text.is_empty()).then(|| (target.to_owned(), text.to_owned()))
}

/// A `pick` argument: words, or double-quoted paths with spaces.
pub fn split_paths(arg: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = arg.trim();
    while !rest.is_empty() {
        if let Some(q) = rest.strip_prefix('"') {
            let end = q.find('"')?;
            if end > 0 {
                out.push(q[..end].to_owned());
            }
            rest = q[end + 1..].trim_start();
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            out.push(rest[..end].to_owned());
            rest = rest[end..].trim_start();
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The semantic ids the Compose simulator's test tags stood for (scripts
/// written for it still run).
fn tag_alias(tag: &str) -> String {
    if let Some(f) = tag.strip_prefix("textfield-send:") {
        format!("send:{f}")
    } else if let Some(f) = tag.strip_prefix("textfield:") {
        format!("field:{f}")
    } else {
        tag.to_owned()
    }
}

/// The node a target names: `tag:<id>` (exact id, else the first id
/// containing it), else text: exact (tappable nodes first), then contained.
/// Quotes around a target are dropped. `<target> in tag:<id>` looks only
/// inside that node's bounds (a stable target in a list that changes daily).
pub fn find(scene: &Scene, target: &str) -> Option<SemanticNode> {
    if let Some((inner, parent)) = target.rsplit_once(" in tag:") {
        let p = find(scene, &format!("tag:{parent}"))?;
        let [px, py, pw, ph] = p.bounds;
        let inside = Scene {
            semantics: scene
                .semantics
                .iter()
                .filter(|n| {
                    let [x, y, w, h] = n.bounds;
                    x >= px - 0.5
                        && y >= py - 0.5
                        && x + w <= px + pw + 0.5
                        && y + h <= py + ph + 0.5
                })
                .cloned()
                .collect(),
            ..scene.clone()
        };
        return find(&inside, inner.trim());
    }
    if let Some(tag) = target.strip_prefix("tag:") {
        let id = tag_alias(tag);
        return scene
            .semantics
            .iter()
            .find(|n| n.id == id)
            .or_else(|| scene.semantics.iter().find(|n| n.id.contains(&id)))
            .cloned();
    }
    let text = target
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(target);
    let has = |n: &&SemanticNode, exact: bool| {
        n.text_content
            .as_deref()
            .is_some_and(|t| if exact { t == text } else { t.contains(text) })
    };
    let all = &scene.semantics;
    all.iter()
        .find(|n| has(n, true) && (n.interactive || n.tap.is_some()))
        .or_else(|| all.iter().find(|n| has(n, true)))
        .or_else(|| all.iter().find(|n| has(n, false)))
        .cloned()
}

/// The texts on screen, for a failure's message.
pub fn texts(scene: &Scene) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for n in &scene.semantics {
        if let Some(t) = n.text_content.as_deref()
            && !t.is_empty()
            && !seen.contains(&t)
        {
            seen.push(t);
        }
    }
    let shown: Vec<String> = seen.iter().take(40).map(|t| format!("\"{t}\"")).collect();
    format!("[{}]", shown.join(", "))
}
