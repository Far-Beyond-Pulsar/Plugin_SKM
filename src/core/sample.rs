//! Sample skeleton + animations used when creating a new asset or running
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

/// A detailed humanoid rig:
/// `root -> pelvis -> spine_01 -> spine_02 -> chest -> {neck -> head, arms}`
/// and legs, with clavicles, fingers, and toes for extra silhouette detail.
pub fn humanoid_skeleton() -> Skeleton {
    let bones = vec![
        bone("root", "Root", None, Vec3::new(0.0, 1.0, 0.0)),
        bone("pelvis", "Pelvis", Some("root"), Vec3::ZERO),
        bone("spine_01", "Spine_01", Some("pelvis"), Vec3::new(0.0, 0.10, 0.0)),
        bone("spine_02", "Spine_02", Some("spine_01"), Vec3::new(0.0, 0.12, 0.0)),
        bone("chest", "Chest", Some("spine_02"), Vec3::new(0.0, 0.14, 0.0)),
        bone("neck", "Neck", Some("chest"), Vec3::new(0.0, 0.20, 0.0)),
        bone("head", "Head", Some("neck"), Vec3::new(0.0, 0.14, 0.0)),
        // Left arm: hangs down at the side (bind pose), so a sagittal
        // (X-axis) swing pumps it forward/back like the legs.
        bone("clavicle_l", "Clavicle.L", Some("chest"), Vec3::new(-0.06, 0.18, 0.0)),
        bone("shoulder_l", "Shoulder.L", Some("clavicle_l"), Vec3::new(-0.12, 0.02, 0.0)),
        bone("upper_arm_l", "UpperArm.L", Some("shoulder_l"), Vec3::new(0.0, -0.26, 0.0)),
        bone("lower_arm_l", "LowerArm.L", Some("upper_arm_l"), Vec3::new(0.0, -0.24, 0.0)),
        bone("hand_l", "Hand.L", Some("lower_arm_l"), Vec3::new(0.0, -0.18, 0.0)),
        bone("fingers_l", "Fingers.L", Some("hand_l"), Vec3::new(0.0, -0.10, 0.0)),
        // Right arm: mirrored, also hanging down at the side.
        bone("clavicle_r", "Clavicle.R", Some("chest"), Vec3::new(0.06, 0.18, 0.0)),
        bone("shoulder_r", "Shoulder.R", Some("clavicle_r"), Vec3::new(0.12, 0.02, 0.0)),
        bone("upper_arm_r", "UpperArm.R", Some("shoulder_r"), Vec3::new(0.0, -0.26, 0.0)),
        bone("lower_arm_r", "LowerArm.R", Some("upper_arm_r"), Vec3::new(0.0, -0.24, 0.0)),
        bone("hand_r", "Hand.R", Some("lower_arm_r"), Vec3::new(0.0, -0.18, 0.0)),
        bone("fingers_r", "Fingers.R", Some("hand_r"), Vec3::new(0.0, -0.10, 0.0)),
        // Left leg. Feet/toes point toward -Z, the direction the torso
        // leans into (forward), so the run cycle's leg swing carries them
        // the right way.
        bone("thigh_l", "Thigh.L", Some("pelvis"), Vec3::new(-0.10, -0.05, 0.0)),
        bone("shin_l", "Shin.L", Some("thigh_l"), Vec3::new(0.0, -0.42, 0.0)),
        bone("foot_l", "Foot.L", Some("shin_l"), Vec3::new(0.0, -0.40, -0.05)),
        bone("toes_l", "Toes.L", Some("foot_l"), Vec3::new(0.0, -0.04, -0.12)),
        // Right leg
        bone("thigh_r", "Thigh.R", Some("pelvis"), Vec3::new(0.10, -0.05, 0.0)),
        bone("shin_r", "Shin.R", Some("thigh_r"), Vec3::new(0.0, -0.42, 0.0)),
        bone("foot_r", "Foot.R", Some("shin_r"), Vec3::new(0.0, -0.40, -0.05)),
        bone("toes_r", "Toes.R", Some("foot_r"), Vec3::new(0.0, -0.04, -0.12)),
    ];

    Skeleton { bones }
}

