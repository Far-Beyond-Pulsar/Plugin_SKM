// Bone meshes, joint markers, and the ghost / x-ray variants of the bone
// shading.

// ── Bone meshes ─────────────────────────────────────────────────────────────

struct MeshVertexIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    // Extra emissive brightness (selection / hover).
    @location(3) glow: f32,
};

struct MeshVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world: vec3<f32>,
    @location(3) glow: f32,
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
    out.world = input.pos;
    out.glow = input.glow;
    return out;
}

fn shade(input: MeshVertexOut) -> vec3<f32> {
    let view = normalize(u.eye.xyz - input.world);
    var n = normalize(input.normal);
    // Bones are thin and double sided; always light the face toward the camera.
    if dot(n, view) < 0.0 {
        n = -n;
    }

    let key = normalize(vec3<f32>(0.45, 0.85, 0.30));
    let fill = normalize(vec3<f32>(-0.65, 0.25, -0.55));
    let ndotv = max(dot(n, view), 0.0);

    // Hemisphere ambient taken from the backdrop so the rig sits in the scene.
    let ambient = mix(u.sky_bottom.rgb, u.sky_top.rgb, n.y * 0.5 + 0.5) * 0.9 + vec3<f32>(0.10);
    let diffuse = max(dot(n, key), 0.0) * 0.62 + max(dot(n, fill), 0.0) * 0.20;
    let headlight = ndotv * 0.22;
    let half_v = normalize(key + view);
    let spec = pow(max(dot(n, half_v), 0.0), 56.0) * 0.30;
    let rim = pow(1.0 - ndotv, 3.0);

    let base = input.color.rgb;
    var rgb = base * (ambient * 0.85 + vec3<f32>(diffuse + headlight));
    rgb += vec3<f32>(spec);
    rgb += mix(base, vec3<f32>(1.0), 0.35) * rim * 0.38;
    rgb += base * input.glow;
    return rgb;
}

@fragment
fn fs_mesh(input: MeshVertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(shade(input), input.color.a);
}

// The part of a bone hidden behind other geometry, drawn as a faint ghost so
// the whole skeleton stays readable.
@fragment
fn fs_xray(input: MeshVertexOut) -> @location(0) vec4<f32> {
    let view = normalize(u.eye.xyz - input.world);
    let n = normalize(input.normal);
    let edge = pow(1.0 - abs(dot(n, view)), 1.5);
    return vec4<f32>(input.color.rgb * (0.7 + 0.5 * edge), (0.10 + 0.20 * edge) * input.color.a);
}

// Translucent onion-skin poses.
@fragment
fn fs_ghost(input: MeshVertexOut) -> @location(0) vec4<f32> {
    let view = normalize(u.eye.xyz - input.world);
    let n = normalize(input.normal);
    let edge = pow(1.0 - abs(dot(n, view)), 1.2);
    return vec4<f32>(input.color.rgb * (0.85 + 0.4 * edge), input.color.a * (0.55 + 0.45 * edge));
}

// ── Joint markers (screen-facing billboards, constant pixel size) ───────────

struct JointInstanceIn {
    @location(0) center: vec3<f32>,
    // Diameter in pixels.
    @location(1) size: f32,
    @location(2) color: vec4<f32>,
    // 0 = pivot dot, 1 = selected (filled ring), 2 = hover (ring),
    // 3 = keyframe on the motion path (small diamond).
    @location(3) style: f32,
};

struct JointVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
    @location(2) style: f32,
    @location(3) size: f32,
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

    // The quad is a pixel larger than the marker so anti-aliased edges fit.
    let half_px = input.size * 0.5 + 1.0;
    let ndc_size = (vec2<f32>(half_px) / u.viewport) * 2.0 * clip.w;
    clip.x += offset.x * ndc_size.x;
    clip.y += offset.y * ndc_size.y;

    out.clip_pos = clip;
    out.color = input.color;
    out.local = offset * half_px;
    out.style = input.style;
    out.size = input.size;
    return out;
}

fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_joint(input: JointVertexOut) -> @location(0) vec4<f32> {
    let r = input.size * 0.5;
    let style = i32(round(input.style));
    let dist = length(input.local);
    let color = input.color;

    if style == 0 {
        // Pivot dot with a dark rim for contrast on any bone color.
        let fill = coverage(dist - (r - 1.2));
        let outer = coverage(dist - r);
        let rim = vec3<f32>(0.02, 0.02, 0.03);
        let rgb = mix(rim, color.rgb, fill / max(outer, 1e-3));
        return vec4<f32>(rgb * outer * color.a, outer * color.a);
    } else if style == 1 {
        // Selected: soft disc plus a bright ring and a hot core.
        let ring = coverage(abs(dist - (r - 1.5)) - 1.5);
        let disc = coverage(dist - r) * 0.22;
        let core = coverage(dist - r * 0.34);
        let rgb = mix(color.rgb, vec3<f32>(1.0), core * 0.6);
        let a = max(max(ring, disc), core) * color.a;
        return vec4<f32>(rgb * a, a);
    } else if style == 2 {
        // Hover: thin ring.
        let ring = coverage(abs(dist - (r - 1.0)) - 1.0);
        let a = ring * color.a;
        return vec4<f32>(color.rgb * a, a);
    }

    // Keyframe marker: diamond with a dark outline.
    let d = (abs(input.local.x) + abs(input.local.y)) * 0.70710678 - r * 0.70710678;
    let fill = coverage(d + 1.0);
    let outer = coverage(d);
    let rgb = mix(vec3<f32>(0.02), color.rgb, fill / max(outer, 1e-3));
    return vec4<f32>(rgb * outer * color.a, outer * color.a);
}
