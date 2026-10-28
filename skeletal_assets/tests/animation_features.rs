use skeletal_assets::{
    animation::{AnimationClip, BoneTrack, Keyframe},
    assets::{AnimSeqAsset, SkeletonAsset},
    curves::{AnimationCurve, CurveData, CurveInterpolation, CurveKey, CurveValue},
    math::{Mat4, Transform, Vec3},
    notifies::{collect_crossed_notifies, AnimationNotify, PlaybackInterval},
    root_motion::{extract_root_motion, RootMotionSettings},
    sequence_features::AnimationFeatures,
    skeleton::{Bone, Skeleton},
};

fn advance(from: f64, to: f64, looping: bool) -> PlaybackInterval {
    PlaybackInterval::Advance { from, to, looping }
}
fn notify(id: &str, time: f32) -> AnimationNotify {
    AnimationNotify {
        target: Default::default(),
        fields: Vec::new(),
        id: id.into(),
        name: id.into(),
        time,
    }
}
fn crossed(events: &[AnimationNotify], from: f64, to: f64, looping: bool) -> Vec<String> {
    collect_crossed_notifies(events, 1.0, advance(from, to, looping))
        .unwrap()
        .iter()
        .map(|h| h.notify.id.clone())
        .collect()
}
#[test]
fn notify_boundaries_are_composable_in_both_directions() {
    let events = vec![
        notify("zero", 0.0),
        notify("a", 0.5),
        notify("b", 0.5),
        notify("end", 1.0),
    ];
    assert_eq!(crossed(&events, 0.0, 1.0, false), ["a", "b", "end"]);
    assert_eq!(crossed(&events, 1.0, 0.0, false), ["a", "b", "zero"]);
    assert_eq!(crossed(&events, 0.0, 1.0, true), ["a", "b", "end", "zero"]);
    assert_eq!(crossed(&events, 1.0, 0.0, true), ["a", "b", "zero", "end"]);
    for (from, mid, to) in [(0.0, 1.0, 2.0), (2.0, 1.0, 0.0), (-1.2, 0.0, 2.4)] {
        let all = crossed(&events, from, to, true);
        let mut split = crossed(&events, from, mid, true);
        split.extend(crossed(&events, mid, to, true));
        assert_eq!(all, split);
    }
}
#[test]
fn notify_multiple_cycles_order_and_non_playback() {
    let events = vec![notify("late", 0.75), notify("early", 0.25)];
    assert_eq!(
        crossed(&events, 0.0, 3.0, true),
        ["early", "late", "early", "late", "early", "late"]
    );
    assert_eq!(
        crossed(&events, 3.0, 0.0, true),
        ["late", "early", "late", "early", "late", "early"]
    );
    for interval in [
        PlaybackInterval::Seek,
        PlaybackInterval::Paused,
        advance(0.5, 0.5, true),
    ] {
        assert!(collect_crossed_notifies(&events, 1.0, interval)
            .unwrap()
            .is_empty());
    }
    assert!(collect_crossed_notifies(&[], 0.0, advance(0.0, 5.0, true))
        .unwrap()
        .is_empty());
    assert_eq!(crossed(&events, -5.0, 8.0, false), ["early", "late"]);
}
#[test]
fn notify_invalid_inputs_are_errors() {
    for time in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
        assert!(
            collect_crossed_notifies(&[notify("bad", time)], 1.0, advance(0.0, 1.0, true)).is_err()
        );
    }
    assert!(collect_crossed_notifies(&[], 1.0, advance(f64::NAN, 1.0, true)).is_err());
    assert!(collect_crossed_notifies(&[], -1.0, advance(0.0, 1.0, true)).is_err());
}
fn scalar_keys() -> Vec<CurveKey<f32>> {
    vec![CurveKey::new(1.0, 10.0), CurveKey::new(0.0, 0.0)]
}
#[test]
fn scalar_sampling_sorts_once_holds_endpoints_and_interpolates() {
    let curve = AnimationCurve::from_data(
        "speed".into(),
        "Speed".into(),
        CurveData::Float(scalar_keys()),
    )
    .unwrap();
    for (time, value) in [
        (-1.0, 0.0),
        (0.0, 0.0),
        (0.25, 2.5),
        (1.0, 10.0),
        (2.0, 10.0),
    ] {
        assert_eq!(curve.sample(time), Some(CurveValue::Float(value)));
    }
    assert_eq!(curve.sample(f32::NAN), None);
    let mut keys = scalar_keys();
    keys[1].interpolation = CurveInterpolation::Step;
    let curve =
        AnimationCurve::from_data("speed".into(), "Speed".into(), CurveData::Float(keys)).unwrap();
    assert_eq!(curve.sample(0.9), Some(CurveValue::Float(0.0)));
    assert_eq!(curve.sample(1.0), Some(CurveValue::Float(10.0)));
}
#[test]
fn empty_single_and_vector_curves() {
    let mut curve = AnimationCurve::new("Offset".into(), true).unwrap();
    assert_eq!(curve.sample(0.0), None);
    let value = Vec3::new(2.0, -4.0, 6.0);
    curve
        .set_key(
            "a".into(),
            0.0,
            CurveValue::Vector(value),
            CurveInterpolation::Linear,
            1.0,
        )
        .unwrap();
    assert_eq!(curve.sample(100.0), Some(CurveValue::Vector(value)));
    curve
        .set_key(
            "b".into(),
            1.0,
            CurveValue::Vector(Vec3::ZERO),
            CurveInterpolation::Linear,
            1.0,
        )
        .unwrap();
    assert_eq!(
        curve.sample(0.5),
        Some(CurveValue::Vector(value.scale(0.5)))
    );
    assert!(curve.remove_key("a"));
    assert!(!curve.remove_key("a"));
    assert_eq!(curve.sample(-1.0), Some(CurveValue::Vector(Vec3::ZERO)));
}
#[test]
fn curve_edits_preserve_ids_and_reject_collisions_atomically() {
    let mut curve = AnimationCurve::new("Speed".into(), false).unwrap();
    for (id, time) in [("a", 0.0), ("b", 0.5)] {
        curve
            .set_key(
                id.into(),
                time,
                CurveValue::Float(time),
                CurveInterpolation::Linear,
                1.0,
            )
            .unwrap();
    }
    let before = curve.clone();
    assert!(curve
        .set_key(
            "a".into(),
            0.5,
            CurveValue::Float(1.0),
            CurveInterpolation::Linear,
            1.0
        )
        .is_err());
    assert_eq!(curve, before);
    for (time, value) in [
        (f32::NAN, CurveValue::Float(1.0)),
        (-0.1, CurveValue::Float(1.0)),
        (0.1, CurveValue::Float(f32::INFINITY)),
        (0.1, CurveValue::Vector(Vec3::ZERO)),
    ] {
        assert!(curve
            .set_key("a".into(), time, value, CurveInterpolation::Linear, 1.0)
            .is_err());
        assert_eq!(curve, before);
    }
    curve
        .set_key(
            "a".into(),
            0.8,
            CurveValue::Float(8.0),
            CurveInterpolation::Linear,
            1.0,
        )
        .unwrap();
    curve.rename("Velocity".into()).unwrap();
    assert_eq!(curve.key_index("a"), Some(1));
    assert_eq!(curve.key(1).unwrap().1, 0.8);
}
#[test]
fn malformed_curve_records_and_duplicate_names_fail() {
    let mut keys = scalar_keys();
    keys[1].time = 1.0;
    assert!(AnimationCurve::from_data("id".into(), "Name".into(), CurveData::Float(keys)).is_err());
    let mut keys = scalar_keys();
    keys[1].id = keys[0].id.clone();
    assert!(AnimationCurve::from_data("id".into(), "Name".into(), CurveData::Float(keys)).is_err());
    let features = AnimationFeatures {
        curves: vec![
            AnimationCurve::new("Speed".into(), false).unwrap(),
            AnimationCurve::new("Speed".into(), true).unwrap(),
        ],
        ..Default::default()
    };
    assert!(features.validate(1.0).is_err());
    let bad = r#"{"id":"x","name":"Bad","data":{"kind":"Float","keys":[{"id":"k","time":-1,"value":0}]}}"#;
    assert!(serde_json::from_str::<AnimationCurve>(bad).is_err());
}
#[test]
fn sequence_round_trip_migrates_safely_and_guards_old_readers() {
    let old = r#"{"version":1,"name":"Walk","duration":1,"fps":30,"tracks":[]}"#;
    let mut asset = AnimSeqAsset::parse(old).unwrap();
    assert!(asset.features.is_empty());
    assert!(!asset.features.root_motion.enabled);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&asset.to_text()).unwrap()["version"],
        1
    );
    asset.features.notifies.push(notify("footstep", 0.5));
    asset.features.curves.push(
        AnimationCurve::from_data(
            "curve".into(),
            "Speed".into(),
            CurveData::Float(scalar_keys()),
        )
        .unwrap(),
    );
    asset
        .features
        .curves
        .push(AnimationCurve::new("Offset".into(), true).unwrap());
    asset.features.root_motion.enabled = true;
    let text = asset.to_text();
    let back = AnimSeqAsset::parse(&text).unwrap();
    assert_eq!(back.version, 2);
    assert_eq!(asset.features, back.features);
    assert_eq!(asset.id, back.id);
    assert!(SkeletonAsset::parse(&text).unwrap_err().contains("newer"));
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["version"] = (skeletal_assets::assets::ANIMSEQ_FORMAT_VERSION + 1).into();
    assert!(AnimSeqAsset::parse(&value.to_string()).is_err());
    value["version"] = 2.into();
    value["notifies"][0]["time"] = (-1.0).into();
    assert!(AnimSeqAsset::parse(&value.to_string()).is_err());
}
fn skeleton() -> Skeleton {
    Skeleton {
        bones: vec![
            Bone {
                id: "root".into(),
                name: "Root".into(),
                parent: None,
                bind_transform: Transform::IDENTITY,
            },
            Bone {
                id: "child".into(),
                name: "Hips".into(),
                parent: Some("root".into()),
                bind_transform: Transform {
                    translation: Vec3::new(0.0, 2.0, 0.0),
                    ..Transform::IDENTITY
                },
            },
        ],
    }
}
fn clip(end: Transform) -> AnimationClip {
    AnimationClip {
        name: "Motion".into(),
        duration: 1.0,
        fps: 30.0,
        tracks: vec![BoneTrack {
            bone_id: "root".into(),
            keyframes: vec![
                Keyframe::linear(0.0, Transform::IDENTITY),
                Keyframe::linear(1.0, end),
            ],
        }],
    }
}
fn settings() -> RootMotionSettings {
    RootMotionSettings {
        enabled: true,
        ..Default::default()
    }
}
fn motion(skeleton: &Skeleton, clip: &AnimationClip, from: f64, to: f64, looping: bool) -> Mat4 {
    extract_root_motion(skeleton, clip, &settings(), advance(from, to, looping)).unwrap()
}
#[test]
fn root_motion_bind_translation_reverse_and_masks() {
    let sk = skeleton();
    let bind = AnimationClip::bind_pose();
    assert!(motion(&sk, &bind, 0.0, 1.0, false).approx_eq(&Mat4::IDENTITY, 1e-5));
    let clip = clip(Transform {
        translation: Vec3::new(4.0, 0.0, 0.0),
        ..Transform::IDENTITY
    });
    assert!(motion(&sk, &clip, 0.25, 0.75, false)
        .approx_eq(&Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0)), 1e-5));
    assert!(motion(&sk, &clip, 0.75, 0.25, false)
        .approx_eq(&Mat4::from_translation(Vec3::new(-2.0, 0.0, 0.0)), 1e-5));
    assert!(motion(&sk, &clip, 0.75, 3.25, true)
        .approx_eq(&Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)), 1e-5));
    let mut mask = settings();
    mask.translation[0] = false;
    assert!(
        extract_root_motion(&sk, &clip, &mask, advance(0.0, 1.0, false))
            .unwrap()
            .approx_eq(&Mat4::IDENTITY, 1e-5)
    );
}
#[test]
fn root_motion_rotation_translation_compose_across_cycles() {
    let sk = skeleton();
    let end = Transform {
        translation: Vec3::new(2.0, 0.0, 0.0),
        rotation: Vec3::new(0.0, 0.0, 90.0),
        ..Transform::IDENTITY
    };
    let clip = clip(end.clone());
    let one = motion(&sk, &clip, 0.0, 1.0, true);
    assert!(one.approx_eq(&end.to_matrix(), 1e-5));
    assert!(motion(&sk, &clip, 0.0, 2.0, true).approx_eq(&one.mul(&one), 1e-5));
    let a = motion(&sk, &clip, 0.2, 1.0, true);
    let b = motion(&sk, &clip, 1.0, 2.4, true);
    let all = motion(&sk, &clip, 0.2, 2.4, true);
    assert!(a.mul(&b).approx_eq(&all, 2e-5));
    assert!(all
        .mul(&motion(&sk, &clip, 2.4, 0.2, true))
        .approx_eq(&Mat4::IDENTITY, 2e-5));
    assert!(motion(&sk, &clip, -0.6, 0.0, true)
        .mul(&motion(&sk, &clip, 0.0, 0.7, true))
        .approx_eq(&motion(&sk, &clip, -0.6, 0.7, true), 2e-5));
}
#[test]
fn root_motion_source_chain_errors_and_no_pose_mutation() {
    let sk = skeleton();
    let mut clip = clip(Transform {
        translation: Vec3::new(4.0, 0.0, 0.0),
        ..Transform::IDENTITY
    });
    clip.tracks.push(BoneTrack {
        bone_id: "child".into(),
        keyframes: vec![
            Keyframe::linear(0.0, sk.bones[1].bind_transform.clone()),
            Keyframe::linear(
                1.0,
                Transform {
                    translation: Vec3::new(0.0, 3.0, 0.0),
                    ..Transform::IDENTITY
                },
            ),
        ],
    });
    let source = RootMotionSettings {
        source_bone: Some("child".into()),
        ..settings()
    };
    let before = serde_json::to_string(&clip).unwrap();
    let delta = extract_root_motion(&sk, &clip, &source, advance(0.0, 1.0, false)).unwrap();
    assert!(delta.approx_eq(&Mat4::from_translation(Vec3::new(4.0, 1.0, 0.0)), 1e-5));
    assert_eq!(serde_json::to_string(&clip).unwrap(), before);
    assert!(extract_root_motion(
        &sk,
        &clip,
        &RootMotionSettings {
            source_bone: Some("missing".into()),
            ..settings()
        },
        advance(0.0, 1.0, false)
    )
    .is_err());
    let mut scaled = sk.clone();
    scaled.bones[0].bind_transform.scale = Vec3::new(1.0, 2.0, 1.0);
    assert!(extract_root_motion(
        &scaled,
        &AnimationClip::bind_pose(),
        &source,
        advance(0.0, 1.0, false)
    )
    .is_err());
    for interval in [PlaybackInterval::Seek, PlaybackInterval::Paused] {
        assert!(extract_root_motion(&sk, &clip, &settings(), interval)
            .unwrap()
            .approx_eq(&Mat4::IDENTITY, 1e-5));
    }
    let mut zero = clip.clone();
    zero.duration = 0.0;
    assert!(motion(&sk, &zero, 0.0, 100.0, true).approx_eq(&Mat4::IDENTITY, 1e-5));
}

