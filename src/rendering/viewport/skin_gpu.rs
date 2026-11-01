//! renderer-owned resources; replacing this owner also drops its device's cache.

use std::{mem::size_of, sync::Arc};
use wgpu::util::DeviceExt;

use super::renderer::{depth_state, module, pipeline, PipelineSpec, OFFSCREEN_FORMAT};
use super::skinning::{PreparedSkin, SkinFrame, SkinJoint, SkinUniforms, SkinVertex};

const VERTEX_BYTES: u64 = size_of::<SkinVertex>() as u64;
const JOINT_BYTES: u64 = size_of::<SkinJoint>() as u64;
const UNIFORM_BYTES: u64 = size_of::<SkinUniforms>() as u64;

pub const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Uint32x4, 4 => Float32x4
];

pub fn palette_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let entry = |binding, ty, size| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: std::num::NonZeroU64::new(size),
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("viewport_skin_palette_layout"),
        entries: &[
            entry(
                0,
                wgpu::BufferBindingType::Storage { read_only: true },
                JOINT_BYTES,
            ),
            entry(1, wgpu::BufferBindingType::Uniform, UNIFORM_BYTES),
        ],
    })
}

struct GpuLod {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

struct ResidentSkin {
    mesh: Arc<PreparedSkin>,
    lods: Vec<Option<GpuLod>>,
    palette: wgpu::Buffer,
    uniforms: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    last_joints: Vec<SkinJoint>,
    last_uniforms: Option<SkinUniforms>,
}

#[cfg(test)]
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadStats {
    pub buffers: usize,
    pub lod_uploads: usize,
    pub static_bytes: usize,
    pub palette_uploads: usize,
    pub palette_bytes: usize,
}

pub struct SkinGpu {
    pipeline: wgpu::RenderPipeline,
    #[cfg(test)]
    pub stats: UploadStats,
    layout: wgpu::BindGroupLayout,
    resident: Option<ResidentSkin>,
    // remember failures until the mesh changes, so huge assets don't spam every frame.
    rejected: Option<Arc<PreparedSkin>>,
}

pub fn supported(limits: &wgpu::Limits) -> bool {
    limits.max_storage_buffers_per_shader_stage > 0
        && limits.max_storage_buffer_binding_size >= JOINT_BYTES
        && limits.max_bind_groups >= 2
        && limits.max_uniform_buffers_per_shader_stage >= 2
        && limits.max_uniform_buffer_binding_size >= UNIFORM_BYTES
        && limits.max_vertex_attributes >= 5
        && limits.max_vertex_buffer_array_stride >= VERTEX_BYTES as u32
}

pub fn validate_sizes(mesh: &PreparedSkin, limits: &wgpu::Limits) -> Result<(), &'static str> {
    if !supported(limits) {
        return Err("device limits do not support skeletal preview skinning");
    }
    let palette_size = mesh.asset.joints.len().max(1) as u64 * JOINT_BYTES;
    if palette_size > limits.max_storage_buffer_binding_size
        || palette_size > limits.max_buffer_size
    {
        return Err("joint palette exceeds the device's storage buffer limit");
    }
    for lod in &mesh.lods {
        if lod.vertex_count as u64 * VERTEX_BYTES > limits.max_buffer_size
            || lod.indices.len() as u64 * 4 > limits.max_buffer_size
            || lod.indices.len() > u32::MAX as usize
        {
            return Err("skeletal mesh exceeds the device's vertex/index buffer limit");
        }
    }
    Ok(())
}

