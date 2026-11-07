//! A native window drawing a [`Controller`]'s session with wgpu: Metal on
//! macOS, Vulkan (or GL) on Linux and Android, presented straight from the
//! surface texture. Input goes through [`crate::viewer`].

use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::StartCause;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::gpu::{Gpu, Renderer};
use crate::interact::CursorMove;
use crate::interact::DragAxis;
pub use crate::platform::{
    Platform, PlatformEvent, Reopen, clipboard_here, open_link_here, set_clipboard_here,
};
use crate::viewer::{Controller, EditKey, Event};

/// Writes a line to the platform log (logcat on Android, stderr elsewhere).
pub fn log(line: &str) {
    #[cfg(target_os = "android")]
    crate::android::log(line);
    #[cfg(not(target_os = "android"))]
    eprintln!("{line}");
}

struct Surface {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    renderer: Renderer,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// A finger on the screen.
struct Drag {
    start: (f32, f32),
    last: (f32, f32),
    at: Instant,
    /// Recent positions (time, x, y), for the release velocity.
    samples: std::collections::VecDeque<(Instant, f32, f32)>,
    /// Moved far enough to be a drag, not a tap.
    dragging: bool,
    /// Something scrolled during it (so it is not a swipe).
    scrolled: bool,
    /// The sheet or panel it could move (started on a sheet's handle, or
    /// on a panel), and whether it does.
    frame: Option<DragAxis>,
    moving_frame: bool,
    /// The carousel under its start and where its track was, and whether
    /// the drag pages it.
    pager: Option<(String, f32)>,
    paging: bool,
    /// Scrolling not applied yet: applied once per frame, in `draw`.
    pending: f32,
    /// When the finger went down.
    started: Instant,
    /// It is moving an edit handle.
    grabbing: bool,
    /// It was held long enough to be a long press (which already ran).
    long_pressed: bool,
}

impl Drag {
    /// The release velocity over the last 100 ms, points per second (x, y),
    /// estimated as Compose's `VelocityTracker` does by default.
    fn velocity(&self) -> (f32, f32) {
        let Some(&(t0, _, _)) = self.samples.front() else {
            return (0.0, 0.0);
        };
        let t: Vec<f32> = self
            .samples
            .iter()
            .map(|s| s.0.duration_since(t0).as_secs_f32())
            .collect();
        let x: Vec<f32> = self.samples.iter().map(|s| s.1).collect();
        let y: Vec<f32> = self.samples.iter().map(|s| s.2).collect();
        (impulse_velocity(&t, &x), impulse_velocity(&t, &y))
    }
}

/// Android's impulse velocity strategy (`VelocityTracker`'s default, and
/// Compose's): the kinetic energy each movement adds, oldest to newest, so
/// the speed at release weighs more than an average over the window.
/// `t` in seconds, `p` positions, both oldest first.
fn impulse_velocity(t: &[f32], p: &[f32]) -> f32 {
    let speed = |work: f32| work.signum() * (2.0 * work.abs()).sqrt();
    let mut work = 0.0f32;
    let mut first = true;
    for k in 1..t.len().min(p.len()) {
        let dt = t[k] - t[k - 1];
        if dt <= 0.0 {
            continue;
        }
        let v = (p[k] - p[k - 1]) / dt;
        let before = speed(work);
        work += (v - before) * v.abs();
        if first {
            work *= 0.5;
            first = false;
        }
    }
    speed(work)
}

/// A scroll still moving after the finger left, along Android's fling
/// curve (`OverScroller`'s spline, which Compose's default fling uses too).
struct Fling {
    at: (f32, f32),
    start: Instant,
    /// Up (content moves up) or down.
    sign: f32,
    duration: f32,
    distance: f32,
    applied: f32,
}

/// Android's fling constants (`ViewConfiguration.getScrollFriction()`,
/// `OverScroller`), in points.
const INFLEXION: f32 = 0.35;
const SCROLL_FRICTION: f32 = 0.015;
/// `ln(0.78) / ln(0.9)`.
const DECELERATION_RATE: f32 = 2.358_201_7;
/// g × inches per meter × 160 points per inch × 0.84 (`OverScroller`).
const PHYSICAL_COEFF: f32 = 9.806_65 * 39.37 * 160.0 * 0.84;

impl Fling {
    /// A fling at `velocity` points per second: how long and how far it goes.
    fn new(at: (f32, f32), velocity: f32, start: Instant) -> Fling {
        let v = velocity.abs().min(MAX_FLING);
        let l = (INFLEXION * v / (SCROLL_FRICTION * PHYSICAL_COEFF)).ln();
        let duration = (l / (DECELERATION_RATE - 1.0)).exp();
        let distance = SCROLL_FRICTION
            * PHYSICAL_COEFF
            * (DECELERATION_RATE / (DECELERATION_RATE - 1.0) * l).exp();
        Fling {
            at,
            start,
            sign: velocity.signum(),
            duration,
            distance,
            applied: 0.0,
        }
    }

