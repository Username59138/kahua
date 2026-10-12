//! Rendering without a window, for `kahua render` and tests.

use anyhow::{Context, Result};
use image::RgbaImage;
use kahua_io::rbx_dom_weak::WeakDom;

use crate::camera::Camera;
use crate::renderer::{Overlay, Renderer};
use crate::scene::Scene;

pub struct Headless {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    adapter_name: String,
}

impl Headless {
    /// Picks any available GPU, or a software rasterizer (e.g. lavapipe) if
    /// there is none, so this works on servers and in CI.
    pub fn new() -> Result<Self> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        desc.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .context(
            "no graphics adapter found. Install a Vulkan driver; on servers \
             without a GPU, mesa's software driver works (package mesa-vulkan-drivers)",
        )?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kahua headless"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .context("failed to open the graphics device")?;
        let samples =
            adapter.get_texture_format_features(crate::renderer::COLOR_FORMAT).flags.sample_count_supported(4);
        let renderer = Renderer::new(&device, if samples { 4 } else { 1 });
        Ok(Self { device, queue, renderer, adapter_name })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn render(
        &mut self,
        scene: &Scene,
        camera: &Camera,
        overlay: &Overlay,
        width: u32,
        height: u32,
    ) -> Result<RgbaImage> {
        self.renderer.set_scene(&self.device, &self.queue, scene);
        self.renderer.render(&self.device, &self.queue, (width, height), camera, overlay);
        let texture = self.renderer.color_texture().context("nothing rendered")?;
        read_texture(&self.device, &self.queue, texture, width, height)
    }

    /// Renders a DOM with the camera framing its contents.
    pub fn screenshot(&mut self, dom: &WeakDom, camera: Option<Camera>, width: u32, height: u32) -> Result<RgbaImage> {
        let scene = Scene::extract(dom);
        let camera = camera.unwrap_or_else(|| default_camera(&scene));
        self.render(&scene, &camera, &Overlay::default(), width, height)
    }
}

/// A camera looking at the interesting part of the scene.
pub fn default_camera(scene: &Scene) -> Camera {
    let mut camera = Camera::default();
    if let Some((min, max)) = scene.focus_bounds() {
        // Don't zoom right into a tiny scene (like a lone spawn point).
        let center = (min + max) * 0.5;
        let half = ((max - min) * 0.5).max(glam::Vec3::splat(16.0));
        camera.look_at(glam::Vec3::new(1.0, 0.8, 1.2), glam::Vec3::ZERO);
        camera.frame(center - half, center + half);
    }
    camera
}

fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<RgbaImage> {
    let unpadded = width * 4;
    let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kahua readback"),
        size: (padded * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).context("GPU did not finish rendering")?;
    rx.recv()?.context("failed to read the rendered image")?;

    let data = slice.get_mapped_range().context("failed to map the rendered image")?;
    let mut pixels = Vec::with_capacity((unpadded * height) as usize);
    for row in data.chunks(padded as usize).take(height as usize) {
        pixels.extend_from_slice(&row[..unpadded as usize]);
    }
    drop(data);
    buffer.unmap();
    RgbaImage::from_raw(width, height, pixels).context("bad image size")
}

#[cfg(test)]
mod tests {
    use kahua_io::rbx_dom_weak::types::{Color3, Vector3};

    use super::*;
    use crate::assets::{AssetStore, fixtures};
    use crate::scene::Scene;

    #[test]
    fn textured_mesh_shows_its_texture_as_is() {
        let project = tempfile::tempdir().unwrap();
        // A green texture on a 4x4 quad facing the camera. The part's Color is
        // red, which must not tint a textured surface.
        fixtures::quad_gltf(project.path(), "quad.gltf", [0, 255, 0, 255]);
        let mut dom = kahua_io::template::empty();
        let workspace = kahua_io::template::workspace(&dom);
        let quad = kahua_io::template::mesh_part(
            "Quad",
            "kahua://quad.gltf",
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(0.0, 0.0, 0.0),
        )
        .with_property("Size", Vector3::new(4.0, 4.0, 1.0))
        .with_property("Color", Color3::new(1.0, 0.0, 0.0));
        dom.insert(workspace, quad);

        let mut headless = match Headless::new() {
            Ok(headless) => headless,
            Err(e) => {
                eprintln!("skipping textured render test: {e:#}");
                return;
            }
        };
        let mut assets = AssetStore::new(project.path());
        let scene = Scene::extract_with_assets(&dom, &mut assets);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        let mut camera = Camera::default();
        camera.look_at(glam::Vec3::new(0.0, 0.0, 6.0), glam::Vec3::ZERO);
        let image = headless.render(&scene, &camera, &Overlay::default(), 128, 128).unwrap();

        let [r, g, b, _] = image.get_pixel(64, 64).0;
        assert!(g > 90 && g > 2 * r && g > 2 * b, "center should be the green texture, got {:?}", [r, g, b]);
    }

    #[test]
    fn downloaded_roblox_mesh_renders_after_it_arrives() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::assets::{AssetState, fixtures};

        let project = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // A red 2x2 square facing the camera, served by a fake downloader.
        let mut dom = kahua_io::template::empty();
        let workspace = kahua_io::template::workspace(&dom);
        let part = kahua_io::template::mesh_part(
            "Remote",
            "rbxassetid://4242",
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(0.0, 0.0, 0.0),
        )
        .with_property("Size", Vector3::new(4.0, 4.0, 1.0))
        .with_property("Color", Color3::new(0.0, 0.0, 1.0));
        dom.insert(workspace, part);

        let mut headless = match Headless::new() {
            Ok(headless) => headless,
            Err(e) => {
                eprintln!("skipping downloaded mesh render test: {e:#}");
                return;
            }
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let mut assets = AssetStore::new(project.path()).with_cache_dir(cache.path());
        assets.set_downloader(fixtures::counting_downloader(
            crate::roblox::test_data::square_mesh_bytes(),
            calls.clone(),
        ));
        let mut camera = Camera::default();
        camera.look_at(glam::Vec3::new(0.0, 0.0, 6.0), glam::Vec3::ZERO);

        // Before the download finishes the part is a blue box, which is not the mesh.
        let before = Scene::extract_with_assets(&dom, &mut assets);
        assert!(before.parts[0].mesh.is_none());
        assert!(matches!(assets.mesh_state("rbxassetid://4242"), AssetState::Pending));
        fixtures::wait_for_poll(&mut assets);

        let scene = Scene::extract_with_assets(&dom, &mut assets);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert!(scene.parts[0].mesh.is_some());
        let image = headless.render(&scene, &camera, &Overlay::default(), 128, 128).unwrap();
        let [r, g, b, _] = image.get_pixel(64, 64).0;
        assert!(b > 90 && b > 2 * r && b > 2 * g, "the blue part should be drawn, got {:?}", [r, g, b]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
