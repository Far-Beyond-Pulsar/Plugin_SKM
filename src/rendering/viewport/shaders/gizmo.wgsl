// Orientation gizmo: 6 axis spokes radiating from the origin, each capped
// with a colored "bubble" sized in screen-space pixels. Drawn into a small
// square viewport in the corner of the main 3D view, using its own
// rotation-only view-projection matrix.

struct Uniforms {
    view_proj: mat4x4<f32>,
    viewport: vec2<f32>,
    time: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// ── Axis spokes (line list) ─────────────────────────────────────────────────

struct AxisVertexIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct AxisVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_axis(input: AxisVertexIn) -> AxisVertexOut {
    var out: AxisVertexOut;
    out.clip_pos = u.view_proj * vec4<f32>(input.pos, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_axis(input: AxisVertexOut) -> @location(0) vec4<f32> {
    return input.color;
}

// ── Axis-end bubbles (instanced billboard circles) ──────────────────────────

struct BubbleInstanceIn {
    @location(0) center: vec3<f32>,
    @location(1) size: f32,
    @location(2) color: vec4<f32>,
};

struct BubbleVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
};

@vertex
fn vs_bubble(
    @builtin(vertex_index) vertex_index: u32,
    input: BubbleInstanceIn,
) -> BubbleVertexOut {
    var out: BubbleVertexOut;
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

    let ndc_size = (input.size / u.viewport) * 2.0 * clip.w;
    clip.x += offset.x * ndc_size.x;
    clip.y += offset.y * ndc_size.y;

    out.clip_pos = clip;
    out.color = input.color;
    out.local = offset;
    return out;
}

@fragment
fn fs_bubble(input: BubbleVertexOut) -> @location(0) vec4<f32> {
    let d = length(input.local);
    if d > 1.0 {
        discard;
    }
    // Simple rim-lit "bubble" look: brighter core, slightly darker edge.
    let shade = smoothstep(1.0, 0.55, d);
    let rgb = input.color.rgb * (0.7 + 0.3 * shade);
    return vec4<f32>(rgb, input.color.a);
}
