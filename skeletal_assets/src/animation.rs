//! Animation clip data model and pose evaluation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::math::{Mat4, Transform};
use super::skeleton::Skeleton;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keyframe {
    /// Time in seconds.
    pub time: f32,
    pub transform: Transform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoneTrack {
    pub bone_id: String,
    pub keyframes: Vec<Keyframe>,
}

impl BoneTrack {
    /// Sample the track's transform at `time`, holding the nearest keyframe
    /// outside the track's range and interpolating linearly between the two
    /// surrounding keyframes inside it.
    pub fn sample(&self, time: f32) -> Option<Transform> {
        let kfs = &self.keyframes;
        if kfs.is_empty() {
            return None;
        }
        if kfs.len() == 1 || time <= kfs[0].time {
            return Some(kfs[0].transform.clone());
        }
        if time >= kfs[kfs.len() - 1].time {
            return Some(kfs[kfs.len() - 1].transform.clone());
        }
        for i in 0..kfs.len() - 1 {
            let a = &kfs[i];
            let b = &kfs[i + 1];
            if time >= a.time && time <= b.time {
                let span = (b.time - a.time).max(1e-6);
                let t = (time - a.time) / span;
                return Some(a.transform.lerp(&b.transform, t));
            }
        }
        Some(kfs[kfs.len() - 1].transform.clone())
    }
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
