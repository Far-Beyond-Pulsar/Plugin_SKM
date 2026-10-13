//! `ViewportRenderer` — owns the WGPU pipelines that draw the 3D bone scene.
//!
//! A frame is built from these passes:
//!
//! 1. **Scene** (jittered, offscreen HDR color + depth): gradient backdrop,
//!    lit bone meshes, an x-ray ghost of occluded bone parts, onion-skin
//!    poses, anti-aliased ribbon lines, the analytic ground grid, and joint
//!    markers.
//! 2. **Resolve**: temporal anti-aliasing against last frame's output, written
//!    to the swapchain and the history texture.
//! 3. **Gizmo**: the orientation widget, drawn unjittered on top.

use bytemuck::Pod;

use crate::core::Mat4;

use super::shaders;
use super::types::{
    GizmoBubbleInstance, JointInstance, MeshVertex, ResolveUniforms, RibbonSegment, ViewportUniforms,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Format of the offscreen scene color and TAA history textures. Float so
/// the resolve pass can blend/clamp without banding.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Internal scene render resolution as a fraction of the output size.
const RENDER_SCALE: f32 = 1.0;

/// Weight of the reprojected history sample in the TAA resolve.
const HISTORY_BLEND: f32 = 0.85;

/// Halton(2,3) sequence, mapped to `[-0.5, 0.5)` sub-pixel offsets, used to
/// jitter the camera projection for TAA sampling.
const JITTER_SAMPLES: [[f32; 2]; 8] = [
    [0.0, 0.333333 - 0.5],
    [0.25 - 0.5, 0.666667 - 0.5],
    [0.75 - 0.5, 0.111111 - 0.5],
    [0.125 - 0.5, 0.444444 - 0.5],
    [0.625 - 0.5, 0.777778 - 0.5],
    [0.375 - 0.5, 0.222222 - 0.5],
    [0.875 - 0.5, 0.555556 - 0.5],
    [0.0625 - 0.5, 0.888889 - 0.5],
];

/// Everything one frame draws.
pub struct SceneFrame<'a> {
    pub uniforms: &'a ViewportUniforms,
    pub mesh: &'a [MeshVertex],
    /// Translucent onion-skin geometry.
    pub ghost: &'a [MeshVertex],
    pub ribbons: &'a [RibbonSegment],
    pub joints: &'a [JointInstance],
    pub gizmo_ribbons: &'a [RibbonSegment],
    /// Backdrop disc first, then the bubbles back to front.
    pub gizmo_bubbles: &'a [GizmoBubbleInstance],
    pub show_grid: bool,
    pub xray: bool,
}

/// A GPU buffer that grows to fit what's uploaded to it.
struct GrowBuf {
    buf: wgpu::Buffer,
    cap: u64,
    label: &'static str,
    usage: wgpu::BufferUsages,
}

impl GrowBuf {
    fn new(device: &wgpu::Device, label: &'static str, usage: wgpu::BufferUsages, init: u64) -> Self {
        let usage = usage | wgpu::BufferUsages::COPY_DST;
        Self {
            buf: Self::alloc(device, label, usage, init),
            cap: init,
            label,
            usage,
        }
    }

    fn alloc(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        size: u64,
    ) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        })
    }

    /// Upload `data`, reallocating if it doesn't fit. Returns the item count.
    fn upload<T: Pod>(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[T]) -> u32 {
        let bytes = bytemuck::cast_slice::<T, u8>(data);
        let needed = bytes.len() as u64;
        if needed > self.cap {
            self.cap = (needed * 2).max(256);
            self.buf = Self::alloc(device, self.label, self.usage, self.cap);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.buf, 0, bytes);
        }
        data.len() as u32
    }
}

