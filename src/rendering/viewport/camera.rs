//! Orbit camera for the viewport: yaw/pitch around a target at a distance.

use crate::core::{Mat4, Vec3};

pub const FOV_Y_DEG: f32 = 45.0;
pub const NEAR: f32 = 0.05;
pub const FAR: f32 = 500.0;
pub const MIN_DISTANCE: f32 = 0.1;
pub const MAX_DISTANCE: f32 = 200.0;
pub const MAX_PITCH: f32 = 89.0;

pub const DEFAULT_YAW: f32 = 38.0;
pub const DEFAULT_PITCH: f32 = 16.0;

/// One of the six axis directions the gizmo can snap the camera to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewAxis {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

impl ViewAxis {
    pub const ALL: [ViewAxis; 6] = [
        ViewAxis::PosX,
        ViewAxis::NegX,
        ViewAxis::PosY,
        ViewAxis::NegY,
        ViewAxis::PosZ,
        ViewAxis::NegZ,
    ];

    /// Unit direction of the axis in world space.
    pub fn direction(self) -> Vec3 {
        match self {
            ViewAxis::PosX => Vec3::new(1.0, 0.0, 0.0),
            ViewAxis::NegX => Vec3::new(-1.0, 0.0, 0.0),
            ViewAxis::PosY => Vec3::new(0.0, 1.0, 0.0),
            ViewAxis::NegY => Vec3::new(0.0, -1.0, 0.0),
            ViewAxis::PosZ => Vec3::new(0.0, 0.0, 1.0),
            ViewAxis::NegZ => Vec3::new(0.0, 0.0, -1.0),
        }
    }

    /// `(yaw, pitch)` in degrees that look at the target *from* this axis.
    pub fn yaw_pitch(self) -> (f32, f32) {
        match self {
            ViewAxis::PosX => (90.0, 0.0),
            ViewAxis::NegX => (-90.0, 0.0),
            ViewAxis::PosY => (0.0, MAX_PITCH),
            ViewAxis::NegY => (0.0, -MAX_PITCH),
            ViewAxis::PosZ => (0.0, 0.0),
            ViewAxis::NegZ => (180.0, 0.0),
        }
    }
}

/// Movement keys held while flying.
#[derive(Default, Clone, Copy)]
pub struct FlyInput {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// Orbit camera: yaw/pitch around a target point at a fixed distance.
#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            yaw_deg: DEFAULT_YAW,
            pitch_deg: DEFAULT_PITCH,
            distance: 3.0,
        }
    }
}

impl OrbitCamera {
    /// Unit vector from the target toward the eye.
    pub fn eye_dir(&self) -> Vec3 {
        let yaw = self.yaw_deg.to_radians();
        let pitch = self.pitch_deg.to_radians();
        Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos())
    }

    pub fn eye(&self) -> Vec3 {
        self.target.add(self.eye_dir().scale(self.distance))
    }

    /// Screen-space right and up vectors in world space.
    pub fn right_up(&self) -> (Vec3, Vec3) {
        let forward = self.eye_dir().scale(-1.0);
        let world_up = Vec3::new(0.0, 1.0, 0.0);
        let right = forward.cross(world_up).normalize();
        let up = right.cross(forward).normalize();
        (right, up)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at(self.eye(), self.target, Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective(FOV_Y_DEG, aspect.max(0.01), NEAR, FAR);
        proj.mul(&view)
    }

    pub fn set_distance(&mut self, distance: f32) {
        self.distance = distance.clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Aim at the bounding sphere `(center, radius)` so it just fills the
    /// vertical field of view, with some breathing room.
    pub fn frame_sphere(&mut self, center: Vec3, radius: f32) {
        let half_fov = (FOV_Y_DEG * 0.5).to_radians();
        self.target = center;
        self.set_distance((radius.max(0.1) / half_fov.tan() * 1.25).max(0.5));
    }

    /// Unit vector the camera is facing (from the eye toward the target).
    pub fn forward(&self) -> Vec3 {
        self.eye_dir().scale(-1.0)
    }

    /// RMB look: turn the view in place. The eye stays put and the target
    /// swings around it. Dragging right turns right, dragging down looks down.
    pub fn look(&mut self, dx: f32, dy: f32) {
        let eye = self.eye();
        self.yaw_deg -= dx * 0.3;
        self.pitch_deg = (self.pitch_deg + dy * 0.3).clamp(-MAX_PITCH, MAX_PITCH);
        self.target = eye.sub(self.eye_dir().scale(self.distance));
    }

    /// Alt+LMB orbit around the target. The scene follows the cursor: dragging
    /// right turns the near side of the model to the right, dragging down
    /// tips its top toward you.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw_deg -= dx * 0.4;
        self.pitch_deg = (self.pitch_deg + dy * 0.4).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// MMB pan: the scene follows the cursor.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let (right, up) = self.right_up();
        let scale = self.distance * 0.0015;
        self.target = self
            .target
            .add(right.scale(-dx * scale))
            .add(up.scale(dy * scale));
    }

    /// Alt+RMB dolly: dragging down pulls the camera back.
    pub fn dolly(&mut self, dy: f32) {
        self.set_distance(self.distance + dy * self.distance * 0.005);
    }

    /// Fly with the RMB-held movement keys. Returns whether the camera moved.
    pub fn fly(&mut self, keys: &FlyInput) -> bool {
        let forward = self.forward();
        let world_up = Vec3::new(0.0, 1.0, 0.0);
        let right = forward.cross(world_up).normalize();
        let mut movement = Vec3::ZERO;
        if keys.forward {
            movement = movement.add(forward);
        }
        if keys.backward {
            movement = movement.sub(forward);
        }
        if keys.right {
            movement = movement.add(right);
        }
        if keys.left {
            movement = movement.sub(right);
        }
        if keys.up {
            movement = movement.add(world_up);
        }
        if keys.down {
            movement = movement.sub(world_up);
        }
        if movement.length() < 0.001 {
            return false;
        }
        let speed = (self.distance * 0.02).clamp(0.05, 1.0);
        self.target = self.target.add(movement.normalize().scale(speed));
        true
    }

    /// Look at the target from one of the six axis directions.
    pub fn snap_to(&mut self, axis: ViewAxis) {
        let (yaw, pitch) = axis.yaw_pitch();
        self.yaw_deg = yaw;
        self.pitch_deg = pitch;
    }
}