#[test]
fn stripping_extracted_motion_prevents_double_application() {
    use skeletal_assets::{
        animation::evaluate_world_transforms, root_motion::remove_root_motion_from_pose,
    };
    let sk = skeleton();
    let clip = clip(Transform {
        translation: Vec3::new(3.0, 0.0, 0.0),
        rotation: Vec3::new(0.0, 60.0, 0.0),
        ..Transform::IDENTITY
    });
    let original = evaluate_world_transforms(&sk, &clip, 0.75);
    let mut residual = original.clone();
    let extracted = motion(&sk, &clip, 0.0, 0.75, false);
    remove_root_motion_from_pose(&mut residual, &extracted).unwrap();
    for (id, matrix) in &original {
        assert!(extracted.mul(&residual[id]).approx_eq(matrix, 1e-5));
    }
    assert!(residual["root"].approx_eq(&Mat4::IDENTITY, 1e-5));
}

#[test]
fn invalid_save_preserves_the_previous_asset() {
    use skeletal_assets::assets::{load_animseq_asset, save_animseq_asset};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.animseq");
    let mut asset = AnimSeqAsset::new(None, AnimationClip::bind_pose());
    save_animseq_asset(&path, &asset).unwrap();
    let before = std::fs::read(&path).unwrap();
    asset.features.notifies.push(notify("outside", 2.0));
    assert!(save_animseq_asset(&path, &asset).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(load_animseq_asset(&path).unwrap().features.is_empty());
}
#[test]
fn extreme_values_and_long_loop_intervals_are_predictable() {
    let curve = AnimationCurve::from_data(
        "id".into(),
        "Extreme".into(),
        CurveData::Float(vec![
            CurveKey::new(0.0, -f32::MAX),
            CurveKey::new(1.0, f32::MAX),
        ]),
    )
    .unwrap();
    assert_eq!(curve.sample(0.5), Some(CurveValue::Float(0.0)));
    let clip = clip(Transform {
        translation: Vec3::new(2.0, 0.0, 0.0),
        rotation: Vec3::new(0.0, 0.0, 90.0),
        ..Transform::IDENTITY
    });
    let a = motion(&skeleton(), &clip, 0.0, 1000.0, true);
    assert!(a.approx_eq(&Mat4::IDENTITY, 1e-3));
    assert!(a.approx_eq(&motion(&skeleton(), &clip, 0.0, 1000.0, true), 0.0));
    assert!(
        collect_crossed_notifies(&[notify("step", 0.5)], 1.0, advance(0.0, 1e12, true)).is_err()
    );
}

#[test]
fn looping_object_motion_uses_a_single_cycle_residual_pose() {
    use skeletal_assets::{
        animation::evaluate_world_transforms, root_motion::remove_root_motion_from_pose,
    };
    let sk = skeleton();
    let clip = clip(Transform {
        translation: Vec3::new(4.0, 0.0, 0.0),
        ..Transform::IDENTITY
    });
    let mut pose = evaluate_world_transforms(&sk, &clip, 0.25);
    let phase_motion = motion(&sk, &clip, 0.0, 0.25, false);
    remove_root_motion_from_pose(&mut pose, &phase_motion).unwrap();
    let object = motion(&sk, &clip, 0.0, 1.25, true);
    let result = object.mul(&pose["root"]);
    assert!(result.approx_eq(&Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0)), 1e-5));
}