impl SkinGpu {
    pub fn new(device: &wgpu::Device, scene_layout: &wgpu::BindGroupLayout) -> Self {
        let layout = palette_layout(device);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport_skin_layout"),
            bind_group_layouts: &[Some(scene_layout), Some(&layout)],
            immediate_size: 0,
        });
        let shader = module(device, "viewport_skin", super::shaders::skin());
        let pipeline = pipeline(
            device,
            PipelineSpec {
                label: "viewport_skin",
                layout: &pipeline_layout,
                shader: &shader,
                vs: "vs_skin",
                fs: "fs_mesh",
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_BYTES,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTRIBUTES,
                })],
                format: OFFSCREEN_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                depth: Some(depth_state(true, wgpu::CompareFunction::Less)),
            },
        );
        Self {
            pipeline,
            #[cfg(test)]
            stats: UploadStats::default(),
            layout,
            resident: None,
            rejected: None,
        }
    }

    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: Option<&SkinFrame>,
    ) {
        let Some(frame) = frame else {
            self.resident = None;
            self.rejected = None;
            return;
        };
        if self
            .rejected
            .as_ref()
            .is_some_and(|m| Arc::ptr_eq(m, &frame.mesh))
        {
            return;
        }
        if self
            .resident
            .as_ref()
            .is_none_or(|r| !Arc::ptr_eq(&r.mesh, &frame.mesh))
        {
            self.resident = None;
            self.rejected = None;
            if let Err(error) = validate_sizes(&frame.mesh, &device.limits()) {
                log::warn!("cannot draw skeletal preview: {error}");
                self.rejected = Some(frame.mesh.clone());
                return;
            }
            #[cfg(test)]
            {
                self.stats.buffers += 2;
            }
            let palette = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("viewport_skin_palette"),
                size: frame.joints.len().max(1) as u64 * JOINT_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("viewport_skin_uniforms"),
                size: UNIFORM_BYTES,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewport_skin_bindings"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: palette.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: uniforms.as_entire_binding(),
                    },
                ],
            });
            self.resident = Some(ResidentSkin {
                lods: (0..frame.mesh.lods.len()).map(|_| None).collect(),
                mesh: frame.mesh.clone(),
                palette,
                uniforms,
                bindings,
                last_joints: Vec::new(),
                last_uniforms: None,
            });
        }
        let Some(r) = &mut self.resident else {
            return;
        };
        // queue writes precede this frame's submission, so bones and skin share one pose.
        if r.last_joints != frame.joints {
            if !frame.joints.is_empty() {
                queue.write_buffer(&r.palette, 0, bytemuck::cast_slice(&frame.joints));
                #[cfg(test)]
                {
                    self.stats.palette_uploads += 1;
                    self.stats.palette_bytes += frame.joints.len() * size_of::<SkinJoint>();
                }
            }
            r.last_joints.clone_from(&frame.joints);
        }
        if r.last_uniforms != Some(frame.uniforms) {
            queue.write_buffer(&r.uniforms, 0, bytemuck::bytes_of(&frame.uniforms));
            r.last_uniforms = Some(frame.uniforms);
        }
        let lod = &frame.mesh.lods[frame.lod];
        if r.lods[frame.lod].is_none() && !lod.indices.is_empty() {
            #[cfg(test)]
            {
                self.stats.buffers += 2;
                self.stats.lod_uploads += 1;
                self.stats.static_bytes +=
                    lod.vertex_count * size_of::<SkinVertex>() + lod.indices.len() * 4;
            }
            let upload = |label, bytes, usage| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytes,
                    usage,
                })
            };
            let vertices = frame.mesh.pack_vertices(frame.lod);
            r.lods[frame.lod] = Some(GpuLod {
                vertices: upload(
                    "viewport_skin_vertices",
                    bytemuck::cast_slice(&vertices),
                    wgpu::BufferUsages::VERTEX,
                ),
                indices: upload(
                    "viewport_skin_indices",
                    bytemuck::cast_slice(&lod.indices),
                    wgpu::BufferUsages::INDEX,
                ),
                count: lod.indices.len() as u32,
            });
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, frame: Option<&SkinFrame>) {
        let (Some(r), Some(frame)) = (&self.resident, frame) else {
            return;
        };
        if !Arc::ptr_eq(&r.mesh, &frame.mesh) {
            return;
        }
        let Some(lod) = &r.lods[frame.lod] else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, &r.bindings, &[]);
        pass.set_vertex_buffer(0, lod.vertices.slice(..));
        pass.set_index_buffer(lod.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..lod.count, 0, 0..1);
    }
}
