struct SkinJoint {
    matrix: mat4x4<f32>,
    selected: vec4<f32>,
};
struct SkinUniforms {
    color: vec4<f32>,
    params: vec4<u32>,
};
@group(1) @binding(0) var<storage, read> skin_joints: array<SkinJoint>;
@group(1) @binding(1) var<uniform> skin: SkinUniforms;

struct SkinResult {
    position: vec3<f32>,
    normal: vec3<f32>,
    weight: f32,
};

fn skin_vertex(position: vec3<f32>, normal: vec3<f32>, joints: vec4<u32>, weights: vec4<f32>) -> SkinResult {
    var p = vec3<f32>(0.0);
    var n = vec3<f32>(0.0);
    var used = 0.0;
    var selected = 0.0;
    let count = min(skin.params.x, arrayLength(&skin_joints));
    for (var i = 0u; i < 4u; i += 1u) {
        // the heat map sums raw weights, including negative ones, like bone_influence().
        if joints[i] < count {
            selected += weights[i] * skin_joints[joints[i]].selected.x;
            if weights[i] > 0.0 {
                let m = skin_joints[joints[i]].matrix;
                p += (m * vec4<f32>(position, 1.0)).xyz * weights[i];
                n += (m * vec4<f32>(normal, 0.0)).xyz * weights[i];
                used += weights[i];
            }
        }
    }
    if used <= 0.0 {
        return SkinResult(position, normal, selected);
    }
    // keep the cpu's upper 3x3 normal blend and its near-zero normalization rule.
    let len = length(n);
    if len >= 1e-8 {
        n *= 1.0 / len;
    }
    return SkinResult(p * (1.0 / used), n, selected);
}
