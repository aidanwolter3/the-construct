//! Runs a script against a session of the Rust engine and writes PNGs,
//! `steps.log` and `steps.json` (docs/simulator.md).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use construct_render::interact::{DragAxis, Effect, Outcome, Session, Target};
use construct_render::remote::PodClient;
use construct_render::scene::{Draw, Scene, SemanticNode, Tap};
use serde_json::{Value, json};

use crate::fixture::{FixturePod, default_bundle, read_bundle};
use crate::screen::Screen;
use crate::script::{Direction, Line, Presentation, Step};

/// How long `assert-text` and `assert-no-text` retry.
const ASSERT_RETRY: Duration = Duration::from_secs(2);
/// How long a `shot` waits for loading images first.
const SHOT_IMAGE_WAIT: Duration = Duration::from_secs(5);
/// How long an `open-layout` and the like wait for the layout's data.
const LAYOUT_TIMEOUT: Duration = Duration::from_secs(20);
/// A flick's speed (points per second): well past every fling threshold.
const FLICK: f32 = 3000.0;
/// How often a waiting step looks again.
const TICK: Duration = Duration::from_millis(20);

/// Where the session's layouts and data come from.
pub enum Source {
    /// The fake pod over `data/`; `bundle` is the start layout's bundle
    /// (else the workspace's default one).
    Fixture {
        pod: FixturePod,
        bundle: Option<PathBuf>,
    },
    /// A live pod (`token`: a session token or pod code; empty: the pod's
    /// sign-in screen).
    Live { base: String, token: String },
}

pub struct Options {
    pub workspace: String,
    pub screen: Screen,
    pub density: Option<f32>,
    pub out: PathBuf,
    /// Live mode: how long the first load may take.
    pub timeout: Duration,
}

/// One step's result, as `steps.json` records it.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub index: usize,
    pub command: String,
    pub arg: String,
    pub ok: bool,
    pub message: String,
    pub file: Option<String>,
    pub elapsed_ms: u128,
}

impl StepResult {
    pub fn log_line(&self) -> String {
        let cmd = if self.arg.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.arg)
        };
        let mut tail = String::new();
        if let Some(f) = &self.file {
            tail.push_str(&format!(" -> {f}"));
        }
        if !self.message.is_empty() {
            tail.push_str(&format!(" : {}", self.message));
        }
        format!(
            "[{:02}] {:<4} {cmd}{tail} ({} ms)",
            self.index,
            if self.ok { "ok" } else { "fail" },
            self.elapsed_ms
        )
    }
}

pub struct Runner {
    pub session: Session,
    source: Source,
    opts: Options,
    /// Links `action.link.open` opened (`assert-link`).
    pub links: Vec<String>,
    /// Files `pick` queued for the next media picker tap.
    picks: Vec<std::path::PathBuf>,
}

/// A file name part from a shot's name.
pub fn sanitize(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "shot".into()
    } else {
        out.into()
    }
}

impl Runner {
    pub fn new(source: Source, opts: Options) -> Result<Runner, String> {
        let session = open(&source, &opts)?;
        let mut r = Runner {
            session,
            source,
            opts,
            links: Vec::new(),
            picks: Vec::new(),
        };
        if matches!(r.source, Source::Live { .. }) {
            let timeout = r.opts.timeout;
            r.session.settle(timeout);
        }
        Ok(r)
    }