/// Number of evenly-spaced samples per running cycle. The final sample
/// (`phase == TAU`) is identical to the first (`phase == 0`) so the clip
/// loops seamlessly.
const RUN_STEPS: usize = 24;
/// Duration of one running cycle, in seconds.
const RUN_DURATION: f32 = 0.6;

const TAU: f32 = std::f32::consts::TAU;
const PI: f32 = std::f32::consts::PI;

/// Wrapped difference `a - b`, normalized to `(-PI, PI]`.
fn angle_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b) % TAU;
    if d > PI {
        d -= TAU;
    } else if d <= -PI {
        d += TAU;
    }
    d
}

/// A smooth raised-cosine "bump" of total angular `width`, centered on
/// `center`: zero outside the window, rising to `amplitude` at `center`
/// with zero slope at both edges. Used to shape leg/foot motion so it
/// snaps into a pose (e.g. a high knee lift) over part of the cycle and
/// rests for the remainder, instead of a plain sine wave.
fn bump(phase: f32, center: f32, width: f32, amplitude: f32) -> f32 {
    let half = width * 0.5;
    let d = angle_diff(phase, center);
    if d.abs() >= half {
        0.0
    } else {
        amplitude * 0.5 * (1.0 + (PI * d / half).cos())
    }
}

/// Knee flexion for a leg whose hip is at `phase`. A deep bend lifts the
/// heel toward the body during the forward-recovery swing (centered on
/// `phase == 0`, where the thigh sweeps from behind the body to in front of
/// it), and a shallow bend cushions the stance phase (centered on
/// `phase == PI`, where the thigh sweeps back under and behind the body).
fn knee_bend(phase: f32) -> f32 {
    -bump(phase, 0.0, PI * 1.1, KNEE_RECOVERY_BEND) - bump(phase, PI, PI * 0.9, KNEE_STANCE_BEND)
}

/// Ankle flexion for a leg whose hip is at `phase`: the foot dorsiflexes
/// (toes lift) during the high-knee recovery swing for ground clearance,
/// then plantarflexes (toes point down) for the toe-off push just before
/// the leg swings forward again.
fn ankle_flex(phase: f32) -> f32 {
    bump(phase, 0.0, PI, ANKLE_SWING) - bump(phase, -PI * 0.5, PI * 0.8, ANKLE_SWING * 1.3)
}

// --- Leg drive ---
const THIGH_SWING: f32 = 32.0;
const KNEE_RECOVERY_BEND: f32 = 95.0;
const KNEE_STANCE_BEND: f32 = 15.0;
const ANKLE_SWING: f32 = 20.0;
const TOE_CURL: f32 = 15.0;

// --- Arm swing (contralateral to the legs) ---
// The torso (pelvis + spine_01 + spine_02 + chest) carries a constant
// forward-lean bias of about -20 degrees about X. Since the arms hang from
// the chest, that lean is inherited by the whole arm chain; SHOULDER_COUNTER
// cancels it so the upper arm hangs straight down at rest, with the swing
// oscillating around vertical instead of being biased backward.
const SHOULDER_COUNTER: f32 = 20.0;
const SHOULDER_SWING: f32 = 45.0;
// Elbow flexion, relative to the upper arm: ELBOW_BASE is the (mostly
// extended) angle during the forward swing, and it bends further toward
// ELBOW_BASE + ELBOW_PUMP during the backswing/recovery.
const ELBOW_BASE: f32 = 20.0;
const ELBOW_PUMP: f32 = 55.0;
const HAND_FOLLOW: f32 = 10.0;

