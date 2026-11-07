//! A native window drawing a [`Controller`]'s session with wgpu: Metal on
//! macOS, Vulkan (or GL) on Linux and Android, presented straight from the
//! surface texture. Input goes through [`crate::viewer`].

use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::gpu::{Gpu, Renderer};
use crate::interact::DragAxis;
use crate::viewer::{Controller, Event};

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

/// What a platform adds to the window: the soft keyboard, and the space its
/// own UI covers (status bar, cutout, navigation bar).
pub trait Platform {
    /// Shows the keyboard while a text field is focused, hides it otherwise.
    fn show_keyboard(&self, show: bool);
    /// Insets in physical pixels: top, right, bottom, left.
    fn insets_px(&self, window_px: (u32, u32)) -> [f32; 4];
    /// Opens an `http(s)` address in the browser.
    fn open_link(&self, url: &str) {
        open_link_here(url);
    }
    /// The platform closed the window for good (Android destroyed the
    /// activity): the event loop ends.
    fn gone(&self) -> bool {
        false
    }
}

/// Opens an address with the desktop's opener (`open`, `xdg-open`, `start`).
pub fn open_link_here(url: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    if let Err(e) = cmd.arg(url).spawn() {
        log(&format!("can't open {url}: {e}"));
    }
}

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
}

impl Drag {
    /// The release velocity over the last 100 ms, points per second (x, y).
    fn velocity(&self) -> (f32, f32) {
        match (self.samples.front(), self.samples.back()) {
            (Some(a), Some(b)) if b.0 > a.0 => {
                let t = b.0.duration_since(a.0).as_secs_f32();
                ((b.1 - a.1) / t, (b.2 - a.2) / t)
            }
            _ => (0.0, 0.0),
        }
    }
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
        let tap = matches!(e, Event::Tap { .. });
        if let Some(out) = self.controller.handle(e) {
            if let Ok(json) = serde_json::to_string(&out.effects) {
                log(&json);
            }
            for e in &out.effects {
                if let crate::interact::Effect::OpenLink { url } = e {
                    match &self.platform {
                        Some(p) => p.open_link(url),
                        None => open_link_here(url),
                    }
                }
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
        if self.update_insets()
            && let Some(s) = &self.surface
        {
            s.window.request_redraw();
        }
    }

    /// An image arrived: draw again.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
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
                            pending: 0.0,
                        });
                    }
                    TouchPhase::Moved => {
                        let Some(mut d) = self.drag.take() else {
                            return;
                        };
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
    app.controller.session.images.set_waker(move || {
        let _ = proxy.send_event(());
    });
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}
