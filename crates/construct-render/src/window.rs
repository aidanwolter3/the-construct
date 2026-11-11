//! A native window drawing a [`Controller`]'s session with wgpu: Metal on
//! macOS, Vulkan (or GL) on Linux and Android, presented straight from the
//! surface texture. Input goes through [`crate::viewer`]. On the desktop,
//! screen readers read and act on the scene's semantic tree through
//! AccessKit ([`crate::accessibility`]).

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
#[cfg(not(target_os = "android"))]
use crate::interact::Target;
pub use crate::platform::{
    Platform, PlatformEvent, Reopen, clipboard_here, open_link_here, set_clipboard_here,
};
use crate::scroll::{
    Fling, Flinging, Stretch, TOUCH_SLOP, VelocityTracker, WHEEL_NOTCH, WheelGlide,
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
    /// Whether `config` is applied; a failed configure is tried again
    /// before the next frame.
    configured: bool,
}

impl Surface {
    /// Applies `config`. It can fail without anything being wrong with the
    /// app: Android's driver sometimes can't wait for the GPU to go idle
    /// right after the app comes back. wgpu's default answer to that is a
    /// panic, which closes the activity, so catch it here; true when it
    /// worked.
    fn configure(&mut self) -> bool {
        let scope = self
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        self.surface.configure(&self.gpu.device, &self.config);
        self.configured = match pollster::block_on(scope.pop()) {
            None => true,
            Some(e) => {
                log(&format!(
                    "couldn't configure the surface (trying again): {e}"
                ));
                false
            }
        };
        self.configured
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// What a screen reader asked for, from AccessKit's threads, until the
/// event loop runs it.
#[derive(Default)]
struct A11yInbox {
    /// A screen reader started: send the whole tree.
    wants_tree: bool,
    actions: Vec<accesskit::ActionRequest>,
}

/// The window's AccessKit adapter and what its tree stands for.
#[cfg(not(target_os = "android"))]
struct A11y {
    adapter: accesskit_winit::Adapter,
    nodes: std::collections::HashMap<accesskit::NodeId, crate::accessibility::Exposed>,
}

#[cfg(not(target_os = "android"))]
struct Inbox {
    inbox: Arc<std::sync::Mutex<A11yInbox>>,
    waker: Option<Waker>,
}

#[cfg(not(target_os = "android"))]
impl Inbox {
    fn with(&self, f: impl FnOnce(&mut A11yInbox)) {
        if let Ok(mut i) = self.inbox.lock() {
            f(&mut i);
        }
        if let Some(w) = &self.waker {
            w();
        }
    }
}

#[cfg(not(target_os = "android"))]
impl accesskit::ActivationHandler for Inbox {
    /// The tree is built on the event loop (it owns the session).
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        self.with(|i| i.wants_tree = true);
        None
    }
}

#[cfg(not(target_os = "android"))]
impl accesskit::ActionHandler for Inbox {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        self.with(|i| i.actions.push(request));
    }
}

#[cfg(not(target_os = "android"))]
impl accesskit::DeactivationHandler for Inbox {
    fn deactivate_accessibility(&mut self) {}
}

/// A finger on the screen.
struct Drag {
    start: (f32, f32),
    last: (f32, f32),
    /// Its positions across and down, for the release velocity.
    vx: VelocityTracker,
    vy: VelocityTracker,
    /// It landed on a moving fling: it stopped it, and is no tap.
    caught: bool,
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
    /// The chip row under its start and where it was, and whether the drag
    /// scrolls it.
    slide: Option<(String, f32)>,
    sliding: bool,
    /// Scrolling not applied yet: applied once per frame, in `draw`.
    pending: f32,
    /// When the finger went down.
    started: Instant,
    /// It is moving an edit handle.
    grabbing: bool,
    /// It was held long enough to be a long press (which already ran).
    long_pressed: bool,
}

/// A touch held this long without moving is a long press (Android's
/// `ViewConfiguration.getLongPressTimeout()`).
const LONG_PRESS: f32 = 0.5;
/// Movement for a drag that scrolled nothing to count as a swipe (points).
const SWIPE_DISTANCE: f32 = 48.0;

