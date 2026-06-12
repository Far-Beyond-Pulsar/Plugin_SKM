struct Uniforms {
    viewport: vec2<f32>,
    scroll_x: f32,
    px_per_sec: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct RectInstanceIn {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) kind: u32,
    @location(4) _pad: vec3<u32>,
};

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) kind: u32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    input: RectInstanceIn,
) -> VertexOut {
    var out: VertexOut;

    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let uv = corners[vertex_index];

    var px = input.pos + uv * input.size;
    if input.kind != 2u {
        px.x -= u.scroll_x;
    }

    let ndc_x = (px.x / u.viewport.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / u.viewport.y) * 2.0;

    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = input.color;
    out.uv = uv;
    out.kind = input.kind;
    return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    if input.kind == 1u {
        let d = abs(input.uv.x - 0.5) + abs(input.uv.y - 0.5);
        if d > 0.5 {
            discard;
        }
    }
    return input.color;
}
