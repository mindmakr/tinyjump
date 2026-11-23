//! `wgpu` 3D renderer: supports both native window surfaces (`render_to_view`) and headless
//! offscreen RGBA8/PNG capture (`render_rgba` / `render_png`).

use crate::mesh::{build_mesh, Geometry, Vertex};
use crate::png::encode_png_rgba;
use crate::scene::{ResolvedScene, SceneGraph, MAX_LIGHTS};
use std::collections::HashMap;
use std::fmt;

const SCENE_UNIFORM_SIZE: usize = 64 + 16 + 16 + MAX_LIGHTS * 32; // 352 bytes
const MESH_UNIFORM_SIZE: usize = 64 + 64 + 16 + 16; // 160 bytes
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const DEFAULT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[derive(Debug)]
pub struct GfxError(pub String);

impl fmt::Display for GfxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GfxError {}

struct GpuMesh {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    index_count: u32,
}

pub struct GfxRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    solid_pipeline: wgpu::RenderPipeline,
    wire_pipeline: Option<wgpu::RenderPipeline>,
    scene_buf: wgpu::Buffer,
    scene_bg: wgpu::BindGroup,
    mesh_bgl: wgpu::BindGroupLayout,
    mesh_buf: wgpu::Buffer,
    mesh_bg: wgpu::BindGroup,
    mesh_stride: u32,
    mesh_capacity: usize,
    geometries: HashMap<Geometry, GpuMesh>,
    depth: Option<(u32, u32, wgpu::TextureView)>,
    adapter_name: String,
    adapter_backend: String,
}