/// The window's state across the event loop.
pub struct App {
    controller: Controller,
    platform: Option<Box<dyn Platform>>,
    title: String,
    surface: Option<Surface>,
    cursor: (f32, f32),
    drag: Option<Drag>,
    /// A fling under way, and the point whose scrolling view it moves.
    fling: Option<(Flinging, (f32, f32))>,
    /// Mouse wheel notches still gliding, and where the pointer was.
    wheel: (WheelGlide, (f32, f32)),
    /// A scrolling view stretched at an end (by its id).
    stretch: Option<(Stretch, String)>,
    /// Time zero for the scroll physics, which take seconds.
    epoch: Instant,
    /// Wakes the event loop (a session's images and answers arriving).
    waker: Option<Waker>,
    /// Shift, Control, Command held on a hardware keyboard.
    mods: ModifiersState,
    /// Screen readers' requests, and the adapter that serves them.
    inbox: Arc<std::sync::Mutex<A11yInbox>>,
    #[cfg(not(target_os = "android"))]
    a11y: Option<A11y>,
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
            wheel: Default::default(),
            stretch: None,
            epoch: Instant::now(),
            waker: None,
            mods: ModifiersState::empty(),
            inbox: Default::default(),
            #[cfg(not(target_os = "android"))]
            a11y: None,
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

    /// What the platform reported: a sign-in's redirect goes to the
    /// engine's sign-in (`crate::signin`); picked photos are staged for the
    /// media picker that asked; the rest shows as a notice for now (the
    /// bridge's prototype).
    fn platform_events(&mut self) {
        let events = self
            .platform
            .as_ref()
            .map(|p| p.events())
            .unwrap_or_default();
        for e in events {
            #[cfg(feature = "remote")]
            if let PlatformEvent::Link(uri) = &e
                && crate::signin::is_redirect(uri, crate::signin::APP_LINK_HOST)
            {
                // The code is not logged: the engine exchanges it.
                let taken = self.controller.session.auth_redirect(uri);
                log(&format!(
                    "sign-in redirect {}",
                    if taken { "taken" } else { "not used" }
                ));
                if let Some(s) = &self.surface {
                    s.window.request_redraw();
                }
                continue;
            }
            log(&format!("platform: {}", e.name()));
            let (title, message) = match e {
                PlatformEvent::Link(uri) => ("Opened with a link".to_owned(), uri),
                PlatformEvent::Picked(files) => {
                    self.controller.session.picked(files);
                    if let Some(s) = &self.surface {
                        s.window.request_redraw();
                    }
                    continue;
                }
                PlatformEvent::Message(m) => ("Message received".to_owned(), m),
                PlatformEvent::Notification(n) => {
                    self.open_notification(n);
                    continue;
                }
            };
            self.controller.session.notice(&title, &message);
            if let Some(s) = &self.surface {
                s.window.request_redraw();
            }
        }
    }

    /// A tapped notification: opens its layout and screen as the inbox
    /// does (`open_notification`), from the pod's own record of it when the
    /// session hasn't listed it.
    fn open_notification(&mut self, n: serde_json::Value) {
        let Some(id) = n["id"].as_str().map(str::to_owned) else {
            return;
        };
        let session = &mut self.controller.session;
        let uri = format!("local://notifications/{id}");
        if !session.data.entities.contains_key(&uri)
            && let serde_json::Value::Object(fields) = n
        {
            session
                .data
                .entities
                .insert(uri, protocol::EntityRecord(fields));
        }
        let intent = serde_json::json!({ "intent": "action.device", "command": "open_notification", "id": id });
        match session.dispatch(&intent) {
            Ok(out) => self.after(Some(out), false),
            Err(e) => log(&format!("opening notification {id}: {e}")),
        }
    }

