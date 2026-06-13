// Orientation gizmo: 6 axis spokes radiating from the origin, each capped
// with a colored "bubble" sized in screen-space pixels. Drawn into a small
// square viewport in the corner of the main 3D view, using its own
// rotation-only view-projection matrix.

struct Uniforms {
    view_proj: mat4x4<f32>,
    viewport: vec2<f32>,
    time: f32,
    _pad: f32,
    // Unused by the gizmo (always zero): kept for layout parity with
    // `scene.wgsl`'s `Uniforms`, since both are filled from the same
    // `ViewportUniforms` Rust struct.
    jitter: vec2<f32>,
    _pad2: vec2<f32>,
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
    @location(3) letter: f32,
};

struct BubbleVertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
    @location(2) letter: f32,
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

    // `offset` spans -1..1 (the full quad width), so halving here makes the
    // rendered diameter equal `input.size` pixels rather than 2x that.
    let ndc_size = (input.size / u.viewport) * clip.w;
    clip.x += offset.x * ndc_size.x;
    clip.y += offset.y * ndc_size.y;

    out.clip_pos = clip;
    out.color = input.color;
    out.local = offset;
    out.letter = input.letter;
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
// coordinate space (letter < 0 returns a large distance, i.e. no glyph).
fn letter_dist(p: vec2<f32>, letter: i32) -> f32 {
    if letter == 0 {
        // X
        let d1 = sd_segment(p, vec2<f32>(-0.45, -0.45), vec2<f32>(0.45, 0.45));
        let d2 = sd_segment(p, vec2<f32>(-0.45, 0.45), vec2<f32>(0.45, -0.45));
        return min(d1, d2);
    } else if letter == 1 {
        // Y
        let d1 = sd_segment(p, vec2<f32>(-0.45, 0.5), vec2<f32>(0.0, 0.0));
        let d2 = sd_segment(p, vec2<f32>(0.45, 0.5), vec2<f32>(0.0, 0.0));
        let d3 = sd_segment(p, vec2<f32>(0.0, 0.0), vec2<f32>(0.0, -0.5));
        return min(min(d1, d2), d3);
    } else if letter == 2 {
        // Z
        let d1 = sd_segment(p, vec2<f32>(-0.4, 0.45), vec2<f32>(0.4, 0.45));
        let d2 = sd_segment(p, vec2<f32>(0.4, 0.45), vec2<f32>(-0.4, -0.45));
        let d3 = sd_segment(p, vec2<f32>(-0.4, -0.45), vec2<f32>(0.4, -0.45));
        return min(min(d1, d2), d3);
    }
    return 1e9;
}

@fragment
fn fs_bubble(input: BubbleVertexOut) -> @location(0) vec4<f32> {
    let d = length(input.local);
    if d > 1.0 {
        discard;
    }

    // Flat backdrop disc behind the whole gizmo: no rim shading or label.
    if input.letter < -1.5 {
        return input.color;
    }

    // Simple rim-lit "bubble" look: brighter core, slightly darker edge.
    let shade = smoothstep(1.0, 0.55, d);
    var rgb = input.color.rgb * (0.7 + 0.3 * shade);

    let letter = i32(round(input.letter));
    if letter >= 0 {
        let ld = letter_dist(input.local, letter);
        let mask = 1.0 - smoothstep(0.12, 0.18, ld);
        // Label color: a darker shade of the bubble's own color, for
        // contrast without going all the way to flat black.
        let label_color = input.color.rgb * 0.35;
        rgb = mix(rgb, label_color, mask);
    }

    return vec4<f32>(rgb, input.color.a);
}
