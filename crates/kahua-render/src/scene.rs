//! Turning a DOM into a list of drawable parts, plus picking and bounds.

use glam::{Mat3, Mat4, Quat, Vec3};
use kahua_io::rbx_dom_weak::types::{CFrame, Matrix3, Ref, Variant, Vector3};
use kahua_io::rbx_dom_weak::{Instance, Ustr, WeakDom};
use kahua_io::reflect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    Block,
    Ball,
    Cylinder,
    Wedge,
    CornerWedge,
}

impl Shape {
    pub const ALL: [Shape; 5] = [Shape::Block, Shape::Ball, Shape::Cylinder, Shape::Wedge, Shape::CornerWedge];
}

/// One drawable part.
#[derive(Debug, Clone)]
pub struct ScenePart {
    /// The instance this part was made from.
    pub owner: Ref,
    pub shape: Shape,
    /// Position and rotation, without scale.
    pub transform: Mat4,
    /// The part's Size property.
    pub size: Vec3,
    /// The scale applied to the unit mesh (differs from size for balls and cylinders).
    pub mesh_scale: Vec3,
    /// Linear RGB + alpha (1 - Transparency).
    pub color: [f32; 4],
}

impl ScenePart {
    pub fn model_matrix(&self) -> Mat4 {
        self.transform * Mat4::from_scale(self.mesh_scale)
    }

    pub fn position(&self) -> Vec3 {
        self.transform.w_axis.truncate()
    }

    /// The 8 corners of the part's box in world space.
    pub fn corners(&self) -> [Vec3; 8] {
        let h = self.size * 0.5;
        let mut out = [Vec3::ZERO; 8];
        for (i, c) in out.iter_mut().enumerate() {
            let local = Vec3::new(
                if i & 1 == 0 { -h.x } else { h.x },
                if i & 2 == 0 { -h.y } else { h.y },
                if i & 4 == 0 { -h.z } else { h.z },
            );
            *c = self.transform.transform_point3(local);
        }
        out
    }
}

#[derive(Debug, Default, Clone)]
pub struct Scene {
    pub parts: Vec<ScenePart>,
}

pub fn cframe_to_mat4(cf: &CFrame) -> Mat4 {
    let rot = matrix3_to_mat3(&cf.orientation);
    let mut m = Mat4::from_mat3(rot);
    m.w_axis = glam::Vec4::new(cf.position.x, cf.position.y, cf.position.z, 1.0);
    m
}

/// rbx-dom stores CFrame rotation by rows (R00 R01 R02 / R10 ...).
pub fn matrix3_to_mat3(m: &Matrix3) -> Mat3 {
    Mat3::from_cols(Vec3::new(m.x.x, m.y.x, m.z.x), Vec3::new(m.x.y, m.y.y, m.z.y), Vec3::new(m.x.z, m.y.z, m.z.z))
}

pub fn mat4_to_cframe(m: &Mat4) -> CFrame {
    let (_, rot, pos) = m.to_scale_rotation_translation();
    let r = Mat3::from_quat(rot);
    let row = |i: usize| Vector3::new(r.x_axis[i], r.y_axis[i], r.z_axis[i]);
    CFrame::new(Vector3::new(pos.x, pos.y, pos.z), Matrix3::new(row(0), row(1), row(2)))
}

pub fn cframe_from_pos_rot(pos: Vec3, rot: Quat) -> CFrame {
    mat4_to_cframe(&Mat4::from_rotation_translation(rot, pos))
}

pub fn prop<'a>(inst: &'a Instance, key: &str) -> Option<&'a Variant> {
    inst.properties.get(&Ustr::from(key))
}

pub fn part_cframe(inst: &Instance) -> Option<CFrame> {
    match prop(inst, "CFrame") {
        Some(Variant::CFrame(cf)) => Some(*cf),
        _ => None,
    }
}

pub fn part_size(inst: &Instance) -> Vec3 {
    match prop(inst, "Size").or_else(|| prop(inst, "size")) {
        Some(Variant::Vector3(v)) => Vec3::new(v.x, v.y, v.z),
        _ => Vec3::new(4.0, 1.0, 2.0),
    }
}

