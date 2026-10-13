// Uniform block shared by every viewport pass (concatenated in front of each
// shader by `shaders.rs`). Mirrors `ViewportUniforms` in `types.rs`; all colors
// are linear.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // Inverse of the *unjittered* view_proj.
    inv_view_proj: mat4x4<f32>,
    // xyz = camera position, w = distance at which the ground grid has faded out.
    eye: vec4<f32>,
    viewport: vec2<f32>,
    time: f32,
    _pad: f32,
    // Sub-pixel clip-space offset for TAA sample jittering (zero when unused).
    jitter: vec2<f32>,
    _pad2: vec2<f32>,
    sky_top: vec4<f32>,
    sky_bottom: vec4<f32>,
    grid_minor: vec4<f32>,
    grid_major: vec4<f32>,
    axis_x: vec4<f32>,
    axis_z: vec4<f32>,
    // x, z = center of the contact shadow on the ground, y = radius, w = strength.
    shadow: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// Premultiplied-alpha "over".
fn over(dst: vec4<f32>, src: vec4<f32>) -> vec4<f32> {
    return src + dst * (1.0 - src.a);
}