#[test]
fn looping_translation_keeps_small_deltas_at_large_absolute_times() {
    let clip = clip(Transform {
        translation: Vec3::new(1.0, -2.0, 0.5),
        ..Transform::IDENTITY
    });
    for offset in [
        0.0,
        16_777_216.0,
        -16_777_216.0,
        1_099_511_627_776.0,
        -1_099_511_627_776.0,
    ] {
        for (from, to) in [
            (0.0, 1.0),
            (1.0, 0.0),
            (0.25, 0.75),
            (0.75, 1.25),
            (-0.25, 2.5),
            (2.5, -0.25),
        ] {
            // constant velocity makes displacement equal velocity * elapsed seconds.
            let expected =
                Mat4::from_translation(Vec3::new(1.0, -2.0, 0.5).scale((to - from) as f32));
            assert!(
                motion(&skeleton(), &clip, offset + from, offset + to, true)
                    .approx_eq(&expected, 1e-5),
                "offset {offset}, {from} -> {to}"
            );
        }
    }
}

#[test]
fn looping_rigid_intervals_cancel_common_cycles_without_changing_order() {
    let clip = clip(Transform {
        translation: Vec3::new(2.0, 0.0, 1.0),
        rotation: Vec3::new(0.0, 0.0, 90.0),
        ..Transform::IDENTITY
    });
    let a = 22.5_f32.to_radians();
    let b = 67.5_f32.to_radians();
    // independent planar geometry: turn the displacement into the departure frame.
    let cases = [
        (0.0, 1.0, Vec3::new(2.0, 0.0, 1.0), 90.0),
        (0.25, 0.75, Vec3::new(a.cos(), -a.sin(), 0.5), 45.0),
        (
            0.75,
            1.25,
            Vec3::new(0.5 * (b.cos() + b.sin()), 0.5 * (b.cos() - b.sin()), 0.5),
            45.0,
        ),
        // two quarter turns place the cycle origin at (2, 2, 2).
        (
            0.25,
            2.75,
            Vec3::new(2.0 * a.sin(), 2.0 * a.cos(), 2.5),
            225.0,
        ),
    ];
    for (from, to, translation, angle) in cases {
        let expected = Transform {
            translation,
            rotation: Vec3::new(0.0, 0.0, angle),
            ..Transform::IDENTITY
        }
        .to_matrix();
        for offset in [
            0.0,
            16_777_216.0,
            -16_777_216.0,
            1_099_511_627_776.0,
            -1_099_511_627_776.0,
        ] {
            let forward = motion(&skeleton(), &clip, offset + from, offset + to, true);
            let reverse = motion(&skeleton(), &clip, offset + to, offset + from, true);
            assert!(
                forward.approx_eq(&expected, 2e-5),
                "offset {offset}, {from} -> {to}"
            );
            assert!(
                reverse.approx_eq(&expected.inverse(), 2e-5),
                "reverse offset {offset}, {to} -> {from}"
            );
            assert!(forward.approx_eq(&motion(&skeleton(), &clip, from, to, true), 2e-5));
        }
    }
}