    /// How far along the curve it is at `now` (points), and whether done.
    fn position(&self, now: Instant) -> (f32, bool) {
        let u = (now.duration_since(self.start).as_secs_f32() / self.duration).min(1.0);
        (
            self.distance * (1.0 - (1.0 - u).powf(DECELERATION_RATE)),
            u >= 1.0,
        )
    }
}

/// Movement before a touch counts as a drag (points).
const TOUCH_SLOP: f32 = 8.0;
/// A touch held this long without moving is a long press (Android's
/// `ViewConfiguration.getLongPressTimeout()`).
const LONG_PRESS: f32 = 0.5;
/// Movement for a drag that scrolled nothing to count as a swipe (points).
const SWIPE_DISTANCE: f32 = 48.0;
/// Slowest and fastest flings (points per second, Android's
/// `ViewConfiguration` minimum and maximum fling velocities).
const MIN_FLING: f32 = 50.0;
const MAX_FLING: f32 = 8000.0;
/// How far back the release velocity looks, and how long a finger held
/// still before lifting means no fling (Compose's `VelocityTracker`).
const VELOCITY_HORIZON: f32 = 0.1;
const STILL_BEFORE_LIFT: f32 = 0.04;

/// The window's state across the event loop.
pub struct App {
    controller: Controller,
    platform: Option<Box<dyn Platform>>,
    title: String,
    surface: Option<Surface>,
    cursor: (f32, f32),
    drag: Option<Drag>,
    fling: Option<Fling>,
    /// Wakes the event loop (a session's images and answers arriving).
    waker: Option<Waker>,
    /// Shift, Control, Command held on a hardware keyboard.
    mods: ModifiersState,
}

impl App {
    pub fn new(controller: Controller, title: &str) -> App {
        App {
            controller,
            title: title.to_owned(),
            surface: None,
            platform: None,
            cursor: (0.0, 0.0),
            drag: None,
            fling: None,
            waker: None,
            mods: ModifiersState::empty(),
        }
    }

    fn clipboard(&self) -> Option<String> {
        match &self.platform {
            Some(p) => p.clipboard(),
            None => clipboard_here(),
        }
    }

    fn set_clipboard(&self, text: &str) {
        match &self.platform {
            Some(p) => p.set_clipboard(text),
            None => set_clipboard_here(text),
        }
    }

    /// What the platform reported: shown as a notice for now (the bridge's
    /// prototype); sign-in redirects and attachments build on these.
    fn platform_events(&mut self) {
        let events = self
            .platform
            .as_ref()
            .map(|p| p.events())
            .unwrap_or_default();
        for e in events {
            log(&format!("platform: {e:?}"));
            let (title, message) = match e {
                PlatformEvent::Link(uri) => ("Opened with a link".to_owned(), uri),
                PlatformEvent::Picked { count: 0, .. } => (
                    "No photos picked".to_owned(),
                    "The picker was closed.".to_owned(),
                ),
                PlatformEvent::Picked { count, bytes } => (
                    format!("{count} photo{} picked", if count == 1 { "" } else { "s" }),
                    format!(
                        "{:.1} MB in all, read from Rust.",
                        bytes as f64 / 1_000_000.0
                    ),
                ),
                PlatformEvent::Message(m) => ("Message received".to_owned(), m),
            };
            self.controller.session.notice(&title, &message);
            if let Some(s) = &self.surface {
                s.window.request_redraw();
            }
        }
    }

    /// Shows another session (signed in, or left the pod), at the window's
    /// size and insets.
    fn replace_session(&mut self, mut session: crate::interact::Session) {
        if let Some(w) = self.waker.clone() {
            session.images.set_waker(move || w());
        }
        let old = &self.controller.session;
        session.resize(old.viewport);
        session.set_insets(old.insets);
        self.controller = Controller::new(session);
        self.drag = None;
        self.fling = None;
        if let Some(s) = self.surface.as_mut() {
            s.renderer.images = Some(self.controller.session.images.clone());
            s.window.request_redraw();
        }
        if let Some(p) = &self.platform {
            p.show_keyboard(false);
        }
    }

