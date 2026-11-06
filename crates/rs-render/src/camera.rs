use glam::{Mat4, Quat, Vec3, Vec4};

/// Turntable camera orbiting a target point.
#[derive(Debug, Clone, PartialEq)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    /// Rotation around `up`, radians.
    pub yaw: f32,
    /// Elevation above the horizontal plane, radians.
    pub pitch: f32,
    pub up: Vec3,
    pub fov_y: f32,
}

/// Orbit sensitivity: a full turn in about 840 pixels of drag.
pub const ORBIT_RADIANS_PER_PIXEL: f32 = 0.0075;
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.01;

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 5.0,
            yaw: 0.6,
            pitch: 0.5,
            up: Vec3::Y,
            fov_y: 50f32.to_radians(),
        }
    }
}

impl OrbitCamera {
    /// Frame a bounding box: look at its centre from far enough to see all of it.
    pub fn framing(min: Vec3, max: Vec3, up: Vec3) -> Self {
        let center = (min + max) * 0.5;
        let radius = ((max - min).length() * 0.5).max(1e-3);
        let mut cam = Self { target: center, up, ..Self::default() };
        cam.distance = radius / (cam.fov_y * 0.5).sin() * 1.1;
        cam
    }

    pub fn eye(&self) -> Vec3 {
        let (forward, right) = self.basis();
        // Spherical coordinates: yaw around `up`, then elevation towards it. (An earlier
        // version scaled the elevation by cos(yaw) too, which made the pitch fade and the
        // camera creep closer as the view turned sideways.)
        let horizontal = forward * self.yaw.cos() + right * self.yaw.sin();
        let dir = horizontal * self.pitch.cos() + self.up * self.pitch.sin();
        self.target + dir * self.distance
    }

    /// Reference directions perpendicular to `up`.
    fn basis(&self) -> (Vec3, Vec3) {
        let helper = if self.up.dot(Vec3::X).abs() < 0.9 { Vec3::X } else { Vec3::Z };
        let forward = helper.cross(self.up).normalize();
        let right = self.up.cross(forward).normalize();
        (forward, right)
    }

    pub fn view(&self) -> Mat4 {
        look_at(self.eye(), self.target, self.up)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 0.001).max(1e-4);
        let far = self.distance * 100.0;
        perspective(self.fov_y, aspect, near, far) * self.view()
    }

    /// Rotate by screen-space deltas (pixels) around the target.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * ORBIT_RADIANS_PER_PIXEL;
        self.pitch = (self.pitch + dy * ORBIT_RADIANS_PER_PIXEL).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Rotate by screen-space deltas around `pivot`, a world point that keeps its place on
    /// screen (the surface under the pointer, rather than the framing centre): yaw about the
    /// up axis, pitch about the view's right axis, both through the pivot, so the horizon
    /// stays level and the pitch clamp still applies.
    pub fn orbit_about(&mut self, pivot: Vec3, dx: f32, dy: f32) {
        let yaw = -dx * ORBIT_RADIANS_PER_PIXEL;
        let wanted = (self.pitch + dy * ORBIT_RADIANS_PER_PIXEL).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        let pitch = wanted - self.pitch;
        let view = self.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let rotation = Quat::from_axis_angle(self.up, yaw) * Quat::from_axis_angle(right, -pitch);
        let eye = pivot + rotation * (self.eye() - pivot);
        let target = pivot + rotation * (self.target - pivot);
        self.set_eye_target(eye, target);
    }

    /// Place the camera at `eye` looking at `target` (the up axis is kept).
    pub fn set_eye_target(&mut self, eye: Vec3, target: Vec3) {
        self.target = target;
        let d = eye - target;
        self.distance = d.length().max(1e-4);
        let dir = d / self.distance;
        let (forward, right) = self.basis();
        let elevation = dir.dot(self.up).clamp(-1.0, 1.0);
        self.pitch = elevation.asin().clamp(-PITCH_LIMIT, PITCH_LIMIT);
        let horizontal = dir - self.up * elevation;
        if horizontal.length() > 1e-6 {
            self.yaw = horizontal.dot(right).atan2(horizontal.dot(forward));
        }
    }

    /// Move the target in the view plane by screen-space deltas (pixels).
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let scale = 2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_height.max(1.0);
        let view = self.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        self.target += (-right * dx + up * dy) * scale;
    }

    /// Zoom by a factor (>1 moves closer).
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance / factor).clamp(1e-3, 1e6);
    }
}

