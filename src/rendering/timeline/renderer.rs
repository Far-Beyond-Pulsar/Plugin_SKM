//! `TimelineRenderer` — a single instanced-rectangle pipeline used for track
//! row backgrounds, keyframe diamonds, and the playhead.

use super::types::{RectInstance, TimelineUniforms};

struct RectState {
    pipeline: wgpu::RenderPipeline,
    uni_buf: wgpu::Buffer,
    uni_bg: wgpu::BindGroup,
    inst_buf: wgpu::Buffer,
    inst_cap: u64,
}

pub struct TimelineRenderer {
    rects: Option<RectState>,
}

impl TimelineRenderer {
    pub fn new() -> Self {
        Self { rects: None }
    }

    pub fn render_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        _w: u32,
        _h: u32,
        fmt: wgpu::TextureFormat,
        uniforms: &TimelineUniforms,
        rects: &[RectInstance],
    ) {
        if self.rects.is_none() {
            self.rects = Some(Self::create_rects(device, fmt));
        }

        let uni_bytes = bytemuck::bytes_of(uniforms);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("timeline_encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("timeline_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.07,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if !rects.is_empty() {
                if let Some(rs) = &mut self.rects {
                    queue.write_buffer(&rs.uni_buf, 0, uni_bytes);
                    let bytes = bytemuck::cast_slice(rects);
                    Self::ensure_buf(
                        device,
                        &mut rs.inst_buf,
                        &mut rs.inst_cap,
                        bytes,
                        wgpu::BufferUsages::VERTEX,
                    );
                    queue.write_buffer(&rs.inst_buf, 0, bytes);
                    pass.set_pipeline(&rs.pipeline);
                    pass.set_bind_group(0, &rs.uni_bg, &[]);
                    pass.set_vertex_buffer(0, rs.inst_buf.slice(..));
                    pass.draw(0..6, 0..rects.len() as u32);
                }
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
    }

    fn ensure_buf(
        device: &wgpu::Device,
        buf: &mut wgpu::Buffer,
        cap: &mut u64,
        data: &[u8],
        usage: wgpu::BufferUsages,
    ) {
        let needed = data.len() as u64;
        if needed > *cap {
            *cap = (needed * 2).max(256);
            *buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: *cap,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
    }

    fn create_rects(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> RectState {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("timeline_rects"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/timeline.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("timeline_uni_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uni_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("timeline_uni"),
            size: std::mem::size_of::<TimelineUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uni_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("timeline_uni_bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uni_buf.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("timeline_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let attrs = wgpu::vertex_attr_array![
            0 => Float32x2, // pos
            1 => Float32x2, // size
            2 => Float32x4, // color
            3 => Uint32,    // kind
            4 => Uint32x3,  // _pad
        ];
        let vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<RectInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &attrs,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("timeline_rects"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vbl],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: fmt,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let init_cap = 1024 * std::mem::size_of::<RectInstance>() as u64;
        let inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("timeline_rect_inst"),
            size: init_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        RectState {
            pipeline,
            uni_buf,
            uni_bg,
            inst_buf,
            inst_cap: init_cap,
        }
    }
}
