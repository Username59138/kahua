//! The wgpu renderer. It draws into its own offscreen texture, which the GUI
//! shows as an image and the CLI reads back as a screenshot.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::mesh::{self, Vertex};
use crate::scene::{Scene, Shape};

/// Color format of the rendered image (sRGB, ready to display or save).
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    sun_dir: [f32; 4],
    sky: [f32; 4],
    ground: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceRaw {
    model: [[f32; 4]; 4],
    normal: [[f32; 4]; 3],
    color: [f32; 4],
}

/// A colored line vertex for overlays (selection boxes, gizmos).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct LineVertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
}

impl LineVertex {
    pub fn new(p: Vec3, color: [f32; 4]) -> Self {
        Self { position: p.to_array(), color }
    }
}

/// Lines drawn on top of the scene. Use two vertices per segment.
#[derive(Default, Clone)]
pub struct Overlay {
    pub lines: Vec<LineVertex>,
}

impl Overlay {
    pub fn line(&mut self, a: Vec3, b: Vec3, color: [f32; 4]) {
        self.lines.push(LineVertex::new(a, color));
        self.lines.push(LineVertex::new(b, color));
    }

    /// The 12 edges of a box given its 8 corners (bit 0 = x, bit 1 = y, bit 2 = z).
    pub fn box_edges(&mut self, corners: &[Vec3; 8], color: [f32; 4]) {
        for i in 0..8usize {
            for bit in [1usize, 2, 4] {
                if i & bit == 0 {
                    self.line(corners[i], corners[i | bit], color);
                }
            }
        }
    }
}

struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

struct Batch {
    buffer: Option<wgpu::Buffer>,
    capacity: usize,
    count: u32,
}

