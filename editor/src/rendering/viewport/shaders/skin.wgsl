struct SkinVertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) joints: vec4<u32>,
    @location(4) weights: vec4<f32>,
};

fn skin_linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn skin_weight_color(weight: f32) -> vec4<f32> {
    let t = clamp(weight, 0.0, 1.0);
    var rgb: vec3<f32>;
    if t < 0.25 { rgb = vec3<f32>(0.0, t * 4.0, 1.0); }
    else if t < 0.5 { rgb = vec3<f32>(0.0, 1.0, 1.0 - (t - 0.25) * 4.0); }
    else if t < 0.75 { rgb = vec3<f32>((t - 0.5) * 4.0, 1.0, 0.0); }
    else { rgb = vec3<f32>(1.0, 1.0 - (t - 0.75) * 4.0, 0.0); }
    return vec4<f32>(skin_linear(rgb.r), skin_linear(rgb.g), skin_linear(rgb.b), 1.0);
}

@vertex
fn vs_skin(input: SkinVertexIn) -> MeshVertexOut {
    let posed = skin_vertex(input.position, input.normal, input.joints, input.weights);
    var out: MeshVertexOut;
    var clip = u.view_proj * vec4<f32>(posed.position, 1.0);
    clip.x += u.jitter.x * clip.w;
    clip.y += u.jitter.y * clip.w;
    out.clip_pos = clip;
    out.normal = posed.normal;
    out.color = skin.color;
    if skin.params.y != 0u { out.color = skin_weight_color(posed.weight); }
    out.world = posed.position;
    out.glow = 0.0;
    return out;
}
