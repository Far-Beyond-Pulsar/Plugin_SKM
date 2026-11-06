//! Animation clip data model and pose evaluation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::math::{Mat4, Transform, Vec3};
use super::skeleton::Skeleton;

/// Interpolation used from this key to the next key for one transform channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyInterpolation {
    #[default]
    Linear,
    Step,
    CubicSpline,
}

impl KeyInterpolation {
    fn is_linear(&self) -> bool {
        *self == Self::Linear
    }
}

/// Per-second glTF spline tangents, stored as components so quaternion tangents
/// survive the `.animseq` round trip without being converted to Euler angles.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct KeyframeTangent {
    #[serde(default)]
    pub translation: [f32; 3],
    #[serde(default)]
    pub rotation: [f32; 4],
    #[serde(default)]
    pub scale: [f32; 3],
}

impl KeyframeTangent {
    fn is_zero(&self) -> bool {
        self.translation == [0.0; 3] && self.rotation == [0.0; 4] && self.scale == [0.0; 3]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keyframe {
    /// Time in seconds.
    pub time: f32,
    pub transform: Transform,
    /// Interpolation from this key to the next, independently per channel.
    #[serde(default, skip_serializing_if = "KeyInterpolation::is_linear")]
    pub translation_interpolation: KeyInterpolation,
    #[serde(default, skip_serializing_if = "KeyInterpolation::is_linear")]
    pub rotation_interpolation: KeyInterpolation,
    #[serde(default, skip_serializing_if = "KeyInterpolation::is_linear")]
    pub scale_interpolation: KeyInterpolation,
    /// Cubic spline tangents. Linear/step keys ignore these values.
    #[serde(default, skip_serializing_if = "KeyframeTangent::is_zero")]
    pub in_tangent: KeyframeTangent,
    #[serde(default, skip_serializing_if = "KeyframeTangent::is_zero")]
    pub out_tangent: KeyframeTangent,
    /// Quaternion components `[x, y, z, w]`. Cubic subdivision keys keep
    /// the raw polynomial value: normalize only after interpolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_quaternion: Option<[f32; 4]>,
}

impl Keyframe {
    pub(crate) fn has_interpolation_data(&self) -> bool {
        self.translation_interpolation != KeyInterpolation::Linear
            || self.rotation_interpolation != KeyInterpolation::Linear
            || self.scale_interpolation != KeyInterpolation::Linear
            || self.rotation_quaternion.is_some()
            || !self.in_tangent.is_zero()
            || !self.out_tangent.is_zero()
    }

    pub fn linear(time: f32, transform: Transform) -> Self {
        Self {
            time,
            transform,
            translation_interpolation: KeyInterpolation::Linear,
            rotation_interpolation: KeyInterpolation::Linear,
            scale_interpolation: KeyInterpolation::Linear,
            in_tangent: KeyframeTangent::default(),
            out_tangent: KeyframeTangent::default(),
            rotation_quaternion: None,
        }
    }

    /// Change an authored pose without leaving an imported quaternion stale.
    /// Translation/scale edits retain the raw quaternion and all derivatives.
    pub fn set_transform(&mut self, transform: Transform) {
        if transform.rotation != self.transform.rotation {
            if let Some(previous) = self.rotation_quaternion {
                let mut q = quaternion_from_euler(transform.rotation);
                // Keep the authored hemisphere; a sign flip changes a cubic.
                if q.dot(glam::Quat::from_array(previous)) < 0.0 {
                    q = -q;
                }
                self.rotation_quaternion = Some(q.to_array());
            }
        }
        self.transform = transform;
    }

    fn sampled_transform(&self) -> Transform {
        let mut transform = self.transform.clone();
        if let Some(q) = self.rotation_quaternion.and_then(normalize_quaternion) {
            transform.rotation = Transform::from_trs([0.0; 3], q.to_array(), [1.0; 3]).rotation;
        }
        transform
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoneTrack {
    pub bone_id: String,
    pub keyframes: Vec<Keyframe>,
}

impl BoneTrack {
    /// Insert a pose key without changing the curve on either side. Cubic
    /// segments are split using their raw values and time derivatives.
    pub fn insert_keyframe(&mut self, time: f32, fallback: Transform) -> usize {
        let pos = self.keyframes.partition_point(|k| k.time < time);
        if self.keyframes.get(pos).is_some_and(|k| k.time == time) {
            return pos;
        }
        let mut key = Keyframe::linear(time, self.sample(time).unwrap_or(fallback));
        if pos > 0 && pos < self.keyframes.len() {
            let (a, b) = (&self.keyframes[pos - 1], &self.keyframes[pos]);
            let span = b.time - a.time;
            if span.is_finite() && span > 0.0 {
                let t = (time - a.time) / span;
                key.translation_interpolation = a.translation_interpolation;
                key.rotation_interpolation = a.rotation_interpolation;
                key.scale_interpolation = a.scale_interpolation;
                for i in 0..3 {
                    if a.translation_interpolation == KeyInterpolation::CubicSpline {
                        key.in_tangent.translation[i] = hermite_derivative(
                            a.transform.translation.to_array()[i],
                            b.transform.translation.to_array()[i],
                            a.out_tangent.translation[i],
                            b.in_tangent.translation[i],
                            t,
                            span,
                        );
                    }
                    if a.scale_interpolation == KeyInterpolation::CubicSpline {
                        key.in_tangent.scale[i] = hermite_derivative(
                            a.transform.scale.to_array()[i],
                            b.transform.scale.to_array()[i],
                            a.out_tangent.scale[i],
                            b.in_tangent.scale[i],
                            t,
                            span,
                        );
                    }
                }
                if let (Some(qa), Some(qb)) = (a.rotation_quaternion, b.rotation_quaternion) {
                    key.rotation_quaternion = Some(
                        if a.rotation_interpolation == KeyInterpolation::CubicSpline {
                            std::array::from_fn(|i| {
                                hermite(
                                    qa[i],
                                    qb[i],
                                    a.out_tangent.rotation[i],
                                    b.in_tangent.rotation[i],
                                    t,
                                    span,
                                )
                            })
                        } else {
                            quaternion_from_euler(key.transform.rotation).to_array()
                        },
                    );
                    if a.rotation_interpolation == KeyInterpolation::CubicSpline {
                        key.in_tangent.rotation = std::array::from_fn(|i| {
                            hermite_derivative(
                                qa[i],
                                qb[i],
                                a.out_tangent.rotation[i],
                                b.in_tangent.rotation[i],
                                t,
                                span,
                            )
                        });
                    }
                }
                key.out_tangent = key.in_tangent;
            }
        } else {
            // Extend the held endpoint. Linear/step keep their legacy modes;
            // only a cubic's unused endpoint tangents must not extrapolate.
            if let Some(last) = self.keyframes.last_mut().filter(|_| pos > 0) {
                if last.translation_interpolation == KeyInterpolation::CubicSpline {
                    last.translation_interpolation = KeyInterpolation::Step;
                }
                if last.rotation_interpolation == KeyInterpolation::CubicSpline {
                    last.rotation_interpolation = KeyInterpolation::Step;
                }
                if last.scale_interpolation == KeyInterpolation::CubicSpline {
                    last.scale_interpolation = KeyInterpolation::Step;
                }
                key.translation_interpolation = last.translation_interpolation;
                key.rotation_interpolation = last.rotation_interpolation;
                key.scale_interpolation = last.scale_interpolation;
                key.rotation_quaternion = last.rotation_quaternion;
            } else if let Some(first) = self.keyframes.first() {
                key.rotation_quaternion = first.rotation_quaternion;
                let held = |mode| {
                    if mode == KeyInterpolation::CubicSpline {
                        KeyInterpolation::Step
                    } else {
                        mode
                    }
                };
                key.translation_interpolation = held(first.translation_interpolation);
                key.rotation_interpolation = held(first.rotation_interpolation);
                key.scale_interpolation = held(first.scale_interpolation);
            }
        }
        self.keyframes.insert(pos, key);
        pos
    }

    /// Sample the track's transform at `time`, holding the nearest keyframe
    /// outside the track's range and applying each channel's interpolation
    /// mode between surrounding keys.
    pub fn sample(&self, time: f32) -> Option<Transform> {
        let kfs = &self.keyframes;
        if kfs.is_empty() {
            return None;
        }
        if kfs.len() == 1 || time.is_nan() || time <= kfs[0].time {
            return Some(kfs[0].sampled_transform());
        }
        if time >= kfs[kfs.len() - 1].time {
            return Some(kfs[kfs.len() - 1].sampled_transform());
        }
        for pair in kfs.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if time >= a.time && time <= b.time {
                if time == b.time {
                    return Some(b.sampled_transform());
                }
                let span = b.time - a.time;
                return Some(if span.is_finite() && span > 0.0 {
                    sample_pair(a, b, (time - a.time) / span, span)
                } else {
                    a.sampled_transform()
                });
            }
        }
        Some(kfs[kfs.len() - 1].sampled_transform())
    }
}

fn sample_pair(a: &Keyframe, b: &Keyframe, t: f32, span: f32) -> Transform {
    let mut result = a.transform.lerp(&b.transform, t);
    if a.translation_interpolation == KeyInterpolation::Step {
        result.translation = a.transform.translation;
    } else if a.translation_interpolation == KeyInterpolation::CubicSpline {
        result.translation = Vec3::new(
            hermite(
                a.transform.translation.x,
                b.transform.translation.x,
                a.out_tangent.translation[0],
                b.in_tangent.translation[0],
                t,
                span,
            ),
            hermite(
                a.transform.translation.y,
                b.transform.translation.y,
                a.out_tangent.translation[1],
                b.in_tangent.translation[1],
                t,
                span,
            ),
            hermite(
                a.transform.translation.z,
                b.transform.translation.z,
                a.out_tangent.translation[2],
                b.in_tangent.translation[2],
                t,
                span,
            ),
        );
    }
    if a.scale_interpolation == KeyInterpolation::Step {
        result.scale = a.transform.scale;
    } else if a.scale_interpolation == KeyInterpolation::CubicSpline {
        result.scale = Vec3::new(
            hermite(
                a.transform.scale.x,
                b.transform.scale.x,
                a.out_tangent.scale[0],
                b.in_tangent.scale[0],
                t,
                span,
            ),
            hermite(
                a.transform.scale.y,
                b.transform.scale.y,
                a.out_tangent.scale[1],
                b.in_tangent.scale[1],
                t,
                span,
            ),
            hermite(
                a.transform.scale.z,
                b.transform.scale.z,
                a.out_tangent.scale[2],
                b.in_tangent.scale[2],
                t,
                span,
            ),
        );
    }
    if let (Some(qa), Some(qb)) = (a.rotation_quaternion, b.rotation_quaternion) {
        let fallback = || {
            normalize_quaternion(qa).unwrap_or_else(|| quaternion_from_euler(a.transform.rotation))
        };
        let q = match a.rotation_interpolation {
            KeyInterpolation::Step => fallback(),
            KeyInterpolation::Linear => normalize_quaternion(qb)
                .map(|qb| crate::runtime::pose::slerp_short(fallback(), qb, t))
                .unwrap_or_else(fallback),
            KeyInterpolation::CubicSpline => cubic_quaternion(
                qa,
                qb,
                a.out_tangent.rotation,
                b.in_tangent.rotation,
                t,
                span,
            )
            .unwrap_or_else(fallback),
        };
        result.rotation = Transform::from_trs([0.0; 3], q.to_array(), [1.0; 3]).rotation;
    } else if a.rotation_interpolation == KeyInterpolation::Step
        || a.rotation_interpolation == KeyInterpolation::CubicSpline
    {
        result.rotation = a.sampled_transform().rotation;
    }
    result
}

/// Convert just the rotation: zero/negative scales must not affect it.
pub(crate) fn quaternion_from_euler(rotation: Vec3) -> glam::Quat {
    glam::Quat::from_mat4(&glam::Mat4::from_cols_array(
        &Mat4::from_euler_xyz_deg(rotation).0,
    ))
}

#[inline]
pub(crate) fn normalize_quaternion(q: [f32; 4]) -> Option<glam::Quat> {
    let value = glam::Quat::from_array(q);
    let length = value.length();
    if length.is_finite() {
        return (length > 1e-8).then(|| value / length);
    }
    // Large finite components can overflow the squared length. Rescale only
    // in that uncommon case; NaN/Inf inputs use the caller's fallback.
    if value.is_finite() {
        let largest = q.iter().map(|v| v.abs()).fold(0.0, f32::max);
        return Some((value / largest).normalize());
    }
    None
}

#[inline]
pub(crate) fn cubic_quaternion(
    a: [f32; 4],
    b: [f32; 4],
    outgoing: [f32; 4],
    incoming: [f32; 4],
    t: f32,
    span: f32,
) -> Option<glam::Quat> {
    // glTF specifies component-wise Hermite, followed by normalization.
    // No hemisphere correction, slerp or tangent normalization here.
    normalize_quaternion(std::array::from_fn(|i| {
        hermite_value(a[i], b[i], outgoing[i], incoming[i], t, span)
    }))
}

#[inline]
pub(crate) fn hermite(a: f32, b: f32, outgoing: f32, incoming: f32, t: f32, span: f32) -> f32 {
    let value = hermite_value(a, b, outgoing, incoming, t, span);
    if value.is_finite() {
        value
    } else {
        a
    }
}

#[inline]
fn hermite_value(a: f32, b: f32, outgoing: f32, incoming: f32, t: f32, span: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * a
        + (t3 - 2.0 * t2 + t) * span * outgoing
        + (-2.0 * t3 + 3.0 * t2) * b
        + (t3 - t2) * span * incoming
}

fn hermite_derivative(a: f32, b: f32, outgoing: f32, incoming: f32, t: f32, span: f32) -> f32 {
    let t2 = t * t;
    ((6.0 * t2 - 6.0 * t) * a + (-6.0 * t2 + 6.0 * t) * b) / span
        + (3.0 * t2 - 4.0 * t + 1.0) * outgoing
        + (3.0 * t2 - 2.0 * t) * incoming
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationClip {
    pub name: String,
    /// Total length of the clip in seconds.
    pub duration: f32,
    pub fps: f32,
    pub tracks: Vec<BoneTrack>,
}

impl AnimationClip {
    pub(crate) fn validate_keys(&self) -> Result<(), String> {
        for track in &self.tracks {
            for key in &track.keyframes {
                let finite = key.time.is_finite()
                    && key
                        .transform
                        .translation
                        .to_array()
                        .iter()
                        .chain(&key.transform.rotation.to_array())
                        .chain(&key.transform.scale.to_array())
                        .chain(&key.in_tangent.translation)
                        .chain(&key.in_tangent.rotation)
                        .chain(&key.in_tangent.scale)
                        .chain(&key.out_tangent.translation)
                        .chain(&key.out_tangent.rotation)
                        .chain(&key.out_tangent.scale)
                        .all(|v| v.is_finite())
                    && key
                        .rotation_quaternion
                        .is_none_or(|q| q.iter().all(|v| v.is_finite()));
                if !finite {
                    return Err(format!(
                        "track '{}' has a non-finite key value, time or tangent",
                        track.bone_id
                    ));
                }
            }
        }
        Ok(())
    }

    /// A clip with no tracks: every bone holds its bind pose. What a skeleton
    /// or a skeletal mesh is shown with when nothing animates it.
    pub fn bind_pose() -> Self {
        Self {
            name: "Bind pose".to_string(),
            duration: 1.0,
            fps: 30.0,
            tracks: Vec::new(),
        }
    }

    /// Frames per second, never below 1.
    pub fn fps_clamped(&self) -> f32 {
        self.fps.max(1.0)
    }

    /// Nearest frame to `time`.
    pub fn time_to_frame(&self, time: f32) -> i32 {
        (time * self.fps_clamped()).round() as i32
    }

    pub fn frame_to_time(&self, frame: i32) -> f32 {
        frame as f32 / self.fps_clamped()
    }

    /// `time` snapped to the nearest frame boundary.
    pub fn snap_time(&self, time: f32) -> f32 {
        self.frame_to_time(self.time_to_frame(time))
    }

    /// Index of the last whole frame of the clip.
    pub fn last_frame(&self) -> i32 {
        self.time_to_frame(self.duration).max(0)
    }

    /// Total number of keyframes across all tracks.
    pub fn key_count(&self) -> usize {
        self.tracks.iter().map(|t| t.keyframes.len()).sum()
    }

    pub fn track(&self, bone_id: &str) -> Option<&BoneTrack> {
        self.tracks.iter().find(|t| t.bone_id == bone_id)
    }

    pub fn track_mut(&mut self, bone_id: &str) -> Option<&mut BoneTrack> {
        self.tracks.iter_mut().find(|t| t.bone_id == bone_id)
    }

    /// Local transform of `bone_id` at `time`, falling back to the skeleton's
    /// bind pose if the bone has no animation track.
    pub fn local_transform(&self, skeleton: &Skeleton, bone_id: &str, time: f32) -> Transform {
        self.track(bone_id)
            .and_then(|t| t.sample(time))
            .unwrap_or_else(|| {
                skeleton
                    .bone(bone_id)
                    .map(|b| b.bind_transform.clone())
                    .unwrap_or_default()
            })
    }
}

/// Compute every bone's world-space matrix at `time`.
pub fn evaluate_world_transforms(
    skeleton: &Skeleton,
    clip: &AnimationClip,
    time: f32,
) -> HashMap<String, Mat4> {
    let mut world = HashMap::with_capacity(skeleton.bones.len());
    for (bone, _depth) in skeleton.depth_first() {
        let local = clip.local_transform(skeleton, &bone.id, time).to_matrix();
        let parent_world = bone
            .parent
            .as_ref()
            .and_then(|p| world.get(p))
            .copied()
            .unwrap_or(Mat4::IDENTITY);
        world.insert(bone.id.clone(), parent_world.mul(&local));
    }
    world
}

/// World-space matrix of a single bone at `time`, composed along its chain of
/// ancestors. Cheaper than [`evaluate_world_transforms`] when only one bone is
/// needed (motion paths sample the same bone at many times).
pub fn bone_world_transform(
    skeleton: &Skeleton,
    clip: &AnimationClip,
    bone_id: &str,
    time: f32,
) -> Option<Mat4> {
    let mut chain = Vec::new();
    let mut current = skeleton.bone(bone_id);
    // Bound the walk so a malformed (cyclic) hierarchy can't hang the editor.
    while let Some(bone) = current {
        chain.push(bone);
        if chain.len() > skeleton.bones.len() {
            return None;
        }
        current = bone.parent.as_deref().and_then(|p| skeleton.bone(p));
    }
    if chain.is_empty() {
        return None;
    }
    let mut world = Mat4::IDENTITY;
    for bone in chain.iter().rev() {
        let local = clip.local_transform(skeleton, &bone.id, time).to_matrix();
        world = world.mul(&local);
    }
    Some(world)
}

impl AnimationClip {
    /// Tracks whose bone does not exist in `skeleton`. They are kept in the
    /// clip (nothing is deleted) but have no effect on the pose.
    pub fn orphan_tracks<'a>(&'a self, skeleton: &Skeleton) -> Vec<&'a BoneTrack> {
        self.tracks
            .iter()
            .filter(|t| skeleton.bone(&t.bone_id).is_none())
            .collect()
    }
}
