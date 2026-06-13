// TAA / upscale resolve pass.
//
// Reads the jittered, render-resolution scene color + depth produced by the
// main scene pass, reprojects last frame's resolved output (`history`) using
// the depth buffer, and blends the two. The result is written both to the
// swapchain (final image) and back into the history texture for next frame.
//
// Runs as a single fullscreen triangle, so `render_size` may differ from
// `output_size` (this is also where upscaling happens, via the bilinear
// sample of `scene_color`).

struct Uniforms {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    render_size: vec2<f32>,
    output_size: vec2<f32>,
    blend: f32,
    history_valid: f32,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var scene_color: texture_2d<f32>;
@group(0) @binding(2) var scene_depth: texture_depth_2d;
@group(0) @binding(3) var history: texture_2d<f32>;
@group(0) @binding(4) var tex_sampler: sampler;

@vertex
fn vs_fullscreen(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    // Oversized triangle that covers the whole clip-space square.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

struct ResolveOut {
    @location(0) color: vec4<f32>,
    @location(1) history: vec4<f32>,
};

@fragment
fn fs_resolve(@builtin(position) frag_coord: vec4<f32>) -> ResolveOut {
    let uv = frag_coord.xy / u.output_size;
    let render_px = vec2<i32>(floor(uv * u.render_size));
    let depth = textureLoad(scene_depth, render_px, 0);

    let current = textureSampleLevel(scene_color, tex_sampler, uv, 0.0);

    var out: ResolveOut;

    // Empty background: nothing to reproject, just pass the current sample
    // through so the history buffer doesn't accumulate stale clear color.
    if depth >= 1.0 || u.history_valid < 0.5 {
        out.color = current;
        out.history = current;
        return out;
    }

    // Reconstruct this pixel's world-space position from depth, then
    // reproject it through last frame's camera to find where it was on
    // screen a frame ago.
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    var world = u.inv_view_proj * ndc;
    world = world / world.w;

    let prev_clip = u.prev_view_proj * world;
    if prev_clip.w <= 0.0 {
        out.color = current;
        out.history = current;
        return out;
    }
    let prev_ndc = prev_clip.xy / prev_clip.w;
    let prev_uv = vec2<f32>(prev_ndc.x * 0.5 + 0.5, 1.0 - (prev_ndc.y * 0.5 + 0.5));

    if any(prev_uv < vec2<f32>(0.0)) || any(prev_uv > vec2<f32>(1.0)) {
        out.color = current;
        out.history = current;
        return out;
    }

    // Clamp the history sample to the local neighborhood's color bounding
    // box, to suppress ghosting trails on disocclusion/fast motion.
    let texel = 1.0 / u.render_size;
    let n0 = textureSampleLevel(scene_color, tex_sampler, uv + vec2<f32>(texel.x, 0.0), 0.0).rgb;
    let n1 = textureSampleLevel(scene_color, tex_sampler, uv - vec2<f32>(texel.x, 0.0), 0.0).rgb;
    let n2 = textureSampleLevel(scene_color, tex_sampler, uv + vec2<f32>(0.0, texel.y), 0.0).rgb;
    let n3 = textureSampleLevel(scene_color, tex_sampler, uv - vec2<f32>(0.0, texel.y), 0.0).rgb;
    let nmin = min(current.rgb, min(min(n0, n1), min(n2, n3)));
    let nmax = max(current.rgb, max(max(n0, n1), max(n2, n3)));

    let hist = textureSampleLevel(history, tex_sampler, prev_uv, 0.0).rgb;
    let hist_clamped = clamp(hist, nmin, nmax);

    let resolved = mix(hist_clamped, current.rgb, u.blend);
    out.color = vec4<f32>(resolved, current.a);
    out.history = out.color;
    return out;
}
