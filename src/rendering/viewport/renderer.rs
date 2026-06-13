//! `ViewportRenderer` — owns the WGPU pipelines that draw the 3D bone scene:
//! a ground grid, bone segments, and joint markers, all depth-tested against
//! a depth buffer that is recreated whenever the surface is resized.

use crate::core::Mat4;

use super::types::{
    GizmoBubbleInstance, JointInstance, LineVertex, MeshVertex, ResolveUniforms, ViewportUniforms,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Format of the offscreen scene color and TAA history textures. Float so
/// the resolve pass can blend/clamp without banding.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Internal scene render resolution as a fraction of the output size.
/// `1.0` runs TAA at native resolution; values below `1.0` additionally
/// upscale (the resolve pass's bilinear `scene_color` sample does the
/// upscaling).
const RENDER_SCALE: f32 = 1.0;

/// Fraction of the previous frame's resolved color kept when blending with
/// the current frame.
const HISTORY_BLEND: f32 = 0.9;

/// Halton(2,3) sequence, mapped to `[-0.5, 0.5)` sub-pixel offsets, used to
/// jitter the camera projection for TAA sampling.
const JITTER_SAMPLES: [[f32; 2]; 8] = [
    [0.5 - 0.5, 0.333333 - 0.5],
    [0.25 - 0.5, 0.666667 - 0.5],
    [0.75 - 0.5, 0.111111 - 0.5],
    [0.125 - 0.5, 0.444444 - 0.5],
    [0.625 - 0.5, 0.777778 - 0.5],
    [0.375 - 0.5, 0.222222 - 0.5],
    [0.875 - 0.5, 0.555556 - 0.5],
    [0.0625 - 0.5, 0.888889 - 0.5],
];

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

struct MeshState {
    pipeline: wgpu::RenderPipeline,
    uni_buf: wgpu::Buffer,
    uni_bg: wgpu::BindGroup,
    vert_buf: wgpu::Buffer,
    vert_cap: u64,
}

/// Orientation gizmo: axis spokes (lines) + colored end bubbles, drawn into a
/// small square viewport in the corner of the frame.
struct GizmoState {
    axis_pipeline: wgpu::RenderPipeline,
    bubble_pipeline: wgpu::RenderPipeline,
    uni_buf: wgpu::Buffer,
    uni_bg: wgpu::BindGroup,
    axis_vert_buf: wgpu::Buffer,
    axis_vert_cap: u64,
    bubble_inst_buf: wgpu::Buffer,
    bubble_inst_cap: u64,
    bg_inst_buf: wgpu::Buffer,
    bg_inst_cap: u64,
}

struct DepthState {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// Offscreen targets for the TAA/upscale pipeline: the jittered scene is
/// rendered into `scene_color`/`scene_depth` at render resolution, then
/// resolved against a ping-ponged `history` pair at output resolution.
struct OffscreenState {
    scene_color: wgpu::TextureView,
    scene_depth: wgpu::TextureView,
    history: [wgpu::TextureView; 2],
    render_w: u32,
    render_h: u32,
    output_w: u32,
    output_h: u32,
}

struct ResolveState {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uni_buf: wgpu::Buffer,
}

pub struct ViewportRenderer {
    lines: Option<LineState>,
    joints: Option<JointState>,
    mesh: Option<MeshState>,
    gizmo: Option<GizmoState>,
    depth: Option<DepthState>,
    offscreen: Option<OffscreenState>,
    resolve: Option<ResolveState>,
    /// Index into `OffscreenState::history` that was written *last* frame,
    /// i.e. the one to read from this frame.
    history_idx: usize,
    /// True once the history buffer holds a previously-resolved frame.
    history_valid: bool,
    /// Previous frame's unjittered camera view-projection, for reprojection.
    prev_view_proj: Mat4,
    /// TAA jitter sequence position.
    frame_index: u64,
}

impl ViewportRenderer {
    pub fn new() -> Self {
        Self {
            lines: None,
            joints: None,
            mesh: None,
            gizmo: None,
            depth: None,
            offscreen: None,
            resolve: None,
            history_idx: 0,
            history_valid: false,
            prev_view_proj: Mat4::IDENTITY,
            frame_index: 0,
        }
    }

    /// Called every frame. `lines` covers both the ground grid and bone
    /// segments; `joints` is one billboard per bone; `mesh` is the shaded
    /// octahedral bone geometry. `gizmo_axes` and `gizmo_bubbles` are
    /// pre-projected NDC geometry for the orientation gizmo in the corner.
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
        mesh: &[MeshVertex],
        gizmo_axes: &[LineVertex],
        gizmo_bubbles: &[GizmoBubbleInstance],
        gizmo_background: &[GizmoBubbleInstance],
    ) {
        if self.lines.is_none() {
            // Lines/mesh/joints render into the offscreen scene target (see
            // Pass 1 below), not the swapchain, so their pipelines must match
            // `OFFSCREEN_FORMAT` rather than the swapchain's `fmt`.
            self.lines = Some(Self::create_lines(device, OFFSCREEN_FORMAT));
            self.joints = Some(Self::create_joints(device, OFFSCREEN_FORMAT));
            self.mesh = Some(Self::create_mesh(device, OFFSCREEN_FORMAT));
            self.gizmo = Some(Self::create_gizmo(device, fmt));
            self.resolve = Some(Self::create_resolve(device, fmt));
        }
        self.ensure_depth(device, w, h);
        self.ensure_offscreen(device, w, h);

        let off = self.offscreen.as_ref().unwrap();
        let render_w = off.render_w;
        let render_h = off.render_h;

        // Sub-pixel jitter for this frame, in NDC units (so it can be added
        // directly to clip-space x/y after multiplying by clip.w).
        let sample = JITTER_SAMPLES[(self.frame_index as usize) % JITTER_SAMPLES.len()];
        let jitter = [
            sample[0] * 2.0 / render_w as f32,
            sample[1] * 2.0 / render_h as f32,
        ];

        let mut scene_uniforms = *uniforms;
        scene_uniforms.jitter = jitter;
        scene_uniforms.viewport = [render_w as f32, render_h as f32];
        let uni_bytes = bytemuck::bytes_of(&scene_uniforms);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewport_encoder"),
        });

        // ── Pass 1: jittered scene -> offscreen color + depth ──────────────
        {
            let off = self.offscreen.as_ref().unwrap();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport_scene_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &off.scene_color,
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
                    view: &off.scene_depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
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

            if !mesh.is_empty() {
                if let Some(ms) = &mut self.mesh {
                    queue.write_buffer(&ms.uni_buf, 0, uni_bytes);
                    let bytes = bytemuck::cast_slice(mesh);
                    Self::ensure_buf(
                        device,
                        &mut ms.vert_buf,
                        &mut ms.vert_cap,
                        bytes,
                        wgpu::BufferUsages::VERTEX,
                    );
                    queue.write_buffer(&ms.vert_buf, 0, bytes);
                    pass.set_pipeline(&ms.pipeline);
                    pass.set_bind_group(0, &ms.uni_bg, &[]);
                    pass.set_vertex_buffer(0, ms.vert_buf.slice(..));
                    pass.draw(0..mesh.len() as u32, 0..1);
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

        // ── Pass 2: TAA/upscale resolve -> swapchain + history ──────────────
        {
            let off = self.offscreen.as_ref().unwrap();
            let resolve = self.resolve.as_ref().unwrap();
            let read_idx = 1 - self.history_idx;
            let write_idx = self.history_idx;

            let inv_view_proj = Mat4(uniforms.view_proj).inverse();
            let resolve_uniforms = ResolveUniforms {
                inv_view_proj: inv_view_proj.0,
                prev_view_proj: self.prev_view_proj.0,
                render_size: [off.render_w as f32, off.render_h as f32],
                output_size: [off.output_w as f32, off.output_h as f32],
                blend: HISTORY_BLEND,
                history_valid: if self.history_valid { 1.0 } else { 0.0 },
                _pad: [0.0, 0.0],
            };
            queue.write_buffer(&resolve.uni_buf, 0, bytemuck::bytes_of(&resolve_uniforms));

            let bind_group = Self::resolve_bind_group(device, resolve, off, read_idx);

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport_resolve_pass"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &off.history[write_idx],
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&resolve.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        // ── Pass 3: orientation gizmo -> swapchain (unjittered, on top) ─────
        {
            let depth_view = &self.depth.as_ref().unwrap().view;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport_gizmo_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
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

            // Orientation gizmo. Its vertex positions are pre-baked into NDC
            // space (identity projection) and never jittered, so it stays
            // crisp and stable on top of the resolved scene.
            if let Some(gz) = &mut self.gizmo {
                let gizmo_uniforms = ViewportUniforms {
                    view_proj: Mat4::IDENTITY.0,
                    viewport: [w as f32, h as f32],
                    time: uniforms.time,
                    _pad: 0.0,
                    jitter: [0.0, 0.0],
                    _pad2: [0.0, 0.0],
                };
                queue.write_buffer(&gz.uni_buf, 0, bytemuck::bytes_of(&gizmo_uniforms));

                if !gizmo_background.is_empty() {
                    let bytes = bytemuck::cast_slice(gizmo_background);
                    Self::ensure_buf(
                        device,
                        &mut gz.bg_inst_buf,
                        &mut gz.bg_inst_cap,
                        bytes,
                        wgpu::BufferUsages::VERTEX,
                    );
                    queue.write_buffer(&gz.bg_inst_buf, 0, bytes);
                    pass.set_pipeline(&gz.bubble_pipeline);
                    pass.set_bind_group(0, &gz.uni_bg, &[]);
                    pass.set_vertex_buffer(0, gz.bg_inst_buf.slice(..));
                    pass.draw(0..6, 0..gizmo_background.len() as u32);
                }

                if !gizmo_axes.is_empty() {
                    let bytes = bytemuck::cast_slice(gizmo_axes);
                    Self::ensure_buf(
                        device,
                        &mut gz.axis_vert_buf,
                        &mut gz.axis_vert_cap,
                        bytes,
                        wgpu::BufferUsages::VERTEX,
                    );
                    queue.write_buffer(&gz.axis_vert_buf, 0, bytes);
                    pass.set_pipeline(&gz.axis_pipeline);
                    pass.set_bind_group(0, &gz.uni_bg, &[]);
                    pass.set_vertex_buffer(0, gz.axis_vert_buf.slice(..));
                    pass.draw(0..gizmo_axes.len() as u32, 0..1);
                }

                if !gizmo_bubbles.is_empty() {
                    let bytes = bytemuck::cast_slice(gizmo_bubbles);
                    Self::ensure_buf(
                        device,
                        &mut gz.bubble_inst_buf,
                        &mut gz.bubble_inst_cap,
                        bytes,
                        wgpu::BufferUsages::VERTEX,
                    );
                    queue.write_buffer(&gz.bubble_inst_buf, 0, bytes);
                    pass.set_pipeline(&gz.bubble_pipeline);
                    pass.set_bind_group(0, &gz.uni_bg, &[]);
                    pass.set_vertex_buffer(0, gz.bubble_inst_buf.slice(..));
                    pass.draw(0..6, 0..gizmo_bubbles.len() as u32);
                }
            }
        }

        queue.submit(std::iter::once(encoder.finish()));

        self.prev_view_proj = Mat4(uniforms.view_proj);
        self.history_valid = true;
        self.history_idx = 1 - self.history_idx;
        self.frame_index += 1;
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

    /// Creates a single-mip, single-layer 2D render target view.
    fn create_target(
        device: &wgpu::Device,
        label: &str,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> wgpu::TextureView {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// (Re)creates the offscreen render-resolution scene targets and the
    /// output-resolution TAA history pair, if the output size (or the
    /// derived render resolution) has changed since the last frame.
    fn ensure_offscreen(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let render_w = ((w as f32 * RENDER_SCALE).round() as u32).max(1);
        let render_h = ((h as f32 * RENDER_SCALE).round() as u32).max(1);
        let output_w = w.max(1);
        let output_h = h.max(1);

        if let Some(off) = &self.offscreen {
            if off.render_w == render_w
                && off.render_h == render_h
                && off.output_w == output_w
                && off.output_h == output_h
            {
                return;
            }
        }

        let target_usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let scene_color = Self::create_target(
            device,
            "viewport_scene_color",
            render_w,
            render_h,
            OFFSCREEN_FORMAT,
            target_usage,
        );
        let scene_depth = Self::create_target(
            device,
            "viewport_scene_depth",
            render_w,
            render_h,
            DEPTH_FORMAT,
            target_usage,
        );
        let history = [
            Self::create_target(device, "viewport_history_0", output_w, output_h, OFFSCREEN_FORMAT, target_usage),
            Self::create_target(device, "viewport_history_1", output_w, output_h, OFFSCREEN_FORMAT, target_usage),
        ];

        self.offscreen = Some(OffscreenState {
            scene_color,
            scene_depth,
            history,
            render_w,
            render_h,
            output_w,
            output_h,
        });
        // New textures hold garbage; don't reproject from them this frame.
        self.history_idx = 0;
        self.history_valid = false;
    }

    /// Builds the per-frame bind group for the resolve pass: the
    /// just-rendered scene color/depth, plus last frame's history texture
    /// (`history[read_idx]`) to reproject from.
    fn resolve_bind_group(
        device: &wgpu::Device,
        resolve: &ResolveState,
        off: &OffscreenState,
        read_idx: usize,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport_resolve_bg"),
            layout: &resolve.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: resolve.uni_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&off.scene_color),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&off.scene_depth),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&off.history[read_idx]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&resolve.sampler),
                },
            ],
        })
    }

    /// Builds the fullscreen-triangle TAA/upscale resolve pipeline: it reads
    /// the render-resolution scene color/depth and the previous frame's
    /// history, and writes both the final (output-resolution) swapchain
    /// image and the new history texture in one pass (MRT).
    fn create_resolve(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> ResolveState {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport_resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/resolve.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewport_resolve_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport_resolve_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport_resolve"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_resolve"),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: fmt,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: OFFSCREEN_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("viewport_resolve_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let uni_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_resolve_uni"),
            size: std::mem::size_of::<ResolveUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        ResolveState {
            pipeline,
            bgl,
            sampler,
            uni_buf,
        }
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

    /// Depth state for overlay geometry (the orientation gizmo): tested
    /// against nothing, so it always draws on top regardless of what's
    /// already in the depth buffer for that screen region.
    fn overlay_depth_stencil_state() -> wgpu::DepthStencilState {
        wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }
    }

    fn create_mesh(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> MeshState {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport_mesh"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene.wgsl").into()),
        });
        let bgl = Self::uni_bind_group_layout(device);
        let (uni_buf, uni_bg) = Self::uni_buf_and_bg(device, &bgl);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport_mesh_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // pos
            1 => Float32x3, // normal
            2 => Float32x4, // color
        ];
        let vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport_mesh"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_mesh"),
                buffers: &[vbl],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mesh"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: fmt,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(Self::depth_stencil_state()),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let init_cap = 512 * std::mem::size_of::<MeshVertex>() as u64;
        let vert_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_mesh_verts"),
            size: init_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        MeshState {
            pipeline,
            uni_buf,
            uni_bg,
            vert_buf,
            vert_cap: init_cap,
        }
    }

    fn create_gizmo(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> GizmoState {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport_gizmo"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/gizmo.wgsl").into()),
        });
        let bgl = Self::uni_bind_group_layout(device);
        let (uni_buf, uni_bg) = Self::uni_buf_and_bg(device, &bgl);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport_gizmo_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let axis_attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // pos
            1 => Float32x4, // color
        ];
        let axis_vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &axis_attrs,
        };

        let axis_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport_gizmo_axes"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_axis"),
                buffers: &[axis_vbl],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_axis"),
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
            depth_stencil: Some(Self::overlay_depth_stencil_state()),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let bubble_attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // center
            1 => Float32,   // size
            2 => Float32x4, // color
            3 => Float32,   // letter
        ];
        let bubble_vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GizmoBubbleInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &bubble_attrs,
        };

        let bubble_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport_gizmo_bubbles"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bubble"),
                buffers: &[bubble_vbl],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bubble"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: fmt,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(Self::overlay_depth_stencil_state()),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let axis_init_cap = 16 * std::mem::size_of::<LineVertex>() as u64;
        let axis_vert_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_gizmo_axis_verts"),
            size: axis_init_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bubble_init_cap = 8 * std::mem::size_of::<GizmoBubbleInstance>() as u64;
        let bubble_inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_gizmo_bubble_inst"),
            size: bubble_init_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bg_init_cap = std::mem::size_of::<GizmoBubbleInstance>() as u64;
        let bg_inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport_gizmo_bg_inst"),
            size: bg_init_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        GizmoState {
            axis_pipeline,
            bubble_pipeline,
            uni_buf,
            uni_bg,
            axis_vert_buf,
            axis_vert_cap: axis_init_cap,
            bubble_inst_buf,
            bubble_inst_cap: bubble_init_cap,
            bg_inst_buf,
            bg_inst_cap: bg_init_cap,
        }
    }
}