    /// Runs every step, stopping at the first that fails, and writes the
    /// logs. Ok when every step passed.
    pub fn run(&mut self, lines: &[Line]) -> Vec<StepResult> {
        std::fs::create_dir_all(&self.opts.out).ok();
        let mut results = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let index = i + 1;
            let started = Instant::now();
            let outcome = self.step(index, &line.step);
            let (ok, message, file) = match outcome {
                Ok((message, file)) => (true, message, file),
                Err(message) => {
                    let file = self.save(&format!("{index:02}-FAILED.png")).ok();
                    (false, message, file)
                }
            };
            let r = StepResult {
                index,
                command: line.command.clone(),
                arg: line.arg.clone(),
                ok,
                message,
                file,
                elapsed_ms: started.elapsed().as_millis(),
            };
            println!("{}", r.log_line());
            results.push(r);
            if !ok {
                break;
            }
        }
        self.write_logs(&results);
        results
    }

    fn write_logs(&self, results: &[StepResult]) {
        let ok = results.iter().all(|r| r.ok);
        let mut log: String = results.iter().map(|r| r.log_line() + "\n").collect();
        log.push_str(&if ok {
            format!("RESULT: ok ({} steps)\n", results.len())
        } else {
            "RESULT: FAILED\n".into()
        });
        let _ = std::fs::write(self.opts.out.join("steps.log"), log);
        let steps: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({ "index": r.index, "command": r.command, "arg": r.arg,
                        "status": if r.ok { "ok" } else { "fail" }, "message": r.message,
                        "file": r.file, "elapsed_ms": r.elapsed_ms })
            })
            .collect();
        let text = serde_json::to_string_pretty(&steps).unwrap_or_default() + "\n";
        let _ = std::fs::write(self.opts.out.join("steps.json"), text);
    }

    /// Draws the frame to `<out>/<name>`.
    fn save(&mut self, name: &str) -> Result<String, String> {
        let img = self.session.render_cpu().map_err(|e| e.to_string())?;
        std::fs::write(
            self.opts.out.join(name),
            construct_render::encode::png(&img),
        )
        .map_err(|e| format!("{name}: {e}"))?;
        Ok(name.to_owned())
    }

    fn scene(&mut self) -> Result<Scene, String> {
        self.session.poll();
        self.session.scene().cloned().map_err(|e| e.to_string())
    }

    /// Runs one step: a message for the log and the file it wrote.
    fn step(&mut self, index: usize, step: &Step) -> Result<(String, Option<String>), String> {
        self.session.poll();
        let fixture_pod = match &self.source {
            Source::Fixture { pod, .. } => Some(pod.clone()),
            Source::Live { .. } => None,
        };
        if step.fixture_only() && fixture_pod.is_none() {
            return Err(format!("'{}' runs in fixture mode only", step.command()));
        }
        let done = |m: &str| Ok((m.to_owned(), None));
        match step {
            Step::Shot(name) => {
                self.wait_images(SHOT_IMAGE_WAIT);
                self.session.settle(Duration::from_secs(10));
                let file = self.save(&format!("{index:02}-{}.png", sanitize(name)))?;
                Ok((String::new(), Some(file)))
            }
            Step::Click(target) => {
                let node = self.reveal(target)?;
                let (x, y) = center(&node);
                let o = self.session.tap(&Target::Point(x, y))?;
                self.effects(o)
            }
            Step::Type { target, text } => {
                let field = self.field(target)?;
                let o = self.session.type_text(&field, text);
                self.effects(o)
            }
            Step::Pick(paths) => {
                // Relative paths resolve against the repository root.
                let cwd = std::env::current_dir().unwrap_or_default();
                let root = crate::fixture::repo_root(&cwd).unwrap_or(cwd);
                self.picks = paths.iter().map(|p| root.join(p)).collect();
                done(&format!("{} queued for the next picker", self.picks.len()))
            }
            Step::Swipe(dir, target) => self.swipe(*dir, target),
            Step::DragDown { points, target } => {
                let (x, y) = self.drag_point(target)?;
                if self.session.frame_drag_at(x, y).is_some() {
                    self.session.drag_frame(0.0, *points as f32);
                    let closed = self.session.release_frame(0.0, 0.0);
                    done(if closed { "closed" } else { "settled back" })
                } else {
                    let o = self.session.scroll_by(Some(&Target::Point(x, y)), -(*points as f32))?;
                    self.effects(o)
                }
            }
            Step::ScrollTo(target) => {
                self.reveal(target)?;
                done("")
            }
            Step::Back => {
                let o = self
                    .session
                    .dispatch(&json!({ "intent": "action.navigation.back" }))?;
                self.effects(o)
            }
            Step::Wait(ms) => {
                self.pump(Duration::from_millis(*ms));
                done("")
            }
            Step::Idle => {
                self.session.settle(Duration::from_secs(10));
                if self.session.busy() {
                    Err("still loading after 10 s".into())
                } else {
                    done("")
                }
            }
            Step::AssertText(text) => self.wait_text(text, ASSERT_RETRY, true),
            Step::AssertNoText(text) => self.wait_text(text, ASSERT_RETRY, false),
            Step::WaitText { ms, text } => {
                self.wait_text(text, Duration::from_millis(*ms), true)
            }
            Step::AssertLink(text) => {
                if self.links.iter().any(|l| l.contains(text.as_str())) {
                    done("")
                } else {
                    Err(format!("no opened link contains '{text}' (opened: {:?})", self.links))
                }
            }
            Step::WaitImages(ms) => {
                if self.wait_images(Duration::from_millis(*ms)) {
                    done("")
                } else {
                    Err("images still loading".into())
                }
            }
            Step::Screen(screen) => {
                self.opts.screen = screen.clone();
                let vp = screen.viewport(self.opts.density);
                self.session.resize(vp);
                done("")
            }
            Step::Dark(dark) => {
                self.session.set_dark(*dark);
                done("")
            }
            Step::Offline | Step::Online => {
                let pod = fixture_pod.expect("fixture mode");
                pod.set_online(matches!(step, Step::Online));
                self.device_data(&pod);
                done("")
            }
            Step::PublishVersion(_) => Err(
                "'publish-version' isn't supported yet: the Rust engine always shows a layout's latest version"
                    .into(),
            ),
            Step::OpenLayout { layout, how } => self.open_layout(layout, *how),
            Step::OpenSettings => self.open_layout("settings", Presentation::Panel),
            Step::OpenInbox => self.open_layout("inbox", Presentation::Sheet),
            Step::PinVersion { layout, version } => {
                let intent = match version {
                    Some(v) => json!({ "intent": "action.device", "command": "pin_version",
                                       "app": layout, "id": v.to_string() }),
                    None => json!({ "intent": "action.device", "command": "follow_latest", "app": layout }),
                };
                let o = self.session.dispatch(&intent)?;
                self.effects(o)
            }
            Step::Notify {
                app,
                channel,
                title,
            } => {
                let pod = fixture_pod.expect("fixture mode");
                pod.notify(app, channel, title);
                self.device_data(&pod);
                done("")
            }
            Step::ToggleChannel(channel) => {
                let (app, id) = channel.split_once('.').unwrap_or((channel, ""));
                let o = self.session.dispatch(&json!({ "intent": "action.device",
                    "command": "toggle_channel", "app": app, "id": id }))?;
                self.effects(o)
            }
        }
    }

    /// The fixture pod's device data into the session (connection, inbox).
    fn device_data(&mut self, pod: &FixturePod) {
        let d = pod.device();
        self.session.data.entities.extend(d.entities);
        self.session.data.queries.extend(d.queries);
        self.session.invalidate();
    }

    /// Keeps drawing for `d`, as a window does while data or images arrive.
    fn pump(&mut self, d: Duration) {
        let until = Instant::now() + d;
        while Instant::now() < until {
            let _ = self.scene();
            std::thread::sleep(TICK.min(until.saturating_duration_since(Instant::now())));
        }
    }

    /// Waits until no image on screen is loading; false at the timeout.
    fn wait_images(&mut self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        loop {
            let Ok(scene) = self.scene() else {
                return true;
            };
            let loading = scene.draws.iter().any(|d| match d {
                Draw::Image(i) => matches!(self.session.images.status(&i.url), "loading" | "none"),
                _ => false,
            });
            if !loading {
                // The frame picks up what arrived.
                self.session.invalidate();
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(TICK);
        }
    }

    fn wait_text(
        &mut self,
        text: &str,
        timeout: Duration,
        present: bool,
    ) -> Result<(String, Option<String>), String> {
        let until = Instant::now() + timeout;
        loop {
            let scene = self.scene()?;
            let found = scene
                .semantics
                .iter()
                .any(|n| n.text_content.as_deref().is_some_and(|t| t.contains(text)));
            if found == present {
                return Ok((String::new(), None));
            }
            if Instant::now() >= until {
                return Err(if present {
                    format!("no text '{text}' on screen; texts: {}", texts(&scene))
                } else {
                    format!("text '{text}' is still on screen")
                });
            }
            std::thread::sleep(TICK);
        }
    }

    /// Finds `target` and scrolls it into view.
    fn reveal(&mut self, target: &str) -> Result<SemanticNode, String> {
        let viewport = self.session.viewport;
        for _ in 0..40 {
            let node = match find(&self.scene()?, target) {
                Some(n) => n,
                None => self.search(target)?,
            };
            let scene = self.scene()?;
            let (_, y) = center(&node);
            // Inside its scrolling view (else the screen).
            let view = scroll_ancestor(&scene, &node);
            let [_, vy, _, vh] = view.as_ref().map(|v| v.bounds).unwrap_or([
                0.0,
                0.0,
                viewport.width,
                viewport.height,
            ]);
            let (top, bottom) = (vy.max(0.0), (vy + vh).min(viewport.height));
            if y >= top && y < bottom {
                return Ok(node);
            }
            let Some(view) = view else {
                return Err(format!(
                    "'{target}' is off screen and not in a scrolling view"
                ));
            };
            let dy = y - (top + bottom) / 2.0;
            let o = self
                .session
                .scroll_by(Some(&Target::Id(view.id.clone())), dy)?;
            if o.effects == vec![Effect::Nothing] {
                return Err(format!("'{target}' is off screen and can't be scrolled to"));
            }
        }
        Err(format!("couldn't scroll '{target}' into view"))
    }

    /// Scrolls the screen's largest scrolling view through its content,
    /// down then back from the top, until `target` is drawn (the engine
    /// leaves what is off screen out of the semantic tree).
    fn search(&mut self, target: &str) -> Result<SemanticNode, String> {
        let first = self.scene()?;
        let missing =
            |scene: &Scene| format!("nothing matches '{target}'; texts: {}", texts(scene));
        let Some(view) = first
            .semantics
            .iter()
            .filter(|n| n.node_type == "core://Scroll")
            .max_by(|a, b| (a.bounds[2] * a.bounds[3]).total_cmp(&(b.bounds[2] * b.bounds[3])))
            .cloned()
        else {
            return Err(missing(&first));
        };
        let step = (view.bounds[3] * 0.6).max(40.0);
        let by = Target::Id(view.id.clone());
        for dir in [1.0, -1.0] {
            for _ in 0..100 {
                let o = self.session.scroll_by(Some(&by), dir * step)?;
                if let Some(n) = find(&self.scene()?, target) {
                    return Ok(n);
                }
                if o.effects == vec![Effect::Nothing] {
                    break;
                }
            }
        }
        Err(missing(&first))
    }

    /// The text field `target` names: `field:<f>`, `tag:textfield:<f>`, a
    /// field's placeholder or label, or a bare field name.
    fn field(&mut self, target: &str) -> Result<String, String> {
        let scene = self.scene()?;
        if let Some(node) = find(&scene, target) {
            if let Some(f) = node.id.strip_prefix("field:") {
                return Ok(f.to_owned());
            }
            if let Some(Tap::Focus { field }) = &node.tap {
                return Ok(field.clone());
            }
        }
        if scene.fields.iter().any(|f| f.field == target) {
            return Ok(target.to_owned());
        }
        Err(format!(
            "no text field matches '{target}'; fields: {:?}",
            scene.fields.iter().map(|f| &f.field).collect::<Vec<_>>()
        ))
    }

    /// Where a drag on `target` starts: a sheet's handle for
    /// `tag:sheet-handle`, else the node's centre.
    fn drag_point(&mut self, target: &str) -> Result<(f32, f32), String> {
        if target == "tag:sheet-handle" {
            let vp = self.session.viewport;
            let x = vp.width / 2.0;
            let mut y = 0.0;
            while y < vp.height {
                if self.session.frame_drag_at(x, y) == Some(DragAxis::Vertical) {
                    return Ok((x, y + 4.0));
                }
                y += 4.0;
            }
            return Err("no sheet is open".into());
        }
        Ok(center(&self.reveal(target)?))
    }

    fn swipe(&mut self, dir: Direction, target: &str) -> Result<(String, Option<String>), String> {
        let (x, y) = self.drag_point(target)?;
        match dir {
            Direction::Left | Direction::Right => {
                if let Some(id) = self.session.pager_at(x, y) {
                    let at = self.session.pager_offset(&id);
                    let (dx, v) = if dir == Direction::Left {
                        (40.0, -FLICK)
                    } else {
                        (-40.0, FLICK)
                    };
                    self.session.drag_pager(&id, at + dx);
                    self.session.release_pager(&id, v);
                    return Ok((String::new(), None));
                }
            }
            Direction::Up | Direction::Down => {
                if self.session.frame_drag_at(x, y).is_some() {
                    let (dy, v) = if dir == Direction::Down {
                        (40.0, FLICK)
                    } else {
                        (-40.0, -FLICK)
                    };
                    self.session.drag_frame(0.0, dy);
                    let closed = self.session.release_frame(0.0, v);
                    return Ok((if closed { "closed" } else { "settled back" }.into(), None));
                }
            }
        }
        let kind = match dir {
            Direction::Left => "swipe_left",
            Direction::Right => "swipe_right",
            Direction::Up => "swipe_up",
            Direction::Down => "swipe_down",
        };
        let o = self.session.gesture(x, y, kind)?;
        if o.node.is_none() {
            return Err(format!("nothing at '{target}' takes a {kind}"));
        }
        self.effects(o)
    }

    fn open_layout(
        &mut self,
        layout: &str,
        how: Presentation,
    ) -> Result<(String, Option<String>), String> {
        let o = self
            .session
            .dispatch(&json!({ "intent": "action.navigation.open",
            "layout": layout, "presentation": how.wire() }))?;
        let opened = o
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Opened { layout: l } if l == layout));
        let r = self.effects(o)?;
        if !opened {
            return Err(format!("'{layout}' didn't open: {}", r.0));
        }
        self.session.settle(LAYOUT_TIMEOUT);
        Ok(r)
    }

    /// Acts on what an interaction did as the platform would; a load that
    /// failed or an intent nothing ran fails the step.
    fn effects(&mut self, o: Outcome) -> Result<(String, Option<String>), String> {
        let mut notes = Vec::new();
        for e in &o.effects {
            match e {
                Effect::OpenLink { url } => self.links.push(url.clone()),
                Effect::LoadFailed { what, error } => {
                    return Err(format!("{what} failed: {error}"));
                }
                Effect::NotSent { intent } => {
                    return Err(format!("nothing in this mode runs {intent}"));
                }
                Effect::SignedIn { token, .. } => {
                    if let Source::Live { token: t, .. } = &mut self.source {
                        *t = token.clone();
                    }
                    self.reopen()?;
                    notes.push("signed in".to_owned());
                    continue;
                }
                Effect::PickMedia { max, .. } => {
                    // The queued files, at most `max`; none is a cancelled pick.
                    let mut files = std::mem::take(&mut self.picks);
                    files.truncate(*max as usize);
                    let n = files.len();
                    self.session.picked(files.into_iter().map(Ok).collect());
                    if n > 0 && self.session.staging == 0 {
                        return Err("nothing in this mode stages photos".into());
                    }
                    notes.push(format!("picked {n}"));
                    continue;
                }
                Effect::LeftPod => {
                    if let Source::Live { token: t, .. } = &mut self.source {
                        t.clear();
                    }
                    self.reopen()?;
                    notes.push("left the pod".to_owned());
                    continue;
                }
                _ => {}
            }
            if !matches!(e, Effect::Nothing) {
                notes.push(serde_json::to_string(e).unwrap_or_default());
            }
        }
        self.session.settle(Duration::from_secs(10));
        Ok((notes.join(" "), None))
    }

    /// Opens the start layout again (after signing in or out).
    fn reopen(&mut self) -> Result<(), String> {
        self.session = open(&self.source, &self.opts)?;
        self.session.settle(self.opts.timeout);
        Ok(())
    }
}

