use super::tests::{fixture, world};
use super::*;
use crate::core::skinning::skin_vertex;
use crate::core::skinning::{bone_influence, joint_matrices};

const READBACK_SHADER: &str = r#"
@group(2) @binding(0) var<storage, read> input_words: array<u32>;
struct Output { position: vec4<f32>, normal: vec4<f32>, color: vec4<f32> };
@group(2) @binding(1) var<storage, read_write> output_vertices: array<Output>;
fn floats(i: u32) -> vec3<f32> {
    return vec3<f32>(bitcast<f32>(input_words[i]), bitcast<f32>(input_words[i+1u]), bitcast<f32>(input_words[i+2u]));
}
@compute @workgroup_size(64)
fn parity(@builtin(global_invocation_id) id: vec3<u32>) {
    let base = id.x * 16u;
    if base >= arrayLength(&input_words) { return; }
    let j = vec4<u32>(input_words[base+8u], input_words[base+9u], input_words[base+10u], input_words[base+11u]);
    let w = vec4<f32>(floats(base+12u), bitcast<f32>(input_words[base+15u]));
    let v = skin_vertex(floats(base), floats(base+3u), j, w);
    output_vertices[id.x] = Output(vec4<f32>(v.position, 1.0), vec4<f32>(v.normal, 0.0), skin_weight_color(v.weight));
}
"#;

#[test]
fn readback_shader_validates_portably() {
    let source = format!("{}\n{READBACK_SHADER}", super::super::shaders::skin());
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
}

#[test]
#[ignore = "requires a wgpu adapter; explicitly run with --ignored --nocapture"]
fn gpu_skinning_matches_cpu_reference() {
    use wgpu::util::DeviceExt;
    let gpu = crate::rendering::preview::Gpu::new().expect("gpu parity requires an adapter");
    let device = &gpu.device;
    let queue = &gpu.queue;
    let errors = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let source = format!("{}\n{READBACK_SHADER}", super::super::shaders::skin());
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("skin_parity"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let storage_entry = |binding, read_only| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let palette_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            storage_entry(0, true),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let io_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[storage_entry(0, true), storage_entry(1, false)],
    });
    let empty = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[],
    });
    let empty_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &empty,
        entries: &[],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&empty), Some(&palette_layout), Some(&io_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(&layout),
        module: &shader,
        entry_point: Some("parity"),
        compilation_options: Default::default(),
        cache: None,
    });
    let check = |mesh: &Arc<PreparedSkin>, w: &HashMap<String, Mat4>, selected: &str, time: f32| {
        let frame = SkinFrame::new(mesh.clone(), 0, w, Some(selected), [1.0; 4]).unwrap();
        let upload = |contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents,
                usage,
            })
        };
        let dummy = SkinJoint {
            matrix: Mat4::IDENTITY.0,
            selected: [0.0; 4],
        };
        let palette_data = if frame.joints.is_empty() {
            bytemuck::bytes_of(&dummy)
        } else {
            bytemuck::cast_slice(&frame.joints)
        };
        let palette = upload(palette_data, wgpu::BufferUsages::STORAGE);
        let uniforms = upload(
            bytemuck::bytes_of(&frame.uniforms),
            wgpu::BufferUsages::UNIFORM,
        );
        let vertices = upload(
            bytemuck::cast_slice(&mesh.pack_vertices(0)),
            wgpu::BufferUsages::STORAGE,
        );
        let size = mesh.lods[0].vertex_count as u64 * 48;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = |layout: &wgpu::BindGroupLayout, a: &wgpu::Buffer, b: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: a.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: b.as_entire_binding(),
                    },
                ],
            })
        };
        let palette_group = bind(&palette_layout, &palette, &uniforms);
        let io = bind(&io_layout, &vertices, &output);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &empty_group, &[]);
            pass.set_bind_group(1, &palette_group, &[]);
            pass.set_bind_group(2, &io, &[]);
            pass.dispatch_workgroups((mesh.lods[0].vertex_count as u32).div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
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
        let actual: &[[f32; 12]] = bytemuck::cast_slice(&data);
        let matrices = joint_matrices(&mesh.asset, w);
        let influences = bone_influence(&mesh.asset, &mesh.asset.lods[0], selected);
        for (i, (source, actual)) in mesh.asset.lods[0].vertices.iter().zip(actual).enumerate() {
            let expected = skin_vertex(source, &matrices);
            let color = super::super::scene::weight_color(influences[i]);
            let expected = [
                expected.position.x,
                expected.position.y,
                expected.position.z,
                1.0,
                expected.normal.x,
                expected.normal.y,
                expected.normal.z,
                0.0,
                color[0],
                color[1],
                color[2],
                color[3],
            ];
            for (component, (&a, &e)) in actual.iter().zip(&expected).enumerate() {
                // absolute + relative tolerance covers shader fma and reciprocal rounding.
                let tolerance = if component < 3 {
                    2e-5 + 2e-6 * e.abs()
                } else {
                    3e-6
                };
                assert!(a.is_finite() && (a-e).abs() <= tolerance,"joints={}, time={time}, vertex={i}, component={component}: gpu {a} cpu {e}, tolerance {tolerance}",mesh.asset.joints.len());
            }
        }
        drop(data);
        readback.unmap();
        eprintln!(
            "readback parity: {} joints, {} vertices at {time}",
            mesh.asset.joints.len(),
            mesh.lods[0].vertex_count
        );
    };
    for count in [0, 1, 4, 65536] {
        let mesh = fixture(count);
        for time in [-2.0, -1.0, 0.0, 0.3, 2.0] {
            let w = if time == -2.0 {
                mesh.asset
                    .joints
                    .iter()
                    .map(|j| (j.bone.clone(), Mat4::from_scale(Vec3::ZERO)))
                    .collect()
            } else if time < 0.0 {
                mesh.asset
                    .joints
                    .iter()
                    .map(|j| (j.bone.clone(), j.inverse_bind_matrix().inverse()))
                    .collect()
            } else {
                world(time)
            };
            check(&mesh, &w, "bone1", time);
        }
    }
    if let Some(dir) = std::env::var_os("SKM_SAMPLE_OUT") {
        use crate::core::assets::{load_animseq_asset, load_skeleton_asset};
        use crate::core::{evaluate_world_transforms, AnimationClip};
        let files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let file = |ext: &str| {
            files
                .iter()
                .find(|p| p.extension().is_some_and(|e| e == ext))
                .unwrap()
        };
        let skeleton = load_skeleton_asset(file("skeleton")).unwrap().skeleton();
        let asset = crate::core::skmesh::load_skmesh_asset(file("skmesh")).unwrap();
        let sequence = load_animseq_asset(file("animseq")).unwrap();
        let selected = asset.joints[0].bone.clone();
        let mesh = PreparedSkin::new(Arc::new(asset)).unwrap();
        check(
            &mesh,
            &evaluate_world_transforms(&skeleton, &AnimationClip::bind_pose(), 0.0),
            &selected,
            0.0,
        );
        for time in [
            0.0,
            sequence.clip.duration * 0.3,
            sequence.clip.duration * 0.8,
        ] {
            check(
                &mesh,
                &evaluate_world_transforms(&skeleton, &sequence.clip, time),
                &selected,
                time,
            );
        }
    }
    assert!(smol::block_on(errors.pop()).is_none());
    eprintln!("GPU position, normal and heat-map parity passed");
}