#[test]
fn indexed_queries_match_the_pure_evaluator_and_keep_an_immutable_snapshot() {
    use skeletal_assets::notifies::NotifyIndex;
    let mut events = vec![
        notify("end", 1.0),
        notify("b", 0.5),
        notify("zero", 0.0),
        notify("a", 0.5),
        notify("near", 0.048),
        notify("close-later", 0.50000006),
        notify("close-earlier", 0.49999997),
    ];
    let index = NotifyIndex::new(&events, 1.0).unwrap();
    for offset in [-1_099_511_627_776.0, -10.0, 0.0, 1_099_511_627_776.0] {
        for from in [0.0, 0.048, 0.5, 0.99, 1.0] {
            for to in [0.0, 0.048, 0.5, 1.0, 3.25] {
                for looping in [false, true] {
                    let interval = advance(offset + from, offset + to, looping);
                    let expected = collect_crossed_notifies(&events, 1.0, interval).unwrap();
                    let actual = index.collect(interval).unwrap();
                    let tuples = |hits: Vec<skeletal_assets::notifies::CrossedNotify<'_>>| {
                        hits.into_iter()
                            .map(|h| (h.notify.id.clone(), h.playback_time, h.cycle))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(tuples(actual), tuples(expected), "{interval:?}");
                }
            }
        }
    }
    events.clear();
    assert_eq!(index.collect(advance(0.0, 1.0, false)).unwrap().len(), 6);
    assert!(index.collect(PlaybackInterval::Seek).unwrap().is_empty());
    assert!(index.collect(PlaybackInterval::Paused).unwrap().is_empty());
    assert!(index.collect(advance(0.0, 1_000_002.0, true)).is_err());
    assert!(index.collect(advance(f64::NAN, 1.0, true)).is_err());
    assert!(NotifyIndex::new(&[notify("bad", 2.0)], 1.0).is_err());
}