/// A session on the start layout.
fn open(source: &Source, opts: &Options) -> Result<Session, String> {
    let vp = opts.screen.viewport(opts.density);
    match source {
        Source::Fixture { pod, bundle } => {
            use construct_render::interact::Loader;
            let (root, mut data) = match bundle {
                Some(path) => read_bundle(path)?,
                None => pod.layout(&opts.workspace).map_err(|e| {
                    if default_bundle(&opts.workspace).is_none() {
                        format!(
                            "no default fixture for workspace '{}'; pass --fixture <bundle.json> or use live mode",
                            opts.workspace
                        )
                    } else {
                        e
                    }
                })?,
            };
            let device = pod.device();
            for (k, v) in device.entities {
                data.entities.entry(k).or_insert(v);
            }
            for (k, v) in device.queries {
                data.queries.entry(k).or_insert(v);
            }
            let mut s = Session::new(&opts.workspace, root, data, vp);
            s.loader = Some(Box::new(pod.clone()));
            pod.serve_images(&s.images);
            Ok(s)
        }
        Source::Live { base, token } => {
            PodClient::connect(base, token).and_then(|c| c.session(&opts.workspace, vp))
        }
    }
}

fn center(n: &SemanticNode) -> (f32, f32) {
    (
        n.bounds[0] + n.bounds[2] / 2.0,
        n.bounds[1] + n.bounds[3] / 2.0,
    )
}

fn scroll_ancestor(scene: &Scene, node: &SemanticNode) -> Option<SemanticNode> {
    let index = scene.semantics.iter().position(|n| n.id == node.id)?;
    let mut at = scene.semantics[index].parent;
    while let Some(i) = at {
        let n = &scene.semantics[i];
        if n.node_type == "core://Scroll" {
            return Some(n.clone());
        }
        at = n.parent;
    }
    None
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
/// Quotes around a target are dropped.
pub fn find(scene: &Scene, target: &str) -> Option<SemanticNode> {
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
fn texts(scene: &Scene) -> String {
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
