//! Unit meshes for the basic part shapes, centered on the origin in a 1x1x1 box.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::scene::Shape;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// Texture coordinates (0 for the basic shapes, which have no texture).
    pub uv: [f32; 2],
}

#[derive(Default)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl MeshData {
    fn push(&mut self, p: Vec3, n: Vec3) -> u32 {
        self.vertices.push(Vertex { position: p.to_array(), normal: n.to_array(), uv: [0.0, 0.0] });
        (self.vertices.len() - 1) as u32
    }

    /// A flat-shaded triangle, wound counter-clockwise when seen from outside.
    /// Shapes are convex and centered, so "outside" is away from the origin.
    fn flat_tri(&mut self, a: Vec3, b: Vec3, c: Vec3) {
        let mut n = (b - a).cross(c - a).normalize();
        let (b, c) = if n.dot(a + b + c) < 0.0 {
            n = -n;
            (c, b)
        } else {
            (b, c)
        };
        let i = self.push(a, n);
        let j = self.push(b, n);
        let k = self.push(c, n);
        self.indices.extend([i, j, k]);
    }

    fn flat_quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3) {
        self.flat_tri(a, b, c);
        self.flat_tri(a, c, d);
    }
}

pub fn build(shape: Shape) -> MeshData {
    match shape {
        Shape::Block => block(),
        Shape::Ball => sphere(32, 20),
        Shape::Cylinder => cylinder(32),
        Shape::Wedge => wedge(),
        Shape::CornerWedge => corner_wedge(),
    }
}

fn corner(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x * 0.5, y * 0.5, z * 0.5)
}

fn block() -> MeshData {
    let mut m = MeshData::default();
    let c = |x, y, z| corner(x, y, z);
    m.flat_quad(c(1., -1., -1.), c(1., 1., -1.), c(1., 1., 1.), c(1., -1., 1.));
    m.flat_quad(c(-1., -1., -1.), c(-1., -1., 1.), c(-1., 1., 1.), c(-1., 1., -1.));
    m.flat_quad(c(-1., 1., -1.), c(-1., 1., 1.), c(1., 1., 1.), c(1., 1., -1.));
    m.flat_quad(c(-1., -1., -1.), c(1., -1., -1.), c(1., -1., 1.), c(-1., -1., 1.));
    m.flat_quad(c(-1., -1., 1.), c(1., -1., 1.), c(1., 1., 1.), c(-1., 1., 1.));
    m.flat_quad(c(-1., -1., -1.), c(-1., 1., -1.), c(1., 1., -1.), c(1., -1., -1.));
    m
}

/// Roblox wedge: full bottom and back (+Z) faces, slope facing front (-Z).
fn wedge() -> MeshData {
    let mut m = MeshData::default();
    let c = |x, y, z| corner(x, y, z);
    m.flat_quad(c(-1., -1., -1.), c(1., -1., -1.), c(1., -1., 1.), c(-1., -1., 1.)); // bottom
    m.flat_quad(c(-1., -1., 1.), c(1., -1., 1.), c(1., 1., 1.), c(-1., 1., 1.)); // back
    m.flat_quad(c(-1., -1., -1.), c(-1., 1., 1.), c(1., 1., 1.), c(1., -1., -1.)); // slope
    m.flat_tri(c(1., -1., -1.), c(1., -1., 1.), c(1., 1., 1.)); // sides
    m.flat_tri(c(-1., -1., -1.), c(-1., -1., 1.), c(-1., 1., 1.));
    m
}

/// Roblox corner wedge: full bottom, peak above the (+X, -Z)... corner.
fn corner_wedge() -> MeshData {
    let mut m = MeshData::default();
    let c = |x, y, z| corner(x, y, z);
    let top = c(1., 1., -1.);
    let (a, b, cc, d) = (c(-1., -1., -1.), c(1., -1., -1.), c(1., -1., 1.), c(-1., -1., 1.));
    m.flat_quad(a, b, cc, d);
    m.flat_tri(a, b, top);
    m.flat_tri(b, cc, top);
    m.flat_tri(cc, d, top);
    m.flat_tri(d, a, top);
    m
}

fn sphere(segments: u32, rings: u32) -> MeshData {
    let mut m = MeshData::default();
    for ring in 0..=rings {
        let v = ring as f32 / rings as f32;
        let phi = v * std::f32::consts::PI;
        for seg in 0..=segments {
            let u = seg as f32 / segments as f32;
            let theta = u * std::f32::consts::TAU;
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            m.push(n * 0.5, n);
        }
    }
    let stride = segments + 1;
    for ring in 0..rings {
        for seg in 0..segments {
            let a = ring * stride + seg;
            let b = a + stride;
            m.indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    m
}

/// Cylinder along the X axis, like Roblox's cylinder shape.
fn cylinder(segments: u32) -> MeshData {
    let mut m = MeshData::default();
    let ring = |i: u32| {
        let t = i as f32 / segments as f32 * std::f32::consts::TAU;
        Vec3::new(0.0, t.cos(), t.sin())
    };
    for i in 0..segments {
        let (n0, n1) = (ring(i), ring(i + 1));
        let base = m.vertices.len() as u32;
        m.push(Vec3::new(-0.5, 0.0, 0.0) + n0 * 0.5, n0);
        m.push(Vec3::new(0.5, 0.0, 0.0) + n0 * 0.5, n0);
        m.push(Vec3::new(0.5, 0.0, 0.0) + n1 * 0.5, n1);
        m.push(Vec3::new(-0.5, 0.0, 0.0) + n1 * 0.5, n1);
        m.indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
        for x in [-0.5f32, 0.5] {
            m.flat_tri(
                Vec3::new(x, 0.0, 0.0) * 1.0,
                Vec3::new(x, 0.0, 0.0) + n0 * 0.5,
                Vec3::new(x, 0.0, 0.0) + n1 * 0.5,
            );
        }
    }
    m
}
