struct Uniforms {
    view_proj: mat4x4<f32>,
    viewport: vec2<f32>,
    time: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// ── Lines (grid + bone segments) ────────────────────────────────────────────

struct LineVertexIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LineVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(input: LineVertexIn) -> LineVertexOut {
    var out: LineVertexOut;
    out.clip_pos = u.view_proj * vec4<f32>(input.pos, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_line(input: LineVertexOut) -> @location(0) vec4<f32> {
    return input.color;
}

// ── Joints (instanced screen-facing billboards) ─────────────────────────────

struct JointInstanceIn {
    @location(0) center: vec3<f32>,
    @location(1) size: f32,
    @location(2) color: vec4<f32>,
};

struct JointVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_joint(
    @builtin(vertex_index) vertex_index: u32,
    input: JointInstanceIn,
) -> JointVertexOut {
    var out: JointVertexOut;
    var clip = u.view_proj * vec4<f32>(input.center, 1.0);

    var offsets = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    let offset = offsets[vertex_index];

    // Keep the billboard a constant size in pixels regardless of depth.
    let ndc_size = (input.size / u.viewport) * 2.0 * clip.w;
    clip.x += offset.x * ndc_size.x;
    clip.y += offset.y * ndc_size.y;

    out.clip_pos = clip;
    out.color = input.color;
    return out;
}

@fragment
fn fs_joint(input: JointVertexOut) -> @location(0) vec4<f32> {
    return input.color;
}