    pub fn with_platform(mut self, platform: Box<dyn Platform>) -> App {
        self.platform = Some(platform);
        self
    }

    /// Re-reads the platform's insets (they change on rotation, or arrive
    /// after the first frame).
    /// True when they changed (the frame must be drawn again).
    fn update_insets(&mut self) -> bool {
        let (Some(p), Some(s)) = (&self.platform, &self.surface) else {
            return false;
        };
        let scale = s.window.scale_factor() as f32;
        let px = p.insets_px((s.config.width, s.config.height));
        let insets = px.map(|v| v / scale);
        if insets == self.controller.session.insets {
            return false;
        }
        self.controller.session.set_insets(insets);
        true
    }

    /// Moves a fling on by the time since the last frame; true while it
    /// still moves.
    fn step_fling(&mut self) -> bool {
        let Some(f) = self.fling.as_mut() else {
            return false;
        };
        let (pos, done) = f.position(Instant::now());
        let dy = f.sign * (pos - f.applied);
        f.applied = pos;
        let (x, y) = f.at;
        let moved = dy == 0.0 || self.scroll(x, y, dy);
        if !moved || done {
            self.fling = None;
        }
        moved && !done
    }

    /// Scrolls the view at a point; true if it moved.
    fn scroll(&mut self, x: f32, y: f32, dy: f32) -> bool {
        matches!(
            self.controller.handle(Event::Scroll { x, y, dy }),
            Some(out) if out.effects.iter().any(|e| matches!(e, crate::interact::Effect::Scrolled { .. }))
        )
    }

    fn scale(&self) -> f32 {
        self.surface
            .as_ref()
            .map_or(1.0, |s| s.window.scale_factor() as f32)
    }