    /// Brings the platform's keyboard and the focused field together
    /// ([`Platform::text_input`]): what the keyboard typed, corrected or
    /// composed goes into the session, and the session's own changes reach
    /// the keyboard.
    fn sync_text_input(&mut self) {
        // Twice at most: the keyboard's edit, then the session's answer to it
        // (a value cut to the field's maximum length).
        for _ in 0..2 {
            let field = self.controller.session.text_state();
            let Some(edit) = self
                .platform
                .as_ref()
                .and_then(|p| p.text_input(field.as_ref()))
            else {
                return;
            };
            if let Some(state) = &edit.state {
                let out = self.controller.session.set_text_state(state);
                self.after(Some(out), false);
            }
            if let Some(intent) = edit.menu {
                let out = self
                    .controller
                    .session
                    .dispatch(&serde_json::json!({ "intent": intent }))
                    .ok();
                self.after(out, false);
            }
            if edit.done {
                // As Enter on a hardware keyboard.
                if let Some(p) = &self.platform {
                    p.show_keyboard(false);
                }
                self.controller.unfocus();
                if let Some(s) = &self.surface {
                    s.window.request_redraw();
                }
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
        session.set_dark(old.dark);
        self.controller = Controller::new(session);
        self.drag = None;
        self.fling = None;
        self.wheel.0.stop();
        self.stretch = None;
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

    /// Follows the system's light or dark mode; true when it changed.
    fn update_dark(&mut self) -> bool {
        let dark = match self.platform.as_ref().and_then(|p| p.dark()) {
            Some(d) => d,
            None => match self.surface.as_ref().and_then(|s| s.window.theme()) {
                Some(t) => t == winit::window::Theme::Dark,
                None => return false,
            },
        };
        if dark == self.controller.session.dark {
            return false;
        }
        log(&format!("{} mode", if dark { "dark" } else { "light" }));
        self.controller.session.set_dark(dark);
        true
    }

    /// Seconds since `epoch`, as the scroll physics count time.
    fn secs(&self, at: Instant) -> f64 {
        at.duration_since(self.epoch).as_secs_f64()
    }

    /// The stretch of the view `id` (any other view's ends).
    fn take_stretch(&mut self, id: &str) -> Stretch {
        match self.stretch.take() {
            Some((st, at)) if at == id => st,
            Some((_, other)) => {
                self.controller.session.set_stretch(&other, 0.0);
                Stretch::default()
            }
            None => Stretch::default(),
        }
    }

    /// A drag's scrolling `dy` on the view at `at`: it undoes a stretch,
    /// scrolls, and stretches past the ends. True if the content moved.
    fn drag_scroll(&mut self, at: (f32, f32), dy: f32) -> bool {
        let Some(v) = self.controller.session.scroll_view_at(at.0, at.1) else {
            return false;
        };
        let mut st = self.take_stretch(&v.id);
        let to = st.drag(v.offset, v.max, v.height, dy);
        let session = &mut self.controller.session;
        if to != v.offset {
            session.set_scroll(&v.id, to);
        }
        session.set_stretch(&v.id, st.scale());
        if st.active() {
            self.stretch = Some((st, v.id));
        }
        to != v.offset
    }

    /// Moves a fling and a wheel glide on to this frame; true while either
    /// still moves. Each stops at its view's end.
    fn step_scrolling(&mut self) -> bool {
        let now = self.secs(Instant::now());
        let mut moving = false;
        if let Some((mut f, (x, y))) = self.fling.take() {
            let (dy, done) = f.step(now);
            let session = &mut self.controller.session;
            if let Some(v) = session.scroll_view_at(x, y) {
                let to = (v.offset + dy).clamp(0.0, v.max);
                if to != v.offset {
                    session.set_scroll(&v.id, to);
                }
                if to != v.offset + dy {
                    // It met an end: the content stretches for a moment.
                    if v.max > 0.0 {
                        let mut st = self.take_stretch(&v.id);
                        st.absorb(f.velocity(now), v.height, now);
                        self.stretch = Some((st, v.id));
                    }
                } else if !done {
                    self.fling = Some((f, (x, y)));
                    moving = true;
                }
            }
        }
        if let Some((mut st, id)) = self.stretch.take() {
            moving |= st.step(now);
            self.controller.session.set_stretch(&id, st.scale());
            if st.active() {
                self.stretch = Some((st, id));
            }
        }
        if self.wheel.0.active() {
            let (x, y) = self.wheel.1;
            let dy = self.wheel.0.step(now);
            if dy != 0.0 && !self.scroll(x, y, dy) {
                self.wheel.0.stop();
            }
            moving |= self.wheel.0.active();
        }
        moving
    }

    /// A finger or the mouse button went down: the row there darkens.
    fn press(&mut self, x: f32, y: f32) {
        if self.controller.session.press(x, y)
            && let Some(s) = &self.surface
        {
            s.window.request_redraw();
        }
    }

    /// The press ended (lifted, cancelled, or turned into a drag).
    fn release_press(&mut self) {
        if self.controller.session.release_press()
            && let Some(s) = &self.surface
        {
            s.window.request_redraw();
        }
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
        // AccessKit's adapter must exist before the window is first shown.
        #[cfg(not(target_os = "android"))]
        let attrs = attrs.with_visible(false);
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        #[cfg(not(target_os = "android"))]
        {
            let handler = || Inbox {
                inbox: self.inbox.clone(),
                waker: self.waker.clone(),
            };
            let adapter = accesskit_winit::Adapter::with_direct_handlers(
                event_loop,
                &window,
                handler(),
                handler(),
                handler(),
            );
            self.a11y = Some(A11y {
                adapter,
                nodes: Default::default(),
            });
            window.set_visible(true);
        }
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
        // Any other GPU error is logged: wgpu's default handler panics, and
        // a panic here closes the app.
        gpu.device
            .on_uncaptured_error(Arc::new(|e| log(&format!("GPU error: {e}"))));
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
        let mut s = Surface {
            window,
            surface,
            config,
            gpu,
            renderer,
            configured: false,
        };
        s.configure();
        self.surface = Some(s);
        self.update_insets();
        self.update_dark();
        Ok(())
    }

    fn draw(&mut self) {
        self.update_insets();
        // The drag's scrolling since the last frame, in one step.
        if let Some(mut d) = self.drag.take() {
            if d.pending != 0.0 {
                let dy = std::mem::take(&mut d.pending);
                if self.drag_scroll(d.start, dy) {
                    d.scrolled = true;
                }
            }
            self.drag = Some(d);
        }
        let flinging = self.step_scrolling();
        let Some(s) = self.surface.as_mut() else {
            return;
        };
        if !s.configured && !s.configure() {
            s.window.request_redraw();
            return;
        }
        let started = Instant::now();
        let frame = match s.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(_) | wgpu::CurrentSurfaceTexture::Outdated => {
                s.configure();
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
        self.update_a11y();
    }

    /// Sends the frame's tree to a screen reader, if one is listening.
    #[cfg(not(target_os = "android"))]
    fn update_a11y(&mut self) {
        let Some(a11y) = self.a11y.as_mut() else {
            return;
        };
        let session = &mut self.controller.session;
        let title = &self.title;
        let nodes = &mut a11y.nodes;
        a11y.adapter.update_if_active(|| {
            let form = session.form.clone();
            let focused = session.focused.clone();
            let empty = crate::scene::Scene::default();
            let scene = match session.scene() {
                Ok(scene) => scene,
                Err(_) => &empty,
            };
            let tree = crate::accessibility::tree(scene, &form, focused.as_deref(), title);
            *nodes = tree.nodes;
            tree.update
        });
    }

    #[cfg(target_os = "android")]
    fn update_a11y(&mut self) {}

    /// Runs what screen readers asked for since the last time.
    fn serve_a11y(&mut self) {
        let (wants_tree, actions) = match self.inbox.lock() {
            Ok(mut i) => (
                std::mem::take(&mut i.wants_tree),
                std::mem::take(&mut i.actions),
            ),
            Err(_) => return,
        };
        #[cfg(not(target_os = "android"))]
        for req in actions {
            use crate::accessibility::Request;
            let Some(a11y) = &self.a11y else { break };
            match crate::accessibility::request(&a11y.nodes, &req) {
                Some(Request::Tap(id)) => {
                    let out = self.controller.session.tap(&Target::Id(id)).ok();
                    self.controller.dirty = true;
                    self.after(out, true);
                }
                Some(Request::Scroll { id, dy }) => {
                    let _ = self.controller.session.scroll_by(Some(&Target::Id(id)), dy);
                    self.controller.dirty = true;
                }
                Some(Request::SetValue { field, value }) => {
                    self.controller.session.type_text(&field, &value);
                    self.controller.dirty = true;
                }
                None => {}
            }
        }
        #[cfg(target_os = "android")]
        let _ = actions;
        if wants_tree || self.controller.dirty {
            self.update_a11y();
        }
        if self.controller.dirty
            && let Some(s) = &self.surface
        {
            s.window.request_redraw();
        }
    }

    fn apply(&mut self, e: Event) {
        let tap = matches!(e, Event::Tap { .. } | Event::LongPress { .. });
        let out = self.controller.handle(e);
        self.after(out, tap);
    }

    /// Carries out what an interaction did (links, sign-in, the clipboard,
    /// the keyboard) and draws again.
    fn after(&mut self, out: Option<crate::interact::Outcome>, tap: bool) {
        if let Some(out) = out {
            // Only the effects' names: their values can be what was typed
            // (a pod code) or a session token.
            if !out.effects.is_empty() {
                let names: Vec<&str> = out.effects.iter().map(|e| e.name()).collect();
                log(&format!("effects: {}", names.join(", ")));
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
                    crate::interact::Effect::SwitchServer { url } => {
                        reopen = Some(Reopen::Server(url.clone()))
                    }
                    crate::interact::Effect::Paste { field } => match self.clipboard() {
                        Some(text) => {
                            self.controller.session.paste(field, &text);
                        }
                        None => log("nothing to paste"),
                    },
                    crate::interact::Effect::Copy { text } => self.set_clipboard(text),
                    crate::interact::Effect::PickMedia { max, .. } => {
                        if !self.platform.as_ref().is_some_and(|p| p.pick_images(*max))
                            && !self.controller.session.pick_here(*max)
                        {
                            self.controller.session.notice(
                                "No photo picker",
                                "Picking photos isn't available here yet.",
                            );
                        }
                    }
                    crate::interact::Effect::Device { command }
                        if command == protocol::intent::NOTIFICATION_PERMISSION_COMMAND =>
                    {
                        if let Some(p) = &self.platform {
                            p.ask_notifications();
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
        self.sync_text_input();
        let insets = self.update_insets();
        if (insets | self.update_dark())
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
            .filter(|d| !d.dragging && !d.long_pressed && !d.grabbing && !d.caught)
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

    /// An image or a platform event arrived, or a screen reader asked for
    /// something: draw again.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        self.platform_events();
        self.sync_text_input();
        self.serve_a11y();
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
        #[cfg(not(target_os = "android"))]
        {
            self.a11y = None;
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let scale = self.scale();
        #[cfg(not(target_os = "android"))]
        if let (Some(a11y), Some(s)) = (self.a11y.as_mut(), self.surface.as_ref()) {
            a11y.adapter.process_event(&s.window, &event);
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Resized(size) => {
                if let Some(s) = self.surface.as_mut() {
                    s.config.width = size.width.max(1);
                    s.config.height = size.height.max(1);
                    s.configure();
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
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.press(self.cursor.0, self.cursor.1),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.release_press();
                self.apply(Event::Tap {
                    x: self.cursor.0,
                    y: self.cursor.1,
                })
            }
            // A mouse wheel's notches glide; a trackpad's pixels apply as
            // they come (macOS sends its own momentum after the fingers lift).
            // A sideways one (or Shift and a plain wheel) scrolls a chip row.
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x * WHEEL_NOTCH, -y * WHEEL_NOTCH),
                    MouseScrollDelta::PixelDelta(p) => (-p.x as f32 / scale, -p.y as f32 / scale),
                };
                let dx = if self.mods.shift_key() && dx == 0.0 {
                    dy
                } else {
                    dx
                };
                if dx.abs() > dy.abs() || (dx != 0.0 && self.mods.shift_key()) {
                    let (x, y) = self.cursor;
                    self.apply(Event::ScrollSideways { x, y, dx });
                    return;
                }
                match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        let now = self.secs(Instant::now());
                        self.fling = None;
                        self.wheel.1 = self.cursor;
                        self.wheel.0.push(now, -y * WHEEL_NOTCH);
                        if let Some(s) = &self.surface {
                            s.window.request_redraw();
                        }
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        self.wheel.0.stop();
                        self.apply(Event::Scroll {
                            x: self.cursor.0,
                            y: self.cursor.1,
                            dy: -p.y as f32 / scale,
                        });
                    }
                }
            }
            WindowEvent::Touch(t) => {
                let p = (t.location.x as f32 / scale, t.location.y as f32 / scale);
                let now = Instant::now();
                match t.phase {
                    TouchPhase::Started => {
                        // A finger on a moving fling stops it, and is no tap.
                        let secs = self.secs(now);
                        let mut caught = self.fling.take().is_some_and(|(f, _)| f.caught(secs));
                        self.wheel.0.stop();
                        // And a stretch stays where it is, for the drag.
                        if let Some((st, _)) = self.stretch.as_mut() {
                            st.hold();
                            caught = true;
                        }
                        let (mut vx, mut vy) = (VelocityTracker::new(), VelocityTracker::new());
                        vx.add(secs, p.0);
                        vy.add(secs, p.1);
                        self.drag = Some(Drag {
                            start: p,
                            last: p,
                            vx,
                            vy,
                            caught,
                            dragging: false,
                            scrolled: false,
                            frame: self.controller.session.frame_drag_at(p.0, p.1),
                            moving_frame: false,
                            pager: self.controller.session.pager_at(p.0, p.1).map(|id| {
                                let at = self.controller.session.pager_offset(&id);
                                (id, at)
                            }),
                            paging: false,
                            slide: self.controller.session.slide_at(p.0, p.1).map(|id| {
                                let at = self.controller.session.slide_offset(&id);
                                (id, at)
                            }),
                            sliding: false,
                            started: now,
                            grabbing: self.controller.session.grab_at(p.0, p.1).is_some(),
                            long_pressed: false,
                            pending: 0.0,
                        });
                        if !caught {
                            self.press(p.0, p.1);
                        }
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
                            // A drag is no tap: the row stops looking pressed.
                            self.release_press();
                            // A sheet moves on a drag along its axis; a panel on
                            // a sideways one.
                            d.moving_frame = match d.frame {
                                Some(DragAxis::Vertical) => ty.abs() >= tx.abs(),
                                Some(DragAxis::Horizontal) => tx.abs() > ty.abs(),
                                None => false,
                            };
                            d.paging = !d.moving_frame && d.pager.is_some() && tx.abs() > ty.abs();
                            d.sliding = !d.moving_frame
                                && !d.paging
                                && d.slide.is_some()
                                && tx.abs() > ty.abs();
                        }
                        if d.sliding
                            && let Some((id, at)) = &d.slide
                        {
                            self.controller.session.drag_slide(id, at - tx);
                        } else if d.paging
                            && let Some((id, at)) = &d.pager
                        {
                            self.controller.session.drag_pager(id, at - tx);
                        } else if d.moving_frame {
                            self.controller.session.drag_frame(tx, ty);
                        } else if d.dragging && ty.abs() >= tx.abs() {
                            d.pending += d.last.1 - p.1;
                        }
                        d.last = p;
                        let secs = self.secs(now);
                        d.vx.add(secs, p.0);
                        d.vy.add(secs, p.1);
                        self.drag = Some(d);
                        if let Some(s) = &self.surface {
                            s.window.request_redraw();
                        }
                    }
                    TouchPhase::Ended => {
                        self.release_press();
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
                        if d.pending != 0.0 && self.drag_scroll(d.start, d.pending) {
                            d.scrolled = true;
                        }
                        // A stretch springs back.
                        let secs = self.secs(now);
                        if let Some((st, _)) = self.stretch.as_mut() {
                            st.release(secs);
                        }
                        let (tx, ty) = (p.0 - d.start.0, p.1 - d.start.1);
                        // The release velocity over the last 100 ms; none if
                        // the finger was held still before lifting.
                        let secs = self.secs(now);
                        let (vx, vy) = (d.vx.velocity(secs), d.vy.velocity(secs));
                        if d.paging
                            && let Some((id, _)) = &d.pager
                        {
                            self.controller.session.release_pager(id, vx);
                            if let Some(s) = &self.surface {
                                s.window.request_redraw();
                            }
                        } else if d.sliding {
                            // The row stays where the finger left it.
                        } else if d.moving_frame {
                            if self.controller.session.release_frame(vx, vy)
                                && let Some(p) = &self.platform
                            {
                                p.show_keyboard(false);
                            }
                            if let Some(s) = &self.surface {
                                s.window.request_redraw();
                            }
                        } else if !d.dragging {
                            if !d.caught {
                                self.apply(Event::Tap { x: p.0, y: p.1 });
                            }
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
                        } else if d.scrolled
                            // Content moving up is a positive scroll velocity.
                            && let Some(fling) = Fling::new(-vy)
                        {
                            self.fling = Some((Flinging::new(fling, secs), d.start));
                            if let Some(s) = &self.surface {
                                s.window.request_redraw();
                            }
                        }
                    }
                    TouchPhase::Cancelled => {
                        self.release_press();
                        let secs = self.secs(now);
                        if let Some((st, _)) = self.stretch.as_mut() {
                            st.release(secs);
                        }
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
