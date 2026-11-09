use super::super::{
    camera::OrbitCamera,
    skinning::{
        tests::{fixture, world},
        PreparedSkin, SkinFrame,
    },
};
use super::*;
use crate::rendering::preview::{self, Gpu};
use crate::style::Palette;
use std::sync::Arc;

fn draw_skin(
    gpu: &Gpu,
    renderer: &mut ViewportRenderer,
    uniforms: &ViewportUniforms,
    skin: Option<&SkinFrame>,
    size: u32,
) -> Vec<u8> {
    gpu.render(size, size, |device, queue, view| {
        renderer.render_frame(
            device,
            queue,
            view,
            size,
            size,
            preview::TARGET_FORMAT,
            &SceneFrame {
                uniforms,
                mesh: &[],
                transform_gizmo: &[],
                skin,
                ghost: &[],
                ribbons: &[],
                joints: &[],
                gizmo_ribbons: &[],
                gizmo_bubbles: &[],
                show_grid: true,
                xray: true,
            },
        );
    })
}

#[test]
#[ignore = "requires a wgpu adapter; explicitly run with --ignored --nocapture"]
fn skin_buffers_survive_playback_lod_switches_and_rebuild_on_reload() {
    let gpu = Gpu::new().expect("gpu resource test requires an adapter");
    let errors = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut renderer = ViewportRenderer::new();
    let mesh = fixture(4);
    let mut camera = OrbitCamera::default();
    camera.frame_sphere(crate::core::Vec3::ZERO, 30.0);
    let uniforms = ViewportUniforms::new(
        &Palette::test_dark(),
        camera.view_proj(1.0),
        camera.eye().to_array(),
        [128.0; 2],
        100.0,
        [0.0; 4],
    );
    let mut render = |mesh: Arc<PreparedSkin>, lod, time, size| {
        let frame = SkinFrame::new(mesh, lod, &world(time), Some("bone1"), [0.5; 4]).unwrap();
        let rgba = draw_skin(&gpu, &mut renderer, &uniforms, Some(&frame), size);
        assert!(preview::coverage(&rgba) > 0.2);
        renderer
            .pipelines
            .as_ref()
            .unwrap()
            .skin
            .as_ref()
            .unwrap()
            .stats
    };
    let initial = render(mesh.clone(), 0, 0.0, 128);
    assert_eq!(initial.buffers, 4);
    assert_eq!(initial.lod_uploads, 1);
    assert_eq!(initial.palette_uploads, 1);
    assert_eq!(initial.palette_bytes, 4 * 80);
    assert_eq!(
        render(mesh.clone(), 0, 0.0, 128),
        initial,
        "stationary frames upload nothing"
    );
    let animated = render(mesh.clone(), 0, 0.3, 128);
    assert_eq!(animated.buffers, initial.buffers);
    assert_eq!(animated.static_bytes, initial.static_bytes);
    assert_eq!(animated.palette_uploads, 2);
    let lod1 = render(mesh.clone(), 1, 0.3, 256);
    assert_eq!(lod1.buffers, 6);
    assert_eq!(lod1.lod_uploads, 2);
    assert_eq!(lod1.palette_uploads, 2);
    assert_eq!(
        render(mesh.clone(), 0, 0.3, 128),
        lod1,
        "returning to a cached lod and resizing retains buffers"
    );
    let mut replacement = (*mesh.asset).clone();
    replacement.lods[0].vertices[0].position[0] += 7.0;
    let reloaded = PreparedSkin::new(Arc::new(replacement)).unwrap();
    let reload = render(reloaded, 0, 0.3, 128);
    assert_eq!(reload.buffers, 10);
    assert_eq!(reload.palette_uploads, 3);
    let active = fixture(64);
    let switched = render(active.clone(), 0, 0.3, 128);
    assert_eq!(switched.buffers, 14);
    assert_eq!(switched.palette_uploads, 4);
    drop(render);
    let skin = renderer.pipelines.as_mut().unwrap().skin.as_mut().unwrap();
    let changed_skeleton = SkinFrame::new(
        active,
        0,
        &std::collections::HashMap::new(),
        Some("bone1"),
        [0.5; 4],
    )
    .unwrap();
    skin.update(&gpu.device, &gpu.queue, Some(&changed_skeleton));
    assert_eq!(skin.stats.buffers, 14);
    assert_eq!(skin.stats.palette_uploads, 5);
    skin.update(&gpu.device, &gpu.queue, None);
    skin.update(&gpu.device, &gpu.queue, Some(&changed_skeleton));
    assert_eq!(skin.stats.buffers, 18, "clearing a mesh drops its buffers");
    assert_eq!(skin.stats.palette_uploads, 6);
    let small = SkinFrame::new(fixture(1), 0, &world(0.0), None, [1.0; 4]).unwrap();
    skin.update(&gpu.device, &gpu.queue, Some(&small));
    assert_eq!(
        skin.stats.buffers, 22,
        "shrinking replaces the palette binding"
    );
    assert_eq!(skin.stats.palette_bytes, 4 * 80 * 3 + 64 * 80 * 3 + 80);
    let unweighted = SkinFrame::new(fixture(0), 0, &world(0.0), None, [1.0; 4]).unwrap();
    skin.update(&gpu.device, &gpu.queue, Some(&unweighted));
    assert_eq!(skin.stats.buffers, 26);
    assert_eq!(
        skin.stats.palette_uploads, 7,
        "empty palettes need no write"
    );
    assert!(smol::block_on(errors.pop()).is_none());
    eprintln!("gpu execution: playback, stationary frames, lods, resize, reload, palette resizing, skeleton replacement and clearing passed; {:?}", skin.stats);
}

