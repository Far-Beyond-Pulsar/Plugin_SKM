//! Headless renders of the viewport. Set `SKM_PREVIEW_DIR` to also write the
//! frames out as PNGs for inspection.

use crate::core::sample;
use crate::editor::panel::ViewOptions;
use crate::rendering::preview::{self, Gpu};
use crate::style::Palette;

use super::camera::{OrbitCamera, ViewAxis};
use super::renderer::{SceneFrame, ViewportRenderer};
use super::scene::{build_gizmo, build_scene, SceneInput};
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
}

fn render_shot(gpu: &Gpu, shot: Shot) -> Vec<u8> {
    let skeleton = sample::humanoid_skeleton();
    let clip = sample::run_animation(&skeleton);
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
        },
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
