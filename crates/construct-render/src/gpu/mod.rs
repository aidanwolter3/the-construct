//! The wgpu pipeline: instanced SDF quads (`quad.wgsl`) and glyphon text,
//! drawn into any texture view: an offscreen [`HeadlessSurface`] or a
//! window's surface (`crate::window`, Android).
//!
//! Quads and text interleave in paint order. Draws are grouped into as few
//! layers as overlap allows: within a layer quads are drawn first, then
//! text, and a draw goes to a later layer only when it overlaps an earlier
//! draw of the other kind.

use glyphon::{
    Cache, ColorMode, ContentType, CustomGlyph, RasterizedCustomGlyph, Resolution, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer,
};
use image::RgbaImage;
use wgpu::util::DeviceExt;

use crate::scene::{Draw, IconRun, QuadInstance, Scene, TextRun};
use crate::style::Type;
use crate::text::TextEngine;

/// Why the GPU couldn't draw.
#[derive(Debug)]
pub struct GpuError(pub String);

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GpuError {}

/// A device and queue: the first adapter wgpu finds (a real GPU, else a
/// software one such as lavapipe).
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// A headless device (no surface to be compatible with).
    pub async fn headless() -> Result<Gpu, GpuError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        Gpu::with_instance(instance, None).await
    }

    pub async fn with_instance(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Gpu, GpuError> {
        let mut adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: surface,
                ..Default::default()
            })
            .await;
        if adapter.is_err() {
            adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: true,
                    compatible_surface: surface,
                    ..Default::default()
                })
                .await;
        }
        let adapter = adapter.map_err(|e| GpuError(format!("no GPU adapter: {e}")))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("construct-render"),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError(format!("no GPU device: {e}")))?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// The adapter's name and backend, for logs.
    pub fn describe(&self) -> String {
        let i = self.adapter.get_info();
        format!("{} ({:?})", i.name, i.backend)
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    size: [f32; 2],
    scale: f32,
    _pad: f32,
}

const QUAD_ATTRS: [wgpu::VertexAttribute; 9] = wgpu::vertex_attr_array![
    0 => Float32x4, // rect
    1 => Float32x4, // bg_color
    2 => Float32x4, // border_color
    3 => Float32x4, // radii
    4 => Float32,   // border_width
    5 => Float32x2, // shadow_offset
    6 => Float32x2, // shadow_params
    7 => Float32x4, // shadow_color
    8 => Float32x4, // clip
];

/// Draws scenes into texture views of one format.
pub struct Renderer {
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: Option<(wgpu::Buffer, u64)>,
    atlas: TextAtlas,
    viewport: glyphon::Viewport,
    texts: Vec<TextRenderer>,
    swash: SwashCache,
}

/// One layer: a range of quad instances, then text runs.
struct Layer {
    quads: std::ops::Range<u32>,
    texts: Vec<usize>,
}

fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

/// Paint-order layers (see the module doc): (quads per layer, texts per
/// layer), each draw by index into its kind.
type Layers = (Vec<Vec<QuadInstance>>, Vec<Vec<TextRun>>, Vec<Vec<IconRun>>);

fn layers(scene: &Scene) -> Layers {
    let mut quads: Vec<Vec<QuadInstance>> = vec![Vec::new()];
    let mut texts: Vec<Vec<TextRun>> = vec![Vec::new()];
    let mut icons: Vec<Vec<IconRun>> = vec![Vec::new()];
    // (extent, layer) of placed quads and texts.
    let mut placed_q: Vec<([f32; 4], usize)> = Vec::new();
    let mut placed_t: Vec<([f32; 4], usize)> = Vec::new();
    for d in &scene.draws {
        match d {
            Draw::Quad(q) => {
                let e = q.extent();
                let l = placed_t
                    .iter()
                    .filter(|(r, _)| overlaps(*r, e))
                    .map(|(_, l)| l + 1)
                    .chain(
                        placed_q
                            .iter()
                            .filter(|(r, _)| overlaps(*r, e))
                            .map(|(_, l)| *l),
                    )
                    .max()
                    .unwrap_or(0);
                while quads.len() <= l {
                    quads.push(Vec::new());
                    texts.push(Vec::new());
                    icons.push(Vec::new());
                }
                quads[l].push(*q);
                placed_q.push((e, l));
            }
            Draw::Text(t) => {
                let e = t.rect;
                let l = placed_q
                    .iter()
                    .chain(placed_t.iter())
                    .filter(|(r, _)| overlaps(*r, e))
                    .map(|(_, l)| *l)
                    .max()
                    .unwrap_or(0);
                while texts.len() <= l {
                    quads.push(Vec::new());
                    texts.push(Vec::new());
                    icons.push(Vec::new());
                }
                texts[l].push(t.clone());
                placed_t.push((e, l));
            }
            // Icons go with text: drawn after the quads of their layer.
            Draw::Icon(i) => {
                let e = i.rect;
                let l = placed_q
                    .iter()
                    .chain(placed_t.iter())
                    .filter(|(r, _)| overlaps(*r, e))
                    .map(|(_, l)| *l)
                    .max()
                    .unwrap_or(0);
                while icons.len() <= l {
                    quads.push(Vec::new());
                    texts.push(Vec::new());
                    icons.push(Vec::new());
                }
                icons[l].push(i.clone());
                placed_t.push((e, l));
            }
        }
    }
    (quads, texts, icons)
}

