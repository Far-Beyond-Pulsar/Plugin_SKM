// Anti-aliased screen-space lines of constant pixel width, with a color
// gradient along each segment. Used for bone outlines, tracer trails, the
// motion path and the gizmo spokes.

struct RibbonIn {
    @location(0) a: vec3<f32>,
    // Width in pixels.
    @location(1) width: f32,
    @location(2) b: vec3<f32>,
    // Pulls the line toward the camera (NDC depth) so outlines don't z-fight
    // with the surface they trace.
    @location(3) bias: f32,
    @location(4) color_a: vec4<f32>,
    @location(5) color_b: vec4<f32>,
};

struct RibbonOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Signed pixel distance from the line's center, and its half width.
    @location(1) offset_px: f32,
    @location(2) half_width: f32,
};

@vertex
fn vs_ribbon(@builtin(vertex_index) vertex_index: u32, input: RibbonIn) -> RibbonOut {
    var out: RibbonOut;
    let orig_a = u.view_proj * vec4<f32>(input.a, 1.0);
    let orig_b = u.view_proj * vec4<f32>(input.b, 1.0);
    var ca = orig_a;
    var cb = orig_b;

    // Clip against the near plane (clip-space z >= 0) so segments crossing the
    // camera don't flip.
    if orig_a.z < 0.0 && orig_b.z < 0.0 {
        out.pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.color = vec4<f32>(0.0);
        out.offset_px = 0.0;
        out.half_width = 1.0;
        return out;
    }
    if orig_a.z < 0.0 {
        ca = mix(orig_a, orig_b, orig_a.z / (orig_a.z - orig_b.z));
    }
    if orig_b.z < 0.0 {
        cb = mix(orig_b, orig_a, orig_b.z / (orig_b.z - orig_a.z));
    }

    let half_vp = 0.5 * u.viewport;
    let sa = ca.xy / ca.w * half_vp;
    let sb = cb.xy / cb.w * half_vp;
    var dir = sb - sa;
    let len = length(dir);
    dir = select(vec2<f32>(1.0, 0.0), dir / max(len, 1e-5), len > 1e-4);
    let normal = vec2<f32>(-dir.y, dir.x);

    var ends = array<f32, 6>(0.0, 0.0, 1.0, 0.0, 1.0, 1.0);
    var sides = array<f32, 6>(-1.0, 1.0, 1.0, -1.0, 1.0, -1.0);
    let end = ends[vertex_index];
    let side = sides[vertex_index];

    let half_w = input.width * 0.5;
    let pad = half_w + 1.0;
    let clip = mix(ca, cb, end);
    var p = clip.xy / clip.w * half_vp;
    p += dir * (end * 2.0 - 1.0) * pad;
    p += normal * side * pad;

    out.pos = vec4<f32>(p / half_vp * clip.w, clip.z - input.bias * clip.w, clip.w);
    out.color = mix(input.color_a, input.color_b, end);
    out.offset_px = side * pad;
    out.half_width = half_w;
    return out;
}

@fragment
fn fs_ribbon(input: RibbonOut) -> @location(0) vec4<f32> {
    let cover = clamp(input.half_width + 0.5 - abs(input.offset_px), 0.0, 1.0);
    return vec4<f32>(input.color.rgb, input.color.a * cover);
}
