//! reproducible generated-mesh comparison with the old push_skin geometry path.

use super::*;
use crate::core::skinning::joint_matrices;
use crate::core::{
    skinning::skin_lod,
    skmesh::{Joint, MeshLod, SkinnedVertex},
};
use crate::rendering::viewport::{camera::bounding_sphere, types::MeshVertex};
use std::{hint::black_box, time::Instant};

fn asset(vertices: usize, joint_count: usize) -> Arc<SkeletalMeshAsset> {
    let mut mesh = SkeletalMeshAsset::new(None);
    mesh.joints = (0..joint_count)
        .map(|j| Joint {
            bone: format!("bone{j}"),
            inverse_bind: Mat4::IDENTITY.0,
        })
        .collect();
    let vertices: Vec<_> = (0..vertices)
        .map(|i| SkinnedVertex {
            position: [
                (i % 100) as f32 * 0.01,
                (i / 100) as f32 * 0.001,
                (i % 17) as f32 * 0.02,
            ],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
            joints: std::array::from_fn(|j| ((i + j) % joint_count) as u16),
            weights: [0.1, 0.2, 0.3, 0.4],
        })
        .collect();
    let indices = (0..vertices.len().saturating_sub(2))
        .flat_map(|i| [i as u32, (i + 1) as u32, (i + 2) as u32])
        .collect();
    mesh.lods.push(MeshLod {
        vertices,
        indices,
        ..MeshLod::default()
    });
    Arc::new(mesh)
}

fn pose(time: f32, joint_count: usize) -> HashMap<String, Mat4> {
    (0..joint_count)
        .map(|j| {
            (
                format!("bone{j}"),
                Mat4::from_translation(Vec3::new(time.sin(), j as f32 * 0.001, 0.0))
                    .mul(&Mat4::from_euler_xyz_deg(Vec3::new(0.0, 0.0, time * 30.0))),
            )
        })
        .collect()
}

// this is upstream push_skin's plain-color path, including its bounds work.
fn cpu_geometry(mesh: &SkeletalMeshAsset, world: &HashMap<String, Mat4>) -> Vec<MeshVertex> {
    let lod = &mesh.lods[0];
    let posed = skin_lod(lod, &joint_matrices(mesh, world));
    let mut out = Vec::with_capacity(lod.indices.len());
    for triangle in lod.indices.as_chunks::<3>().0 {
        if triangle.iter().any(|&i| i as usize >= posed.len()) {
            continue;
        }
        for &i in triangle {
            let v = posed[i as usize];
            out.push(MeshVertex {
                pos: v.position.to_array(),
                normal: v.normal.to_array(),
                color: [0.5; 4],
                glow: 0.0,
            });
        }
    }
    let points: Vec<_> = posed.iter().map(|v| v.position).collect();
    black_box(bounding_sphere(&points));
    out
}

fn measure(iterations: usize, mut f: impl FnMut(usize)) -> f64 {
    for i in 0..5 {
        f(i);
    }
    let start = Instant::now();
    for i in 0..iterations {
        f(i);
    }
    start.elapsed().as_secs_f64() * 1e6 / iterations as f64
}

#[test]
#[ignore = "cpu benchmark; run optimized, separately from gpu tests"]
fn skinning_geometry_benchmark() {
    eprintln!("vertices,joints,mode,cpu_geometry_us,palette_bounds_us,prepare_mesh_us,reload_gpu_bytes,old_frame_bytes,animated_palette_bytes");
    for (count, joint_count) in [(10_000, 1), (60_000, 64), (500_000, 128)] {
        let asset = asset(count, joint_count);
        let start = Instant::now();
        let mesh = PreparedSkin::new(asset.clone()).unwrap();
        let prepare_us = start.elapsed().as_secs_f64() * 1e6;
        let worlds: Vec<_> = (0..64)
            .map(|i| pose(i as f32 * 0.017, joint_count))
            .collect();
        for mode in ["stationary", "playback", "scrubbing"] {
            let index = |i: usize| match mode {
                "stationary" => 0,
                "playback" => i % 64,
                _ => (i * 37) % 64,
            };
            let old = measure(30, |i| {
                black_box(cpu_geometry(&asset, black_box(&worlds[index(i)])));
            });
            let new = measure(1000, |i| {
                let frame = SkinFrame::new(
                    mesh.clone(),
                    0,
                    black_box(&worlds[index(i)]),
                    None,
                    [0.5; 4],
                )
                .unwrap();
                let bounds = mesh.lods[0].posed_bounds(&frame.joints).unwrap();
                black_box((bounding_sphere(&bounds), frame));
            });
            eprintln!(
                "{count},{joint_count},{mode},{old:.3},{new:.3},{prepare_us:.3},{},{},{}",
                mesh.lods[0].vertex_count * 64 + mesh.lods[0].indices.len() * 4,
                asset.lods[0].indices.len() * std::mem::size_of::<MeshVertex>(),
                joint_count * std::mem::size_of::<SkinJoint>()
            );
        }
    }
}