impl GfxRenderer {
    /// Initializes a headless `wgpu` renderer, preferring hardware GPU and falling back to
    /// a software adapter (WARP / Lavapipe) when no hardware GPU is present.
    pub fn new_headless() -> Result<Self, GfxError> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            }))
        })
        .map_err(|e| GfxError(format!("no wgpu adapter available: {e}")))?;

        let info = adapter.get_info();
        let wants_wire = adapter.features().contains(wgpu::Features::POLYGON_MODE_LINE);
        let required_features = if wants_wire { wgpu::Features::POLYGON_MODE_LINE } else { wgpu::Features::empty() };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("tj-gfx"),
            required_features,
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| GfxError(format!("failed to create wgpu device: {e}")))?;

        Ok(Self::from_device_with_info(device, queue, DEFAULT_FORMAT, wants_wire, info.name, format!("{:?}", info.backend)))
    }

    /// Wraps an existing host `wgpu::Device`, `wgpu::Queue`, and surface `TextureFormat`.
    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let supports_wire = device.features().contains(wgpu::Features::POLYGON_MODE_LINE);
        Self::from_device_with_info(device, queue, format, supports_wire, "host-device".into(), "host".into())
    }

    fn from_device_with_info(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        supports_wire: bool,
        adapter_name: String,
        adapter_backend: String,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tj-gfx-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tj-gfx-scene-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(SCENE_UNIFORM_SIZE as u64),
                },
                count: None,
            }],
        });

        let mesh_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tj-gfx-mesh-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(MESH_UNIFORM_SIZE as u64),
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tj-gfx-pipeline-layout"),
            bind_group_layouts: &[&scene_bgl, &mesh_bgl],
            push_constant_ranges: &[],
        });

        let make_pipeline = |polygon_mode: wgpu::PolygonMode| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("tj-gfx-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: Vertex::STRIDE as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
                            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
                        ],
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        let solid_pipeline = make_pipeline(wgpu::PolygonMode::Fill);
        let wire_pipeline = supports_wire.then(|| make_pipeline(wgpu::PolygonMode::Line));

        let scene_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tj-gfx-scene-uniform"),
            size: SCENE_UNIFORM_SIZE as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tj-gfx-scene-bg"),
            layout: &scene_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: scene_buf.as_entire_binding() }],
        });

        let align = device.limits().min_uniform_buffer_offset_alignment.max(256);
        let mesh_stride = align_up(MESH_UNIFORM_SIZE as u32, align);
        let mesh_capacity = 64usize;
        let (mesh_buf, mesh_bg) = create_mesh_uniforms(&device, &mesh_bgl, mesh_stride, mesh_capacity);

        let mut geometries = HashMap::new();
        for g in Geometry::ALL {
            let data = build_mesh(g);
            let vbytes = data.vertex_bytes();
            let ibytes = data.index_bytes();
            let vb = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tj-gfx-vb"),
                size: vbytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vb, 0, &vbytes);
            let ib = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tj-gfx-ib"),
                size: ibytes.len() as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&ib, 0, &ibytes);
            geometries.insert(g, GpuMesh { vb, ib, index_count: data.indices.len() as u32 });
        }

        Self {
            device,
            queue,
            format,
            solid_pipeline,
            wire_pipeline,
            scene_buf,
            scene_bg,
            mesh_bgl,
            mesh_buf,
            mesh_bg,
            mesh_stride,
            mesh_capacity,
            geometries,
            depth: None,
            adapter_name,
            adapter_backend,
        }
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn adapter_backend(&self) -> &str {
        &self.adapter_backend
    }

    fn ensure_depth(&mut self, width: u32, height: u32) {
        if self.depth.as_ref().is_some_and(|(w, h, _)| *w == width && *h == height) {
            return;
        }
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tj-gfx-depth"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth = Some((width, height, view));
    }

    /// Renders `scene` into an existing `wgpu::TextureView` (surface or offscreen target).
    pub fn render_to_view(&mut self, scene: &ResolvedScene, target: &wgpu::TextureView, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        self.ensure_depth(width, height);

        if scene.meshes.len() > self.mesh_capacity {
            let new_cap = scene.meshes.len().next_power_of_two();
            let (buf, bg) = create_mesh_uniforms(&self.device, &self.mesh_bgl, self.mesh_stride, new_cap);
            self.mesh_buf = buf;
            self.mesh_bg = bg;
            self.mesh_capacity = new_cap;
        }

        let scene_bytes = pack_scene_uniform(scene);
        self.queue.write_buffer(&self.scene_buf, 0, &scene_bytes);

        if !scene.meshes.is_empty() {
            let stride = self.mesh_stride as usize;
            let mut staging = vec![0u8; scene.meshes.len() * stride];
            for (idx, m) in scene.meshes.iter().enumerate() {
                let offset = idx * stride;
                pack_mesh_uniform(m, &mut staging[offset..offset + MESH_UNIFORM_SIZE]);
            }
            self.queue.write_buffer(&self.mesh_buf, 0, &staging);
        }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("tj-gfx-enc") });
        let depth_view = &self.depth.as_ref().expect("depth created").2;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tj-gfx-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(scene.clear_color[0]),
                            g: f64::from(scene.clear_color[1]),
                            b: f64::from(scene.clear_color[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.scene_bg, &[]);
            for (idx, m) in scene.meshes.iter().enumerate() {
                let pipe = if m.wireframe {
                    self.wire_pipeline.as_ref().unwrap_or(&self.solid_pipeline)
                } else {
                    &self.solid_pipeline
                };
                pass.set_pipeline(pipe);
                pass.set_bind_group(1, &self.mesh_bg, &[idx as u32 * self.mesh_stride]);
                let gpu = &self.geometries[&m.geometry];
                pass.set_vertex_buffer(0, gpu.vb.slice(..));
                pass.set_index_buffer(gpu.ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..gpu.index_count, 0, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// Renders `scene_id` offscreen to tightly packed `width * height * 4` RGBA8 bytes.
    pub fn render_rgba(&mut self, graph: &SceneGraph, scene_id: u32, width: u32, height: u32) -> Result<Vec<u8>, GfxError> {
        let max_dim = self.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max_dim || height > max_dim {
            return Err(GfxError(format!("render dimensions {width}x{height} must be within 1..={max_dim}")));
        }
        let bgra = match self.format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
            other => return Err(GfxError(format!("render_rgba only supports 8-bit RGBA/BGRA formats, got {other:?}"))),
        };
        let unpadded = width.checked_mul(4).ok_or_else(|| GfxError("render row byte size overflow".into()))?;
        let padded = align_up_checked(unpadded, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .ok_or_else(|| GfxError("padded row byte size overflow".into()))?;
        let readback_size = u64::from(padded)
            .checked_mul(u64::from(height))
            .ok_or_else(|| GfxError("readback buffer size overflow".into()))?;
        let out_bytes = (unpadded as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| GfxError("output buffer size overflow".into()))?;

        let scene = graph.resolve(scene_id, width, height).ok_or_else(|| GfxError(format!("scene node {scene_id} not found")))?;
        let target_tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tj-gfx-offscreen"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.render_to_view(&scene, &view, width, height);

        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tj-gfx-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("tj-gfx-copy") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(height) },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        let _ = self.device.poll(wgpu::PollType::Wait);
        rx.recv()
            .map_err(|e| GfxError(format!("readback channel error: {e}")))?
            .map_err(|e| GfxError(format!("buffer map failed: {e}")))?;

        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity(out_bytes);
        for row in 0..height as usize {
            let start = row * padded as usize;
            let row_slice = &mapped[start..start + unpadded as usize];
            if bgra {
                for px in row_slice.chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            } else {
                out.extend_from_slice(row_slice);
            }
        }
        drop(mapped);
        readback.unmap();
        Ok(out)
    }

    /// Renders `scene_id` offscreen and encodes it as a valid PNG (for `/_tj/screenshot` & golden diffs).
    pub fn render_png(&mut self, graph: &SceneGraph, scene_id: u32, width: u32, height: u32) -> Result<Vec<u8>, GfxError> {
        let rgba = self.render_rgba(graph, scene_id, width, height)?;
        encode_png_rgba(width, height, &rgba).map_err(GfxError)
    }
}

fn align_up(val: u32, align: u32) -> u32 {
    let rem = val % align;
    if rem == 0 { val } else { val + (align - rem) }
}

fn align_up_checked(val: u32, align: u32) -> Option<u32> {
    let rem = val % align;
    if rem == 0 { Some(val) } else { val.checked_add(align - rem) }
}

fn create_mesh_uniforms(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    stride: u32,
    capacity: usize,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tj-gfx-mesh-uniforms"),
        size: (stride as u64) * (capacity as u64),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tj-gfx-mesh-bg"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buf,
                offset: 0,
                size: wgpu::BufferSize::new(MESH_UNIFORM_SIZE as u64),
            }),
        }],
    });
    (buf, bg)
}