    fn create(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(winit::dpi::LogicalSize::new(390.0, 844.0));
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
                Box::new(event_loop.owned_display_handle()),
            ));
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let gpu = pollster::block_on(Gpu::with_instance(instance, Some(&surface)))
            .map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&gpu.adapter);
        // A non-sRGB format blends as the CPU path and the goldens do.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or("the surface supports no format")?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &config);
        let mut renderer = Renderer::new(&gpu, format);
        renderer.images = Some(self.controller.session.images.clone());
        log(&format!(
            "{} surface initialized successfully: {} {:?}",
            match gpu.adapter.get_info().backend {
                wgpu::Backend::Vulkan => "Vulkan",
                wgpu::Backend::Metal => "Metal",
                wgpu::Backend::Gl => "GL",
                _ => "GPU",
            },
            gpu.describe(),
            format
        ));
        self.controller.handle(Event::Resized {
            width_px: config.width,
            height_px: config.height,
            scale: window.scale_factor() as f32,
        });
        window.request_redraw();
        self.surface = Some(Surface {
            window,
            surface,
            config,
            gpu,
            renderer,
        });
        self.update_insets();
        Ok(())
    }

    fn draw(&mut self) {
        self.update_insets();
        // The drag's scrolling since the last frame, in one step.
        if let Some(mut d) = self.drag.take() {
            if d.pending != 0.0 {
                let dy = std::mem::take(&mut d.pending);
                if self.scroll(d.start.0, d.start.1, dy) {
                    d.scrolled = true;
                }
            }
            self.drag = Some(d);
        }
        let flinging = self.step_fling();
        let Some(s) = self.surface.as_mut() else {
            return;
        };
        let started = Instant::now();
        let frame = match s.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(_) | wgpu::CurrentSurfaceTexture::Outdated => {
                s.surface.configure(&s.gpu.device, &s.config);
                s.window.request_redraw();
                return;
            }
            other => {
                log(&format!("no frame: {other:?}"));
                s.window.request_redraw();
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let px = (s.config.width, s.config.height);
        match self.controller.session.scene_and_text() {
            Ok((scene, text)) => {
                if let Err(e) = s.renderer.render(&s.gpu, &view, px, scene, text) {
                    log(&format!("draw failed: {e}"));
                }
            }
            Err(e) => log(&format!("layout failed: {e}")),
        }
        s.gpu.queue.present(frame);
        if flinging || self.controller.session.animating() {
            s.window.request_redraw();
        }
        if self.controller.frames == 0 {
            log(&format!(
                "Frame 0 drawn in {:.2} ms",
                started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        self.controller.frames += 1;
        self.controller.dirty = false;
    }

    fn apply(&mut self, e: Event) {
        let tap = matches!(e, Event::Tap { .. } | Event::LongPress { .. });
        if let Some(out) = self.controller.handle(e) {
            if let Ok(json) = serde_json::to_string(&out.effects) {
                log(&json);
            }
            let mut reopen = None;
            for e in &out.effects {
                match e {
                    crate::interact::Effect::OpenLink { url } => match &self.platform {
                        Some(p) => p.open_link(url),
                        None => open_link_here(url),
                    },
                    crate::interact::Effect::SignedIn { token, code } => {
                        reopen = Some(Reopen::SignedIn {
                            token: token.clone(),
                            code: code.clone(),
                        })
                    }
                    crate::interact::Effect::LeftPod => reopen = Some(Reopen::Left),
                    crate::interact::Effect::Paste { field } => match self.clipboard() {
                        Some(text) => {
                            self.controller.session.paste(field, &text);
                        }
                        None => log("nothing to paste"),
                    },
                    crate::interact::Effect::Copy { text } => self.set_clipboard(text),
                    crate::interact::Effect::PickMedia { max, .. } => {
                        if !self.platform.as_ref().is_some_and(|p| p.pick_images(*max)) {
                            self.controller.session.notice(
                                "No photo picker",
                                "Picking photos isn't available here yet.",
                            );
                        }
                    }
                    crate::interact::Effect::Device { command } if command == "refresh" => {
                        reopen = Some(Reopen::Reload)
                    }
                    _ => {}
                }
            }
            if let Some(how) = reopen
                && let Some(p) = &self.platform
                && let Some(session) = p.reopen(how)
            {
                self.replace_session(session);
            }
        }
        if tap && let Some(p) = &self.platform {
            p.show_keyboard(self.controller.focused().is_some());
        }
        if let Some(s) = &self.surface {
            s.window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    /// Android doesn't tell the app when the keyboard or system bars change
    /// the visible area (winit drops it): check after every batch of events.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.platform.as_ref().is_some_and(|p| p.gone()) {
            log("the window is gone");
            event_loop.exit();
            return;
        }
        self.platform_events();
        if self.update_insets()
            && let Some(s) = &self.surface
        {
            s.window.request_redraw();
        }
        // A finger held still long enough is a long press, while it is still
        // down; dragging on from a word it selected moves the selection's end.
        let mut wake = self.controller.session.next_wake();
        let held = self
            .drag
            .as_ref()
            .filter(|d| !d.dragging && !d.long_pressed && !d.grabbing)
            .map(|d| {
                (
                    d.started + std::time::Duration::from_secs_f32(LONG_PRESS),
                    d.last,
                )
            });
        if let Some((due, at)) = held {
            if Instant::now() >= due {
                self.apply(Event::LongPress { x: at.0, y: at.1 });
                let grabbing = self.controller.session.grab_selection_end();
                if let Some(d) = self.drag.as_mut() {
                    d.long_pressed = true;
                    d.grabbing = grabbing;
                }
            } else {
                wake = Some(wake.map_or(due, |w| w.min(due)));
            }
        }
        event_loop.set_control_flow(match wake {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }

    /// A timer ran out (the caret blinks, a long press is due): draw again.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. })
            && let Some(s) = &self.surface
        {
            s.window.request_redraw();
        }
    }

    /// An image or a platform event arrived: draw again.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        self.platform_events();
        if let Some(s) = &self.surface {
            s.window.request_redraw();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_none()
            && let Err(e) = self.create(event_loop)
        {
            log(&format!("couldn't open the window: {e}"));
            event_loop.exit();
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // Android takes the native window away while paused.
        self.surface = None;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let scale = self.scale();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Resized(size) => {
                if let Some(s) = self.surface.as_mut() {
                    s.config.width = size.width.max(1);
                    s.config.height = size.height.max(1);
                    s.surface.configure(&s.gpu.device, &s.config);
                }
                self.apply(Event::Resized {
                    width_px: size.width.max(1),
                    height_px: size.height.max(1),
                    scale,
                });
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32 / scale, position.y as f32 / scale);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.apply(Event::Tap {
                x: self.cursor.0,
                y: self.cursor.1,
            }),
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 40.0,
                    MouseScrollDelta::PixelDelta(p) => -p.y as f32 / scale,
                };
                self.apply(Event::Scroll {
                    x: self.cursor.0,
                    y: self.cursor.1,
                    dy,
                });
            }
            WindowEvent::Touch(t) => {
                let p = (t.location.x as f32 / scale, t.location.y as f32 / scale);
                let now = Instant::now();
                match t.phase {
                    TouchPhase::Started => {
                        self.fling = None;
                        self.drag = Some(Drag {
                            start: p,
                            last: p,
                            at: now,
                            samples: std::iter::once((now, p.0, p.1)).collect(),
                            dragging: false,
                            scrolled: false,
                            frame: self.controller.session.frame_drag_at(p.0, p.1),
                            moving_frame: false,
                            pager: self.controller.session.pager_at(p.0, p.1).map(|id| {
                                let at = self.controller.session.pager_offset(&id);
                                (id, at)
                            }),
                            paging: false,
                            started: now,
                            grabbing: self.controller.session.grab_at(p.0, p.1).is_some(),
                            long_pressed: false,
                            pending: 0.0,
                        });
                    }
                    TouchPhase::Moved => {
                        let Some(mut d) = self.drag.take() else {
                            return;
                        };
                        // An edit handle follows the finger; after a long
                        // press, nothing scrolls.
                        if d.grabbing || d.long_pressed {
                            if d.grabbing {
                                self.controller.session.drag_grab(p.0, p.1);
                                if let Some(s) = &self.surface {
                                    s.window.request_redraw();
                                }
                            }
                            d.last = p;
                            self.drag = Some(d);
                            return;
                        }
                        let (tx, ty) = (p.0 - d.start.0, p.1 - d.start.1);
                        if !d.dragging && (tx.abs() > TOUCH_SLOP || ty.abs() > TOUCH_SLOP) {
                            d.dragging = true;
                            // A sheet moves on a drag along its axis; a panel on
                            // a sideways one.
                            d.moving_frame = match d.frame {
                                Some(DragAxis::Vertical) => ty.abs() >= tx.abs(),
                                Some(DragAxis::Horizontal) => tx.abs() > ty.abs(),
                                None => false,
                            };
                            d.paging = !d.moving_frame && d.pager.is_some() && tx.abs() > ty.abs();
                        }
                        if d.paging
                            && let Some((id, at)) = &d.pager
                        {
                            self.controller.session.drag_pager(id, at - tx);
                        } else if d.moving_frame {
                            self.controller.session.drag_frame(tx, ty);
                        } else if d.dragging && ty.abs() >= tx.abs() {
                            d.pending += d.last.1 - p.1;
                        }
                        d.last = p;
                        d.at = now;
                        d.samples.push_back((now, p.0, p.1));
                        while d.samples.len() > 2
                            && now.duration_since(d.samples[0].0).as_secs_f32() > VELOCITY_HORIZON
                        {
                            d.samples.pop_front();
                        }
                        self.drag = Some(d);
                        if let Some(s) = &self.surface {
                            s.window.request_redraw();
                        }
                    }
                    TouchPhase::Ended => {
                        let Some(mut d) = self.drag.take() else {
                            return;
                        };
                        if d.grabbing || d.long_pressed {
                            if d.grabbing {
                                self.controller.session.release_grab();
                            }
                            if let Some(s) = &self.surface {
                                s.window.request_redraw();
                            }
                            return;
                        }
                        if d.pending != 0.0 && self.scroll(d.start.0, d.start.1, d.pending) {
                            d.scrolled = true;
                        }
                        let (tx, ty) = (p.0 - d.start.0, p.1 - d.start.1);
                        // The release velocity over the last 100 ms (content
                        // moving up is positive); none if the finger was held
                        // still before lifting.
                        let stale = now.duration_since(d.at).as_secs_f32() > STILL_BEFORE_LIFT;
                        let (vx, vy) = d.velocity();
                        // Content moving up is a positive scroll velocity.
                        let velocity = -vy;
                        if d.paging
                            && let Some((id, _)) = &d.pager
                        {
                            self.controller
                                .session
                                .release_pager(id, if stale { 0.0 } else { vx });
                            if let Some(s) = &self.surface {
                                s.window.request_redraw();
                            }
                        } else if d.moving_frame {
                            let (vx, vy) = if stale { (0.0, 0.0) } else { (vx, vy) };
                            if self.controller.session.release_frame(vx, vy)
                                && let Some(p) = &self.platform
                            {
                                p.show_keyboard(false);
                            }
                            if let Some(s) = &self.surface {
                                s.window.request_redraw();
                            }
                        } else if !d.dragging {
                            self.apply(Event::Tap { x: p.0, y: p.1 });
                        } else if !d.scrolled && tx.abs().max(ty.abs()) > SWIPE_DISTANCE {
                            let kind = if tx.abs() > ty.abs() {
                                if tx > 0.0 {
                                    "swipe_right"
                                } else {
                                    "swipe_left"
                                }
                            } else if ty > 0.0 {
                                "swipe_down"
                            } else {
                                "swipe_up"
                            };
                            self.apply(Event::Swipe {
                                x: d.start.0,
                                y: d.start.1,
                                kind,
                            });
                        } else if d.scrolled && !stale && velocity.abs() > MIN_FLING {
                            self.fling = Some(Fling::new(d.start, velocity, now));
                            if let Some(s) = &self.surface {
                                s.window.request_redraw();
                            }
                        }
                    }
                    TouchPhase::Cancelled => {
                        if let Some(d) = self.drag.take() {
                            if d.grabbing {
                                self.controller.session.release_grab();
                            }
                            if d.moving_frame {
                                self.controller.session.release_frame(0.0, 0.0);
                            }
                            if d.paging
                                && let Some((id, _)) = &d.pager
                            {
                                self.controller.session.release_pager(id, 0.0);
                            }
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match &event.logical_key {
                    Key::Named(NamedKey::Escape | NamedKey::BrowserBack | NamedKey::GoBack) => {
                        if self.controller.unfocus() {
                            if let Some(p) = &self.platform {
                                p.show_keyboard(false);
                            }
                        } else {
                            self.apply(Event::Back);
                        }
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(p) = &self.platform {
                            p.show_keyboard(false);
                        }
                        self.controller.unfocus();
                    }
                    Key::Named(NamedKey::Backspace) => self.apply(Event::Backspace),
                    Key::Named(NamedKey::Delete) => self.apply(Event::Edit(EditKey::Delete)),
                    Key::Named(
                        k @ (NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::Home
                        | NamedKey::End),
                    ) => {
                        let to = match k {
                            NamedKey::ArrowLeft => CursorMove::Left,
                            NamedKey::ArrowRight => CursorMove::Right,
                            NamedKey::Home => CursorMove::Home,
                            _ => CursorMove::End,
                        };
                        self.apply(Event::Edit(EditKey::Move(to, self.mods.shift_key())));
                    }
                    Key::Character(c)
                        if (self.mods.control_key() || self.mods.super_key())
                            && self.controller.focused().is_some() =>
                    {
                        match c.to_lowercase().as_str() {
                            "a" => self.apply(Event::Edit(EditKey::SelectAll)),
                            "c" | "x" => {
                                if let Some(t) =
                                    self.controller.session.copy(c.eq_ignore_ascii_case("x"))
                                {
                                    self.set_clipboard(&t);
                                }
                                if let Some(s) = &self.surface {
                                    s.window.request_redraw();
                                }
                            }
                            "v" => {
                                if let Some(t) = self.clipboard() {
                                    self.apply(Event::Text(
                                        t.trim_end_matches(['\n', '\r']).to_owned(),
                                    ));
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        if let Some(t) = event
                            .text
                            .as_ref()
                            .filter(|t| !t.chars().any(char::is_control))
                        {
                            self.apply(Event::Text(t.to_string()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Runs the window until it closes.
pub fn run(event_loop: EventLoop<()>, controller: Controller, title: &str) -> Result<(), String> {
    run_app(event_loop, App::new(controller, title))
}

/// Runs a prepared [`App`] (with a [`Platform`]) until it closes.
pub fn run_app(event_loop: EventLoop<()>, mut app: App) -> Result<(), String> {
    let proxy = event_loop.create_proxy();
    let waker: Waker = Arc::new(move || {
        let _ = proxy.send_event(());
    });
    let w = waker.clone();
    app.controller.session.images.set_waker(move || w());
    app.waker = Some(waker);
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::impulse_velocity;

    #[test]
    fn a_steady_drag_gives_its_speed_and_a_flick_its_last_speed() {
        let t: Vec<f32> = (0..7).map(|i| i as f32 * 0.016).collect();
        let steady: Vec<f32> = t.iter().map(|t| t * 1000.0).collect();
        let v = impulse_velocity(&t, &steady);
        assert!((v - 1000.0).abs() < 1.0, "{v}");
        // Speeding up to 3000 points/s: well above the average (about 1700).
        let mut flick = vec![0.0f32];
        for k in 1..7 {
            flick.push(flick[k - 1] + 0.016 * 500.0 * k as f32);
        }
        let v = impulse_velocity(&t, &flick);
        assert!(v > 2200.0, "{v}");
    }
}
