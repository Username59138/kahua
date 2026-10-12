//! The wgpu renderer. It draws into its own offscreen texture, which the GUI
//! shows as an image and the CLI reads back as a screenshot.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::assets::{LoadedImage, LoadedMesh};
use crate::camera::Camera;
use crate::mesh::{self, Vertex};
use crate::scene::{Scene, ScenePart, Shape};

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

/// What a draw group renders: a basic shape, or one primitive of a loaded mesh
/// with a texture. Meshes and textures are identified by their `Arc` address,
/// which stays unique while the renderer holds the `Arc`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum GroupKey {
    Shape(Shape),
    Asset { mesh: usize, primitive: usize, texture: usize },
}

struct GpuMesh {
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

/// Everything drawn with one mesh and one texture. Opaque and transparent
/// instances are kept apart; the transparent ones are sorted every frame.
struct Group {
    mesh: Arc<GpuMesh>,
    texture: Arc<wgpu::BindGroup>,
    opaque: Batch,
    transparent: Batch,
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
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// A 1x1 white texture, used when a draw has no texture.
    white: Option<Arc<wgpu::BindGroup>>,
    opaque_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    shape_meshes: HashMap<Shape, Arc<GpuMesh>>,
    /// Uploaded primitives of loaded meshes, by (mesh address, primitive index).
    asset_meshes: HashMap<(usize, usize), Arc<GpuMesh>>,
    /// The loaded meshes behind `asset_meshes`. Holding them keeps their addresses valid.
    mesh_sources: HashMap<usize, Arc<LoadedMesh>>,
    /// Uploaded textures by address, with the image that keeps the address valid.
    textures: HashMap<usize, (Arc<wgpu::BindGroup>, Arc<LoadedImage>)>,
    groups: HashMap<GroupKey, Group>,
    /// Transparent instances kept on the CPU so they can be sorted per frame.
    transparent_items: Vec<(GroupKey, Vec3, InstanceRaw)>,
    lines: Option<wgpu::Buffer>,
    lines_capacity: usize,
    target: Option<Target>,
}

fn create_gpu_mesh(device: &wgpu::Device, vertices: &[Vertex], indices: &[u32]) -> GpuMesh {
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("kahua mesh vertices"),
        contents: bytemuck::cast_slice(vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("kahua mesh indices"),
        contents: bytemuck::cast_slice(indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    GpuMesh { vertices: vertex_buffer, indices: index_buffer, index_count: indices.len() as u32 }
}

fn instance_raw(model: Mat4, color: [f32; 4]) -> InstanceRaw {
    let normal = Mat3::from_mat4(model).inverse().transpose();
    InstanceRaw {
        model: model.to_cols_array_2d(),
        normal: [
            normal.x_axis.extend(0.0).to_array(),
            normal.y_axis.extend(0.0).to_array(),
            normal.z_axis.extend(0.0).to_array(),
        ],
        color,
    }
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
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kahua texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kahua sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        // Parts use both groups; the sky and overlays only need the globals.
        let part_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kahua part layout"),
            bind_group_layouts: &[Some(&bind_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let overlay_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kahua overlay layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
        };
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4,
                7 => Float32x4, 8 => Float32x4, 9 => Float32x4, 10 => Float32x4
            ],
        };
        let multisample = wgpu::MultisampleState { count: sample_count, ..Default::default() };

        let part_pipeline = |label: &str, blend: Option<wgpu::BlendState>, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&part_layout),
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
            layout: Some(&overlay_layout),
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
            layout: Some(&overlay_layout),
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

        let shape_meshes = Shape::ALL
            .iter()
            .map(|&shape| {
                let data = mesh::build(shape);
                (shape, Arc::new(create_gpu_mesh(device, &data.vertices, &data.indices)))
            })
            .collect();

        Self {
            sample_count,
            globals,
            bind_group,
            texture_layout,
            sampler,
            white: None,
            opaque_pipeline,
            transparent_pipeline,
            line_pipeline,
            sky_pipeline,
            shape_meshes,
            asset_meshes: HashMap::new(),
            mesh_sources: HashMap::new(),
            textures: HashMap::new(),
            groups: HashMap::new(),
            transparent_items: Vec::new(),
            lines: None,
            lines_capacity: 0,
            target: None,
        }
    }