#[test]
fn extended_notify_schema_versions_round_trips_and_rejects_invalid_saves() {
    use skeletal_assets::notifies::{NotifyField, NotifyTarget, NotifyValue};
    let mut asset = AnimSeqAsset::new(None, AnimationClip::bind_pose());
    let mut n = notify("step", 0.5);
    asset.features.notifies.push(n.clone());
    assert_eq!(serde_json::to_value(&asset).unwrap()["version"], 2);
    n.target = NotifyTarget::ClassVariable("current_target".into());
    n.fields = vec![
        NotifyField {
            name: "grounded".into(),
            value: NotifyValue::Bool(true),
        },
        NotifyField {
            name: "count".into(),
            value: NotifyValue::Int(i64::MAX),
        },
        NotifyField {
            name: "strength".into(),
            value: NotifyValue::Float(0.25),
        },
        NotifyField {
            name: "surface".into(),
            value: NotifyValue::String("stone".into()),
        },
    ];
    asset.features.notifies[0] = n.clone();
    let text = asset.to_text();
    let value = serde_json::to_value(&asset).unwrap();
    assert_eq!(value["version"], 3);
    assert!(value.get("features").is_none());
    assert!(value.get("clip").is_none());
    assert_eq!(AnimSeqAsset::parse(&text).unwrap().features, asset.features);
    assert_eq!(
        serde_json::from_value::<AnimSeqAsset>(value.clone())
            .unwrap()
            .features,
        asset.features
    );
    let mut future = value.clone();
    future["version"] = 4.into();
    assert!(AnimSeqAsset::parse(&future.to_string()).is_err());
    let old = r#"{"version":2,"duration":1.0,"fps":30.0,"tracks":[],"name":"old","notifies":[{"id":"old","name":"Old","time":0.5}]}"#;
    let old = AnimSeqAsset::parse(old).unwrap();
    assert!(old.features.notifies[0].target.is_self());
    assert!(old.features.notifies[0].fields.is_empty());
    for bad in [
        "entity",
        "source",
        "target",
        "class",
        "exec",
        "exec_out",
        "Body",
        "bad field",
        "",
        "2bad",
    ] {
        let mut invalid = n.clone();
        invalid.fields[0].name = bad.into();
        assert!(invalid.validate(1.0).is_err());
    }
    let mut invalid = n.clone();
    invalid.fields.push(invalid.fields[0].clone());
    assert!(invalid.validate(1.0).is_err());
    invalid = n.clone();
    invalid.target = NotifyTarget::ClassVariable("bad reference".into());
    assert!(invalid.validate(1.0).is_err());
    invalid = n;
    invalid.fields[2].value = NotifyValue::Float(f64::NAN);
    let file = tempfile::tempdir().unwrap();
    let file = file.path().join("sequence.animseq");
    skeletal_assets::assets::save_animseq_asset(&file, &asset).unwrap();
    let saved = std::fs::read(&file).unwrap();
    asset.features.notifies[0] = invalid;
    assert!(skeletal_assets::assets::save_animseq_asset(&file, &asset).is_err());
    assert_eq!(std::fs::read(file).unwrap(), saved);
}

