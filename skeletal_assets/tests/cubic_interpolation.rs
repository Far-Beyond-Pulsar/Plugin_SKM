//! glTF 2.0 Appendix C: tangents are derivatives per second, quaternion
//! components use Hermite before normalization (without a shortest-arc flip).
use glam::Quat;
use skeletal_assets::animation::{AnimationClip, BoneTrack, KeyInterpolation, Keyframe};
use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::math::{Transform, Vec3};
use skeletal_assets::runtime::clip::CompiledClip;
use skeletal_assets::runtime::pose::{BoneTransform, RuntimeSkeleton};
use skeletal_assets::skeleton::{Bone, Skeleton};

fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 2e-5, "{a} != {b}");
}
fn skeleton() -> RuntimeSkeleton {
    RuntimeSkeleton::new(
        "s",
        &Skeleton {
            bones: vec![Bone {
                id: "root".into(),
                name: "Root".into(),
                parent: None,
                bind_transform: Transform::IDENTITY,
            }],
        },
    )
    .unwrap()
}
fn asset(track: BoneTrack) -> AnimSeqAsset {
    AnimSeqAsset::new(
        None,
        AnimationClip {
            name: "Cubic".into(),
            duration: 5.0,
            fps: 30.0,
            tracks: vec![track],
        },
    )
}
fn track() -> BoneTrack {
    let mut a = Keyframe::linear(1.0, Transform::IDENTITY);
    a.translation_interpolation = KeyInterpolation::CubicSpline;
    a.scale_interpolation = KeyInterpolation::CubicSpline;
    a.rotation_interpolation = KeyInterpolation::CubicSpline;
    a.rotation_quaternion = Some([0.0, 0.0, 0.0, 1.0]);
    a.out_tangent.translation = [2.0, -1.0, 0.0];
    a.out_tangent.scale = [1.0, 0.0, -2.0];
    // Includes radial components: projecting these changes the cubic.
    a.out_tangent.rotation = [1.0, 0.0, 2.0, 1.0];
    let mut b = Keyframe::linear(3.0, Transform::IDENTITY);
    b.transform.translation = Vec3::new(4.0, 2.0, -2.0);
    b.transform.scale = Vec3::new(0.0, -1.0, 3.0);
    b.rotation_quaternion = Some([0.0, 0.0, 1.0, 0.0]);
    b.in_tangent.translation = [-1.0, 3.0, 1.0];
    b.in_tangent.scale = [-2.0, 1.0, 0.0];
    b.in_tangent.rotation = [0.0, 1.0, -1.0, 2.0];
    // Deliberately disagreeing Euler cache: quaternion values are authoritative.
    BoneTrack {
        bone_id: "root".into(),
        keyframes: vec![a, b],
    }
}
fn runtime(track: &BoneTrack, time: f32) -> BoneTransform {
    let skel = skeleton();
    CompiledClip::compile(&asset(track.clone()), &skel).sample_bone(
        0,
        time,
        BoneTransform::IDENTITY,
    )
}
fn assert_same(track: &BoneTrack, time: f32) {
    let editor = track.sample(time).unwrap();
    let rt = runtime(track, time);
    let q = BoneTransform::from_asset(&Transform {
        scale: Vec3::ONE,
        ..editor.clone()
    })
    .rotation;
    assert!(
        q.dot(rt.rotation).abs() > 1.0 - 2e-5,
        "time {time}: {q:?} != {:?}",
        rt.rotation
    );
    for (a, b) in editor
        .translation
        .to_array()
        .into_iter()
        .zip(rt.translation.to_array())
    {
        near(a, b);
    }
    for (a, b) in editor.scale.to_array().into_iter().zip(rt.scale.to_array()) {
        near(a, b);
    }
}

#[test]
fn known_hermite_values_include_interval_duration_and_signed_scales() {
    let track = track();
    // u = 1/2, dt = 2: h00=h01=1/2, h10=1/8, h11=-1/8.
    let value = track.sample(2.0).unwrap();
    assert_eq!(value.translation, Vec3::new(2.75, 0.0, -1.25));
    assert_eq!(value.scale, Vec3::new(1.25, -0.25, 1.5));
    let q = runtime(&track, 2.0).rotation;
    let expected = Quat::from_xyzw(0.25, -0.25, 1.25, 0.25).normalize();
    assert!(q.dot(expected) > 1.0 - 1e-6);
    near(q.length(), 1.0);
    for t in [0.0, 1.0, 1.125, 2.0, 2.9, 3.0, 4.0] {
        assert_same(&track, t);
    }
    near(runtime(&track, 3.0).scale.x, 0.0);
}