    /// Uploads a texture with its mip levels (`(width, height, RGBA8 pixels)`,
    /// largest first) and returns the bind group that samples it.
    fn upload_texture(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        levels: &[(u32, u32, &[u8])],
    ) -> wgpu::BindGroup {
        let (width, height, _) = levels[0];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kahua texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, &(w, h, pixels)) in levels.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        let view = texture.create_view(&Default::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kahua texture"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// Uploads a loaded mesh primitive and its texture, unless already uploaded.
    fn ensure_asset(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mesh: &Arc<LoadedMesh>,
        primitive: usize,
        texture: Option<&Arc<LoadedImage>>,
    ) {
        let mesh_key = Arc::as_ptr(mesh) as usize;
        if let Entry::Vacant(entry) = self.asset_meshes.entry((mesh_key, primitive)) {
            let p = &mesh.primitives[primitive];
            entry.insert(Arc::new(create_gpu_mesh(device, &p.vertices, &p.indices)));
            self.mesh_sources.entry(mesh_key).or_insert_with(|| mesh.clone());
        }
        if let Some(image) = texture {
            let key = Arc::as_ptr(image) as usize;
            if !self.textures.contains_key(&key) {
                let levels: Vec<(u32, u32, &[u8])> =
                    image.levels.iter().map(|l| (l.width, l.height, l.rgba.as_slice())).collect();
                let bind_group = self.upload_texture(device, queue, &levels);
                self.textures.insert(key, (Arc::new(bind_group), image.clone()));
            }
        }
    }

    fn new_group(&self, key: GroupKey) -> Group {
        let white = self.white.clone().expect("white texture is created by set_scene");
        let (mesh, texture) = match key {
            GroupKey::Shape(shape) => (self.shape_meshes[&shape].clone(), white),
            GroupKey::Asset { mesh, primitive, texture } => {
                let texture = if texture == 0 { white } else { self.textures[&texture].0.clone() };
                (self.asset_meshes[&(mesh, primitive)].clone(), texture)
            }
        };
        Group { mesh, texture, opaque: Batch::new(), transparent: Batch::new() }
    }

    /// Uploads the parts to draw. Call when the scene changes, not every frame.
    pub fn set_scene(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene) {
        if self.white.is_none() {
            let white = [255u8; 4];
            self.white = Some(Arc::new(self.upload_texture(device, queue, &[(1, 1, &white[..])])));
        }

        let mut opaque: HashMap<GroupKey, Vec<InstanceRaw>> = HashMap::new();
        let mut transparent: Vec<(GroupKey, Vec3, InstanceRaw)> = Vec::new();
        let mut used_assets = HashSet::new();
        let mut used_meshes = HashSet::new();
        let mut used_textures = HashSet::new();

        for part in &scene.parts {
            let model = part.model_matrix();
            let mut add = |key: GroupKey, color: [f32; 4], part: &ScenePart| {
                let raw = instance_raw(model, color);
                if color[3] < 0.999 {
                    transparent.push((key, part.position(), raw));
                } else {
                    opaque.entry(key).or_default().push(raw);
                }
            };
            match &part.mesh {
                None => add(GroupKey::Shape(part.shape), part.color, part),
                Some(mesh_ref) => {
                    for (index, primitive) in mesh_ref.mesh.primitives.iter().enumerate() {
                        if primitive.vertices.is_empty() || primitive.indices.is_empty() {
                            continue;
                        }
                        let texture = mesh_ref.texture.as_ref().or(primitive.texture.as_ref());
                        self.ensure_asset(device, queue, &mesh_ref.mesh, index, texture);
                        let mesh_key = Arc::as_ptr(&mesh_ref.mesh) as usize;
                        used_assets.insert((mesh_key, index));
                        used_meshes.insert(mesh_key);
                        let texture_key = texture.map_or(0, |t| {
                            used_textures.insert(Arc::as_ptr(t) as usize);
                            Arc::as_ptr(t) as usize
                        });
                        // A texture is shown as it is; Color only tints untextured surfaces.
                        let color = if texture.is_some() { [1.0, 1.0, 1.0, part.color[3]] } else { part.color };
                        let key = GroupKey::Asset { mesh: mesh_key, primitive: index, texture: texture_key };
                        add(key, color, part);
                    }
                }
            }
        }

        self.asset_meshes.retain(|k, _| used_assets.contains(k));
        self.mesh_sources.retain(|k, _| used_meshes.contains(k));
        self.textures.retain(|k, _| used_textures.contains(k));

        let keys: HashSet<GroupKey> = opaque.keys().copied().chain(transparent.iter().map(|(k, _, _)| *k)).collect();
        let mut groups = HashMap::with_capacity(keys.len());
        for key in keys {
            // Reuse the batch buffers of groups that were drawn before.
            let mut group = match self.groups.remove(&key) {
                Some(group) => group,
                None => self.new_group(key),
            };
            let data: &[InstanceRaw] = opaque.get(&key).map_or(&[], |v| v.as_slice());
            group.opaque.upload(device, queue, data);
            groups.insert(key, group);
        }
        self.groups = groups;
        self.transparent_items = transparent;
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

        // Sort transparent instances back to front, then upload them per group.
        let cam = camera.position;
        self.transparent_items.sort_by(|a, b| b.1.distance_squared(cam).total_cmp(&a.1.distance_squared(cam)));
        for (key, group) in self.groups.iter_mut() {
            let data: Vec<InstanceRaw> =
                self.transparent_items.iter().filter(|(k, _, _)| k == key).map(|(_, _, raw)| *raw).collect();
            group.transparent.upload(device, queue, &data);
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

            for (pipeline, transparent) in [(&self.opaque_pipeline, false), (&self.transparent_pipeline, true)] {
                pass.set_pipeline(pipeline);
                for group in self.groups.values() {
                    let batch = if transparent { &group.transparent } else { &group.opaque };
                    let (Some(buffer), true) = (&batch.buffer, batch.count > 0) else { continue };
                    pass.set_bind_group(1, &*group.texture, &[]);
                    pass.set_vertex_buffer(0, group.mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, buffer.slice(..));
                    pass.set_index_buffer(group.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..group.mesh.index_count, 0, 0..batch.count);
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