/// Right-handed view matrix looking from `eye` to `target` (OpenGL convention).
pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    let f = (target - eye).normalize();
    let s = f.cross(up).normalize();
    let u = s.cross(f);
    Mat4::from_cols(
        Vec4::new(s.x, u.x, -f.x, 0.0),
        Vec4::new(s.y, u.y, -f.y, 0.0),
        Vec4::new(s.z, u.z, -f.z, 0.0),
        Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
    )
}

/// OpenGL perspective projection (clip z in -1..1).
pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    Mat4::from_cols(
        Vec4::new(f / aspect, 0.0, 0.0, 0.0),
        Vec4::new(0.0, f, 0.0, 0.0),
        Vec4::new(0.0, 0.0, (far + near) / (near - far), -1.0),
        Vec4::new(0.0, 0.0, 2.0 * far * near / (near - far), 0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_at_maps_target_onto_negative_z() {
        let v = look_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let p = v.transform_point3(Vec3::ZERO);
        assert!((p - Vec3::new(0.0, 0.0, -5.0)).length() < 1e-5, "{p}");
        let eye = v.transform_point3(Vec3::new(0.0, 0.0, 5.0));
        assert!(eye.length() < 1e-5);
    }

    #[test]
    fn perspective_maps_near_and_far_planes() {
        let p = perspective(90f32.to_radians(), 1.0, 1.0, 10.0);
        let near = p.project_point3(Vec3::new(0.0, 0.0, -1.0));
        let far = p.project_point3(Vec3::new(0.0, 0.0, -10.0));
        assert!((near.z + 1.0).abs() < 1e-5 && (far.z - 1.0).abs() < 1e-5, "{near} {far}");
    }

    #[test]
    fn framing_sees_whole_box() {
        let cam = OrbitCamera::framing(Vec3::splat(-1.0), Vec3::splat(1.0), Vec3::Y);
        assert_eq!(cam.target, Vec3::ZERO);
        assert!(cam.distance > 3.0_f32.sqrt(), "camera must be outside the bounding sphere");
        assert!((cam.eye() - cam.target).length() - cam.distance < 1e-4);
    }

    #[test]
    fn orbit_about_the_target_matches_orbit() {
        let mut a = OrbitCamera::framing(Vec3::splat(-1.0), Vec3::splat(1.0), Vec3::Z);
        let mut b = a.clone();
        a.orbit(40.0, -25.0);
        b.orbit_about(b.target, 40.0, -25.0);
        assert!((a.eye() - b.eye()).length() < 1e-3, "{} vs {}", a.eye(), b.eye());
        assert!((a.target - b.target).length() < 1e-4);
    }

    #[test]
    fn orbit_about_keeps_the_pivot_on_screen() {
        let mut cam = OrbitCamera::framing(Vec3::splat(-1.0), Vec3::splat(1.0), Vec3::Z);
        let pivot = Vec3::new(0.7, -0.4, 0.3);
        let before = cam.view_projection(1.5).project_point3(pivot);
        cam.orbit_about(pivot, 60.0, 30.0);
        let after = cam.view_projection(1.5).project_point3(pivot);
        assert!(
            (before.x - after.x).abs() < 1e-3 && (before.y - after.y).abs() < 1e-3,
            "{before} vs {after}"
        );
        // Still a level turntable: the up axis is unchanged and the eye is where the
        // parameters say it is.
        assert!((cam.eye() - cam.target).length() - cam.distance < 1e-3);
    }

    #[test]
    fn eye_is_at_the_orbit_distance_at_every_yaw() {
        let mut cam = OrbitCamera { up: Vec3::Z, distance: 3.0, pitch: 0.4, ..OrbitCamera::default() };
        for i in 0..16 {
            cam.yaw = i as f32 * std::f32::consts::TAU / 16.0;
            let d = (cam.eye() - cam.target).length();
            assert!((d - 3.0).abs() < 1e-4, "yaw {}: distance {d}", cam.yaw);
            assert!(
                ((cam.eye() - cam.target).z / 3.0 - 0.4f32.sin()).abs() < 1e-4,
                "elevation must not depend on yaw"
            );
        }
    }

    #[test]
    fn pitch_is_clamped() {
        let mut cam = OrbitCamera::default();
        cam.orbit(0.0, 1000.0);
        assert!(cam.pitch < std::f32::consts::FRAC_PI_2);
        cam.orbit(0.0, -5000.0);
        assert!(cam.pitch > -std::f32::consts::FRAC_PI_2);
    }
}
