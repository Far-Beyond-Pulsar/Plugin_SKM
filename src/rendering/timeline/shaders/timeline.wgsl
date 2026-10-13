// Timeline shape renderer.
//
// Every instance is a screen-space quad (grown by a small pad so anti-aliasing
// and glow have room) shaded with a signed distance function. Output is
// premultiplied alpha.

struct Uniforms {
    viewport: vec2<f32>,
    scroll_x: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

const SHAPE_RECT: u32 = 0u;
const SHAPE_DIAMOND: u32 = 1u;
const SHAPE_HATCH: u32 = 2u;
const SHAPE_TRIANGLE_DOWN: u32 = 3u;
const SHAPE_GRADIENT: u32 = 4u;
const FLAG_FIXED: u32 = 0x100u;

struct RectInstanceIn {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) border_color: vec4<f32>,
    @location(4) kind: u32,
    @location(5) radius: f32,
    @location(6) border: f32,
    @location(7) glow: f32,
};

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) border_color: vec4<f32>,
    // Position relative to the shape's center, in pixels.
    @location(2) local: vec2<f32>,
    @location(3) half_size: vec2<f32>,
    @location(4) screen: vec2<f32>,
    @location(5) @interpolate(flat) params: vec4<f32>, // radius, border, glow, unused
    @location(6) @interpolate(flat) kind: u32,
    // 0 at the top edge of the shape, 1 at the bottom edge.
    @location(7) v: f32,
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
    let c = corners[vertex_index];

    let pad = input.glow * 2.0 + 2.0;
    let grown = input.size + vec2<f32>(pad * 2.0);
    var origin = input.pos - vec2<f32>(pad);
    if (input.kind & FLAG_FIXED) == 0u {
        origin.x -= u.scroll_x;
    }
    let px = origin + c * grown;

    let ndc_x = (px.x / u.viewport.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / u.viewport.y) * 2.0;

    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = input.color;
    out.border_color = input.border_color;
    out.local = (c - vec2<f32>(0.5)) * grown;
    out.half_size = input.size * 0.5;
    out.screen = px;
    out.params = vec4<f32>(input.radius, input.border, input.glow, 0.0);
    out.kind = input.kind;
    out.v = (c.y * grown.y - pad) / max(input.size.y, 1.0);
    return out;
}

fn sd_round_box(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let rr = min(r, min(half.x, half.y));
    let q = abs(p) - half + vec2<f32>(rr);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

// Rounded rhombus with half-diagonals `half`.
fn sd_diamond(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let h = min(half.x, half.y);
    let inset = max(h - r * 1.41421356, 0.0);
    return (abs(p.x) + abs(p.y) - inset) * 0.70710678 - r;
}

// Isosceles triangle pointing down, bounding box `half`.
fn sd_triangle_down(p: vec2<f32>, half: vec2<f32>) -> f32 {
    // Apex at (0, +half.y); the slanted edges run from there to the top
    // corners (+-half.x, -half.y). Distance to the right edge's line, mirrored
    // for the left; the flat top closes the shape.
    let edge = (abs(p.x) * 2.0 * half.y + (p.y - half.y) * half.x)
        / length(vec2<f32>(2.0 * half.y, half.x));
    let top = -p.y - half.y;
    return max(edge, top);
}

fn over(dst: vec4<f32>, src: vec4<f32>) -> vec4<f32> {
    return src + dst * (1.0 - src.a);
}

fn premul(c: vec4<f32>, coverage: f32) -> vec4<f32> {
    let a = c.a * coverage;
    return vec4<f32>(c.rgb * a, a);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let shape = input.kind & 0xFFu;
    let radius = input.params.x;
    let border = input.params.y;
    let glow = input.params.z;

    if shape == SHAPE_HATCH {
        // 45-degree stripes over a darkened fill.
        let period = 12.0;
        let coord = input.screen.x + input.screen.y;
        let m = coord - floor(coord / period) * period;
        let stripe = 1.0 - smoothstep(1.0, 2.2, abs(m - 1.5));
        let fill = premul(input.color, 1.0);
        let line = premul(vec4<f32>(input.border_color.rgb, input.border_color.a), stripe);
        // The hatch quad is grown by `pad`; clip it back to the nominal rect.
        let inside = step(abs(input.local.x), input.half_size.x) * step(abs(input.local.y), input.half_size.y);
        return over(fill, line) * inside;
    }

    var d: f32;
    if shape == SHAPE_DIAMOND {
        d = sd_diamond(input.local, input.half_size, radius);
    } else if shape == SHAPE_TRIANGLE_DOWN {
        d = sd_triangle_down(input.local, input.half_size);
    } else {
        d = sd_round_box(input.local, input.half_size, radius);
    }

    let cover = clamp(0.5 - d, 0.0, 1.0);
    let inner = select(cover, clamp(0.5 - (d + border), 0.0, 1.0), border > 0.0);

    var fill_color = input.color;
    if shape == SHAPE_GRADIENT {
        fill_color = mix(input.color, input.border_color, clamp(input.v, 0.0, 1.0));
    }

    var result = vec4<f32>(0.0);
    if glow > 0.0 {
        // Soft halo that falls off outside the shape.
        let halo = exp(-max(d, 0.0) / max(glow * 0.45, 0.001)) * (1.0 - cover);
        result = premul(input.border_color, halo * 0.55);
    }
    if border > 0.0 {
        // The outline is the ring between the shape's edge and its inset copy;
        // the fill covers the inset part. They never overlap, so a translucent
        // fill doesn't pick up the outline color.
        let ring = max(cover - inner, 0.0);
        result = over(result, premul(input.border_color, ring) + premul(fill_color, inner));
    } else {
        result = over(result, premul(fill_color, cover));
    }
    return result;
}
