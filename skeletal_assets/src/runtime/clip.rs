//! A sequence compiled against a runtime skeleton: per-bone tracks indexed by
//! bone, quaternion keys, curves and root-motion extraction.
//!
//! Compiled clips are immutable and shared by every instance that plays them.

use glam::{Mat4, Quat, Vec3};

use crate::animation::KeyInterpolation;
use crate::assets::AnimSeqAsset;
use crate::curves::{AnimationCurve, CurveValue};
use crate::root_motion::RootMotionSettings;

use super::pose::{slerp_short, BoneTransform, Curves, Pose, RootMotion, RuntimeSkeleton};

#[derive(Clone, Debug)]
struct Key {
    time: f32,
    value: BoneTransform,
    translation: KeyInterpolation,
    rotation: KeyInterpolation,
    scale: KeyInterpolation,
    in_tangent: crate::animation::KeyframeTangent,
    out_tangent: crate::animation::KeyframeTangent,
    quaternion: Option<Quat>,
}

#[derive(Clone, Debug)]
pub struct CompiledClip {
    pub id: String,
    pub name: String,
    pub duration: f32,
    pub fps: f32,
    tracks: Vec<Option<Vec<Key>>>,
    curves: Vec<AnimationCurve>,
    pub root_motion: RootMotionSettings,
    root_bone: Option<usize>,
    /// Tracks whose bone the skeleton does not have (kept for diagnostics).
    pub orphan_tracks: Vec<String>,
    /// The asset this was compiled from, when it was compiled from a shared one:
    /// notify consumers read the clip's notifies and identity from it.
    pub source: Option<std::sync::Arc<AnimSeqAsset>>,
}

impl CompiledClip {
    pub fn compile(asset: &AnimSeqAsset, skel: &RuntimeSkeleton) -> Self {
        let mut tracks: Vec<Option<Vec<Key>>> = vec![None; skel.len()];
        let mut orphan_tracks = Vec::new();
        for track in &asset.clip.tracks {
            let Some(bone) = skel.bone(&track.bone_id) else {
                orphan_tracks.push(track.bone_id.clone());
                continue;
            };
            let mut keys: Vec<Key> = track
                .keyframes
                .iter()
                .map(|k| Key {
                    time: k.time,
                    value: BoneTransform::from_asset(&k.transform),
                    translation: k.translation_interpolation,
                    rotation: k.rotation_interpolation,
                    scale: k.scale_interpolation,
                    in_tangent: k.in_tangent,
                    out_tangent: k.out_tangent,
                    quaternion: k.rotation_quaternion.map(|q| Quat::from_xyzw(q[0], q[1], q[2], q[3])),
                })
                .collect();
            keys.sort_by(|a, b| a.time.total_cmp(&b.time));
            tracks[bone] = Some(keys);
        }
        let root_motion = asset.features.root_motion.clone();
        let root_bone = root_motion
            .source_bone
            .as_deref()
            .and_then(|b| skel.bone(b))
            .or_else(|| (0..skel.len()).find(|&i| skel.parents[i].is_none()));
        Self {
            id: asset.id.to_string(),
            name: asset.clip.name.clone(),
            duration: asset.clip.duration.max(0.0),
            fps: asset.clip.fps_clamped(),
            tracks,
            curves: asset.features.curves.clone(),
            root_motion,
            root_bone,
            orphan_tracks,
            source: None,
        }
    }

    /// Compile from a shared asset and keep it, so notify consumers can reach the
    /// sequence's notifies without a second load.
    pub fn compile_shared(asset: &std::sync::Arc<AnimSeqAsset>, skel: &RuntimeSkeleton) -> Self {
        let mut clip = Self::compile(asset, skel);
        clip.source = Some(asset.clone());
        clip
    }

    pub fn has_track(&self, bone: usize) -> bool {
        self.tracks.get(bone).is_some_and(Option::is_some)
    }