#[test]
fn step_and_linear_keys_hold_exact_intermediate_and_final_values() {
    let mut track = track();
    for k in &mut track.keyframes {
        k.translation_interpolation = KeyInterpolation::Step;
        k.scale_interpolation = KeyInterpolation::Linear;
        k.rotation_interpolation = KeyInterpolation::Linear;
    }
    let mut last = track.keyframes[1].clone();
    last.time = 5.0;
    last.transform.translation.x = 10.0;
    track.keyframes.push(last);
    near(track.sample(2.999).unwrap().translation.x, 0.0);
    near(track.sample(3.0).unwrap().translation.x, 4.0);
    near(track.sample(4.0).unwrap().translation.x, 4.0);
    near(track.sample(5.0).unwrap().translation.x, 10.0);
    for t in [1.0, 2.0, 3.0, 4.0, 5.0] {
        assert_same(&track, t);
    }
    near(
        runtime(&track, 2.0).rotation.z,
        std::f32::consts::FRAC_1_SQRT_2,
    );
}

#[test]
fn small_positive_intervals_are_not_replaced_by_an_epsilon() {
    let mut t = track();
    t.keyframes[0].time = 0.0;
    t.keyframes[1].time = 2e-7;
    t.keyframes[0].out_tangent.translation = [0.0; 3];
    t.keyframes[1].in_tangent.translation = [0.0; 3];
    near(t.sample(1e-7).unwrap().translation.x, 2.0);
    near(runtime(&t, 1e-7).translation.x, 2.0);
}

#[test]
fn empty_single_duplicate_and_nonfinite_times_do_not_panic() {
    let mut t = track();
    t.keyframes.clear();
    assert!(t.sample(0.0).is_none());
    assert_eq!(runtime(&t, 0.0), BoneTransform::IDENTITY);
    t.keyframes.push(track().keyframes.remove(0));
    for time in [-10.0, 1.0, 10.0, f32::NAN] {
        assert_same(&t, time);
    }
    t.keyframes.push(t.keyframes[0].clone());
    for time in [1.0, 1.1, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
        assert!(runtime(&t, time).is_finite());
        assert!(t.sample(time).unwrap().translation.x.is_finite());
    }
    t.keyframes.reverse();
    assert!(runtime(&t, 2.0).is_finite());
}

#[test]
fn zero_or_invalid_cubic_quaternions_hold_the_start_rotation() {
    let mut t = track();
    t.keyframes[0].out_tangent.rotation = [0.0; 4];
    t.keyframes[1].in_tangent.rotation = [0.0; 4];
    t.keyframes[1].rotation_quaternion = Some([0.0, 0.0, 0.0, -1.0]);
    assert_eq!(runtime(&t, 2.0).rotation, Quat::IDENTITY);
    assert_same(&t, 2.0);
    t.keyframes[1].rotation_quaternion = None;
    assert_eq!(runtime(&t, 2.0).rotation, Quat::IDENTITY);
    assert_same(&t, 2.0);
    t.keyframes[1].rotation_quaternion = Some([0.0, 0.0, 0.0, -1.0]);
    t.keyframes[0].out_tangent.rotation[1] = f32::INFINITY;
    assert_eq!(runtime(&t, 2.0).rotation, Quat::IDENTITY);
    assert_same(&t, 2.0);
}

#[test]
fn inserting_nonuniform_keys_preserves_the_entire_cubic() {
    let original = track();
    let mut split = original.clone();
    for time in [1.25, 2.125, 2.75] {
        split.insert_keyframe(time, Transform::IDENTITY);
    }
    for i in 0..=240 {
        let t = 1.0 + i as f32 / 120.0;
        let a = runtime(&original, t);
        let b = runtime(&split, t);
        assert!(a.translation.distance(b.translation) < 3e-5);
        assert!(a.scale.distance(b.scale) < 3e-5);
        assert!(a.rotation.dot(b.rotation) > 1.0 - 1e-6);
        assert_same(&split, t);
    }
}

#[test]
fn pose_edits_keep_tangents_and_update_quaternion_values() {
    let mut t = track();
    let k = &mut t.keyframes[0];
    let mut pose = k.transform.clone();
    pose.translation.x = 8.0;
    k.set_transform(pose.clone());
    assert_eq!(k.rotation_quaternion, Some([0.0, 0.0, 0.0, 1.0]));
    pose.rotation.z = 90.0;
    k.set_transform(pose);
    assert!(
        Quat::from_array(k.rotation_quaternion.unwrap())
            .dot(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2))
            > 1.0 - 1e-6
    );
    assert_eq!(k.out_tangent.rotation, [1.0, 0.0, 2.0, 1.0]);
    assert_eq!(k.translation_interpolation, KeyInterpolation::CubicSpline);
}