fn occurrence_tuples(
    hits: Vec<skeletal_assets::notifies::CrossedNotify<'_>>,
) -> Vec<(String, f64, i64)> {
    hits.into_iter()
        .map(|h| (h.notify.id.clone(), h.playback_time, h.cycle))
        .collect()
}

#[test]
fn notify_nonbinary_wraps_share_a_timestamp_and_compose_in_both_directions() {
    use skeletal_assets::notifies::NotifyIndex;
    for duration in [0.1_f32, 0.3, 1.7] {
        let events = vec![
            notify("zero-b", 0.0),
            notify("end-b", duration),
            notify("zero-a", 0.0),
            notify("end-a", duration),
        ];
        let index = NotifyIndex::new(&events, duration).unwrap();
        for cycle in [
            4_294_967_297_i64,
            -4_294_967_297,
            16_777_216,
            -16_777_216,
            1_i64 << 51,
            -(1_i64 << 51),
        ] {
            let boundary = (cycle + 1) as f64 * f64::from(duration);
            for forward in [true, false] {
                let (from, to) = if forward {
                    (boundary.next_down(), boundary.next_up())
                } else {
                    (boundary.next_up(), boundary.next_down())
                };
                let expected = if forward {
                    vec![
                        ("end-b".into(), boundary, cycle),
                        ("end-a".into(), boundary, cycle),
                        ("zero-b".into(), boundary, cycle + 1),
                        ("zero-a".into(), boundary, cycle + 1),
                    ]
                } else {
                    vec![
                        ("zero-b".into(), boundary, cycle + 1),
                        ("zero-a".into(), boundary, cycle + 1),
                        ("end-b".into(), boundary, cycle),
                        ("end-a".into(), boundary, cycle),
                    ]
                };
                for indexed in [false, true] {
                    let query = |a, b| {
                        occurrence_tuples(if indexed {
                            index.collect(advance(a, b, true)).unwrap()
                        } else {
                            collect_crossed_notifies(&events, duration, advance(a, b, true))
                                .unwrap()
                        })
                    };
                    let whole = query(from, to);
                    assert_eq!(
                        whole, expected,
                        "duration={duration}, cycle={cycle}, forward={forward}, indexed={indexed}"
                    );
                    let mut split = query(from, boundary);
                    split.extend(query(boundary, to));
                    assert_eq!(whole, split);
                }
            }
        }
    }
}