    fn sample_track(keys: &[Key], time: f32) -> BoneTransform {
        let first = &keys[0];
        let last = &keys[keys.len() - 1];
        if keys.len() == 1 || time <= first.time {
            return first.value;
        }
        if time >= last.time {
            return last.value;
        }
        let i = keys.partition_point(|k| k.time <= time).saturating_sub(1).min(keys.len() - 2);
        let (a, b) = (&keys[i], &keys[i + 1]);
        let span = (b.time - a.time).max(1e-6);
        let t = ((time - a.time) / span).clamp(0.0, 1.0);

        let translation = match a.translation {
            KeyInterpolation::Step => a.value.translation,
            KeyInterpolation::Linear => a.value.translation.lerp(b.value.translation, t),
            KeyInterpolation::CubicSpline => Vec3::new(
                hermite(a.value.translation.x, b.value.translation.x, a.out_tangent.translation[0], b.in_tangent.translation[0], t, span),
                hermite(a.value.translation.y, b.value.translation.y, a.out_tangent.translation[1], b.in_tangent.translation[1], t, span),
                hermite(a.value.translation.z, b.value.translation.z, a.out_tangent.translation[2], b.in_tangent.translation[2], t, span),
            ),
        };
        let scale = match a.scale {
            KeyInterpolation::Step => a.value.scale,
            KeyInterpolation::Linear => a.value.scale.lerp(b.value.scale, t),
            KeyInterpolation::CubicSpline => Vec3::new(
                hermite(a.value.scale.x, b.value.scale.x, a.out_tangent.scale[0], b.in_tangent.scale[0], t, span),
                hermite(a.value.scale.y, b.value.scale.y, a.out_tangent.scale[1], b.in_tangent.scale[1], t, span),
                hermite(a.value.scale.z, b.value.scale.z, a.out_tangent.scale[2], b.in_tangent.scale[2], t, span),
            ),
        };
        let rotation = match (a.rotation, a.quaternion, b.quaternion) {
            (KeyInterpolation::Step, ..) => a.value.rotation,
            (KeyInterpolation::CubicSpline, Some(qa), Some(qb)) => {
                let (qa, qb) = (qa.to_array(), qb.to_array());
                let mut q = [0.0; 4];
                for (n, v) in q.iter_mut().enumerate() {
                    *v = hermite(qa[n], qb[n], a.out_tangent.rotation[n], b.in_tangent.rotation[n], t, span);
                }
                let q = Quat::from_array(q);
                if q.length() > 1e-8 { q.normalize() } else { a.value.rotation }
            }
            _ => slerp_short(a.value.rotation, b.value.rotation, t),
        };
        BoneTransform::new(translation, rotation, scale)
    }

    /// The local transform of `bone` at `time`, or `fallback` when untracked.
    pub fn sample_bone(&self, bone: usize, time: f32, fallback: BoneTransform) -> BoneTransform {
        match self.tracks.get(bone).and_then(Option::as_ref) {
            Some(keys) if !keys.is_empty() => Self::sample_track(keys, time),
            _ => fallback,
        }
    }

    /// Sample every bone at `time` (clamped to the clip). Untracked bones keep
    /// their bind pose.
    pub fn sample(&self, skel: &RuntimeSkeleton, time: f32) -> Pose {
        let time = time.clamp(0.0, self.duration);
        let bones = (0..skel.len()).map(|b| self.sample_bone(b, time, skel.bind[b])).collect();
        let mut pose = Pose::from_bones(bones);
        pose.curves = self.sample_curves(time);
        pose
    }

    pub fn sample_curves(&self, time: f32) -> Curves {
        let mut out = Curves::new();
        for curve in &self.curves {
            let Some(value) = curve.sample(time) else { continue };
            // Vector curves are exposed per component: `name.x`, `name.y`, `name.z`.
            match value {
                CurveValue::Float(v) => {
                    out.insert(curve.name().to_string(), v);
                }
                CurveValue::Vector(v) => {
                    out.insert(format!("{}.x", curve.name()), v.x);
                    out.insert(format!("{}.y", curve.name()), v.y);
                    out.insert(format!("{}.z", curve.name()), v.z);
                }
            }
        }
        out
    }

    fn motion_bone_mesh(&self, skel: &RuntimeSkeleton, bone: usize, time: f32) -> Mat4 {
        let mut chain = vec![bone];
        let mut at = bone;
        while let Some(p) = skel.parents[at] {
            chain.push(p);
            at = p;
        }
        chain.iter().rev().fold(Mat4::IDENTITY, |acc, &b| acc * self.sample_bone(b, time, skel.bind[b]).to_mat4())
    }

