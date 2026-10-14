//! Renders and checks a real rigged, animated model imported by `asset_import`
//! (see its `real_sample` test, which writes the files). Skipped unless
//! `SKM_SAMPLE_OUT` names the folder with the imported `.skeleton`, `.skmesh`
//! and `.animseq` files; nothing from the model is committed. Set
//! `SKM_PREVIEW_DIR` to get the PNGs, and `SKM_REF_JSON` to a reference of
//! skinned positions computed independently from the source (the glTF skinning
//! formula applied to its accessors) to compare against.

use std::path::{Path, PathBuf};

use crate::core::assets::{load_animseq_asset, load_skeleton_asset};
use crate::core::skinning::{joint_matrices, joint_vertex_counts, skin_lod};
use crate::core::skmesh::load_skmesh_asset;
use crate::core::{evaluate_world_transforms, AnimationClip};
use crate::editor::panel::ViewOptions;
use crate::rendering::preview::{self, Gpu};
use crate::style::Palette;

use super::camera::OrbitCamera;
use super::renderer::{SceneFrame, ViewportRenderer};
use super::scene::{build_gizmo, build_scene, SceneInput, SkinInput};
use super::types::ViewportUniforms;

fn files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == extension))
        .collect();
    found.sort();
    found
}

#[test]
fn render_real_sample() {
    let Some(dir) = std::env::var_os("SKM_SAMPLE_OUT").map(PathBuf::from) else {
        eprintln!("SKM_SAMPLE_OUT not set; skipping");
        return;
    };
    let skeleton_asset = load_skeleton_asset(&files(&dir, "skeleton")[0]).unwrap();
    let skeleton = skeleton_asset.skeleton();
    let mesh = load_skmesh_asset(&files(&dir, "skmesh")[0]).unwrap();
    assert_eq!(mesh.skeleton, Some(skeleton_asset.id.clone()));
    let sequences: Vec<_> = files(&dir, "animseq")
        .iter()
        .map(|p| load_animseq_asset(p).unwrap())
        .collect();

    // Compare with the independent reference, if given.
    if let Some(reference) = std::env::var_os("SKM_REF_JSON") {
        let reference: serde_json::Value =
            serde_json::from_slice(&std::fs::read(reference).unwrap()).unwrap();
        let picks: Vec<usize> = reference["picks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as usize)
            .collect();
        let duration = reference["duration"].as_f64().unwrap() as f32;
        let clip = &sequences[0].clip;
        let mut worst = 0.0f32;
        for (key, frame) in reference["frames"].as_object().unwrap() {
            let (clip_here, time) = if key == "bind" {
                (AnimationClip::bind_pose(), 0.0)
            } else {
                (clip.clone(), key.parse::<f32>().unwrap().min(duration))
            };
            let world = evaluate_world_transforms(&skeleton, &clip_here, time);
            let posed = skin_lod(&mesh.lods[0], &joint_matrices(&mesh, &world));
            for (n, &vertex) in picks.iter().enumerate() {
                let want = &frame["pts"][n];
                let got = posed[vertex].position.to_array();
                let err = (0..3)
                    .map(|i| (got[i] - want[i].as_f64().unwrap() as f32).abs())
                    .fold(0.0, f32::max);
                worst = worst.max(err);
                assert!(
                    err < 2e-3,
                    "frame {key} vertex {vertex}: ours {got:?} vs reference {want}"
                );
            }
        }
        eprintln!("worst deviation from the independent reference: {worst:e}");
    }

    let Some(gpu) = Gpu::new() else { return };
    let palette = Palette::test_dark();
    let render = |name: &str,
                  clip: &AnimationClip,
                  time: f32,
                  selected: Option<&str>,
                  yaw: f32,
                  bones: bool| {
        let view = ViewOptions {
            bones,
            weights: selected.is_some(),
            ..ViewOptions::default()
        };
        let scene = |time| SceneInput {
            skeleton: &skeleton,
            clip,
            time,
            range: (0.0, clip.duration),
            selected,
            hovered: None,
            view,
            palette: &palette,
            tracers: None,
            skin: Some(SkinInput {
                mesh: &mesh,
                weights_of: selected,
            }),
            physics: None,
        };
        // Frame the whole animation, not just this pose, so frames compare.
        let mut bounds: Option<(crate::core::Vec3, f32)> = None;
        for i in 0..=8 {
            let g = build_scene(&scene(clip.duration * i as f32 / 8.0));
            bounds = Some(match bounds {
                None => (g.center, g.radius),
                Some((c0, r0)) => (c0, r0.max(c0.sub(g.center).length() + g.radius)),
            });
        }
        let geo = build_scene(&scene(time));
        let (w, h) = (900u32, 700u32);
        let mut camera = OrbitCamera::default();
        let (c, r) = bounds.unwrap();
        camera.frame_sphere(c, r);
        camera.yaw_deg = yaw;
        camera.pitch_deg = 12.0;
        let uniforms = ViewportUniforms::new(
            &palette,
            camera.view_proj(w as f32 / h as f32),
            camera.eye().to_array(),
            [w as f32, h as f32],
            (camera.distance * 8.0).max(30.0),
            geo.shadow(&palette),
        );
        let gizmo = build_gizmo(&camera, w as f32, h as f32, &palette);
        let mut renderer = ViewportRenderer::new();
        let rgba = gpu.render(w, h, |device, queue, target| {
            for _ in 0..12 {
                renderer.render_frame(
                    device,
                    queue,
                    target,
                    w,
                    h,
                    preview::TARGET_FORMAT,
                    &SceneFrame {
                        uniforms: &uniforms,
                        mesh: &geo.mesh,
                        skin: &geo.skin,
                        ghost: &geo.ghost,
                        ribbons: &geo.ribbons,
                        joints: &geo.joints,
                        gizmo_ribbons: &gizmo.ribbons,
                        gizmo_bubbles: &gizmo.bubbles,
                        show_grid: true,
                        xray: bones,
                    },
                );
            }
        });
        assert!(preview::coverage(&rgba) > 0.3, "{name} rendered blank");
        preview::save(name, w, h, &rgba);
    };

    let bind = AnimationClip::bind_pose();
    render("real_bind_front", &bind, 0.0, None, 0.0, false);
    render("real_bind_side", &bind, 0.0, None, 90.0, false);
    render("real_bind_bones", &bind, 0.0, None, 35.0, true);
    // A joint with many vertices, for the weight view.
    let counts = joint_vertex_counts(&mesh, &mesh.lods[0]);
    let busiest = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| **c)
        .unwrap()
        .0;
    let bone = mesh.joints[busiest].bone.clone();
    render("real_weights", &bind, 0.0, Some(&bone), 35.0, true);
    for (n, sequence) in sequences.iter().enumerate() {
        for step in 0..5 {
            let time = sequence.clip.duration * step as f32 / 4.0;
            render(
                &format!("real_anim{n}_t{step}"),
                &sequence.clip,
                time,
                None,
                35.0,
                false,
            );
        }
    }
}
