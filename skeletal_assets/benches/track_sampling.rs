//! Run with `cargo bench -p skeletal_assets --bench track_sampling`.
//! Fixed, equivalent legacy-linear and cubic workloads for before/after runs.
use skeletal_assets::animation::{AnimationClip, BoneTrack, KeyInterpolation, Keyframe};
use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::math::{Transform, Vec3};
use skeletal_assets::runtime::clip::CompiledClip;
use skeletal_assets::runtime::pose::{BoneTransform, RuntimeSkeleton};
use skeletal_assets::skeleton::{Bone, Skeleton};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let skeleton = Skeleton {
        bones: vec![Bone {
            id: "root".into(),
            name: "root".into(),
            parent: None,
            bind_transform: Transform::IDENTITY,
        }],
    };
    let skel = RuntimeSkeleton::new("s", &skeleton).unwrap();
    for mode in [KeyInterpolation::Linear, KeyInterpolation::CubicSpline] {
        let track = BoneTrack {
            bone_id: "root".into(),
            keyframes: (0..64)
                .map(|i| {
                    let mut k = Keyframe::linear(
                        i as f32 * 0.125,
                        Transform {
                            translation: Vec3::new(i as f32, 0.0, 0.0),
                            ..Transform::IDENTITY
                        },
                    );
                    if mode == KeyInterpolation::CubicSpline {
                        k.translation_interpolation = mode;
                        k.scale_interpolation = mode;
                        k.rotation_interpolation = mode;
                        k.rotation_quaternion =
                            Some([0.0, 0.0, (i as f32 * 0.02).sin(), (i as f32 * 0.02).cos()]);
                        k.in_tangent.translation = [2.0, -1.0, 1.0];
                        k.out_tangent.translation = [1.0, 1.0, -2.0];
                        k.in_tangent.rotation = [0.1, 0.0, 0.2, -0.1];
                        k.out_tangent.rotation = [0.0, 0.2, 0.1, 0.1];
                    }
                    k
                })
                .collect(),
        };
        let asset = AnimSeqAsset::new(
            None,
            AnimationClip {
                name: "bench".into(),
                duration: 8.0,
                fps: 60.0,
                tracks: vec![track.clone()],
            },
        );
        let compiled = CompiledClip::compile(&asset, &skel);
        for trial in 0..5 {
            const N: u32 = 1_000_000;
            let start = Instant::now();
            for i in 0..N {
                black_box(black_box(&track).sample(black_box((i % 8064) as f32 / 1024.0)));
            }
            let editor = start.elapsed().as_nanos() as f64 / N as f64;
            let start = Instant::now();
            for i in 0..N {
                black_box(black_box(&compiled).sample_bone(
                    0,
                    black_box((i % 8064) as f32 / 1024.0),
                    BoneTransform::IDENTITY,
                ));
            }
            let runtime = start.elapsed().as_nanos() as f64 / N as f64;
            println!("{mode:?} trial {trial}: BoneTrack {editor:.1} ns/sample, CompiledClip {runtime:.1} ns/sample");
        }
    }
}