/// The part's color in sRGB 0..1.
pub fn part_color_srgb(inst: &Instance) -> [f32; 3] {
    match prop(inst, "Color").or_else(|| prop(inst, "Color3uint8")) {
        Some(Variant::Color3(c)) => [c.r, c.g, c.b],
        Some(Variant::Color3uint8(c)) => [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0],
        _ => match prop(inst, "BrickColor") {
            Some(Variant::BrickColor(b)) => {
                let c = b.to_color3uint8();
                [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0]
            }
            _ => match reflect::default_value(&inst.class, "Color") {
                Some(Variant::Color3(c)) => [c.r, c.g, c.b],
                Some(Variant::Color3uint8(c)) => [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0],
                _ => [0.64, 0.64, 0.65],
            },
        },
    }
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn shape_of(inst: &Instance) -> Shape {
    match inst.class.as_str() {
        "WedgePart" => Shape::Wedge,
        "CornerWedgePart" => Shape::CornerWedge,
        "Part" => match prop(inst, "Shape") {
            Some(Variant::Enum(e)) => match e.to_u32() {
                0 => Shape::Ball,
                2 => Shape::Cylinder,
                3 => Shape::Wedge,
                4 => Shape::CornerWedge,
                _ => Shape::Block,
            },
            _ => Shape::Block,
        },
        _ => Shape::Block,
    }
}

/// The instance that holds the 3D world: Workspace for places, the root for models.
pub fn world_root(dom: &WeakDom) -> Ref {
    dom.root()
        .children()
        .iter()
        .copied()
        .find(|r| dom.get_by_ref(*r).is_some_and(|i| i.class == "Workspace"))
        .unwrap_or_else(|| dom.root_ref())
}

pub fn is_renderable(inst: &Instance) -> bool {
    reflect::is_base_part(&inst.class) && inst.class != "Terrain"
}

impl Scene {
    pub fn extract(dom: &WeakDom) -> Self {
        let mut parts = Vec::new();
        for inst in dom.descendants_of(world_root(dom)) {
            if !is_renderable(inst) {
                continue;
            }
            let Some(cf) = part_cframe(inst) else { continue };
            let transparency = match prop(inst, "Transparency") {
                Some(Variant::Float32(t)) => *t,
                _ => 0.0,
            };
            if transparency >= 0.999 {
                continue;
            }
            let size = part_size(inst).abs();
            let shape = shape_of(inst);
            let mesh_scale = match shape {
                Shape::Ball => Vec3::splat(size.min_element()),
                Shape::Cylinder => {
                    let d = size.y.min(size.z);
                    Vec3::new(size.x, d, d)
                }
                _ => size,
            };
            let [r, g, b] = part_color_srgb(inst);
            parts.push(ScenePart {
                owner: inst.referent(),
                shape,
                transform: cframe_to_mat4(&cf),
                size,
                mesh_scale,
                color: [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0 - transparency.clamp(0.0, 1.0)],
            });
        }
        Scene { parts }
    }

    /// World-space bounds of all parts, ignoring huge ones like baseplates.
    pub fn focus_bounds(&self) -> Option<(Vec3, Vec3)> {
        let small: Vec<&ScenePart> = self.parts.iter().filter(|p| p.size.max_element() < 256.0).collect();
        let parts: Vec<&ScenePart> = if small.is_empty() { self.parts.iter().collect() } else { small };
        bounds_of(parts.iter().flat_map(|p| p.corners()))
    }

    /// Nearest part hit by a ray, with its distance along the ray.
    pub fn pick(&self, origin: Vec3, dir: Vec3) -> Option<(usize, f32)> {
        let mut best: Option<(usize, f32)> = None;
        for (i, part) in self.parts.iter().enumerate() {
            let inv = part.transform.inverse();
            let o = inv.transform_point3(origin);
            let d = inv.transform_vector3(dir);
            let hit = match part.shape {
                Shape::Ball => ray_sphere(o, d, part.mesh_scale.x * 0.5),
                _ => ray_box(o, d, part.size * 0.5),
            };
            if let Some(t) = hit
                && best.is_none_or(|(_, bt)| t < bt)
            {
                best = Some((i, t));
            }
        }
        best
    }
}

pub fn bounds_of(points: impl Iterator<Item = Vec3>) -> Option<(Vec3, Vec3)> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut any = false;
    for p in points {
        min = min.min(p);
        max = max.max(p);
        any = true;
    }
    any.then_some((min, max))
}

fn ray_box(o: Vec3, d: Vec3, half: Vec3) -> Option<f32> {
    let inv = d.recip();
    let t1 = (-half - o) * inv;
    let t2 = (half - o) * inv;
    let tmin = t1.min(t2).max_element();
    let tmax = t1.max(t2).min_element();
    if tmax < 0.0 || tmin > tmax {
        return None;
    }
    Some(if tmin >= 0.0 { tmin } else { tmax })
}

fn ray_sphere(o: Vec3, d: Vec3, r: f32) -> Option<f32> {
    let a = d.dot(d);
    let b = 2.0 * o.dot(d);
    let c = o.dot(o) - r * r;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let t0 = (-b - sq) / (2.0 * a);
    let t1 = (-b + sq) / (2.0 * a);
    if t1 < 0.0 {
        return None;
    }
    Some(if t0 >= 0.0 { t0 } else { t1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kahua_io::template;

    #[test]
    fn extracts_and_picks_template_parts() {
        let dom = template::baseplate();
        let scene = Scene::extract(&dom);
        assert_eq!(scene.parts.len(), 2);
        // Looking straight down at the spawn hits the spawn (top at y=1), not the baseplate.
        let (i, t) = scene.pick(Vec3::new(0.0, 50.0, 0.0), Vec3::NEG_Y).unwrap();
        assert_eq!(dom.get_by_ref(scene.parts[i].owner).unwrap().name, "SpawnLocation");
        assert!((t - 49.0).abs() < 1e-3);
    }

    #[test]
    fn cframe_roundtrip() {
        let rot = Quat::from_euler(glam::EulerRot::YXZ, 0.3, 0.2, 0.1);
        let cf = cframe_from_pos_rot(Vec3::new(1.0, 2.0, 3.0), rot);
        let m = cframe_to_mat4(&cf);
        let (_, r2, p2) = m.to_scale_rotation_translation();
        assert!(r2.abs_diff_eq(rot, 1e-5) || r2.abs_diff_eq(-rot, 1e-5));
        assert!(p2.abs_diff_eq(Vec3::new(1.0, 2.0, 3.0), 1e-5));
    }
}
