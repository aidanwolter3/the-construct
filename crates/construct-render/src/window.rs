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
}

/// The window's state across the event loop.
pub struct App {
    controller: Controller,
    platform: Option<Box<dyn Platform>>,
    title: String,
    surface: Option<Surface>,
    cursor: (f32, f32),
    touch_start: Option<(f32, f32)>,
    touch_last: (f32, f32),
}

impl App {
    pub fn new(controller: Controller, title: &str) -> App {
        App {
            controller,
            title: title.to_owned(),
            surface: None,
            platform: None,
            cursor: (0.0, 0.0),
            touch_start: None,
            touch_last: (0.0, 0.0),
        }
    }

    pub fn with_platform(mut self, platform: Box<dyn Platform>) -> App {
        self.platform = Some(platform);
        self
    }

    /// Re-reads the platform's insets (they change on rotation, or arrive
    /// after the first frame).
    fn update_insets(&mut self) {
        let (Some(p), Some(s)) = (&self.platform, &self.surface) else {
            return;
        };
        let scale = s.window.scale_factor() as f32;
        let px = p.insets_px((s.config.width, s.config.height));
        self.controller.session.set_insets(px.map(|v| v / scale));
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
        if let Some(out) = self.controller.handle(e)
            && let Ok(json) = serde_json::to_string(&out.effects)
        {
            log(&json);
        }
        if tap && let Some(p) = &self.platform {
            p.show_keyboard(self.controller.focused.is_some());
        }
        if let Some(s) = &self.surface {
            s.window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
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
                match t.phase {
                    TouchPhase::Started => {
                        self.touch_start = Some(p);
                        self.touch_last = p;
                    }
                    TouchPhase::Moved => {
                        let dy = self.touch_last.1 - p.1;
                        self.touch_last = p;
                        self.apply(Event::Scroll { x: p.0, y: p.1, dy });
                    }
                    TouchPhase::Ended => {
                        if let Some(s) = self.touch_start.take()
                            && (s.0 - p.0).abs() < 8.0
                            && (s.1 - p.1).abs() < 8.0
                        {
                            self.apply(Event::Tap { x: p.0, y: p.1 });
                        }
                    }
                    TouchPhase::Cancelled => self.touch_start = None,
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match &event.logical_key {
                    Key::Named(NamedKey::Escape | NamedKey::BrowserBack | NamedKey::GoBack) => {
                        if self.controller.focused.take().is_some() {
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
                        self.controller.focused = None;
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
