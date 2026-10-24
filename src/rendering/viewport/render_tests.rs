//! Headless renders of the viewport. Set `SKM_PREVIEW_DIR` to also write the
//! frames out as PNGs for inspection.

use crate::core::assets::SkeletonAsset;
use crate::core::sample;
use crate::editor::panel::ViewOptions;
use crate::rendering::preview::{self, Gpu};
use crate::style::Palette;

use super::camera::{OrbitCamera, ViewAxis};
use super::renderer::{SceneFrame, ViewportRenderer};
use super::scene::{build_gizmo, build_scene, SceneInput, SkinInput};
use super::types::ViewportUniforms;

struct Shot {
    name: &'static str,
    palette: Palette,
    camera: OrbitCamera,
    selected: Option<&'static str>,
    hovered: Option<&'static str>,
    view: ViewOptions,
    time: f32,
    size: (u32, u32),
    /// Draw the sample skinned mesh, colored by the selected bone's weights.
    skin: bool,
}

fn render_shot(gpu: &Gpu, shot: Shot) -> Vec<u8> {
    let skeleton_asset = SkeletonAsset::new(sample::humanoid_skeleton());
    let skeleton = skeleton_asset.skeleton();
    let clip = sample::run_animation(&skeleton);
    let mesh = sample::sample_mesh(&skeleton_asset);
    let geo = build_scene(&SceneInput {
        skeleton: &skeleton,
        clip: &clip,
        time: shot.time,
        range: (0.0, clip.duration),
        selected: shot.selected,
        hovered: shot.hovered,
        view: shot.view,
        palette: &shot.palette,
        tracers: None,
        skin: shot.skin.then_some(SkinInput {
            mesh: &mesh,
            weights_of: shot.selected,
        }),
        physics: None,
        sockets: None,
    });

    let (w, h) = shot.size;
    let mut camera = shot.camera;
    camera.frame_sphere(geo.center, geo.radius);
    camera.yaw_deg = shot.camera.yaw_deg;
    camera.pitch_deg = shot.camera.pitch_deg;

    let aspect = w as f32 / h as f32;
    let view_proj = camera.view_proj(aspect);
    let uniforms = ViewportUniforms::new(
        &shot.palette,
        view_proj,
        camera.eye().to_array(),
        [w as f32, h as f32],
        (camera.distance * 8.0).max(30.0),
        geo.shadow(&shot.palette),
    );
    let gizmo = build_gizmo(&camera, w as f32, h as f32, &shot.palette);

    let mut renderer = ViewportRenderer::new();
    let rgba = gpu.render(w, h, |device, queue, view| {
        // Let the temporal AA converge, as it would over a few live frames.
        for _ in 0..12 {
            renderer.render_frame(
                device,
                queue,
                view,
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
                    show_grid: shot.view.grid,
                    xray: shot.view.xray,
                },
            );
        }
    });
    preview::save(shot.name, w, h, &rgba);
    rgba
}

#[test]
fn render_viewport_previews() {
    let Some(gpu) = Gpu::new() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };

    let dark = render_shot(
        &gpu,
        Shot {
            name: "viewport_dark",
            palette: Palette::test_dark(),
            camera: OrbitCamera::default(),
            selected: Some("thigh_r"),
            hovered: Some("hand_l"),
            view: ViewOptions {
                onion_skin: true,
                ..ViewOptions::default()
            },
            time: 0.2,
            size: (1000, 640),
            skin: false,
        },
    );
    assert!(preview::coverage(&dark) > 0.5, "viewport rendered blank");

    // Bones must actually be drawn: the centre column differs from the
    // background gradient at the same height somewhere.
    let (w, h) = (1000usize, 640usize);
    let row = |y: usize| -> &[u8] { &dark[y * w * 4..(y + 1) * w * 4] };
    let varied = (h / 4..h * 3 / 4).any(|y| {
        let r = row(y);
        let edge = &r[40 * 4..44 * 4];
        r.chunks_exact(4)
            .any(|px| px.iter().zip(edge).any(|(a, b)| a.abs_diff(*b) > 40))
    });
    assert!(varied, "no geometry stood out from the backdrop");

    let mut camera = OrbitCamera::default();
    camera.snap_to(ViewAxis::PosZ);
    render_shot(
        &gpu,
        Shot {
            name: "viewport_front",
            palette: Palette::test_dark(),
            camera,
            selected: Some("pelvis"),
            hovered: None,
            view: ViewOptions {
                motion_path: true,
                ..ViewOptions::default()
            },
            time: 0.35,
            size: (760, 560),
            skin: false,
        },
    );

    render_shot(
        &gpu,
        Shot {
            name: "viewport_light",
            palette: Palette::test_light(),
            camera: OrbitCamera {
                yaw_deg: -40.0,
                pitch_deg: 24.0,
                ..OrbitCamera::default()
            },
            selected: Some("hand_r"),
            hovered: None,
            view: ViewOptions::default(),
            time: 0.1,
            size: (760, 560),
            skin: false,
        },
    );
}