/// Uniform buffer plus the bind group that exposes it to the shaders.
struct UniformSet {
    buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

struct Pipelines {
    sky: wgpu::RenderPipeline,
    grid: wgpu::RenderPipeline,
    mesh: wgpu::RenderPipeline,
    xray: wgpu::RenderPipeline,
    ghost: wgpu::RenderPipeline,
    ribbon: wgpu::RenderPipeline,
    joints: wgpu::RenderPipeline,
    gizmo_ribbon: wgpu::RenderPipeline,
    gizmo_bubble: wgpu::RenderPipeline,
    scene_uniforms: UniformSet,
    gizmo_uniforms: UniformSet,
    mesh_buf: GrowBuf,
    ghost_buf: GrowBuf,
    ribbon_buf: GrowBuf,
    joint_buf: GrowBuf,
    gizmo_ribbon_buf: GrowBuf,
    gizmo_bubble_buf: GrowBuf,
    resolve: ResolveState,
    fmt: wgpu::TextureFormat,
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
    pipelines: Option<Pipelines>,
    offscreen: Option<OffscreenState>,
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
            pipelines: None,
            offscreen: None,
            history_idx: 0,
            history_valid: false,
            prev_view_proj: Mat4::IDENTITY,
            frame_index: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        w: u32,
        h: u32,
        fmt: wgpu::TextureFormat,
        frame: &SceneFrame,
    ) {
        if self.pipelines.as_ref().is_none_or(|p| p.fmt != fmt) {
            self.pipelines = Some(Pipelines::new(device, fmt));
        }
        self.ensure_offscreen(device, w, h);

        let (render_w, render_h) = {
            let off = self.offscreen.as_ref().unwrap();
            (off.render_w, off.render_h)
        };

        // Sub-pixel jitter for this frame, in NDC units.
        let sample = JITTER_SAMPLES[(self.frame_index as usize) % JITTER_SAMPLES.len()];
        let jitter = [
            sample[0] * 2.0 / render_w as f32,
            sample[1] * 2.0 / render_h as f32,
        ];

        let mut scene_uniforms = *frame.uniforms;
        scene_uniforms.jitter = jitter;
        scene_uniforms.viewport = [render_w as f32, render_h as f32];

        // The gizmo draws in pre-projected NDC with no camera.
        let mut gizmo_uniforms = *frame.uniforms;
        gizmo_uniforms.view_proj = Mat4::IDENTITY.0;
        gizmo_uniforms.inv_view_proj = Mat4::IDENTITY.0;
        gizmo_uniforms.jitter = [0.0, 0.0];
        gizmo_uniforms.viewport = [w as f32, h as f32];

        let p = self.pipelines.as_mut().unwrap();
        queue.write_buffer(&p.scene_uniforms.buf, 0, bytemuck::bytes_of(&scene_uniforms));
        queue.write_buffer(&p.gizmo_uniforms.buf, 0, bytemuck::bytes_of(&gizmo_uniforms));
        let mesh_n = p.mesh_buf.upload(device, queue, frame.mesh);
        let ghost_n = p.ghost_buf.upload(device, queue, frame.ghost);
        let ribbon_n = p.ribbon_buf.upload(device, queue, frame.ribbons);
        let joint_n = p.joint_buf.upload(device, queue, frame.joints);
        let gizmo_ribbon_n = p.gizmo_ribbon_buf.upload(device, queue, frame.gizmo_ribbons);
        let gizmo_bubble_n = p.gizmo_bubble_buf.upload(device, queue, frame.gizmo_bubbles);

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
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
            pass.set_bind_group(0, &p.scene_uniforms.bind_group, &[]);

            pass.set_pipeline(&p.sky);
            pass.draw(0..3, 0..1);

            if mesh_n > 0 {
                pass.set_pipeline(&p.mesh);
                pass.set_vertex_buffer(0, p.mesh_buf.buf.slice(..));
                pass.draw(0..mesh_n, 0..1);
                if frame.xray {
                    pass.set_pipeline(&p.xray);
                    pass.draw(0..mesh_n, 0..1);
                }
            }
            if ghost_n > 0 {
                pass.set_pipeline(&p.ghost);
                pass.set_vertex_buffer(0, p.ghost_buf.buf.slice(..));
                pass.draw(0..ghost_n, 0..1);
            }
            if ribbon_n > 0 {
                pass.set_pipeline(&p.ribbon);
                pass.set_vertex_buffer(0, p.ribbon_buf.buf.slice(..));
                pass.draw(0..6, 0..ribbon_n);
            }
            if frame.show_grid {
                pass.set_pipeline(&p.grid);
                pass.draw(0..3, 0..1);
            }
            if joint_n > 0 {
                pass.set_pipeline(&p.joints);
                pass.set_vertex_buffer(0, p.joint_buf.buf.slice(..));
                pass.draw(0..6, 0..joint_n);
            }
        }