    /// The rigid part of the root-motion source bone at `time`: enabled
    /// translation axes and, when enabled, rotation.
    fn motion_at(&self, skel: &RuntimeSkeleton, bone: usize, time: f32, s: &RootMotionSettings) -> RootMotion {
        let m = self.motion_bone_mesh(skel, bone, time.clamp(0.0, self.duration));
        let (_, r, t) = m.to_scale_rotation_translation();
        RootMotion {
            translation: Vec3::new(
                if s.translation[0] { t.x } else { 0.0 },
                if s.translation[1] { t.y } else { 0.0 },
                if s.translation[2] { t.z } else { 0.0 },
            ),
            rotation: if s.rotation { r.normalize() } else { Quat::IDENTITY },
        }
    }

    /// Root motion accumulated while the play position moves from `from` to
    /// `to` (unbounded positions, so a looping player passes its running
    /// total). Reverse playback is the inverse of the forward motion. Loop
    /// seams compose: whole cycles contribute the clip's start-to-end motion.
    pub fn root_motion_between(&self, skel: &RuntimeSkeleton, from: f64, to: f64, looping: bool) -> RootMotion {
        if !self.root_motion.enabled {
            return RootMotion::IDENTITY;
        }
        self.motion_between(skel, from, to, looping, &self.root_motion)
    }

    /// The root bone's motion between two play positions with every axis and
    /// rotation included, whether or not root motion is enabled for playback.
    /// Pose Search trajectories are built from this.
    pub fn trajectory_between(&self, skel: &RuntimeSkeleton, from: f64, to: f64, looping: bool) -> RootMotion {
        let all = RootMotionSettings { enabled: true, source_bone: None, translation: [true; 3], rotation: true };
        self.motion_between(skel, from, to, looping, &all)
    }

    fn motion_between(&self, skel: &RuntimeSkeleton, from: f64, to: f64, looping: bool, settings: &RootMotionSettings) -> RootMotion {
        let Some(bone) = self.root_bone else {
            return RootMotion::IDENTITY;
        };
        if self.duration <= 0.0 || from == to || !from.is_finite() || !to.is_finite() {
            return RootMotion::IDENTITY;
        }
        if to < from {
            return self.motion_between(skel, to, from, looping, settings).inverse();
        }
        let dur = self.duration as f64;
        let at = |t: f32| self.motion_at(skel, bone, t, settings);
        if !looping {
            let a = from.clamp(0.0, dur) as f32;
            let b = to.clamp(0.0, dur) as f32;
            return at(a).inverse().then(&at(b));
        }
        let cycle_a = (from / dur).floor();
        let cycle_b = (to / dur).floor();
        let pa = (from - cycle_a * dur) as f32;
        let pb = (to - cycle_b * dur) as f32;
        if cycle_a == cycle_b {
            return at(pa).inverse().then(&at(pb));
        }
        let per_cycle = at(0.0).inverse().then(&at(self.duration));
        let whole = (cycle_b - cycle_a - 1.0) as u64;
        at(pa)
            .inverse()
            .then(&at(self.duration))
            .then(&motion_power(per_cycle, whole))
            .then(&at(0.0).inverse())
            .then(&at(pb))
    }

    /// Remove the root-motion source's accumulated motion from a sampled pose
    /// so applying the delta to the actor does not move the character twice.
    /// The source keeps the rest of its animation relative to its start frame.
    pub fn remove_root_motion(&self, skel: &RuntimeSkeleton, bones: &mut [BoneTransform], time: f32) {
        let Some(bone) = self.root_bone.filter(|_| self.root_motion.enabled) else {
            return;
        };
        let time = time.clamp(0.0, self.duration);
        let full = self.motion_bone_mesh(skel, bone, time);
        let motion_now = self.motion_at(skel, bone, time, &self.root_motion).to_mat4();
        let motion_start = self.motion_at(skel, bone, 0.0, &self.root_motion).to_mat4();
        let residual = motion_start * motion_now.inverse() * full;
        let local = match skel.parents[bone] {
            Some(p) => skel.bone_mesh_space(bones, p).inverse() * residual,
            None => residual,
        };
        bones[bone] = BoneTransform::from_mat4(local);
    }
}

fn motion_power(base: RootMotion, mut n: u64) -> RootMotion {
    let mut result = RootMotion::IDENTITY;
    let mut square = base;
    while n > 0 {
        if n & 1 == 1 {
            result = result.then(&square);
        }
        square = square.then(&square);
        n >>= 1;
    }
    result
}

fn hermite(a: f32, b: f32, outgoing: f32, incoming: f32, t: f32, span: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * a + (t3 - 2.0 * t2 + t) * span * outgoing + (-2.0 * t3 + 3.0 * t2) * b + (t3 - t2) * span * incoming
}