#[test]
fn serialization_versions_interpolation_and_reads_legacy_keys() {
    let original = asset(track());
    let text = original.to_text();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["version"],
        5
    );
    // The old version gate must reject this document before ignoring fields.
    assert!(skeletal_assets::codec::read_envelope(
        &text,
        skeletal_assets::codec::VersionPolicy {
            oldest: 1,
            current: 4,
            unversioned_as: Some(1),
        }
    )
    .is_err());
    let restored = AnimSeqAsset::parse(&text).unwrap();
    assert_eq!(restored.to_text(), text);
    for time in [1.0, 1.25, 2.0, 3.0] {
        assert_eq!(
            runtime(&original.clip.tracks[0], time),
            runtime(&restored.clip.tracks[0], time)
        );
    }
    let legacy = r#"{"version":1,"name":"Old","duration":2,"fps":30,"tracks":[{"bone_id":"root","keyframes":[{"time":0,"transform":{"translation":{"x":0,"y":0,"z":0},"rotation":{"x":0,"y":0,"z":0},"scale":{"x":1,"y":1,"z":1}}},{"time":2,"transform":{"translation":{"x":2,"y":0,"z":0},"rotation":{"x":0,"y":0,"z":0},"scale":{"x":1,"y":1,"z":1}}}]}]}"#;
    for version in 1..=4 {
        let text = legacy.replace("\"version\":1", &format!("\"version\":{version}"));
        let old = AnimSeqAsset::parse(&text).unwrap();
        near(old.clip.tracks[0].sample(1.0).unwrap().translation.x, 1.0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&old.to_text()).unwrap()["version"],
            version
        );
        assert!(!old.to_text().contains("tangent"));
    }
}

#[test]
fn saving_invalid_tangents_preserves_the_previous_asset() {
    use skeletal_assets::assets::{load_animseq_asset, save_animseq_asset};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("curve.animseq");
    let mut a = asset(track());
    save_animseq_asset(&path, &a).unwrap();
    let original = std::fs::read(&path).unwrap();
    a.clip.tracks[0].keyframes[0].out_tangent.translation[2] = f32::NAN;
    assert!(save_animseq_asset(&path, &a).is_err());
    assert!(serde_json::to_string(&a).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(load_animseq_asset(&path).is_ok());
}

#[test]
fn cubic_rotation_keeps_the_authored_quaternion_hemisphere() {
    let mut t = track();
    let end = -Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    t.keyframes[0].out_tangent.rotation = [0.0; 4];
    t.keyframes[1].in_tangent.rotation = [0.0; 4];
    t.keyframes[1].rotation_quaternion = Some(end.to_array());
    let expected = Quat::from_xyzw(0.0, 0.0, end.z * 0.5, (1.0 + end.w) * 0.5).normalize();
    let actual = runtime(&t, 2.0).rotation;
    assert!(actual.dot(expected) > 1.0 - 1e-6);
    assert!(
        actual
            .dot(Quat::from_rotation_z(std::f32::consts::FRAC_PI_4))
            .abs()
            < 0.01
    );
    assert_same(&t, 2.0);
}

#[test]
fn a_partial_quaternion_track_holds_its_authoritative_start_value() {
    let mut t = track();
    t.keyframes[0].rotation_quaternion = Some(Quat::from_rotation_z(1.0).to_array());
    t.keyframes[1].rotation_quaternion = None;
    for mode in [KeyInterpolation::Step, KeyInterpolation::CubicSpline] {
        t.keyframes[0].rotation_interpolation = mode;
        assert_same(&t, 2.0);
        assert!(runtime(&t, 2.0).rotation.dot(Quat::from_rotation_z(1.0)) > 1.0 - 1e-6);
    }
}

#[test]
fn extending_a_legacy_linear_track_keeps_new_keys_linear() {
    let mut t = BoneTrack {
        bone_id: "root".into(),
        keyframes: vec![Keyframe::linear(1.0, Transform::IDENTITY)],
    };
    let i = t.insert_keyframe(0.0, Transform::IDENTITY);
    let mut pose = Transform::IDENTITY;
    pose.translation.x = -2.0;
    t.keyframes[i].set_transform(pose);
    assert_eq!(
        t.keyframes[0].translation_interpolation,
        KeyInterpolation::Linear
    );
    near(t.sample(0.5).unwrap().translation.x, -1.0);
    let i = t.insert_keyframe(3.0, Transform::IDENTITY);
    let mut pose = Transform::IDENTITY;
    pose.translation.x = 4.0;
    t.keyframes[i].set_transform(pose);
    near(t.sample(2.0).unwrap().translation.x, 2.0);
}

#[test]
fn legacy_linear_reflections_keep_the_existing_runtime_convention() {
    let a = Transform {
        scale: Vec3::new(1.0, -2.0, 3.0),
        rotation: Vec3::new(10.0, 20.0, 30.0),
        ..Transform::IDENTITY
    };
    let b = Transform {
        scale: Vec3::new(2.0, -1.0, 2.0),
        rotation: Vec3::new(20.0, 40.0, 50.0),
        ..Transform::IDENTITY
    };
    let t = BoneTrack {
        bone_id: "root".into(),
        keyframes: vec![
            Keyframe::linear(0.0, a.clone()),
            Keyframe::linear(2.0, b.clone()),
        ],
    };
    let expected = BoneTransform::from_asset(&a).blend(&BoneTransform::from_asset(&b), 0.5);
    let sampled = runtime(&t, 1.0);
    assert!(sampled.rotation.dot(expected.rotation).abs() > 1.0 - 1e-6);
    assert!(sampled.scale.distance(expected.scale) < 1e-6);
    let mut collapsed = t.clone();
    collapsed.keyframes[0].transform.scale.x = 0.0;
    let value = runtime(&collapsed, 0.0);
    assert!(value.is_finite());
    assert_eq!(value.scale.x, 0.0);
}