        // ── Pass 2: TAA/upscale resolve -> swapchain + history ──────────────
        {
            let off = self.offscreen.as_ref().unwrap();
            let read_idx = 1 - self.history_idx;
            let write_idx = self.history_idx;

            let resolve_uniforms = ResolveUniforms {
                inv_view_proj: Mat4(frame.uniforms.view_proj).inverse().0,
                prev_view_proj: self.prev_view_proj.0,
                render_size: [off.render_w as f32, off.render_h as f32],
                output_size: [off.output_w as f32, off.output_h as f32],
                blend: HISTORY_BLEND,
                history_valid: if self.history_valid { 1.0 } else { 0.0 },
                _pad: [0.0, 0.0],
            };
            queue.write_buffer(&p.resolve.uni_buf, 0, bytemuck::bytes_of(&resolve_uniforms));
            let bind_group = Self::resolve_bind_group(device, &p.resolve, off, read_idx);

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
            pass.set_pipeline(&p.resolve.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        // ── Pass 3: orientation gizmo -> swapchain (unjittered, on top) ─────
        if gizmo_ribbon_n > 0 || gizmo_bubble_n > 0 {
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
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &p.gizmo_uniforms.bind_group, &[]);

            // Backdrop first (the first bubble), then spokes, then the rest,
            // so spokes tuck under the end bubbles.
            if gizmo_bubble_n > 0 {
                pass.set_pipeline(&p.gizmo_bubble);
                pass.set_vertex_buffer(0, p.gizmo_bubble_buf.buf.slice(..));
                pass.draw(0..6, 0..1);
            }
            if gizmo_ribbon_n > 0 {
                pass.set_pipeline(&p.gizmo_ribbon);
                pass.set_vertex_buffer(0, p.gizmo_ribbon_buf.buf.slice(..));
                pass.draw(0..6, 0..gizmo_ribbon_n);
            }
            if gizmo_bubble_n > 1 {
                pass.set_pipeline(&p.gizmo_bubble);
                pass.set_vertex_buffer(0, p.gizmo_bubble_buf.buf.slice(..));
                pass.draw(0..6, 1..gizmo_bubble_n);
            }
        }

        queue.submit(std::iter::once(encoder.finish()));

        self.prev_view_proj = Mat4(frame.uniforms.view_proj);
        self.history_valid = true;
        self.history_idx = 1 - self.history_idx;
        self.frame_index += 1;
    }

    // ── targets ────────────────────────────────────────────────────────────

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
    /// output-resolution TAA history pair when the size changes.
    fn ensure_offscreen(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let render_w = ((w as f32 * RENDER_SCALE).round() as u32).max(1);
        let render_h = ((h as f32 * RENDER_SCALE).round() as u32).max(1);
        let (output_w, output_h) = (w.max(1), h.max(1));

        if let Some(off) = &self.offscreen {
            if off.render_w == render_w
                && off.render_h == render_h
                && off.output_w == output_w
                && off.output_h == output_h
            {
                return;
            }
        }

        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        self.offscreen = Some(OffscreenState {
            scene_color: Self::create_target(
                device,
                "viewport_scene_color",
                render_w,
                render_h,
                OFFSCREEN_FORMAT,
                usage,
            ),
            scene_depth: Self::create_target(
                device,
                "viewport_scene_depth",
                render_w,
                render_h,
                DEPTH_FORMAT,
                usage,
            ),
            history: [
                Self::create_target(
                    device,
                    "viewport_history_0",
                    output_w,
                    output_h,
                    OFFSCREEN_FORMAT,
                    usage,
                ),
                Self::create_target(
                    device,
                    "viewport_history_1",
                    output_w,
                    output_h,
                    OFFSCREEN_FORMAT,
                    usage,
                ),
            ],
            render_w,
            render_h,
            output_w,
            output_h,
        });
        // New textures hold garbage; don't reproject from them this frame.
        self.history_idx = 0;
        self.history_valid = false;
    }

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
}

// ── pipeline construction ──────────────────────────────────────────────────