/// Bounding sphere (center of the AABB, farthest point) of `points`.
pub fn bounding_sphere(points: &[Vec3]) -> Option<(Vec3, f32)> {
    let first = *points.first()?;
    let (mut min, mut max) = (first, first);
    for p in &points[1..] {
        min = Vec3::new(min.x.min(p.x), min.y.min(p.y), min.z.min(p.z));
        max = Vec3::new(max.x.max(p.x), max.y.max(p.y), max.z.max(p.z));
    }
    let center = Vec3::new(
        (min.x + max.x) * 0.5,
        (min.y + max.y) * 0.5,
        (min.z + max.z) * 0.5,
    );
    let radius = points.iter().map(|p| p.sub(center).length()).fold(0.0, f32::max);
    Some((center, radius))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_keeps_points_inside_the_frustum() {
        let points = [
            Vec3::new(-1.0, 0.0, 0.2),
            Vec3::new(1.0, 1.8, -0.3),
            Vec3::new(0.2, 0.9, 0.5),
        ];
        let mut cam = OrbitCamera::default();
        let (center, radius) = bounding_sphere(&points).unwrap();
        cam.frame_sphere(center, radius);
        let vp = cam.view_proj(16.0 / 9.0);
        for p in points {
            let (x, y, _, w) = vp.transform_clip(p);
            assert!(w > 0.0);
            assert!(
                (x / w).abs() <= 1.0 && (y / w).abs() <= 1.0,
                "{p:?} fell outside the frame"
            );
        }
    }

    /// Screen-space NDC x/y of a world point.
    fn ndc(cam: &OrbitCamera, p: Vec3) -> (f32, f32) {
        let (x, y, _, w) = cam.view_proj(1.6).transform_clip(p);
        assert!(w > 0.0, "point must be in front of the camera");
        (x / w, y / w)
    }

    fn cam() -> OrbitCamera {
        OrbitCamera {
            target: Vec3::new(0.0, 1.0, 0.0),
            distance: 4.0,
            ..OrbitCamera::default()
        }
    }

    #[test]
    fn w_flies_toward_what_the_camera_sees_and_s_away() {
        let mut c = cam();
        let ahead = c.target; // dead centre of the view
        let start = c.eye().sub(ahead).length();

        let mut w = c;
        assert!(w.fly(&FlyInput {
            forward: true,
            ..Default::default()
        }));
        assert!(
            w.eye().sub(ahead).length() < start,
            "W should close the distance to what's ahead"
        );

        let mut s = c;
        assert!(s.fly(&FlyInput {
            backward: true,
            ..Default::default()
        }));
        assert!(s.eye().sub(ahead).length() > start, "S should back away");

        // A/D strafe: D moves the camera so the centre point shifts left on screen.
        let before = ndc(&c, ahead).0;
        c.fly(&FlyInput {
            right: true,
            ..Default::default()
        });
        assert!(
            ndc(&c, ahead).0 < before,
            "after strafing right, the old centre sits left of centre"
        );
    }

    #[test]
    fn mouse_look_turns_the_view_toward_the_drag() {
        let probe = |c: &OrbitCamera| ndc(c, c.target);
        let c0 = cam();
        let point = c0.target; // what was in the middle of the screen

        let mut right = c0;
        right.look(40.0, 0.0);
        assert!(
            ndc(&right, point).0 < 0.0,
            "looking right pushes the old centre to the left"
        );

        let mut down = c0;
        down.look(0.0, 40.0);
        assert!(
            ndc(&down, point).1 > 0.0,
            "looking down pushes the old centre up the screen"
        );
        let _ = probe;
    }

    #[test]
    fn orbit_and_pan_make_the_scene_follow_the_cursor() {
        let c0 = cam();
        // A point on the near side of the model, between camera and target.
        let near = c0.target.add(c0.eye_dir().scale(0.6));

        let mut orbit = c0;
        orbit.orbit(40.0, 0.0);
        assert!(
            ndc(&orbit, near).0 > 0.0,
            "dragging right swings the near side right"
        );

        let mut orbit_down = c0;
        orbit_down.orbit(0.0, 40.0);
        assert!(
            ndc(&orbit_down, near).1 < 0.0,
            "dragging down swings the near side down"
        );

        let centre = c0.target;
        let mut pan = c0;
        pan.pan(40.0, 0.0);
        assert!(ndc(&pan, centre).0 > 0.0, "panning right moves the scene right");
        let mut pan_down = c0;
        pan_down.pan(0.0, 40.0);
        assert!(
            ndc(&pan_down, centre).1 < 0.0,
            "panning down moves the scene down"
        );
    }

    #[test]
    fn axis_snaps_look_along_the_axis() {
        for axis in ViewAxis::ALL {
            let mut cam = OrbitCamera::default();
            cam.snap_to(axis);
            let dir = cam.eye_dir();
            let want = axis.direction();
            // Poles are clamped just short of vertical.
            assert!(dir.dot(want) > 0.999, "{axis:?}: {dir:?}");
        }
    }
}