impl Renderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Renderer {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("quad.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quads"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quads"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &QUAD_ATTRS,
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            cache: None,
            multiview_mask: None,
        });
        let cache = Cache::new(device);
        let viewport = glyphon::Viewport::new(device, &cache);
        // Colours are blended as stored (sRGB values), as the CPU path does.
        let color_mode = if format.is_srgb() {
            ColorMode::Accurate
        } else {
            ColorMode::Web
        };
        let atlas = TextAtlas::with_color_mode(device, &gpu.queue, &cache, format, color_mode);
        Renderer {
            format,
            pipeline,
            globals,
            bind_group,
            instances: None,
            atlas,
            viewport,
            texts: Vec::new(),
            swash: SwashCache::new(),
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Draws `scene` into `view` (sized `px`), clearing it to the scene's
    /// background.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        view: &wgpu::TextureView,
        px: (u32, u32),
        scene: &Scene,
        text: &mut TextEngine,
    ) -> Result<(), GpuError> {
        let (device, queue) = (&gpu.device, &gpu.queue);
        let s = scene.scale;
        queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&Globals {
                size: [px.0 as f32, px.1 as f32],
                scale: s,
                _pad: 0.0,
            }),
        );
        let (quad_layers, text_layers, icon_layers) = layers(scene);
        let mut all: Vec<QuadInstance> = Vec::new();
        let mut plan: Vec<Layer> = Vec::new();
        for (i, qs) in quad_layers.iter().enumerate() {
            let start = all.len() as u32;
            all.extend_from_slice(qs);
            plan.push(Layer {
                quads: start..all.len() as u32,
                texts: (0..text_layers[i].len() + icon_layers[i].len()).collect(),
            });
        }
        if !all.is_empty() {
            let bytes: &[u8] = bytemuck::cast_slice(&all);
            let fits = self
                .instances
                .as_ref()
                .is_some_and(|(_, cap)| *cap >= bytes.len() as u64);
            if fits {
                queue.write_buffer(&self.instances.as_ref().expect("checked").0, 0, bytes);
            } else {
                let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("quad instances"),
                    contents: bytes,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                });
                self.instances = Some((buf, bytes.len() as u64));
            }
        }
        self.viewport.update(
            queue,
            Resolution {
                width: px.0,
                height: px.1,
            },
        );
        while self.texts.len() < text_layers.len() {
            self.texts.push(TextRenderer::new(
                &mut self.atlas,
                device,
                wgpu::MultisampleState::default(),
                None,
            ));
        }
        for (li, runs) in text_layers.iter().enumerate() {
            // Shape at the frame's scale, as the CPU path does.
            let buffers: Vec<glyphon::Buffer> = runs
                .iter()
                .map(|t| {
                    let ty = Type {
                        face: t.face,
                        size: t.size,
                        line_height: t.line_height,
                    };
                    text.buffer(&t.text, ty, Some(t.rect[2] + 0.5), s)
                })
                .collect();
            let areas = runs.iter().zip(&buffers).map(|(t, b)| {
                let [cx, cy, cw, ch] = t.clip.map(|v| v * s);
                let bounds = TextBounds {
                    left: cx.max(-1.0e6) as i32,
                    top: cy.max(-1.0e6) as i32,
                    right: (cx + cw).min(1.0e6) as i32,
                    bottom: (cy + ch).min(1.0e6) as i32,
                };
                let c = t.color.to_u8();
                TextArea {
                    buffer: b,
                    left: (t.rect[0] * s).round(),
                    top: (t.rect[1] * s).round(),
                    scale: 1.0,
                    bounds,
                    default_color: glyphon::Color::rgba(c[0], c[1], c[2], c[3]),
                    custom_glyphs: &[],
                }
            });
            // Icons are custom glyphs: one area each (each has its own clip),
            // over an empty buffer, rasterized from their paths on demand.
            let glyphs: Vec<[CustomGlyph; 1]> = icon_layers[li]
                .iter()
                .map(|i| {
                    let c = i.color.to_u8();
                    [CustomGlyph {
                        id: crate::icons::id(&i.name).unwrap_or(0),
                        left: (i.rect[0] * s).round(),
                        top: (i.rect[1] * s).round(),
                        width: (i.rect[2] * s).round(),
                        height: (i.rect[3] * s).round(),
                        color: Some(glyphon::Color::rgba(c[0], c[1], c[2], c[3])),
                        snap_to_physical_pixel: true,
                        metadata: 0,
                    }]
                })
                .collect();
            let empty = glyphon::Buffer::new(text.font_system(), glyphon::Metrics::new(1.0, 1.0));
            let icon_areas = icon_layers[li].iter().zip(&glyphs).map(|(i, g)| {
                let [cx, cy, cw, ch] = i.clip.map(|v| v * s);
                TextArea {
                    buffer: &empty,
                    left: 0.0,
                    top: 0.0,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: cx.max(-1.0e6) as i32,
                        top: cy.max(-1.0e6) as i32,
                        right: (cx + cw).min(1.0e6) as i32,
                        bottom: (cy + ch).min(1.0e6) as i32,
                    },
                    default_color: glyphon::Color::rgb(0, 0, 0),
                    custom_glyphs: g,
                }
            });
            self.texts[li]
                .prepare_with_custom(
                    device,
                    queue,
                    text.font_system(),
                    &mut self.atlas,
                    &self.viewport,
                    areas.chain(icon_areas),
                    &mut self.swash,
                    |req| {
                        let name = crate::icons::name(req.id)?;
                        let size = req.width.min(req.height) as u32;
                        let mask = crate::icons::mask(name, size)?;
                        // A square mask in a request that isn't square: pad it.
                        let (w, h) = (req.width as usize, req.height as usize);
                        let mut data = vec![0u8; w * h];
                        for y in 0..size as usize {
                            data[y * w..y * w + size as usize]
                                .copy_from_slice(&mask[y * size as usize..(y + 1) * size as usize]);
                        }
                        Some(RasterizedCustomGlyph {
                            data,
                            content_type: ContentType::Mask,
                        })
                    },
                )
                .map_err(|e| GpuError(format!("text: {e}")))?;
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let bg = scene.background;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: (bg[0] * bg[3]) as f64,
                            g: (bg[1] * bg[3]) as f64,
                            b: (bg[2] * bg[3]) as f64,
                            a: bg[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for (li, layer) in plan.iter().enumerate() {
                if !layer.quads.is_empty()
                    && let Some((buf, _)) = &self.instances
                {
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &self.bind_group, &[]);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..6, layer.quads.clone());
                }
                if !layer.texts.is_empty() {
                    self.texts[li]
                        .render(&self.atlas, &self.viewport, &mut pass)
                        .map_err(|e| GpuError(format!("text: {e}")))?;
                }
            }
        }
        queue.submit(Some(encoder.finish()));
        self.atlas.trim();
        Ok(())
    }
}