fn module(device: &wgpu::Device, label: &str, source: String) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn depth_state(write: bool, compare: wgpu::CompareFunction) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(write),
        depth_compare: Some(compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn target(format: wgpu::TextureFormat, blend: Option<wgpu::BlendState>) -> Option<wgpu::ColorTargetState> {
    Some(wgpu::ColorTargetState {
        format,
        blend,
        write_mask: wgpu::ColorWrites::ALL,
    })
}

struct PipelineSpec<'a> {
    label: &'a str,
    layout: &'a wgpu::PipelineLayout,
    shader: &'a wgpu::ShaderModule,
    vs: &'a str,
    fs: &'a str,
    buffers: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    depth: Option<wgpu::DepthStencilState>,
}

fn pipeline(device: &wgpu::Device, spec: PipelineSpec) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(spec.label),
        layout: Some(spec.layout),
        vertex: wgpu::VertexState {
            module: spec.shader,
            entry_point: Some(spec.vs),
            buffers: spec.buffers,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: spec.shader,
            entry_point: Some(spec.fs),
            targets: &[target(spec.format, spec.blend)],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: spec.depth,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn vertex_layout<'a>(
    stride: usize,
    step_mode: wgpu::VertexStepMode,
    attributes: &'a [wgpu::VertexAttribute],
) -> wgpu::VertexBufferLayout<'a> {
    wgpu::VertexBufferLayout {
        array_stride: stride as wgpu::BufferAddress,
        step_mode,
        attributes,
    }
}

impl Pipelines {
    fn new(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> Self {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let uniform_set = |label: &str| {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<ViewportUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            UniformSet { buf, bind_group }
        };

        let backdrop = module(device, "viewport_backdrop", shaders::backdrop());
        let scene = module(device, "viewport_scene", shaders::scene());
        let ribbon = module(device, "viewport_ribbon", shaders::ribbon());
        let gizmo = module(device, "viewport_gizmo", shaders::gizmo());

        use wgpu::CompareFunction as Cmp;
        let alpha = Some(wgpu::BlendState::ALPHA_BLENDING);
        let premul = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);

        let mesh_attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // pos
            1 => Float32x3, // normal
            2 => Float32x4, // color
            3 => Float32,   // glow
        ];
        let mesh_vbl = || {
            vertex_layout(
                std::mem::size_of::<MeshVertex>(),
                wgpu::VertexStepMode::Vertex,
                &mesh_attrs,
            )
        };
        let mesh_pipeline = |label: &str, fs: &str, blend, depth| {
            pipeline(
                device,
                PipelineSpec {
                    label,
                    layout: &layout,
                    shader: &scene,
                    vs: "vs_mesh",
                    fs,
                    buffers: &[Some(mesh_vbl())],
                    format: OFFSCREEN_FORMAT,
                    blend,
                    depth: Some(depth),
                },
            )
        };