fn write_vec4(dst: &mut [u8], v: [f32; 4]) {
    for (i, f) in v.iter().enumerate() {
        dst[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
}

fn pack_scene_uniform(scene: &ResolvedScene) -> [u8; SCENE_UNIFORM_SIZE] {
    let mut out = [0u8; SCENE_UNIFORM_SIZE];
    out[0..64].copy_from_slice(&scene.view_proj.to_bytes());
    write_vec4(&mut out[64..80], [scene.camera_pos.x, scene.camera_pos.y, scene.camera_pos.z, 1.0]);
    let count = scene.lights.len().min(MAX_LIGHTS);
    write_vec4(&mut out[80..96], [scene.ambient_color[0], scene.ambient_color[1], scene.ambient_color[2], count as f32]);
    for (i, l) in scene.lights.iter().take(MAX_LIGHTS).enumerate() {
        let base = 96 + i * 32;
        write_vec4(&mut out[base..base + 16], [l.pos_or_dir[0], l.pos_or_dir[1], l.pos_or_dir[2], if l.is_point { 1.0 } else { 0.0 }]);
        write_vec4(&mut out[base + 16..base + 32], [l.color[0], l.color[1], l.color[2], l.intensity]);
    }
    out
}

fn pack_mesh_uniform(m: &crate::scene::ResolvedMesh, dst: &mut [u8]) {
    dst[0..64].copy_from_slice(&m.world.to_bytes());
    dst[64..128].copy_from_slice(&m.world.normal_matrix().to_bytes());
    write_vec4(&mut dst[128..144], [m.color[0], m.color[1], m.color[2], 1.0]);
    write_vec4(&mut dst[144..160], [m.metallic, m.roughness, 0.0, 0.0]);
}