/// An offscreen render target with CPU readback.
pub struct HeadlessSurface {
    pub gpu: Gpu,
    renderer: Renderer,
    texture: wgpu::Texture,
    readback: wgpu::Buffer,
    px: (u32, u32),
    padded_row: u32,
}

impl HeadlessSurface {
    /// A target of `width` × `height` pixels on the first adapter found.
    pub async fn new(width: u32, height: u32) -> Result<HeadlessSurface, GpuError> {
        let gpu = Gpu::headless().await?;
        Ok(HeadlessSurface::on(gpu, width, height))
    }

    pub fn on(gpu: Gpu, width: u32, height: u32) -> HeadlessSurface {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let renderer = Renderer::new(&gpu, format);
        let (texture, readback, padded_row) = Self::targets(&gpu, width, height, format);
        HeadlessSurface {
            gpu,
            renderer,
            texture,
            readback,
            px: (width, height),
            padded_row,
        }
    }

    fn targets(
        gpu: &Gpu,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
    ) -> (wgpu::Texture, wgpu::Buffer, u32) {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = (w * 4).div_ceil(align) * align;
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: padded_row as u64 * h as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        (texture, readback, padded_row)
    }

    /// Resizes the target (a no-op at the same size).
    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != self.px {
            let (t, r, p) = Self::targets(&self.gpu, width, height, self.renderer.format());
            self.texture = t;
            self.readback = r;
            self.padded_row = p;
            self.px = (width, height);
        }
    }

    /// Draws `scene` (resizing to its pixel size) and reads the frame back.
    pub fn render(&mut self, scene: &Scene, text: &mut TextEngine) -> Result<RgbaImage, GpuError> {
        let w = (scene.width * scene.scale).round().max(1.0) as u32;
        let h = (scene.height * scene.scale).round().max(1.0) as u32;
        self.resize(w, h);
        let view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.renderer
            .render(&self.gpu, &view, self.px, scene, text)?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit(Some(encoder.finish()));
        let slice = self.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError(format!("poll: {e}")))?;
        rx.recv()
            .map_err(|e| GpuError(e.to_string()))?
            .map_err(|e| GpuError(format!("map: {e}")))?;
        let mut img = RgbaImage::new(w, h);
        {
            let data = slice
                .get_mapped_range()
                .map_err(|e| GpuError(format!("map: {e}")))?;
            for y in 0..h as usize {
                let row = &data[y * self.padded_row as usize..][..w as usize * 4];
                img.as_mut()[y * w as usize * 4..][..w as usize * 4].copy_from_slice(row);
            }
        }
        self.readback.unmap();
        Ok(img)
    }
}
