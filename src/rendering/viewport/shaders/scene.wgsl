struct Uniforms {
    view_proj: mat4x4<f32>,
    viewport: vec2<f32>,
    time: f32,
    _pad: f32,
    // Sub-pixel clip-space offset for TAA sample jittering (zero when unused).
    jitter: vec2<f32>,
    _pad2: vec2<f32>,
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
    var clip_pos = u.view_proj * vec4<f32>(input.pos, 1.0);
    clip_pos.x += u.jitter.x * clip_pos.w;
    clip_pos.y += u.jitter.y * clip_pos.w;
    out.clip_pos = clip_pos;
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
    clip.x += u.jitter.x * clip.w;
    clip.y += u.jitter.y * clip.w;

    out.clip_pos = clip;
    out.color = input.color;
    return out;
}

@fragment
fn fs_joint(input: JointVertexOut) -> @location(0) vec4<f32> {
    return input.color;
}

// ── Bone meshes (shaded octahedra) ──────────────────────────────────────────

struct MeshVertexIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

struct MeshVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_mesh(input: MeshVertexIn) -> MeshVertexOut {
    var out: MeshVertexOut;
    var clip_pos = u.view_proj * vec4<f32>(input.pos, 1.0);
    clip_pos.x += u.jitter.x * clip_pos.w;
    clip_pos.y += u.jitter.y * clip_pos.w;
    out.clip_pos = clip_pos;
    out.normal = input.normal;
    out.color = input.color;
    return out;
}

@fragment
fn fs_mesh(input: MeshVertexOut) -> @location(0) vec4<f32> {
    let light_dir = normalize(vec3<f32>(0.4, 0.8, 0.35));
    let n = normalize(input.normal);
    // Faces pointing away from the light are still lit (abs) so both sides of
    // each octahedron face read as solid rather than going pitch black.
    let diffuse = abs(dot(n, light_dir));
    let shade = 0.45 + 0.55 * diffuse;
    return vec4<f32>(input.color.rgb * shade, input.color.a);
}