#[test]
#[ignore = "requires a wgpu adapter; explicitly run with --ignored --nocapture"]
fn gpu_preview_instances_keep_independent_palettes_and_recreate_on_a_new_device() {
    let gpu = Gpu::new().expect("instance isolation requires an adapter");
    let errors = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mesh = fixture(4);
    let a = SkinFrame::new(mesh.clone(), 0, &world(0.0), None, [1.0; 4]).unwrap();
    let b = SkinFrame::new(mesh.clone(), 0, &world(0.3), None, [1.0; 4]).unwrap();
    let mut camera = OrbitCamera::default();
    camera.frame_sphere(crate::core::Vec3::ZERO, 30.0);
    let uniforms = ViewportUniforms::new(
        &Palette::test_dark(),
        camera.view_proj(1.0),
        camera.eye().to_array(),
        [128.0; 2],
        100.0,
        [0.0; 4],
    );
    let mut first = ViewportRenderer::new();
    let mut second = ViewportRenderer::new();
    for _ in 0..12 {
        draw_skin(&gpu, &mut first, &uniforms, Some(&a), 128);
        draw_skin(&gpu, &mut second, &uniforms, Some(&b), 128);
    }
    let interleaved = draw_skin(&gpu, &mut first, &uniforms, Some(&a), 128);
    let mut reference = ViewportRenderer::new();
    for _ in 0..12 {
        draw_skin(&gpu, &mut reference, &uniforms, Some(&a), 128);
    }
    let isolated = draw_skin(&gpu, &mut reference, &uniforms, Some(&a), 128);
    assert_eq!(
        interleaved, isolated,
        "another preview cannot change the first pose"
    );
    for renderer in [&first, &second] {
        let stats = renderer
            .pipelines
            .as_ref()
            .unwrap()
            .skin
            .as_ref()
            .unwrap()
            .stats;
        assert_eq!(stats.palette_uploads, 1);
        assert_eq!(stats.lod_uploads, 1);
    }
    let selected = SkinFrame::new(mesh.clone(), 0, &world(0.0), Some("bone1"), [1.0; 4]).unwrap();
    let before = first
        .pipelines
        .as_ref()
        .unwrap()
        .skin
        .as_ref()
        .unwrap()
        .stats;
    draw_skin(&gpu, &mut first, &uniforms, Some(&selected), 128);
    let after = first
        .pipelines
        .as_ref()
        .unwrap()
        .skin
        .as_ref()
        .unwrap()
        .stats;
    assert_eq!(after.palette_uploads, before.palette_uploads + 1);
    assert_eq!(
        after.buffers, before.buffers,
        "weight selection retains static buffers"
    );
    assert_eq!(after.static_bytes, before.static_bytes);
    // Reusing immutable preparation on a replacement device must create fresh resources.
    let replacement = Gpu::new().expect("replacement device");
    let replacement_errors = replacement
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    let mut recreated = ViewportRenderer::new();
    draw_skin(&replacement, &mut recreated, &uniforms, Some(&b), 128);
    let stats = recreated
        .pipelines
        .as_ref()
        .unwrap()
        .skin
        .as_ref()
        .unwrap()
        .stats;
    assert_eq!(stats.buffers, 4);
    assert_eq!(stats.palette_uploads, 1);
    assert_eq!(stats.lod_uploads, 1);
    assert!(smol::block_on(replacement_errors.pop()).is_none());
    assert!(smol::block_on(errors.pop()).is_none());
}