// --- Pelvis & torso ---
const PELVIS_BOUNCE: f32 = 0.045;
const PELVIS_SHIFT: f32 = 0.018;
const PELVIS_LEAN: f32 = -8.0;
const PELVIS_LEAN_OSC: f32 = 3.0;
const PELVIS_TWIST: f32 = 12.0;
const PELVIS_SWAY: f32 = 5.0;

/// Build a [`BoneTrack`] by sampling `f(phase)` -> `(translation delta,
/// rotation delta)` at [`RUN_STEPS`] evenly-spaced points around the cycle
/// (`phase` in `[0, TAU]`), added on top of the bone's bind pose.
fn run_track(skeleton: &Skeleton, bone_id: &str, f: fn(f32) -> (Vec3, Vec3)) -> BoneTrack {
    let base = skeleton
        .bone(bone_id)
        .map(|b| b.bind_transform.clone())
        .unwrap_or_default();

    let keyframes = (0..=RUN_STEPS)
        .map(|i| {
            let t = i as f32 / RUN_STEPS as f32;
            let (dt, dr) = f(t * TAU);
            Keyframe {
                time: t * RUN_DURATION,
                transform: Transform {
                    translation: base.translation.add(dt),
                    rotation: base.rotation.add(dr),
                    scale: base.scale,
                },
            }
        })
        .collect();

    BoneTrack {
        bone_id: bone_id.to_string(),
        keyframes,
    }
}

