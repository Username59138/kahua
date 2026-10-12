use glam::{Mat4, Vec2, Vec3};

/// A free-flying camera like Roblox Studio's: position plus yaw/pitch.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub position: Vec3,
    /// Radians; 0 looks toward -Z (Roblox's forward).
    pub yaw: f32,
    /// Radians; positive looks up.
    pub pitch: f32,
    pub fov_y: f32,
}

impl Default for Camera {
    fn default() -> Self {
        let mut cam = Self { position: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 70f32.to_radians() };
        cam.look_at(Vec3::new(0.0, 20.0, 30.0), Vec3::ZERO);
        cam
    }
}

impl Camera {
    pub fn forward(&self) -> Vec3 {
        Vec3::new(-self.yaw.sin() * self.pitch.cos(), self.pitch.sin(), -self.yaw.cos() * self.pitch.cos())
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize_or(Vec3::X)
    }

    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward())
    }

    pub fn look_at(&mut self, eye: Vec3, target: Vec3) {
        self.position = eye;
        let d = (target - eye).normalize_or(Vec3::NEG_Z);
        self.pitch = d.y.clamp(-1.0, 1.0).asin();
        self.yaw = (-d.x).atan2(-d.z);
    }

    pub fn rotate(&mut self, delta_yaw: f32, delta_pitch: f32) {
        self.yaw += delta_yaw;
        self.pitch = (self.pitch + delta_pitch).clamp(-1.55, 1.55);
    }

    pub fn view(&self) -> Mat4 {
        look_to_rh(self.position, self.forward(), Vec3::Y)
    }

    /// Reverse-Z infinite projection: better depth precision for huge maps.
    pub fn projection(&self, aspect: f32) -> Mat4 {
        perspective_infinite_reverse_rh(self.fov_y, aspect.max(1e-3), 0.1)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.projection(aspect) * self.view()
    }

    /// World-space ray through a point in normalized device coordinates (-1..1, y up).
    pub fn ray(&self, ndc: Vec2, aspect: f32) -> (Vec3, Vec3) {
        let half_h = (self.fov_y * 0.5).tan();
        let dir = self.forward() + self.right() * ndc.x * half_h * aspect + self.up() * ndc.y * half_h;
        (self.position, dir.normalize())
    }

    /// Projects a world point to normalized device coordinates; `None` if behind the camera.
    pub fn project(&self, point: Vec3, aspect: f32) -> Option<Vec2> {
        let clip = self.view_proj(aspect) * point.extend(1.0);
        (clip.w > 1e-4).then(|| Vec2::new(clip.x / clip.w, clip.y / clip.w))
    }

    /// Moves the camera so a bounding box fills the view.
    pub fn frame(&mut self, min: Vec3, max: Vec3) {
        let center = (min + max) * 0.5;
        let radius = ((max - min).length() * 0.5).max(1.0);
        let distance = radius / (self.fov_y * 0.5).tan() * 1.1;
        let dir = {
            let f = self.forward();
            if f.y > -0.2 { Vec3::new(f.x, -0.45, f.z).normalize() } else { f }
        };
        self.look_at(center - dir * distance, center);
    }
}

/// Right-handed view matrix looking along `dir`.
pub fn look_to_rh(eye: Vec3, dir: Vec3, up: Vec3) -> Mat4 {
    let f = dir.normalize();
    let s = f.cross(up).normalize_or(Vec3::X);
    let u = s.cross(f);
    Mat4::from_cols(
        glam::Vec4::new(s.x, u.x, -f.x, 0.0),
        glam::Vec4::new(s.y, u.y, -f.y, 0.0),
        glam::Vec4::new(s.z, u.z, -f.z, 0.0),
        glam::Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
    )
}

/// Right-handed perspective with infinite far plane and reversed depth
/// (near plane maps to 1, infinity to 0).
pub fn perspective_infinite_reverse_rh(fov_y: f32, aspect: f32, near: f32) -> Mat4 {
    let f = 1.0 / (0.5 * fov_y).tan();
    Mat4::from_cols(
        glam::Vec4::new(f / aspect, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, f, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, 0.0, -1.0),
        glam::Vec4::new(0.0, 0.0, near, 0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_at_points_forward() {
        let mut cam = Camera::default();
        cam.look_at(Vec3::new(10.0, 5.0, 3.0), Vec3::new(0.0, 0.0, 0.0));
        let expected = (Vec3::ZERO - Vec3::new(10.0, 5.0, 3.0)).normalize();
        assert!(cam.forward().abs_diff_eq(expected, 1e-5));
        let ndc = cam.project(Vec3::ZERO, 1.5).unwrap();
        assert!(ndc.length() < 1e-4);
        let (_, dir) = cam.ray(Vec2::ZERO, 1.5);
        assert!(dir.abs_diff_eq(expected, 1e-5));
    }
}
