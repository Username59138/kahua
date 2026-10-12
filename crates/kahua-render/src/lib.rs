//! Kahua's 3D renderer (wgpu). Works in a window and headless.

pub mod camera;
pub mod headless;
pub mod mesh;
pub mod renderer;
pub mod scene;

pub use camera::Camera;
pub use glam;
pub use headless::{Headless, default_camera};
pub use renderer::{COLOR_FORMAT, LineVertex, Overlay, Renderer};
pub use scene::{Scene, ScenePart, Shape};
pub use wgpu;