/// A looping 0.6s sprint cycle. The right leg leads at `phase == 0`; the
/// left leg/arm follow a half-cycle (PI) behind. Each leg drives its hip
/// through a full forward/back swing, with a high-knee recovery lift and a
/// dorsiflex/plantarflex ankle roll; the arms pump in counter-time to the
/// opposite leg with an elbow bend that deepens on the backswing. The
/// pelvis bounces twice per cycle (once per foot strike), leans forward,
/// and twists/sways with the stride, while the spine and chest
/// counter-rotate to keep the shoulders roughly square.
pub fn run_animation(skeleton: &Skeleton) -> AnimationClip {
    let tracks = vec![
        // Pelvis: double-bounce bob, lateral weight shift, forward lean,
        // hip twist + sway. The right leg leads (phase 0); the left leg is
        // a half-cycle behind.
        run_track(skeleton, "pelvis", |p| {
            (
                Vec3::new(PELVIS_SHIFT * p.sin(), PELVIS_BOUNCE * (1.0 - (2.0 * p).cos()), 0.0),
                Vec3::new(
                    PELVIS_LEAN + PELVIS_LEAN_OSC * (2.0 * p).sin(),
                    PELVIS_TWIST * p.sin(),
                    PELVIS_SWAY * p.sin(),
                ),
            )
        }),
        // Spine/chest counter-rotate the pelvis's hip twist so the shoulders
        // stay roughly square to the direction of travel, with a forward
        // lean carried mainly by the chest.
        run_track(skeleton, "spine_01", |p| {
            (Vec3::ZERO, Vec3::new(1.5 * (2.0 * p).sin(), -4.0 * p.sin(), 0.0))
        }),
        run_track(skeleton, "spine_02", |p| {
            (Vec3::ZERO, Vec3::new(1.5 * (2.0 * p).sin(), -6.0 * p.sin(), 0.0))
        }),
        run_track(skeleton, "chest", |p| {
            (Vec3::ZERO, Vec3::new(-12.0 + 2.0 * (2.0 * p).sin(), -10.0 * p.sin(), 0.0))
        }),
        // Head stays roughly level: a small counter-twist plus a subtle bob.
        run_track(skeleton, "head", |p| {
            (Vec3::ZERO, Vec3::new(3.0 * (2.0 * p).sin(), 6.0 * p.sin(), 0.0))
        }),
        // Clavicles lift slightly with their shoulder's swing.
        run_track(skeleton, "clavicle_l", |p| (Vec3::ZERO, Vec3::new(0.0, 0.0, 6.0 * p.sin()))),
        run_track(skeleton, "clavicle_r", |p| (Vec3::ZERO, Vec3::new(0.0, 0.0, -6.0 * (p + PI).sin()))),
        // Arms: forward/back pendulum swing (X axis, same as the legs),
        // contralateral to the legs (left arm matches the right leg's
        // phase, and vice versa) since the arms now hang at the sides.
        // SHOULDER_COUNTER cancels the torso's forward lean so the arms
        // swing around a vertical "hanging" rest pose.
        run_track(skeleton, "shoulder_l", |p| {
            (Vec3::ZERO, Vec3::new(SHOULDER_COUNTER + SHOULDER_SWING * p.sin(), 0.0, 0.0))
        }),
        run_track(skeleton, "shoulder_r", |p| {
            (Vec3::ZERO, Vec3::new(SHOULDER_COUNTER + SHOULDER_SWING * (p + PI).sin(), 0.0, 0.0))
        }),
        // Elbows stay bent throughout the pump (ELBOW_BASE, mostly
        // extended) and flex deeper toward ELBOW_BASE + ELBOW_PUMP on the
        // backswing (when the shoulder angle goes negative).
        run_track(skeleton, "lower_arm_l", |p| {
            (Vec3::ZERO, Vec3::new(ELBOW_BASE + ELBOW_PUMP * (-p.sin()).max(0.0), 0.0, 0.0))
        }),
        run_track(skeleton, "lower_arm_r", |p| {
            let pr = p + PI;
            (Vec3::ZERO, Vec3::new(ELBOW_BASE + ELBOW_PUMP * (-pr.sin()).max(0.0), 0.0, 0.0))
        }),
        // Wrists follow the forearm's pump with a small lag.
        run_track(skeleton, "hand_l", |p| (Vec3::ZERO, Vec3::new(HAND_FOLLOW * p.sin(), 0.0, 0.0))),
        run_track(skeleton, "hand_r", |p| (Vec3::ZERO, Vec3::new(HAND_FOLLOW * (p + PI).sin(), 0.0, 0.0))),
        // Legs: hip drive (X axis), right leg leads at phase 0, left leg
        // trails by half a cycle.
        run_track(skeleton, "thigh_r", |p| (Vec3::ZERO, Vec3::new(THIGH_SWING * p.sin(), 0.0, 0.0))),
        run_track(skeleton, "thigh_l", |p| (Vec3::ZERO, Vec3::new(THIGH_SWING * (p + PI).sin(), 0.0, 0.0))),
        // Knees: deep high-knee recovery lift, shallow stance cushioning.
        run_track(skeleton, "shin_r", |p| (Vec3::ZERO, Vec3::new(knee_bend(p), 0.0, 0.0))),
        run_track(skeleton, "shin_l", |p| (Vec3::ZERO, Vec3::new(knee_bend(p + PI), 0.0, 0.0))),
        // Ankles: dorsiflex for clearance during recovery, plantarflex for
        // the toe-off push.
        run_track(skeleton, "foot_r", |p| (Vec3::ZERO, Vec3::new(ankle_flex(p), 0.0, 0.0))),
        run_track(skeleton, "foot_l", |p| (Vec3::ZERO, Vec3::new(ankle_flex(p + PI), 0.0, 0.0))),
        // Toes curl through push-off, in time with the plantarflex.
        run_track(skeleton, "toes_r", |p| (Vec3::ZERO, Vec3::new(-bump(p, -PI * 0.5, PI * 0.8, TOE_CURL), 0.0, 0.0))),
        run_track(skeleton, "toes_l", |p| {
            (Vec3::ZERO, Vec3::new(-bump(p + PI, -PI * 0.5, PI * 0.8, TOE_CURL), 0.0, 0.0))
        }),
    ];

    AnimationClip {
        name: "Run".to_string(),
        duration: RUN_DURATION,
        fps: 30.0,
        tracks,
    }
}