#[test]
#[ignore = "render benchmark; run optimized, separately from correctness tests"]
fn skinning_render_benchmark() {
    use crate::rendering::preview::{Gpu, TARGET_FORMAT};
    use crate::rendering::viewport::{
        camera::OrbitCamera,
        renderer::{SceneFrame, ViewportRenderer},
        types::ViewportUniforms,
    };
    use crate::style::Palette;
    let gpu = Gpu::new().expect("render benchmark requires an adapter");
    let device = &gpu.device;
    let timestamps = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
    eprintln!("vertices,joints,path,preparation_us,synchronized_offscreen_frame_us,scene_gpu_us");
    for (count, joint_count) in [(10_000, 1), (60_000, 64), (500_000, 128)] {
        let asset = asset(count, joint_count);
        let prepared = PreparedSkin::new(asset.clone()).unwrap();
        let worlds: Vec<_> = (0..20)
            .map(|i| pose(i as f32 * 0.017, joint_count))
            .collect();
        let mut camera = OrbitCamera::default();
        camera.frame_sphere(Vec3::new(0.5, 2.5, 0.0), 6.0);
        let uniforms = ViewportUniforms::new(
            &Palette::test_dark(),
            camera.view_proj(1.0),
            camera.eye().to_array(),
            [256.0; 2],
            100.0,
            [0.0; 4],
        );
        for gpu_skin in [false, true] {
            let mut renderer = ViewportRenderer::new();
            let queries = timestamps.then(|| {
                device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("skin_benchmark_timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                })
            });
            renderer.scene_timestamps = queries.clone();
            let resolve = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let (mut prep, mut frame, mut scene) = (0.0, 0.0, 0.0);
            // The readback harness owns a stable output target during the measured frames.
            gpu.render(256, 256, |device, queue, target| {
                for (i, world) in worlds.iter().enumerate() {
                    let start = Instant::now();
                    let (skin, cpu) = if gpu_skin {
                        let skin =
                            SkinFrame::new(prepared.clone(), 0, world, None, [0.5; 4]).unwrap();
                        black_box(prepared.lods[0].posed_bounds(&skin.joints));
                        (Some(skin), Vec::new())
                    } else {
                        (None, cpu_geometry(&asset, world))
                    };
                    let prepare = start.elapsed().as_secs_f64();
                    renderer.render_frame(
                        device,
                        queue,
                        target,
                        256,
                        256,
                        TARGET_FORMAT,
                        &SceneFrame {
                            uniforms: &uniforms,
                            mesh: &cpu,
                            transform_gizmo: &[],
                            skin: skin.as_ref(),
                            ghost: &[],
                            ribbons: &[],
                            joints: &[],
                            gizmo_ribbons: &[],
                            gizmo_bubbles: &[],
                            show_grid: false,
                            xray: false,
                        },
                    );
                    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    let elapsed = start.elapsed().as_secs_f64();
                    if i >= 5 {
                        prep += prepare;
                        frame += elapsed;
                    }
                    if let Some(queries) = &queries {
                        let mut encoder = device.create_command_encoder(&Default::default());
                        encoder.resolve_query_set(queries, 0..2, &resolve, 0);
                        encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 16);
                        queue.submit([encoder.finish()]);
                        let (send, recv) = std::sync::mpsc::channel();
                        readback
                            .slice(..)
                            .map_async(wgpu::MapMode::Read, move |result| {
                                send.send(result).unwrap();
                            });
                        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                        recv.recv().unwrap().unwrap();
                        let data = readback.slice(..).get_mapped_range().unwrap();
                        let ticks: &[u64] = bytemuck::cast_slice(&data);
                        assert!(ticks[1] >= ticks[0], "non-monotonic GPU timestamps");
                        if i >= 5 {
                            scene += (ticks[1] - ticks[0]) as f64
                                * queue.get_timestamp_period() as f64
                                * 1e-3;
                        }
                        drop(data);
                        readback.unmap();
                    }
                    black_box((skin, cpu));
                }
            });
            let path = if gpu_skin { "gpu" } else { "cpu" };
            let scene = if timestamps {
                format!("{:.3}", scene / 15.0)
            } else {
                "unsupported".into()
            };
            eprintln!(
                "{count},{joint_count},{path},{:.3},{:.3},{scene}",
                prep * 1e6 / 15.0,
                frame * 1e6 / 15.0
            );
        }
    }
}
