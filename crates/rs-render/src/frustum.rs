use glam::{Mat3, Vec3};

use crate::Segment;

/// A registered camera: centre in world space plus a world-to-camera rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub position: Vec3,
    /// Rows map world directions into camera space (x right, y down, z forward).
    pub rotation: Mat3,
    /// Horizontal field of view, radians.
    pub fov_x: f32,
    /// width / height
    pub aspect: f32,
}

impl Pose {
    /// Camera from a 35 mm-equivalent focal length (along the image's longer side).
    pub fn from_focal_35mm(position: Vec3, rotation: Mat3, focal_35mm: f32, width: f32, height: f32) -> Self {
        Self::from_normalized_focal(position, rotation, focal_35mm / 36.0, width, height)
    }

    /// Camera from RealityScan's normalised focal length: focal / 36 mm film width,
    /// applying along the image's longer side (so portrait images get it on the height).
    pub fn from_normalized_focal(
        position: Vec3,
        rotation: Mat3,
        focal: f32,
        width: f32,
        height: f32,
    ) -> Self {
        let aspect = if height > 0.0 && width > 0.0 { width / height } else { 1.5 };
        let half_long = (0.5 / focal.max(1e-4)).atan();
        let fov_x = if aspect >= 1.0 { 2.0 * half_long } else { 2.0 * (half_long.tan() * aspect).atan() };
        Self { position, rotation, fov_x, aspect }
    }

    /// Direction the camera looks along, in world space.
    pub fn forward(&self) -> Vec3 {
        self.rotation.transpose() * Vec3::Z
    }
}

/// Wireframe pyramid for a camera, `depth` units long, plus a short "up" tick on
/// the top edge so orientation is readable.
pub fn frustum_lines(pose: &Pose, depth: f32, color: [u8; 3]) -> Vec<Segment> {
    let half_w = depth * (pose.fov_x * 0.5).tan();
    let half_h = half_w / pose.aspect;
    let to_world = pose.rotation.transpose();
    let corner = |x: f32, y: f32| pose.position + to_world * Vec3::new(x, y, depth);
    let c =
        [corner(-half_w, -half_h), corner(half_w, -half_h), corner(half_w, half_h), corner(-half_w, half_h)];
    let apex = pose.position;
    let mut out = Vec::with_capacity(9);
    for p in c {
        out.push(Segment { a: apex, b: p, color });
    }
    for i in 0..4 {
        out.push(Segment { a: c[i], b: c[(i + 1) % 4], color });
    }
    // "Up" tick: image y points down in camera space, so the top edge is -half_h.
    let top_mid = pose.position + to_world * Vec3::new(0.0, -half_h, depth);
    let tick = pose.position + to_world * Vec3::new(0.0, -half_h * 1.6, depth);
    out.push(Segment { a: top_mid, b: tick, color });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_camera_looks_down_z() {
        let pose = Pose::from_focal_35mm(Vec3::ZERO, Mat3::IDENTITY, 36.0, 3.0, 2.0);
        assert!((pose.fov_x - 2.0 * 0.5f32.atan()).abs() < 1e-6);
        assert_eq!(pose.forward(), Vec3::Z);
        let lines = frustum_lines(&pose, 2.0, [255, 255, 255]);
        assert_eq!(lines.len(), 9);
        // All far corners sit at z = depth.
        assert!(lines[..4].iter().all(|s| (s.b.z - 2.0).abs() < 1e-6));
        // Horizontal half-extent is depth * tan(fov/2) = 1.0 for a 36 mm lens.
        assert!((lines[1].b.x - 1.0).abs() < 1e-5);
    }
}
