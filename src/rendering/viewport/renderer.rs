//! `ViewportRenderer` — owns the WGPU pipelines that draw the 3D bone scene:
//! a ground grid, bone segments, and joint markers, all depth-tested against
//! a depth buffer that is recreated whenever the surface is resized.

use super::types::{JointInstance, LineVertex, ViewportUniforms};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

struct LineState {
    pipeline: wgpu::RenderPipeline,
    uni_buf: wgpu::Buffer,
    uni_bg: wgpu::BindGroup,
    vert_buf: wgpu::Buffer,
    vert_cap: u64,
}

struct JointState {
    pipeline: wgpu::RenderPipeline,
    uni_buf: wgpu::Buffer,
    uni_bg: wgpu::BindGroup,
    inst_buf: wgpu::Buffer,
    inst_cap: u64,
}

struct DepthState {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

pub struct ViewportRenderer {
    lines: Option<LineState>,
    joints: Option<JointState>,
    depth: Option<DepthState>,
}

impl ViewportRenderer {
    pub fn new() -> Self {
        Self {
            lines: None,
            joints: None,
            depth: None,
        }
    }

    /// Called every frame. `lines` covers both the ground grid and bone
    /// segments; `joints` is one billboard per bone.
    pub fn render_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        w: u32,
        h: u32,
        fmt: wgpu::TextureFormat,
        uniforms: &ViewportUniforms,
        lines: &[LineVertex],
        joints: &[JointInstance],
    ) {
        if self.lines.is_none() {
            self.lines = Some(Self::create_lines(device, fmt));
            self.joints = Some(Self::create_joints(device, fmt));
        }
        self.ensure_depth(device, w, h);

        let uni_bytes = bytemuck::bytes_of(uniforms);
        let depth_view = &self.depth.as_ref().unwrap().view;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewport_encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.06,
                            g: 0.065,
                            b: 0.075,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if !lines.is_empty() {
                if let Some(ls) = &mut self.lines {
                    queue.write_buffer(&ls.uni_buf, 0, uni_bytes);
                    let bytes = bytemuck::cast_slice(lines);
                    Self::ensure_buf(
                        device,
                        &mut ls.vert_buf,
                        &mut ls.vert_cap,
                        bytes,
                        wgpu::BufferUsages::VERTEX,
                    );
                    queue.write_buffer(&ls.vert_buf, 0, bytes);
                    pass.set_pipeline(&ls.pipeline);
                    pass.set_bind_group(0, &ls.uni_bg, &[]);
                    pass.set_vertex_buffer(0, ls.vert_buf.slice(..));
                    pass.draw(0..lines.len() as u32, 0..1);
                }
            }

            if !joints.is_empty() {
                if let Some(js) = &mut self.joints {
                    queue.write_buffer(&js.uni_buf, 0, uni_bytes);
                    let bytes = bytemuck::cast_slice(joints);
                    Self::ensure_buf(
                        device,
                        &mut js.inst_buf,
                        &mut js.inst_cap,
                        bytes,
                        wgpu::BufferUsages::VERTEX,
                    );
                    queue.write_buffer(&js.inst_buf, 0, bytes);
                    pass.set_pipeline(&js.pipeline);
                    pass.set_bind_group(0, &js.uni_bg, &[]);
                    pass.set_vertex_buffer(0, js.inst_buf.slice(..));
                    pass.draw(0..6, 0..joints.len() as u32);
                }
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
    }

    // ── helpers ────────────────────────────────────────────────────────────

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

    fn ensure_depth(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if let Some(d) = &self.depth {
            if d.width == w && d.height == h {
                return;
            }
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewport_depth"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth = Some(DepthState {
            view,
            width: w,
            height: h,
        });
    }

    fn uni_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewport_uni_bgl"),
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
        })
    }

    fn uni_buf_and_bg(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_uni"),
            size: std::mem::size_of::<ViewportUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport_uni_bg"),
            layout: bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        (buf, bg)
    }

    fn depth_stencil_state() -> wgpu::DepthStencilState {
        wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }
    }

    fn create_lines(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> LineState {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport_lines"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene.wgsl").into()),
        });
        let bgl = Self::uni_bind_group_layout(device);
        let (uni_buf, uni_bg) = Self::uni_buf_and_bg(device, &bgl);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport_lines_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // pos
            1 => Float32x4, // color
        ];
        let vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport_lines"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                buffers: &[vbl],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_line"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: fmt,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(Self::depth_stencil_state()),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let init_cap = 1024 * std::mem::size_of::<LineVertex>() as u64;
        let vert_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_line_verts"),
            size: init_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        LineState {
            pipeline,
            uni_buf,
            uni_bg,
            vert_buf,
            vert_cap: init_cap,
        }
    }

    fn create_joints(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> JointState {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport_joints"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene.wgsl").into()),
        });
        let bgl = Self::uni_bind_group_layout(device);
        let (uni_buf, uni_bg) = Self::uni_buf_and_bg(device, &bgl);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport_joints_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // center
            1 => Float32,   // size
            2 => Float32x4, // color
        ];
        let vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<JointInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &attrs,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport_joints"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_joint"),
                buffers: &[vbl],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_joint"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: fmt,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(Self::depth_stencil_state()),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let init_cap = 256 * std::mem::size_of::<JointInstance>() as u64;
        let inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_joint_inst"),
            size: init_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        JointState {
            pipeline,
            uni_buf,
            uni_bg,
            inst_buf,
            inst_cap: init_cap,
        }
    }
}
