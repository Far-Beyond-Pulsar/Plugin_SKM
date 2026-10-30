//! IK solvers over local-space bone transforms: analytic two-bone and FABRIK.
//!
//! Both solve in mesh space and write back local rotations (and, for stretch,
//! local translations), so bones outside the chain are untouched and children
//! of the chain follow. Equal inputs always give equal outputs: there is no
//! randomness, no time dependence and a fixed iteration order.

use std::collections::HashMap;

use glam::{EulerRot, Quat, Vec3};

use super::pose::{slerp_short, BoneTransform, RuntimeSkeleton};

/// Where a chain's tip should go, in mesh space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effector {
    pub translation: Vec3,
    pub rotation: Quat,
}

impl Effector {
    pub fn at(translation: Vec3) -> Self {
        Self { translation, rotation: Quat::IDENTITY }
    }
}

/// Rotation limits per bone in degrees about the bone's local axes (XYZ Euler).
#[derive(Clone, Debug, Default)]
pub struct JointLimits(pub HashMap<usize, ([f32; 3], [f32; 3])>);

impl JointLimits {
    /// Clamp one bone's local rotation to its limits. Returns whether it moved.
    pub fn clamp(&self, bone: usize, local: &mut BoneTransform) -> bool {
        let Some((min, max)) = self.0.get(&bone) else { return false };
        let (x, y, z) = local.rotation.to_euler(EulerRot::XYZ);
        let deg = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
        let clamped = [0, 1, 2].map(|i| deg[i].clamp(min[i], max[i]));
        if clamped == deg {
            return false;
        }
        local.rotation = Quat::from_euler(EulerRot::XYZ, clamped[0].to_radians(), clamped[1].to_radians(), clamped[2].to_radians()).normalize();
        true
    }
}

/// Rotate `bone` in mesh space by `delta`, keeping its position.
fn rotate_in_mesh_space(skel: &RuntimeSkeleton, bones: &mut [BoneTransform], bone: usize, delta: Quat) {
    let parent_rot = match skel.parents[bone] {
        Some(p) => skel.bone_mesh_space(bones, p).to_scale_rotation_translation().1,
        None => Quat::IDENTITY,
    };
    let mesh_rot = parent_rot * bones[bone].rotation;
    bones[bone].rotation = (parent_rot.inverse() * delta * mesh_rot).normalize();
}

/// Set `bone`'s mesh-space rotation outright.
fn set_mesh_rotation(skel: &RuntimeSkeleton, bones: &mut [BoneTransform], bone: usize, mesh_rot: Quat) {
    let parent_rot = match skel.parents[bone] {
        Some(p) => skel.bone_mesh_space(bones, p).to_scale_rotation_translation().1,
        None => Quat::IDENTITY,
    };
    bones[bone].rotation = (parent_rot.inverse() * mesh_rot).normalize();
}

fn mesh_position(skel: &RuntimeSkeleton, bones: &[BoneTransform], bone: usize) -> Vec3 {
    skel.bone_mesh_space(bones, bone).to_scale_rotation_translation().2
}

