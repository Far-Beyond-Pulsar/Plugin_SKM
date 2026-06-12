//! Sample skeleton + idle animation used when creating a new asset or running
//! the standalone example.

use super::animation::{AnimationClip, BoneTrack, Keyframe};
use super::math::{Transform, Vec3};
use super::skeleton::{Bone, Skeleton};

fn bone(id: &str, name: &str, parent: Option<&str>, translation: Vec3) -> Bone {
    Bone {
        id: id.to_string(),
        name: name.to_string(),
        parent: parent.map(|p| p.to_string()),
        bind_transform: Transform {
            translation,
            ..Transform::IDENTITY
        },
    }
}

/// A small humanoid rig: pelvis -> spine -> chest -> {neck->head, arms} and legs.
pub fn humanoid_skeleton() -> Skeleton {
    let bones = vec![
        bone("pelvis", "Pelvis", None, Vec3::new(0.0, 1.0, 0.0)),
        bone("spine", "Spine", Some("pelvis"), Vec3::new(0.0, 0.18, 0.0)),
        bone("chest", "Chest", Some("spine"), Vec3::new(0.0, 0.28, 0.0)),
        bone("neck", "Neck", Some("chest"), Vec3::new(0.0, 0.22, 0.0)),
        bone("head", "Head", Some("neck"), Vec3::new(0.0, 0.14, 0.0)),
        // Left arm
        bone("shoulder_l", "Shoulder.L", Some("chest"), Vec3::new(-0.18, 0.20, 0.0)),
        bone("upper_arm_l", "UpperArm.L", Some("shoulder_l"), Vec3::new(-0.26, 0.0, 0.0)),
        bone("lower_arm_l", "LowerArm.L", Some("upper_arm_l"), Vec3::new(-0.24, 0.0, 0.0)),
        bone("hand_l", "Hand.L", Some("lower_arm_l"), Vec3::new(-0.18, 0.0, 0.0)),
        // Right arm
        bone("shoulder_r", "Shoulder.R", Some("chest"), Vec3::new(0.18, 0.20, 0.0)),
        bone("upper_arm_r", "UpperArm.R", Some("shoulder_r"), Vec3::new(0.26, 0.0, 0.0)),
        bone("lower_arm_r", "LowerArm.R", Some("upper_arm_r"), Vec3::new(0.24, 0.0, 0.0)),
        bone("hand_r", "Hand.R", Some("lower_arm_r"), Vec3::new(0.18, 0.0, 0.0)),
        // Left leg
        bone("thigh_l", "Thigh.L", Some("pelvis"), Vec3::new(-0.10, -0.05, 0.0)),
        bone("shin_l", "Shin.L", Some("thigh_l"), Vec3::new(0.0, -0.42, 0.0)),
        bone("foot_l", "Foot.L", Some("shin_l"), Vec3::new(0.0, -0.40, 0.05)),
        // Right leg
        bone("thigh_r", "Thigh.R", Some("pelvis"), Vec3::new(0.10, -0.05, 0.0)),
        bone("shin_r", "Shin.R", Some("thigh_r"), Vec3::new(0.0, -0.42, 0.0)),
        bone("foot_r", "Foot.R", Some("shin_r"), Vec3::new(0.0, -0.40, 0.05)),
    ];

    Skeleton { bones }
}

fn track(bone_id: &str, keyframes: Vec<(f32, Vec3, Vec3)>) -> BoneTrack {
    BoneTrack {
        bone_id: bone_id.to_string(),
        keyframes: keyframes
            .into_iter()
            .map(|(time, translation, rotation)| Keyframe {
                time,
                transform: Transform {
                    translation,
                    rotation,
                    scale: Vec3::ONE,
                },
            })
            .collect(),
    }
}

/// A gentle 2-second idle loop: breathing chest, swaying arms, bobbing head.
pub fn idle_animation(skeleton: &Skeleton) -> AnimationClip {
    let bind = |id: &str| -> Vec3 {
        skeleton
            .bone(id)
            .map(|b| b.bind_transform.translation)
            .unwrap_or(Vec3::ZERO)
    };

    let tracks = vec![
        track("chest", vec![
            (0.0, bind("chest"), Vec3::new(0.0, 0.0, 0.0)),
            (1.0, bind("chest"), Vec3::new(-2.0, 0.0, 0.0)),
            (2.0, bind("chest"), Vec3::new(0.0, 0.0, 0.0)),
        ]),
        track("head", vec![
            (0.0, bind("head"), Vec3::new(0.0, 0.0, 0.0)),
            (1.0, bind("head"), Vec3::new(3.0, 5.0, 0.0)),
            (2.0, bind("head"), Vec3::new(0.0, 0.0, 0.0)),
        ]),
        track("upper_arm_l", vec![
            (0.0, bind("upper_arm_l"), Vec3::new(0.0, 0.0, -4.0)),
            (1.0, bind("upper_arm_l"), Vec3::new(0.0, 0.0, 6.0)),
            (2.0, bind("upper_arm_l"), Vec3::new(0.0, 0.0, -4.0)),
        ]),
        track("upper_arm_r", vec![
            (0.0, bind("upper_arm_r"), Vec3::new(0.0, 0.0, 4.0)),
            (1.0, bind("upper_arm_r"), Vec3::new(0.0, 0.0, -6.0)),
            (2.0, bind("upper_arm_r"), Vec3::new(0.0, 0.0, 4.0)),
        ]),
        track("pelvis", vec![
            (0.0, Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO),
            (1.0, Vec3::new(0.0, 1.02, 0.0), Vec3::ZERO),
            (2.0, Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO),
        ]),
    ];

    AnimationClip {
        name: "Idle".to_string(),
        duration: 2.0,
        fps: 30.0,
        tracks,
    }
}
