//! Root motion modes and how an extracted delta moves its owner.
//!
//! Extraction is the animation instance's job: a clip with root motion enabled
//! has the root bone's travel taken out of its pose and reported per advance as
//! a rigid delta in the owner's space (`UpdateReport::root_motion`, and the part
//! contributed by Montages in `Montage` mode as `montage_root_motion`). This
//! module decides *which* of that reaches the owner and *how*:
//!
//! | mode | extraction | applied to the owner |
//! |---|---|---|
//! | `NoRootMotionExtraction` | none: the root bone moves inside the pose | nothing |
//! | `IgnoreRootMotion` | yes | nothing (the delta is dropped; the mesh stays on the owner) |
//! | `RootMotionFromEverything` | yes | the whole delta: graph players and Montages as blended |
//! | `RootMotionFromMontagesOnly` | yes | only what Montages (in `Montage` mode) contributed |
//!
//! A [`RootMotionPolicy`] then constrains the applied delta: per-axis
//! translation (in the owner's local frame) and rotation (none, yaw only, or
//! full). Applying a delta composes on the right (`object_next = object * delta`)
//! in the owner's *local* frame, scaled by the owner's scale on translation, and
//! it is the only thing that moves the owner for animation: the transform is
//! written once per frame, in the animation phase, before physics reads it.

use glam::{DVec3, EulerRot, Quat, Vec3};

use super::pose::RootMotion;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootMotionMode {
    /// The root bone's motion stays in the pose; nothing is extracted or applied.
    NoRootMotionExtraction,
    /// Extract and discard: the mesh stays on the owner while the pose stands still.
    #[default]
    IgnoreRootMotion,
    /// Apply the whole extracted delta (graph and Montages).
    RootMotionFromEverything,
    /// Apply only what Montages in `Montage` root-motion mode contributed.
    RootMotionFromMontagesOnly,
}

impl RootMotionMode {
    /// Whether clips give up the root bone's travel at all.
    pub fn extracts(self) -> bool {
        self != Self::NoRootMotionExtraction
    }

    pub fn applies(self) -> bool {
        matches!(self, Self::RootMotionFromEverything | Self::RootMotionFromMontagesOnly)
    }