#[test]
fn notify_actual_arrivals_and_departures_survive_large_clock_rounding() {
    use skeletal_assets::notifies::NotifyIndex;
    for cycle in [16_777_216_i64, -16_777_216, 1_i64 << 40, -(1_i64 << 40)] {
        for offset in [0.05_f32, 1.95, 0.0, 2.0] {
            let events = vec![notify("arrival", offset)];
            let index = NotifyIndex::new(&events, 2.0).unwrap();
            let arrival = cycle as f64 * 2.0 + f64::from(offset);
            for (from, to, count) in [
                (arrival.next_down(), arrival, 1),
                (arrival.next_up(), arrival, 1),
                (arrival, arrival.next_up(), 0),
                (arrival, arrival.next_down(), 0),
            ] {
                for indexed in [false, true] {
                    let hits = if indexed {
                        index.collect(advance(from, to, true)).unwrap()
                    } else {
                        collect_crossed_notifies(&events, 2.0, advance(from, to, true)).unwrap()
                    };
                    assert_eq!(hits.len(), count, "cycle={cycle}, offset={offset}, from={from:?}, to={to:?}, indexed={indexed}");
                    if count == 1 {
                        assert_eq!((hits[0].playback_time, hits[0].cycle), (arrival, cycle));
                    }
                }
            }
        }
    }
}

