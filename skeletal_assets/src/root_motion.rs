//! rigid root motion, independent of pose modification and actor movement.

use super::{
    animation::{bone_world_transform, AnimationClip},
    math::{Mat4, Transform, Vec3},
    notifies::PlaybackInterval,
    skeleton::Skeleton,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RootMotionSettings {
    pub enabled: bool,
    /// None uses the skeleton's first root, in asset order.
    pub source_bone: Option<String>,
    pub translation: [bool; 3],
    pub rotation: bool,
}
impl Default for RootMotionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            source_bone: None,
            translation: [true; 3],
            rotation: true,
        }
    }
}
impl RootMotionSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn validate(&self) -> Result<(), String> {
        if self
            .source_bone
            .as_ref()
            .is_some_and(|s| s.trim().is_empty())
        {
            return Err("Root-motion source cannot be empty".into());
        }
        Ok(())
    }
}

/// returns a rigid delta for right composition: object_next = object * delta.
/// the source is evaluated in skeleton/model space, relative to its pose at zero.
/// full loop transforms compose before the partial cycle, including rotation.
/// positive uniform scale is removed; nonuniform, mirrored, singular or sheared
/// source chains return an error instead of producing misleading rigid motion.
/// this never changes a pose or moves the preview. a runtime applying the delta
/// must remove that same motion from its rendered pose to avoid applying it twice.
pub fn extract_root_motion(
    skeleton: &Skeleton,
    clip: &AnimationClip,
    settings: &RootMotionSettings,
    interval: PlaybackInterval,
) -> Result<Mat4, String> {
    settings.validate()?;
    let PlaybackInterval::Advance { from, to, looping } = interval else {
        return Ok(Mat4::IDENTITY);
    };
    if !clip.duration.is_finite() || clip.duration < 0.0 || !from.is_finite() || !to.is_finite() {
        return Err("Playback times must be finite and duration nonnegative".into());
    }
    if !settings.enabled || clip.duration == 0.0 || from == to {
        return Ok(Mat4::IDENTITY);
    }
    let source = settings
        .source_bone
        .as_deref()
        .or_else(|| {
            skeleton
                .bones
                .iter()
                .find(|b| b.parent.is_none())
                .map(|b| b.id.as_str())
        })
        .ok_or("No root-motion source bone")?;
    let start = rigid_source(skeleton, clip, source, 0.0)?;
    let origin_inverse = start.inverse();
    let relative = |time| -> Result<Mat4, String> {
        let matrix = rigid_source(skeleton, clip, source, time)?.mul(&origin_inverse);
        let mut transform = Transform::try_from_matrix(&matrix)
            .ok_or("Root motion is not a finite rigid transform")?;
        let mut translation = transform.translation.to_array();
        for (axis, enabled) in settings.translation.iter().enumerate() {
            if !enabled {
                translation[axis] = 0.0;
            }
        }
        transform.translation = Vec3::new(translation[0], translation[1], translation[2]);
        if !settings.rotation {
            transform.rotation = Vec3::ZERO;
        }
        Ok(transform.to_matrix())
    };
    let cycle = relative(clip.duration)?;
    let result = if looping {
        let duration = clip.duration as f64;
        let from_cycle = (from / duration).floor();
        let to_cycle = (to / duration).floor();
        if from_cycle.abs().max(to_cycle.abs()) > (1u64 << 52) as f64 {
            return Err("Playback interval is too large".into());
        }
        // cancel common cycles before composing: inverse(Ra) * C^(b-a) * Rb.
        let from_phase = relative(from.rem_euclid(duration) as f32)?;
        let to_phase = relative(to.rem_euclid(duration) as f32)?;
        from_phase
            .inverse()
            .mul(&rigid_power(cycle, (to_cycle - from_cycle) as i64)?)
            .mul(&to_phase)
    } else {
        relative(from.clamp(0.0, clip.duration as f64) as f32)?
            .inverse()
            .mul(&relative(to.clamp(0.0, clip.duration as f64) as f32)?)
    };
    if !result.0.iter().all(|v| v.is_finite()) {
        return Err("Root-motion delta overflowed".into());
    }
    Ok(result)
}

fn rigid_source(
    skeleton: &Skeleton,
    clip: &AnimationClip,
    source: &str,
    time: f32,
) -> Result<Mat4, String> {
    let mut bone = Some(
        skeleton
            .bone(source)
            .ok_or("Root-motion source bone is missing")?,
    );
    let mut count = 0;
    while let Some(current) = bone {
        count += 1;
        if count > skeleton.bones.len() {
            return Err("Root-motion source hierarchy is cyclic".into());
        }
        let transform = clip.local_transform(skeleton, &current.id, time);
        let scale = transform.scale.to_array();
        if !transform.to_matrix().0.iter().all(|v| v.is_finite())
            || scale[0] <= 1e-8
            || scale
                .iter()
                .any(|s| (*s - scale[0]).abs() > 1e-5 * scale[0])
        {
            return Err(
                "Root motion requires positive uniform scale along the source chain".into(),
            );
        }
        bone = match current.parent.as_deref() {
            Some(id) => Some(
                skeleton
                    .bone(id)
                    .ok_or("Root-motion source parent is missing")?,
            ),
            None => None,
        };
    }
    let matrix = bone_world_transform(skeleton, clip, source, time)
        .ok_or("Root-motion source is invalid")?;
    let mut transform = Transform::try_from_matrix(&matrix)
        .ok_or("Root-motion source has shear or singular scale")?;
    transform.scale = Vec3::ONE;
    Ok(transform.to_matrix())
}

fn rigid_power(mut base: Mat4, exponent: i64) -> Result<Mat4, String> {
    let mut count = exponent.unsigned_abs();
    if exponent < 0 {
        base = base.inverse();
    }
    let mut result = Mat4::IDENTITY;
    while count > 0 {
        if count & 1 == 1 {
            result = normalized_rigid(result.mul(&base))?;
        }
        count >>= 1;
        if count > 0 {
            base = normalized_rigid(base.mul(&base))?;
        }
    }
    Ok(result)
}
fn normalized_rigid(matrix: Mat4) -> Result<Mat4, String> {
    let mut transform =
        Transform::try_from_matrix(&matrix).ok_or("Root-motion composition lost precision")?;
    transform.scale = Vec3::ONE;
    Ok(transform.to_matrix())
}

/// removes a zero-to-sample extraction from evaluated model-space bone matrices.
/// apply the interval delta to the object separately, then render this residual pose.
/// pass a non-looping zero-to-phase extraction here, even when the object delta
/// crosses loops. a frame-to-frame delta does not remove the clip's accumulated pose motion.
pub fn remove_root_motion_from_pose(
    world: &mut std::collections::HashMap<String, Mat4>,
    extracted: &Mat4,
) -> Result<(), String> {
    let transform =
        Transform::try_from_matrix(extracted).ok_or("Extracted motion must be rigid and finite")?;
    if transform.scale.sub(Vec3::ONE).length() > 1e-4 {
        return Err("Extracted motion must not contain scale".into());
    }
    let inverse = extracted.inverse();
    for matrix in world.values_mut() {
        *matrix = inverse.mul(matrix);
    }
    Ok(())
}