    /// The delta this mode applies, from what one advance extracted.
    pub fn select(self, everything: &RootMotion, montages: &RootMotion) -> RootMotion {
        match self {
            Self::NoRootMotionExtraction | Self::IgnoreRootMotion => RootMotion::IDENTITY,
            Self::RootMotionFromEverything => *everything,
            Self::RootMotionFromMontagesOnly => *montages,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootRotation {
    /// Drop the rotation of the delta.
    None,
    /// Keep only the turn about the up (Y) axis.
    #[default]
    Yaw,
    /// Apply the complete rotation.
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RootMotionPolicy {
    pub mode: RootMotionMode,
    /// Translation axes of the owner's local frame that are applied.
    pub translation: [bool; 3],
    pub rotation: RootRotation,
}

impl Default for RootMotionPolicy {
    fn default() -> Self {
        Self { mode: RootMotionMode::default(), translation: [true; 3], rotation: RootRotation::default() }
    }
}

impl RootMotionPolicy {
    /// What to apply to the owner for one advance's extraction.
    pub fn delta(&self, everything: &RootMotion, montages: &RootMotion) -> RootMotion {
        let selected = self.mode.select(everything, montages);
        let t = selected.translation;
        let translation = Vec3::new(
            if self.translation[0] { t.x } else { 0.0 },
            if self.translation[1] { t.y } else { 0.0 },
            if self.translation[2] { t.z } else { 0.0 },
        );
        let rotation = match self.rotation {
            RootRotation::None => Quat::IDENTITY,
            RootRotation::Yaw => Quat::from_rotation_y(selected.rotation.to_euler(EulerRot::YXZ).0),
            RootRotation::Full => selected.rotation,
        };
        RootMotion { translation, rotation: rotation.normalize() }
    }
}

/// Orientation of a `Transform` (YXZ Euler degrees) as a quaternion.
pub fn rotation_of(euler_degrees: [f32; 3]) -> Quat {
    let [x, y, z] = euler_degrees.map(f32::to_radians);
    Quat::from_euler(EulerRot::YXZ, y, x, z)
}

/// `Transform` Euler degrees (x, y, z) of an orientation.
pub fn euler_degrees_of(rotation: Quat) -> [f32; 3] {
    let (y, x, z) = rotation.normalize().to_euler(EulerRot::YXZ);
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}

/// Move a transform by a root-motion delta: translation is rotated by the
/// owner's orientation and scaled by its scale; the rotation composes after the
/// owner's. Non-finite input leaves the transform alone.
pub fn advance_transform(position: [f64; 3], rotation_degrees: [f32; 3], scale: [f32; 3], delta: &RootMotion) -> ([f64; 3], [f32; 3]) {
    if delta.is_identity() || !delta.translation.is_finite() || !delta.rotation.is_finite() {
        return (position, rotation_degrees);
    }
    let owner = rotation_of(rotation_degrees);
    let local = delta.translation * Vec3::from_array(scale);
    let moved = DVec3::from_array(position) + (owner * local).as_dvec3();
    let turned = (owner * delta.rotation).normalize();
    (moved.to_array(), if delta.rotation == Quat::IDENTITY { rotation_degrees } else { euler_degrees_of(turned) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rm(t: [f32; 3], yaw_degrees: f32) -> RootMotion {
        RootMotion { translation: Vec3::from_array(t), rotation: Quat::from_rotation_y(yaw_degrees.to_radians()) }
    }

    #[test]
    fn modes_choose_the_source_and_whether_clips_extract() {
        let all = rm([0.0, 0.0, 3.0], 0.0);
        let montages = rm([0.0, 0.0, 1.0], 0.0);
        assert!(!RootMotionMode::NoRootMotionExtraction.extracts());
        assert!(RootMotionMode::IgnoreRootMotion.extracts());
        assert!(RootMotionMode::IgnoreRootMotion.select(&all, &montages).is_identity());
        assert!(RootMotionMode::NoRootMotionExtraction.select(&all, &montages).is_identity());
        assert_eq!(RootMotionMode::RootMotionFromEverything.select(&all, &montages), all);
        assert_eq!(RootMotionMode::RootMotionFromMontagesOnly.select(&all, &montages), montages);
        assert_eq!(RootMotionMode::default(), RootMotionMode::IgnoreRootMotion, "scenes saved before root motion applied keep standing still");
    }

    #[test]
    fn a_policy_masks_axes_and_rotation() {
        let delta = RootMotion { translation: Vec3::new(1.0, 2.0, 3.0), rotation: Quat::from_euler(EulerRot::YXZ, 0.5, 0.3, 0.2) };
        let mut policy = RootMotionPolicy { mode: RootMotionMode::RootMotionFromEverything, translation: [true, false, true], rotation: RootRotation::Yaw };
        let out = policy.delta(&delta, &RootMotion::IDENTITY);
        assert_eq!(out.translation, Vec3::new(1.0, 0.0, 3.0));
        assert!((out.rotation.to_euler(EulerRot::YXZ).0 - 0.5).abs() < 1e-5);
        assert!(out.rotation.to_euler(EulerRot::YXZ).1.abs() < 1e-5, "pitch and roll are dropped");
        policy.rotation = RootRotation::None;
        assert_eq!(policy.delta(&delta, &RootMotion::IDENTITY).rotation, Quat::IDENTITY);
        policy.rotation = RootRotation::Full;
        assert!(policy.delta(&delta, &RootMotion::IDENTITY).rotation.angle_between(delta.rotation) < 1e-5);
    }

    #[test]
    fn the_delta_moves_the_owner_in_its_own_frame_with_its_scale() {
        // owner yawed 90 degrees: local +Z (forward) is world +X
        let (p, r) = advance_transform([10.0, 0.0, 0.0], [0.0, 90.0, 0.0], [1.0; 3], &rm([0.0, 0.0, 2.0], 0.0));
        assert!((p[0] - 12.0).abs() < 1e-5 && p[1].abs() < 1e-5 && p[2].abs() < 1e-5, "{p:?}");
        assert_eq!(r, [0.0, 90.0, 0.0], "no turn leaves the rotation exactly as authored");
        // scale applies to the travel, rotation composes after the owner's
        let (p, r) = advance_transform([0.0; 3], [0.0; 3], [2.0, 2.0, 2.0], &rm([0.0, 0.0, 1.0], 90.0));
        assert!((p[2] - 2.0).abs() < 1e-5);
        assert!((r[1] - 90.0).abs() < 1e-3, "{r:?}");
        // non-finite deltas are ignored
        let bad = RootMotion { translation: Vec3::new(f32::NAN, 0.0, 0.0), rotation: Quat::IDENTITY };
        assert_eq!(advance_transform([1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [1.0; 3], &bad), ([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]));
    }

    #[test]
    fn straight_travel_and_pure_turns_are_independent_of_how_time_is_sliced() {
        let run = |n: usize, travel: f32, yaw: f32| {
            let (mut p, mut r) = ([1.0, 0.0, 0.0], [0.0, 30.0, 0.0f32]);
            for _ in 0..n {
                (p, r) = advance_transform(p, r, [1.0; 3], &rm([0.0, 0.0, travel / n as f32], yaw / n as f32));
            }
            (p, r)
        };
        // a turn without travel: the same heading however it is sliced
        let (_, a) = run(3, 0.0, 90.0);
        let (_, b) = run(90, 0.0, 90.0);
        assert!((a[1] - 120.0).abs() < 1e-2 && (b[1] - 120.0).abs() < 1e-2, "{a:?} {b:?}");
        // travel without a turn: the same distance along the owner heading
        let (p, _) = run(1, 6.0, 0.0);
        let (q, _) = run(48, 6.0, 0.0);
        assert!((p[0] - q[0]).abs() < 1e-3 && (p[2] - q[2]).abs() < 1e-3, "{p:?} {q:?}");
    }
}