#[test]
fn notify_output_limit_counts_accepted_crossings_only() {
    use skeletal_assets::notifies::NotifyIndex;
    for events in [
        vec![notify("zero", 0.0)],
        vec![notify("end", 1.0)],
        vec![notify("zero", 0.0), notify("middle", 0.5)],
    ] {
        let end = 1_000_000.0 / events.len() as f64;
        let index = NotifyIndex::new(&events, 1.0).unwrap();
        for (from, to) in [(0.0, end), (end, 0.0), (-end, 0.0), (0.0, -end)] {
            assert_eq!(
                collect_crossed_notifies(&events, 1.0, advance(from, to, true))
                    .unwrap()
                    .len(),
                1_000_000
            );
            assert_eq!(
                index.collect(advance(from, to, true)).unwrap().len(),
                1_000_000
            );
        }
    }
}

#[test]
fn notify_output_limit_refuses_more_than_one_million_crossings() {
    use skeletal_assets::notifies::NotifyIndex;
    let events = vec![notify("zero", 0.0)];
    let index = NotifyIndex::new(&events, 1.0).unwrap();
    for end in [1_000_001.0, 1e12] {
        for (from, to) in [(0.0, end), (end, 0.0)] {
            assert!(
                collect_crossed_notifies(&events, 1.0, advance(from, to, true))
                    .unwrap_err()
                    .contains("one million")
            );
            assert!(index
                .collect(advance(from, to, true))
                .unwrap_err()
                .contains("one million"));
        }
    }
}

#[test]
fn notify_nonbinary_rounded_windows_keep_asset_order_and_split_equivalence() {
    use skeletal_assets::notifies::NotifyIndex;
    for duration in [0.1_f32, 0.3, 1.7] {
        // At large clocks these different phases may collapse onto a boundary.
        // Deliberately author later times first to check original asset order.
        let events = vec![
            notify("near-end", f32::from_bits(duration.to_bits() - 1)),
            notify("end", duration),
            notify("zero", 0.0),
            notify("near-zero", f32::MIN_POSITIVE),
            notify("middle-b", duration * 0.5),
            notify("middle-a", duration * 0.5),
        ];
        let index = NotifyIndex::new(&events, duration).unwrap();
        for cycle in [
            -((1_i64 << 52) - 8),
            -(1_i64 << 40),
            -3,
            0,
            3,
            1_i64 << 40,
            (1_i64 << 52) - 8,
        ] {
            let from = (cycle - 2) as f64 * f64::from(duration);
            let to = (cycle + 2) as f64 * f64::from(duration);
            for (from, to) in [(from, to), (to, from)] {
                let interval = advance(from, to, true);
                let expected = occurrence_tuples(
                    collect_crossed_notifies(&events, duration, interval).unwrap(),
                );
                let actual = occurrence_tuples(index.collect(interval).unwrap());
                assert_eq!(
                    actual, expected,
                    "duration={duration}, cycle={cycle}, interval={interval:?}"
                );
                for (_, split_at, _) in &expected {
                    let mut pure_split = occurrence_tuples(
                        collect_crossed_notifies(&events, duration, advance(from, *split_at, true))
                            .unwrap(),
                    );
                    pure_split.extend(occurrence_tuples(
                        collect_crossed_notifies(&events, duration, advance(*split_at, to, true))
                            .unwrap(),
                    ));
                    let mut indexed_split =
                        occurrence_tuples(index.collect(advance(from, *split_at, true)).unwrap());
                    indexed_split.extend(occurrence_tuples(
                        index.collect(advance(*split_at, to, true)).unwrap(),
                    ));
                    assert_eq!(pure_split, expected);
                    assert_eq!(indexed_split, expected);
                }
            }
        }
    }
}