impl Batch {
    fn new() -> Self {
        Self { buffer: None, capacity: 0, count: 0 }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[InstanceRaw]) {
        self.count = data.len() as u32;
        if data.is_empty() {
            return;
        }
        if self.capacity < data.len() {
            let capacity = data.len().next_power_of_two();
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kahua instances"),
                size: (capacity * std::mem::size_of::<InstanceRaw>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
        }
        queue.write_buffer(self.buffer.as_ref().unwrap(), 0, bytemuck::cast_slice(data));
    }
}

struct Target {
    size: (u32, u32),
    msaa: Option<wgpu::TextureView>,
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    /// The same texture viewed without sRGB decoding, for UIs (like egui) that
    /// blend in gamma space and expect already-encoded colors.
    display_view: wgpu::TextureView,
    depth: wgpu::TextureView,
}

pub struct Renderer {
    sample_count: u32,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    opaque_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    meshes: HashMap<Shape, Mesh>,
    opaque: HashMap<Shape, Batch>,
    transparent: HashMap<Shape, Batch>,
    /// Transparent parts kept on the CPU so they can be sorted per frame.
    transparent_parts: Vec<(Shape, Vec3, InstanceRaw)>,
    lines: Option<wgpu::Buffer>,
    lines_capacity: usize,
    target: Option<Target>,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, sample_count: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kahua shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kahua globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kahua globals layout"),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kahua globals"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kahua layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        };
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
                6 => Float32x4, 7 => Float32x4, 8 => Float32x4, 9 => Float32x4
            ],
        };
        let multisample = wgpu::MultisampleState { count: sample_count, ..Default::default() };

        let part_pipeline = |label: &str, blend: Option<wgpu::BlendState>, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_part"),
                    compilation_options: Default::default(),
                    buffers: &[Some(vertex_layout.clone()), Some(instance_layout.clone())],
                },
                // Meshes are closed, so culling is only an optimization; keep it off
                // so negative scales and odd windings never make faces vanish.
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_part"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque_pipeline = part_pipeline("kahua opaque", None, true);
        let transparent_pipeline = part_pipeline("kahua transparent", Some(wgpu::BlendState::ALPHA_BLENDING), false);

        let overlay_depth = Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        });
        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kahua lines"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                })],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::LineList, ..Default::default() },
            depth_stencil: overlay_depth.clone(),
            multisample,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_line"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kahua sky"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_sky"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: overlay_depth,
            multisample,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_sky"),
                compilation_options: Default::default(),
                targets: &[Some(COLOR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        let meshes = Shape::ALL
            .iter()
            .map(|&shape| {
                let data = mesh::build(shape);
                let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("kahua mesh vertices"),
                    contents: bytemuck::cast_slice(&data.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("kahua mesh indices"),
                    contents: bytemuck::cast_slice(&data.indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
                (shape, Mesh { vertices, indices, index_count: data.indices.len() as u32 })
            })
            .collect();

        Self {
            sample_count,
            globals,
            bind_group,
            opaque_pipeline,
            transparent_pipeline,
            line_pipeline,
            sky_pipeline,
            meshes,
            opaque: Shape::ALL.iter().map(|&s| (s, Batch::new())).collect(),
            transparent: Shape::ALL.iter().map(|&s| (s, Batch::new())).collect(),
            transparent_parts: Vec::new(),
            lines: None,
            lines_capacity: 0,
            target: None,
        }
    }

    /// Uploads the parts to draw. Call when the scene changes, not every frame.
    pub fn set_scene(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene) {
        let mut opaque: HashMap<Shape, Vec<InstanceRaw>> = HashMap::new();
        self.transparent_parts.clear();
        for part in &scene.parts {
            let model = part.model_matrix();
            let normal = Mat3::from_mat4(model).inverse().transpose();
            let raw = InstanceRaw {
                model: model.to_cols_array_2d(),
                normal: [
                    normal.x_axis.extend(0.0).to_array(),
                    normal.y_axis.extend(0.0).to_array(),
                    normal.z_axis.extend(0.0).to_array(),
                ],
                color: part.color,
            };
            if part.color[3] < 0.999 {
                self.transparent_parts.push((part.shape, part.position(), raw));
            } else {
                opaque.entry(part.shape).or_default().push(raw);
            }
        }
        for shape in Shape::ALL {
            let data = opaque.remove(&shape).unwrap_or_default();
            self.opaque.get_mut(&shape).unwrap().upload(device, queue, &data);
        }
    }

    fn ensure_target(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        let display_format = COLOR_FORMAT.remove_srgb_suffix();
        let texture = |label, format: wgpu::TextureFormat, samples, usage| {
            let view_formats: &[wgpu::TextureFormat] =
                if format == COLOR_FORMAT && samples == 1 { std::slice::from_ref(&display_format) } else { &[] };
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats,
            })
        };
        let color = texture(
            "kahua color",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let msaa = (self.sample_count > 1).then(|| {
            texture("kahua msaa", COLOR_FORMAT, self.sample_count, wgpu::TextureUsages::RENDER_ATTACHMENT)
                .create_view(&Default::default())
        });
        let depth = texture("kahua depth", DEPTH_FORMAT, self.sample_count, wgpu::TextureUsages::RENDER_ATTACHMENT)
            .create_view(&Default::default());
        let color_view = color.create_view(&Default::default());
        let display_view =
            color.create_view(&wgpu::TextureViewDescriptor { format: Some(display_format), ..Default::default() });
        self.target = Some(Target { size, msaa, color, color_view, display_view, depth });
    }

    /// Renders a frame at `size` pixels. Returns a view of the result without
    /// sRGB decoding (bytes are sRGB-encoded), ready to show in egui.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: (u32, u32),
        camera: &Camera,
        overlay: &Overlay,
    ) -> &wgpu::TextureView {
        let size = (size.0.max(1), size.1.max(1));
        self.ensure_target(device, size);
        let aspect = size.0 as f32 / size.1 as f32;

        let sky = Vec4::new(0.45, 0.62, 0.82, 1.0);
        let globals = Globals {
            view_proj: camera.view_proj(aspect).to_cols_array_2d(),
            camera_pos: camera.position.extend(1.0).to_array(),
            sun_dir: Vec3::new(0.35, 0.85, 0.4).normalize().extend(0.0).to_array(),
            sky: sky.to_array(),
            ground: [0.22, 0.2, 0.18, 1.0],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        // Sort transparent parts back to front.
        let cam = camera.position;
        self.transparent_parts.sort_by(|a, b| b.1.distance_squared(cam).total_cmp(&a.1.distance_squared(cam)));
        for shape in Shape::ALL {
            let data: Vec<InstanceRaw> =
                self.transparent_parts.iter().filter(|(s, _, _)| *s == shape).map(|(_, _, raw)| *raw).collect();
            self.transparent.get_mut(&shape).unwrap().upload(device, queue, &data);
        }

        let line_count = overlay.lines.len();
        if line_count > 0 {
            if self.lines_capacity < line_count {
                self.lines_capacity = line_count.next_power_of_two();
                self.lines = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("kahua lines"),
                    size: (self.lines_capacity * std::mem::size_of::<LineVertex>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            queue.write_buffer(self.lines.as_ref().unwrap(), 0, bytemuck::cast_slice(&overlay.lines));
        }

        let target = self.target.as_ref().unwrap();
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let (view, resolve_target) = match &target.msaa {
                Some(msaa) => (msaa, Some(&target.color_view)),
                None => (&target.color_view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kahua scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.bind_group, &[]);

            pass.set_pipeline(&self.sky_pipeline);
            pass.draw(0..3, 0..1);

            for (pipeline, batches) in
                [(&self.opaque_pipeline, &self.opaque), (&self.transparent_pipeline, &self.transparent)]
            {
                pass.set_pipeline(pipeline);
                for shape in Shape::ALL {
                    let batch = &batches[&shape];
                    let (Some(buffer), true) = (&batch.buffer, batch.count > 0) else { continue };
                    let mesh = &self.meshes[&shape];
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, buffer.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..batch.count);
                }
            }

            if line_count > 0 {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_vertex_buffer(0, self.lines.as_ref().unwrap().slice(..));
                pass.draw(0..line_count as u32, 0..1);
            }
        }
        queue.submit([encoder.finish()]);
        &self.target.as_ref().unwrap().display_view
    }

    /// The last rendered color texture.
    pub fn color_texture(&self) -> Option<&wgpu::Texture> {
        self.target.as_ref().map(|t| &t.color)
    }
}
