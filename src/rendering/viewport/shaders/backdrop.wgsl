// Viewport backdrop: a vertical gradient with a soft vignette, and the ground
// grid. Both are full-screen triangles shaded analytically, so there is no
// geometry to alias and the grid stays crisp at any distance.

struct FsIn {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vertex_index: u32) -> FsIn {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: FsIn;
    out.pos = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.ndc = positions[vertex_index];
    return out;
}

@fragment
fn fs_sky(in: FsIn) -> @location(0) vec4<f32> {
    let t = 1.0 - (in.ndc.y * 0.5 + 0.5); // 0 at the top edge, 1 at the bottom
    var color = mix(u.sky_top.rgb, u.sky_bottom.rgb, smoothstep(0.0, 1.0, t));

    // Darken the corners a touch to pull the eye toward the middle.
    let q = in.ndc * vec2<f32>(0.62, 0.82);
    color *= 1.0 - 0.30 * dot(q, q);

    // Dither so the gradient doesn't band on 8-bit output.
    color += (hash12(in.pos.xy) - 0.5) * (1.6 / 255.0);
    return vec4<f32>(color, 1.0);
}

struct GridOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

// Coverage (0..1) of grid lines spaced `cell` apart in a plane coordinate
// `p`, `width` pixels wide, anti-aliased with screen-space derivatives. Cells
// that shrink below a few pixels fade out instead of turning into moire.
fn grid_lines(p: vec2<f32>, cell: f32, width: f32) -> f32 {
    let c = p / cell;
    let fw = max(fwidth(c), vec2<f32>(1e-5));
    let dist_px = abs(fract(c - 0.5) - 0.5) / fw;
    let nearest = min(dist_px.x, dist_px.y);
    let coverage = clamp(width * 0.5 + 0.5 - nearest, 0.0, 1.0);
    let fade = 1.0 - smoothstep(0.16, 0.42, max(fw.x, fw.y));
    return coverage * fade;
}

// Coverage of a single line where `coord` crosses zero.
fn axis_line(coord: f32, width: f32) -> f32 {
    let fw = max(fwidth(coord), 1e-5);
    return clamp(width * 0.5 + 0.5 - abs(coord) / fw, 0.0, 1.0);
}

@fragment
fn fs_grid(in: FsIn) -> GridOut {
    // Ray through this pixel (the grid isn't jittered: it has no history to
    // average, and its anti-aliasing is analytic).
    let near_h = u.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let far_h = u.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let origin = near_h.xyz / near_h.w;
    let dir = far_h.xyz / far_h.w - origin;

    let hits_plane = abs(dir.y) > 1e-7 && (-origin.y / dir.y) > 0.0;
    let t = select(0.0, -origin.y / dir.y, hits_plane);
    let p = origin + dir * t;

    // Derivatives below must run in uniform control flow, so everything is
    // computed unconditionally and masked at the end.
    let minor = grid_lines(p.xz, 0.1, 1.0);
    let unit = grid_lines(p.xz, 1.0, 1.0);
    let major = grid_lines(p.xz, 10.0, 1.4);
    let on_x = axis_line(p.z, 1.6); // the X axis lies along z = 0
    let on_z = axis_line(p.x, 1.6); // the Z axis lies along x = 0

    let dist = length(p.xz - u.eye.xz);
    let grazing = smoothstep(0.0, 0.12, abs(normalize(dir).y));
    let fade = (1.0 - smoothstep(u.eye.w * 0.2, u.eye.w, dist)) * grazing;

    // Faint floor tint so the plane reads as a surface, not just lines.
    var layer = vec4<f32>(u.grid_major.rgb * 0.06 * fade, 0.06 * fade);

    // Soft contact shadow under the rig.
    let shadow_d = length(p.xz - u.shadow.xz) / max(u.shadow.y, 1e-3);
    let shadow = u.shadow.w * pow(1.0 - smoothstep(0.0, 1.0, shadow_d), 1.6);
    layer = over(layer, vec4<f32>(0.0, 0.0, 0.0, shadow));

    let minor_a = max(minor * 0.45, unit) * u.grid_minor.a * fade;
    layer = over(layer, vec4<f32>(u.grid_minor.rgb * minor_a, minor_a));
    let major_a = major * u.grid_major.a * fade;
    layer = over(layer, vec4<f32>(u.grid_major.rgb * major_a, major_a));
    // Axes are drawn a little further out than the rest of the grid.
    let axis_fade = 1.0 - smoothstep(u.eye.w * 0.5, u.eye.w * 1.2, dist);
    let ax_a = on_x * u.axis_x.a * axis_fade * grazing;
    layer = over(layer, vec4<f32>(u.axis_x.rgb * ax_a, ax_a));
    let az_a = on_z * u.axis_z.a * axis_fade * grazing;
    layer = over(layer, vec4<f32>(u.axis_z.rgb * az_a, az_a));

    if !hits_plane || layer.a < 0.002 {
        discard;
    }

    let clip = u.view_proj * vec4<f32>(p, 1.0);
    var out: GridOut;
    out.color = layer;
    out.depth = clamp(clip.z / clip.w, 0.0, 1.0);
    return out;
}