fn arc(from: Vec3, to: Vec3) -> Quat {
    let (f, t) = (from.normalize_or_zero(), to.normalize_or_zero());
    if f == Vec3::ZERO || t == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    if f.dot(t) < -0.999_999 {
        // opposite directions: any perpendicular axis, chosen deterministically
        let axis = if f.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        return Quat::from_axis_angle(f.cross(axis).normalize(), std::f32::consts::PI);
    }
    Quat::from_rotation_arc(f, t)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TwoBoneParams {
    pub alpha: f32,
    pub allow_stretch: bool,
    pub max_stretch: f32,
    pub take_rotation: bool,
}

impl Default for TwoBoneParams {
    fn default() -> Self {
        Self { alpha: 1.0, allow_stretch: false, max_stretch: 1.25, take_rotation: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TwoBoneReport {
    /// The target was farther than the limb can reach (before any stretch).
    pub out_of_reach: bool,
    pub stretch: f32,
}

/// The limb a two-bone solve uses: `(upper, mid, end)`.
pub fn two_bone_limb(skel: &RuntimeSkeleton, end: usize) -> Option<(usize, usize, usize)> {
    let mid = skel.parents[end]?;
    let upper = skel.parents[mid]?;
    Some((upper, mid, end))
}

/// Solve the limb ending at `end` toward `effector`. `pole` is the direction
/// (mesh space) the middle joint should bend toward; without one the limb keeps
/// its current bend plane. Returns `None` when the end bone has no grandparent.
pub fn solve_two_bone(
    skel: &RuntimeSkeleton,
    bones: &mut [BoneTransform],
    end: usize,
    effector: &Effector,
    pole: Option<Vec3>,
    params: &TwoBoneParams,
    limits: &JointLimits,
) -> Option<TwoBoneReport> {
    let (upper, mid, end) = two_bone_limb(skel, end)?;
    let input: [BoneTransform; 3] = [bones[upper], bones[mid], bones[end]];
    let alpha = params.alpha.clamp(0.0, 1.0);

    let a = mesh_position(skel, bones, upper);
    let b = mesh_position(skel, bones, mid);
    let c = mesh_position(skel, bones, end);
    let l1 = (b - a).length();
    let l2 = (c - b).length();
    if l1 < 1e-6 || l2 < 1e-6 {
        return Some(TwoBoneReport { out_of_reach: false, stretch: 1.0 });
    }
    let to_target = effector.translation - a;
    let dist = to_target.length();
    let reach = l1 + l2;
    let out_of_reach = dist > reach;
    let mut stretch = 1.0;
    if params.allow_stretch && dist > reach {
        stretch = (dist / reach).min(params.max_stretch.max(1.0));
        bones[mid].translation *= stretch;
        bones[end].translation *= stretch;
    }
    let (l1, l2) = (l1 * stretch, l2 * stretch);
    let axis = if dist > 1e-6 { to_target / dist } else { (c - a).normalize_or_zero() };
    let min_d = (l1 - l2).abs() + 1e-4;
    let max_d = l1 + l2 - 1e-4;
    let d = dist.clamp(min_d, max_d.max(min_d));

    // bend direction, perpendicular to the reach axis
    let current_bend = (b - a) - axis * (b - a).dot(axis);
    let wanted = pole.map(|p| p - axis * p.dot(axis)).filter(|v| v.length_squared() > 1e-10);
    let bend = wanted
        .or(Some(current_bend).filter(|v| v.length_squared() > 1e-10))
        .map(Vec3::normalize)
        .unwrap_or_else(|| axis.any_orthonormal_vector());

    let cos_a = ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d)).clamp(-1.0, 1.0);
    let sin_a = (1.0 - cos_a * cos_a).max(0.0).sqrt();
    let new_b = a + axis * (l1 * cos_a) + bend * (l1 * sin_a);
    let reached = a + axis * d;

    // upper: swing its direction to the new mid
    let old_b = mesh_position(skel, bones, mid);
    rotate_in_mesh_space(skel, bones, upper, arc(old_b - a, new_b - a));
    // mid: swing so the end lands on the reachable target
    let b_now = mesh_position(skel, bones, mid);
    let c_now = mesh_position(skel, bones, end);
    rotate_in_mesh_space(skel, bones, mid, arc(c_now - b_now, reached - b_now));
    if params.take_rotation {
        set_mesh_rotation(skel, bones, end, effector.rotation);
    }
    for (slot, bone) in [upper, mid, end].into_iter().enumerate() {
        limits.clamp(bone, &mut bones[bone]);
        if alpha < 1.0 {
            bones[bone] = input[slot].blend(&bones[bone], alpha);
        }
    }
    Some(TwoBoneReport { out_of_reach, stretch })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FabrikParams {
    pub alpha: f32,
    pub precision: f32,
    pub max_iterations: u32,
    pub take_rotation: bool,
    pub respect_limits: bool,
}

impl Default for FabrikParams {
    fn default() -> Self {
        Self { alpha: 1.0, precision: 1e-3, max_iterations: 10, take_rotation: false, respect_limits: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FabrikReport {
    pub iterations: u32,
    pub tip_error: f32,
}

/// Solve the chain `chain` (root first, at least two bones) so its tip reaches
/// `effector`, preserving bone lengths and the root's position.
pub fn solve_fabrik(
    skel: &RuntimeSkeleton,
    bones: &mut [BoneTransform],
    chain: &[usize],
    effector: &Effector,
    params: &FabrikParams,
    limits: &JointLimits,
) -> Option<FabrikReport> {
    if chain.len() < 2 {
        return None;
    }
    let input: Vec<BoneTransform> = chain.iter().map(|&b| bones[b]).collect();
    let mut pos: Vec<Vec3> = chain.iter().map(|&b| mesh_position(skel, bones, b)).collect();
    let original = pos.clone();
    let lengths: Vec<f32> = pos.windows(2).map(|w| (w[1] - w[0]).length()).collect();
    let total: f32 = lengths.iter().sum();
    let root = pos[0];
    let target = effector.translation;
    let n = pos.len();
    let mut iterations = 0;

    if (target - root).length() >= total {
        // unreachable: a straight line toward the target
        let dir = (target - root).normalize_or_zero();
        for i in 1..n {
            pos[i] = pos[i - 1] + dir * lengths[i - 1];
        }
    } else {
        while iterations < params.max_iterations.max(1) && (pos[n - 1] - target).length() > params.precision {
            pos[n - 1] = target;
            for i in (0..n - 1).rev() {
                let dir = (pos[i] - pos[i + 1]).normalize_or_zero();
                pos[i] = pos[i + 1] + dir * lengths[i];
            }
            pos[0] = root;
            for i in 1..n {
                let dir = (pos[i] - pos[i - 1]).normalize_or_zero();
                pos[i] = pos[i - 1] + dir * lengths[i - 1];
            }
            iterations += 1;
        }
    }

    // rotations: swing each bone from its original child direction to the new one
    for i in 0..n - 1 {
        let now_dir = mesh_position(skel, bones, chain[i + 1]) - mesh_position(skel, bones, chain[i]);
        let want_dir = pos[i + 1] - pos[i];
        // the bone's child offset is measured after its parent's change
        let _ = &original;
        rotate_in_mesh_space(skel, bones, chain[i], arc(now_dir, want_dir));
        if params.respect_limits {
            limits.clamp(chain[i], &mut bones[chain[i]]);
        }
    }
    if params.take_rotation {
        set_mesh_rotation(skel, bones, chain[n - 1], effector.rotation);
    }
    let alpha = params.alpha.clamp(0.0, 1.0);
    if alpha < 1.0 {
        for (slot, &bone) in chain.iter().enumerate() {
            bones[bone] = BoneTransform {
                translation: input[slot].translation.lerp(bones[bone].translation, alpha),
                rotation: slerp_short(input[slot].rotation, bones[bone].rotation, alpha),
                scale: input[slot].scale.lerp(bones[bone].scale, alpha),
            };
        }
    }
    let tip_error = (mesh_position(skel, bones, chain[n - 1]) - target).length();
    Some(FabrikReport { iterations, tip_error })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::math::{Transform, Vec3 as AV3};
    use crate::skeleton::{Bone, Skeleton};

    /// A straight vertical limb: root at origin, each next bone `len` above.
    pub(crate) fn limb(n: usize, len: f32) -> RuntimeSkeleton {
        let skeleton = Skeleton {
            bones: (0..n)
                .map(|i| Bone {
                    id: format!("b{i}"),
                    name: format!("b{i}"),
                    parent: (i > 0).then(|| format!("b{}", i - 1)),
                    bind_transform: Transform { translation: AV3::new(0.0, if i == 0 { 0.0 } else { len }, 0.0), ..Transform::IDENTITY },
                })
                .collect(),
        };
        RuntimeSkeleton::new("limb", &skeleton).unwrap()
    }

    fn tip(skel: &RuntimeSkeleton, bones: &[BoneTransform], b: usize) -> Vec3 {
        mesh_position(skel, bones, b)
    }

    #[test]
    fn two_bone_reaches_a_reachable_target_and_keeps_bone_lengths() {
        let skel = limb(3, 1.0);
        let mut bones = skel.bind.clone();
        let target = Vec3::new(1.2, 0.8, 0.0);
        let report = solve_two_bone(&skel, &mut bones, 2, &Effector::at(target), Some(Vec3::X), &TwoBoneParams::default(), &JointLimits::default()).unwrap();
        assert!(!report.out_of_reach);
        assert!((tip(&skel, &bones, 2) - target).length() < 1e-3, "{:?}", tip(&skel, &bones, 2));
        assert!(((tip(&skel, &bones, 1) - tip(&skel, &bones, 0)).length() - 1.0).abs() < 1e-3);
        assert!(((tip(&skel, &bones, 2) - tip(&skel, &bones, 1)).length() - 1.0).abs() < 1e-3);
        // the elbow bent toward the pole (+x side is the shorter way round to +x target)
        let again = {
            let mut b2 = skel.bind.clone();
            solve_two_bone(&skel, &mut b2, 2, &Effector::at(target), Some(Vec3::X), &TwoBoneParams::default(), &JointLimits::default());
            b2
        };
        assert_eq!(bones, again, "deterministic");
    }

    #[test]
    fn the_pole_chooses_the_bend_side() {
        let skel = limb(3, 1.0);
        let target = Vec3::new(0.0, 1.0, 0.0);
        let mut left = skel.bind.clone();
        let mut right = skel.bind.clone();
        solve_two_bone(&skel, &mut left, 2, &Effector::at(target), Some(Vec3::X), &TwoBoneParams::default(), &JointLimits::default());
        solve_two_bone(&skel, &mut right, 2, &Effector::at(target), Some(-Vec3::X), &TwoBoneParams::default(), &JointLimits::default());
        assert!(tip(&skel, &left, 1).x > 0.3);
        assert!(tip(&skel, &right, 1).x < -0.3);
        assert!((tip(&skel, &left, 2) - target).length() < 1e-3 && (tip(&skel, &right, 2) - target).length() < 1e-3);
    }

    #[test]
    fn out_of_reach_extends_straight_or_stretches_within_the_limit() {
        let skel = limb(3, 1.0);
        let far = Vec3::new(5.0, 0.0, 0.0);
        let mut bones = skel.bind.clone();
        let r = solve_two_bone(&skel, &mut bones, 2, &Effector::at(far), None, &TwoBoneParams::default(), &JointLimits::default()).unwrap();
        assert!(r.out_of_reach);
        assert!((tip(&skel, &bones, 2) - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-2);
        let mut stretched = skel.bind.clone();
        let params = TwoBoneParams { allow_stretch: true, max_stretch: 1.25, ..Default::default() };
        let r = solve_two_bone(&skel, &mut stretched, 2, &Effector::at(far), None, &params, &JointLimits::default()).unwrap();
        assert!((r.stretch - 1.25).abs() < 1e-6);
        assert!((tip(&skel, &stretched, 2).length() - 2.5).abs() < 1e-2);
    }

    #[test]
    fn alpha_blends_toward_the_input_pose() {
        let skel = limb(3, 1.0);
        let target = Vec3::new(1.0, 1.0, 0.0);
        let mut full = skel.bind.clone();
        let mut none = skel.bind.clone();
        solve_two_bone(&skel, &mut full, 2, &Effector::at(target), Some(Vec3::X), &TwoBoneParams::default(), &JointLimits::default());
        solve_two_bone(&skel, &mut none, 2, &Effector::at(target), Some(Vec3::X), &TwoBoneParams { alpha: 0.0, ..Default::default() }, &JointLimits::default());
        assert_eq!(none, skel.bind);
        assert!((tip(&skel, &full, 2) - target).length() < 1e-3);
    }

    #[test]
    fn fabrik_reaches_preserves_lengths_and_is_deterministic() {
        let skel = limb(5, 1.0);
        let chain = [0, 1, 2, 3, 4];
        let target = Vec3::new(2.0, 2.0, 1.0);
        let mut a = skel.bind.clone();
        let rep = solve_fabrik(&skel, &mut a, &chain, &Effector::at(target), &FabrikParams { max_iterations: 40, precision: 1e-4, ..Default::default() }, &JointLimits::default()).unwrap();
        assert!(rep.tip_error < 1e-2, "{rep:?}");
        for w in chain.windows(2) {
            assert!(((tip(&skel, &a, w[1]) - tip(&skel, &a, w[0])).length() - 1.0).abs() < 1e-3);
        }
        assert!(tip(&skel, &a, 0).length() < 1e-4, "root stays");
        let mut b = skel.bind.clone();
        solve_fabrik(&skel, &mut b, &chain, &Effector::at(target), &FabrikParams { max_iterations: 40, precision: 1e-4, ..Default::default() }, &JointLimits::default());
        assert_eq!(a, b);
        // unreachable: a straight line toward the target
        let mut far = skel.bind.clone();
        solve_fabrik(&skel, &mut far, &chain, &Effector::at(Vec3::new(10.0, 0.0, 0.0)), &FabrikParams::default(), &JointLimits::default());
        assert!((tip(&skel, &far, 4) - Vec3::new(4.0, 0.0, 0.0)).length() < 1e-2);
    }

    #[test]
    fn joint_limits_clamp_local_euler_rotation() {
        let skel = limb(3, 1.0);
        let mut limits = JointLimits::default();
        limits.0.insert(1, ([-5.0, -5.0, -5.0], [5.0, 5.0, 5.0]));
        let mut bones = skel.bind.clone();
        solve_two_bone(&skel, &mut bones, 2, &Effector::at(Vec3::new(1.0, 1.0, 0.0)), Some(Vec3::X), &TwoBoneParams::default(), &limits);
        let (x, y, z) = bones[1].rotation.to_euler(EulerRot::XYZ);
        assert!(x.to_degrees().abs() <= 5.001 && y.to_degrees().abs() <= 5.001 && z.to_degrees().abs() <= 5.001);
    }
}