/// The skinned mesh is drawn, and the selected bone's weights show as a heat
/// map: red where the bone is in full control, blue where it has no say.
#[test]
fn render_skinned_mesh_with_weights() {
    let Some(gpu) = Gpu::new() else { return };
    let shot = |selected, skin| Shot {
        name: if skin {
            "viewport_skin"
        } else {
            "viewport_plain"
        },
        palette: Palette::test_dark(),
        camera: OrbitCamera::default(),
        selected,
        hovered: None,
        view: ViewOptions::default(),
        time: 0.2,
        size: (900, 640),
        skin,
    };
    let count = |rgba: &[u8], pick: fn(u8, u8, u8) -> bool| {
        rgba.chunks_exact(4)
            .filter(|px| pick(px[0], px[1], px[2]))
            .count()
    };
    let red = |r: u8, g: u8, b: u8| r > g.saturating_add(70) && r > b.saturating_add(70);
    let blue = |r: u8, g: u8, b: u8| b > r.saturating_add(70) && b > g.saturating_add(40);

    let weights = render_shot(&gpu, shot(Some("thigh_r"), true));
    let plain = render_shot(&gpu, shot(None, false));
    assert!(preview::coverage(&weights) > 0.5, "viewport rendered blank");
    assert!(
        count(&weights, red) > 200,
        "no pixel shows the bone's full weight"
    );
    assert!(
        count(&weights, blue) > count(&plain, blue) + 200,
        "no pixel shows an unweighted part"
    );
    assert!(
        count(&weights, red) > count(&plain, red),
        "the heat map adds red"
    );
}

/// Render a run of frames with the pose changing every frame, so any temporal
/// ghosting from the TAA history shows up in the last one.
#[test]
fn render_viewport_in_motion() {
    let Some(gpu) = Gpu::new() else { return };
    let skeleton = sample::humanoid_skeleton();
    let clip = sample::run_animation(&skeleton);
    let palette = Palette::test_dark();
    let (w, h) = (760u32, 560u32);

    let mut camera = OrbitCamera::default();
    let mut renderer = ViewportRenderer::new();
    let rgba = gpu.render(w, h, |device, queue, view| {
        for i in 0..24 {
            let geo = build_scene(&SceneInput {
                skeleton: &skeleton,
                clip: &clip,
                time: (i as f32 * 1.0 / 60.0) % clip.duration,
                range: (0.0, clip.duration),
                selected: Some("shin_l"),
                hovered: None,
                view: ViewOptions::default(),
                palette: &palette,
                tracers: None,
                skin: None,
                physics: None,
                sockets: None,
            });
            if i == 0 {
                camera.frame_sphere(geo.center, geo.radius);
            }
            let uniforms = ViewportUniforms::new(
                &palette,
                camera.view_proj(w as f32 / h as f32),
                camera.eye().to_array(),
                [w as f32, h as f32],
                30.0,
                geo.shadow(&palette),
            );
            let gizmo = build_gizmo(&camera, w as f32, h as f32, &palette);
            renderer.render_frame(
                device,
                queue,
                view,
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
                    xray: true,
                },
            );
        }
    });
    preview::save("viewport_motion", w, h, &rgba);
    assert!(preview::coverage(&rgba) > 0.5);
}