        let ribbon_attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // a
            1 => Float32,   // width
            2 => Float32x3, // b
            3 => Float32,   // bias
            4 => Float32x4, // color a
            5 => Float32x4, // color b
        ];
        let ribbon_vbl = || {
            vertex_layout(
                std::mem::size_of::<RibbonSegment>(),
                wgpu::VertexStepMode::Instance,
                &ribbon_attrs,
            )
        };

        let joint_attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // center
            1 => Float32,   // size
            2 => Float32x4, // color
            3 => Float32,   // style
        ];
        let bubble_attrs = wgpu::vertex_attr_array![
            0 => Float32x3, // center
            1 => Float32,   // size
            2 => Float32x4, // color
            3 => Float32,   // letter
            4 => Float32,   // kind
        ];

        let sky = pipeline(
            device,
            PipelineSpec {
                label: "viewport_sky",
                layout: &layout,
                shader: &backdrop,
                vs: "vs_fullscreen",
                fs: "fs_sky",
                buffers: &[],
                format: OFFSCREEN_FORMAT,
                blend: None,
                depth: Some(depth_state(false, Cmp::Always)),
            },
        );
        let grid = pipeline(
            device,
            PipelineSpec {
                label: "viewport_grid",
                layout: &layout,
                shader: &backdrop,
                vs: "vs_fullscreen",
                fs: "fs_grid",
                buffers: &[],
                format: OFFSCREEN_FORMAT,
                blend: premul,
                depth: Some(depth_state(false, Cmp::Less)),
            },
        );
        let mesh = mesh_pipeline("viewport_mesh", "fs_mesh", alpha, depth_state(true, Cmp::Less));
        let xray = mesh_pipeline(
            "viewport_xray",
            "fs_xray",
            alpha,
            depth_state(false, Cmp::Greater),
        );
        let ghost = mesh_pipeline("viewport_ghost", "fs_ghost", alpha, depth_state(false, Cmp::Less));
        let ribbon_pipeline = pipeline(
            device,
            PipelineSpec {
                label: "viewport_ribbon",
                layout: &layout,
                shader: &ribbon,
                vs: "vs_ribbon",
                fs: "fs_ribbon",
                buffers: &[Some(ribbon_vbl())],
                format: OFFSCREEN_FORMAT,
                blend: alpha,
                depth: Some(depth_state(false, Cmp::Less)),
            },
        );
        let joints = pipeline(
            device,
            PipelineSpec {
                label: "viewport_joints",
                layout: &layout,
                shader: &scene,
                vs: "vs_joint",
                fs: "fs_joint",
                buffers: &[Some(vertex_layout(
                    std::mem::size_of::<JointInstance>(),
                    wgpu::VertexStepMode::Instance,
                    &joint_attrs,
                ))],
                format: OFFSCREEN_FORMAT,
                blend: premul,
                depth: Some(depth_state(false, Cmp::Always)),
            },
        );
        let gizmo_ribbon = pipeline(
            device,
            PipelineSpec {
                label: "viewport_gizmo_ribbon",
                layout: &layout,
                shader: &ribbon,
                vs: "vs_ribbon",
                fs: "fs_ribbon",
                buffers: &[Some(ribbon_vbl())],
                format: fmt,
                blend: alpha,
                depth: None,
            },
        );
        let gizmo_bubble = pipeline(
            device,
            PipelineSpec {
                label: "viewport_gizmo_bubble",
                layout: &layout,
                shader: &gizmo,
                vs: "vs_bubble",
                fs: "fs_bubble",
                buffers: &[Some(vertex_layout(
                    std::mem::size_of::<GizmoBubbleInstance>(),
                    wgpu::VertexStepMode::Instance,
                    &bubble_attrs,
                ))],
                format: fmt,
                blend: alpha,
                depth: None,
            },
        );

        use wgpu::BufferUsages as Usage;
        Self {
            sky,
            grid,
            mesh,
            xray,
            ghost,
            ribbon: ribbon_pipeline,
            joints,
            gizmo_ribbon,
            gizmo_bubble,
            scene_uniforms: uniform_set("viewport_scene_uniforms"),
            gizmo_uniforms: uniform_set("viewport_gizmo_uniforms"),
            mesh_buf: GrowBuf::new(device, "viewport_mesh", Usage::VERTEX, 4096),
            ghost_buf: GrowBuf::new(device, "viewport_ghost", Usage::VERTEX, 4096),
            ribbon_buf: GrowBuf::new(device, "viewport_ribbons", Usage::VERTEX, 4096),
            joint_buf: GrowBuf::new(device, "viewport_joints", Usage::VERTEX, 1024),
            gizmo_ribbon_buf: GrowBuf::new(device, "viewport_gizmo_ribbons", Usage::VERTEX, 1024),
            gizmo_bubble_buf: GrowBuf::new(device, "viewport_gizmo_bubbles", Usage::VERTEX, 1024),
            resolve: Self::create_resolve(device, fmt),
            fmt,
        }
    }

    /// Fullscreen-triangle TAA/upscale resolve pipeline: reads the
    /// render-resolution scene color/depth and the previous frame's history,
    /// and writes both the final swapchain image and the new history texture
    /// in one pass (MRT).
    fn create_resolve(device: &wgpu::Device, fmt: wgpu::TextureFormat) -> ResolveState {
        let shader = module(device, "viewport_resolve", shaders::resolve());

        let texture_entry = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let float = wgpu::TextureSampleType::Float { filterable: true };
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
                texture_entry(1, float),
                texture_entry(2, wgpu::TextureSampleType::Depth),
                texture_entry(3, float),
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
                targets: &[target(fmt, None), target(OFFSCREEN_FORMAT, None)],
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
}
