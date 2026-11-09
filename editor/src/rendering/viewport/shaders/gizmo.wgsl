// Orientation gizmo: axis-end "bubbles" and the translucent disc behind them,
// sized in screen pixels and positioned in NDC by the CPU (the gizmo pass uses
// an identity view-projection). The spokes between the bubbles are drawn with
// the ribbon pipeline.

struct BubbleInstanceIn {
    @location(0) center: vec3<f32>,
    // Diameter in pixels.
    @location(1) size: f32,
    @location(2) color: vec4<f32>,
    // Axis label glyph: 0 = X, 1 = Y, 2 = Z, -1 = none.
    @location(3) letter: f32,
    // 0 = filled sphere, 1 = hollow ring (negative axis), 2 = backdrop disc.
    @location(4) kind: f32,
};

struct BubbleVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Position inside the bubble in units of its radius (the quad is a pixel
    // larger than the bubble so anti-aliased edges fit).
    @location(1) local: vec2<f32>,
    @location(2) letter: f32,
    @location(3) kind: f32,
    @location(4) radius_px: f32,
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

    let radius = input.size * 0.5;
    let half_px = radius + 1.0;
    let ndc_half = (vec2<f32>(half_px) / u.viewport) * 2.0 * clip.w;
    clip.x += offset.x * ndc_half.x;
    clip.y += offset.y * ndc_half.y;

    out.clip_pos = clip;
    out.color = input.color;
    out.local = offset * (half_px / radius);
    out.letter = input.letter;
    out.kind = input.kind;
    out.radius_px = radius;
    return out;
}

// Distance from `p` to the line segment between `a` and `b`.
fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

// Distance to a simple stroked X / Y / Z glyph, in the [-1,1] bubble-local
// coordinate space.
fn letter_dist(p: vec2<f32>, letter: i32) -> f32 {
    if letter == 0 {
        let d1 = sd_segment(p, vec2<f32>(-0.42, -0.42), vec2<f32>(0.42, 0.42));
        let d2 = sd_segment(p, vec2<f32>(-0.42, 0.42), vec2<f32>(0.42, -0.42));
        return min(d1, d2);
    } else if letter == 1 {
        let d1 = sd_segment(p, vec2<f32>(-0.42, 0.46), vec2<f32>(0.0, 0.0));
        let d2 = sd_segment(p, vec2<f32>(0.42, 0.46), vec2<f32>(0.0, 0.0));
        let d3 = sd_segment(p, vec2<f32>(0.0, 0.0), vec2<f32>(0.0, -0.46));
        return min(min(d1, d2), d3);
    } else if letter == 2 {
        let d1 = sd_segment(p, vec2<f32>(-0.38, 0.42), vec2<f32>(0.38, 0.42));
        let d2 = sd_segment(p, vec2<f32>(0.38, 0.42), vec2<f32>(-0.38, -0.42));
        let d3 = sd_segment(p, vec2<f32>(-0.38, -0.42), vec2<f32>(0.38, -0.42));
        return min(min(d1, d2), d3);
    }
    return 1e9;
}

@fragment
fn fs_bubble(input: BubbleVertexOut) -> @location(0) vec4<f32> {
    let d = length(input.local);
    // Anti-aliased silhouette: one pixel of falloff at the edge.
    let edge = clamp((1.0 - d) * input.radius_px + 0.5, 0.0, 1.0);
    if edge <= 0.0 {
        discard;
    }
    let kind = i32(round(input.kind));

    if kind == 2 {
        // Backdrop: faint radial fill with a thin lighter rim.
        let rim = 1.0 - smoothstep(0.0, 2.0 / input.radius_px, abs(1.0 - d));
        let a = input.color.a * (0.75 + 0.25 * (1.0 - d)) * edge;
        let rgb = mix(input.color.rgb, vec3<f32>(1.0), 0.10 * rim);
        return vec4<f32>(rgb, max(a, rim * 0.22 * edge));
    }

    if kind == 1 {
        // Negative axis: translucent fill and a crisp ring.
        let ring = clamp(1.6 + 0.5 - abs(d - 1.0) * input.radius_px - 0.0, 0.0, 1.0);
        let fill = 0.28 * edge;
        let a = max(ring * edge, fill) * input.color.a;
        return vec4<f32>(input.color.rgb, a);
    }

    // Positive axis: a lit sphere.
    let z = sqrt(max(1.0 - d * d, 0.0));
    let n = vec3<f32>(input.local.x, -input.local.y, z);
    let light = normalize(vec3<f32>(-0.35, 0.55, 0.75));
    let diffuse = max(dot(n, light), 0.0);
    let spec = pow(max(dot(reflect(-light, n), vec3<f32>(0.0, 0.0, 1.0)), 0.0), 24.0) * 0.45;
    let rim = pow(1.0 - z, 2.5) * 0.25;
    var rgb = input.color.rgb * (0.55 + 0.6 * diffuse) + vec3<f32>(spec + rim);

    let letter = i32(round(input.letter));
    if letter >= 0 {
        let ld = letter_dist(input.local, letter);
        let mask = 1.0 - smoothstep(0.10, 0.17, ld);
        rgb = mix(rgb, vec3<f32>(0.96, 0.97, 1.0) * 0.95, mask);
    }
    return vec4<f32>(rgb, input.color.a * edge);
}