/// Wrap or clamp an unbounded play position into a clip. Returns the in-clip
/// time and the loop cycle it is in.
pub fn wrap_position(position: f64, duration: f32, looping: bool) -> (f32, i64) {
    let dur = duration as f64;
    if dur <= 0.0 {
        return (0.0, 0);
    }
    if looping {
        let cycle = (position / dur).floor();
        ((position - cycle * dur) as f32, cycle as i64)
    } else {
        (position.clamp(0.0, dur) as f32, 0)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::animation::{AnimationClip, BoneTrack, Keyframe};
    use crate::assets::AnimSeqAsset;
    use crate::math::{Transform, Vec3 as AV3};
    use crate::runtime::pose::tests::chain_skeleton;

    pub(crate) fn clip_asset(name: &str, duration: f32, tracks: Vec<BoneTrack>) -> AnimSeqAsset {
        AnimSeqAsset::new(None, AnimationClip { name: name.into(), duration, fps: 30.0, tracks })
    }

    pub(crate) fn translate_track(bone: &str, from: [f32; 3], to: [f32; 3], duration: f32) -> BoneTrack {
        let t = |p: [f32; 3]| Transform { translation: AV3::new(p[0], p[1], p[2]), ..Transform::IDENTITY };
        BoneTrack { bone_id: bone.into(), keyframes: vec![Keyframe::linear(0.0, t(from)), Keyframe::linear(duration, t(to))] }
    }

    #[test]
    fn samples_between_keys_and_holds_outside() {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(2)).unwrap();
        let asset = clip_asset("c", 1.0, vec![translate_track("b1", [0.0, 1.0, 0.0], [2.0, 1.0, 0.0], 1.0)]);
        let clip = CompiledClip::compile(&asset, &skel);
        assert!((clip.sample(&skel, 0.5).bones[1].translation.x - 1.0).abs() < 1e-5);
        assert!((clip.sample(&skel, 9.0).bones[1].translation.x - 2.0).abs() < 1e-5);
        assert_eq!(clip.sample(&skel, 0.5).bones[0].translation, skel.bind[0].translation);
    }

    #[test]
    fn root_motion_composes_across_loop_seams_and_reverses() {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(2)).unwrap();
        let mut asset = clip_asset("c", 1.0, vec![translate_track("b0", [0.0, 0.0, 0.0], [0.0, 0.0, 4.0], 1.0)]);
        asset.features.root_motion.enabled = true;
        let clip = CompiledClip::compile(&asset, &skel);
        let d = clip.root_motion_between(&skel, 0.0, 0.5, true);
        assert!((d.translation.z - 2.0).abs() < 1e-4);
        // 0.75 -> 2.25 crosses two seams: 1 (to end) + 4 (a cycle) + 1 (0..0.25)... = 6
        let across = clip.root_motion_between(&skel, 0.75, 2.25, true);
        assert!((across.translation.z - 6.0).abs() < 1e-3, "{:?}", across.translation);
        // splitting the interval changes nothing
        let a = clip.root_motion_between(&skel, 0.75, 1.5, true);
        let b = clip.root_motion_between(&skel, 1.5, 2.25, true);
        assert!((a.then(&b).translation - across.translation).length() < 1e-4);
        let back = clip.root_motion_between(&skel, 0.5, 0.0, true);
        assert!((back.translation.z + 2.0).abs() < 1e-4);
        // not looping: motion stops at the end
        let held = clip.root_motion_between(&skel, 0.5, 3.0, false);
        assert!((held.translation.z - 2.0).abs() < 1e-4);
    }

    #[test]
    fn removing_root_motion_leaves_the_residual_pose_in_place() {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(2)).unwrap();
        let mut asset = clip_asset("c", 1.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 4.0], 1.0)]);
        asset.features.root_motion.enabled = true;
        let clip = CompiledClip::compile(&asset, &skel);
        let mut pose = clip.sample(&skel, 0.5);
        assert!((pose.bones[0].translation.z - 2.0).abs() < 1e-4);
        clip.remove_root_motion(&skel, &mut pose.bones, 0.5);
        assert!(pose.bones[0].translation.z.abs() < 1e-4);
        // the child is untouched in local space
        assert!((pose.bones[1].translation - skel.bind[1].translation).length() < 1e-5);
    }
}
