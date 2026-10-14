//! CPU linear-blend skinning of a [`SkeletalMeshAsset`] by a pose.
//!
//! Used for the editor's previews. The engine's runtime skinning is separate
//! (and on the GPU); this is the reference the previews and tests use.

use std::collections::HashMap;

use crate::math::{Mat4, Vec3};
use crate::skmesh::{MeshLod, SkeletalMeshAsset, SkinnedVertex};

/// A skinned vertex: where it ends up, and its normal there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Posed {
    pub position: Vec3,
    pub normal: Vec3,
}

/// One matrix per joint: the joint's bone in the pose (`world`, by bone id)
/// times the joint's inverse bind matrix. A joint whose bone is not in the
/// pose gets the identity, so its vertices stay where the mesh put them.
pub fn joint_matrices(mesh: &SkeletalMeshAsset, world: &HashMap<String, Mat4>) -> Vec<Mat4> {
    mesh.joints
        .iter()
        .map(|joint| match world.get(&joint.bone) {
            Some(bone_world) => bone_world.mul(&joint.inverse_bind_matrix()),
            None => Mat4::IDENTITY,
        })
        .collect()
}

/// Blend `vertex` by `matrices` (see [`joint_matrices`]).
///
/// Weights are used as stored; an influence on a joint outside `matrices` is
/// skipped. A vertex with no usable influence is returned unchanged. Normals
/// are blended with the matrices' upper 3x3, which is exact for rotation and
/// uniform scale.
pub fn skin_vertex(vertex: &SkinnedVertex, matrices: &[Mat4]) -> Posed {
    let position = Vec3::new(vertex.position[0], vertex.position[1], vertex.position[2]);
    let normal = Vec3::new(vertex.normal[0], vertex.normal[1], vertex.normal[2]);
    let (mut p, mut n, mut used) = (Vec3::ZERO, Vec3::ZERO, 0.0_f32);
    for (joint, weight) in vertex.joints.iter().zip(vertex.weights) {
        if weight <= 0.0 {
            continue;
        }
        let Some(m) = matrices.get(*joint as usize) else {
            continue;
        };
        p = p.add(m.transform_point(position).scale(weight));
        n = n.add(m.transform_vector(normal).scale(weight));
        used += weight;
    }
    if used <= 0.0 {
        return Posed { position, normal };
    }
    Posed {
        position: p.scale(1.0 / used),
        normal: n.normalize(),
    }
}

/// Skin every vertex of `lod`.
pub fn skin_lod(lod: &MeshLod, matrices: &[Mat4]) -> Vec<Posed> {
    lod.vertices
        .iter()
        .map(|v| skin_vertex(v, matrices))
        .collect()
}

/// For each vertex of `lod`, how strongly the bone `bone_id` influences it:
/// the sum of the weights of every joint that follows that bone.
pub fn bone_influence(mesh: &SkeletalMeshAsset, lod: &MeshLod, bone_id: &str) -> Vec<f32> {
    let follows: Vec<bool> = mesh.joints.iter().map(|j| j.bone == bone_id).collect();
    lod.vertices
        .iter()
        .map(|v| {
            v.joints
                .iter()
                .zip(v.weights)
                .filter(|(j, _)| follows.get(**j as usize).copied().unwrap_or(false))
                .map(|(_, w)| w)
                .sum()
        })
        .collect()
}

/// How many vertices of `lod` each joint influences (any weight above zero).
pub fn joint_vertex_counts(mesh: &SkeletalMeshAsset, lod: &MeshLod) -> Vec<usize> {
    let mut counts = vec![0; mesh.joints.len()];
    for v in &lod.vertices {
        for (j, w) in v.joints.iter().zip(v.weights) {
            if w > 0.0 {
                if let Some(count) = counts.get_mut(*j as usize) {
                    *count += 1;
                }
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SkeletalMeshId;
    use crate::skmesh::{Joint, MeshSection};

    /// Two joints: joint 0 follows "root", joint 1 follows "arm". `arm` binds
    /// one unit along +X from `root`, so its inverse bind translates by -1.
    fn mesh() -> SkeletalMeshAsset {
        let vertex = |position: [f32; 3], joints: [u16; 4], weights: [f32; 4]| SkinnedVertex {
            position,
            normal: [1.0, 0.0, 0.0],
            uv: [0.0; 2],
            joints,
            weights,
        };
        SkeletalMeshAsset {
            id: SkeletalMeshId::new(),
            skeleton: None,
            joints: vec![
                Joint {
                    bone: "root".into(),
                    inverse_bind: Mat4::IDENTITY.0,
                },
                Joint {
                    bone: "arm".into(),
                    inverse_bind: Mat4::from_translation(Vec3::new(-1.0, 0.0, 0.0)).0,
                },
            ],
            material_slots: Vec::new(),
            lods: vec![MeshLod {
                vertices: vec![
                    // Rigid to the root.
                    vertex([0.0, 1.0, 0.0], [0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
                    // Rigid to the arm, at the arm's own origin.
                    vertex([1.0, 0.0, 0.0], [1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
                    // Halfway between the two.
                    vertex([0.5, 0.0, 0.0], [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]),
                ],
                indices: vec![0, 1, 2],
                sections: vec![MeshSection {
                    first_index: 0,
                    index_count: 3,
                    material_slot: 0,
                }],
            }],
        }
    }

    fn pose(root: Mat4, arm_local: Mat4) -> HashMap<String, Mat4> {
        // The arm's bind is one unit along +X from the root.
        let arm = root
            .mul(&Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0)))
            .mul(&arm_local);
        HashMap::from([("root".to_string(), root), ("arm".to_string(), arm)])
    }

    fn near(a: Vec3, b: Vec3) {
        assert!(a.sub(b).length() < 1e-5, "{a:?} != {b:?}");
    }

    #[test]
    fn at_the_bind_pose_every_vertex_stays_where_it_is() {
        let mesh = mesh();
        let matrices = joint_matrices(&mesh, &pose(Mat4::IDENTITY, Mat4::IDENTITY));
        for (posed, v) in skin_lod(&mesh.lods[0], &matrices)
            .iter()
            .zip(&mesh.lods[0].vertices)
        {
            near(
                posed.position,
                Vec3::new(v.position[0], v.position[1], v.position[2]),
            );
            near(posed.normal, Vec3::new(1.0, 0.0, 0.0));
        }
    }

    #[test]
    fn a_rotated_arm_moves_its_vertices_and_blends_the_shared_one() {
        // Hand-computed: turn the arm 90 degrees about +Z. The arm's own origin
        // stays at (1, 0, 0); a point at (1, 0, 0) in mesh space is the origin
        // of the arm's space, so rigid-to-arm vertex 1 does not move, while
        // vertex 2 (0.5, 0, 0) is -0.5 along X in arm space and ends up at
        // arm origin + R(90) * (-0.5, 0, 0) = (1, -0.5, 0).
        let mesh = mesh();
        let arm_turn = Mat4::from_euler_xyz_deg(Vec3::new(0.0, 0.0, 90.0));
        let matrices = joint_matrices(&mesh, &pose(Mat4::IDENTITY, arm_turn));
        let posed = skin_lod(&mesh.lods[0], &matrices);

        near(posed[0].position, Vec3::new(0.0, 1.0, 0.0));
        near(posed[1].position, Vec3::new(1.0, 0.0, 0.0));
        near(posed[1].normal, Vec3::new(0.0, 1.0, 0.0));
        // 50% root (stays at (0.5, 0, 0)) + 50% arm ((1, -0.5, 0)) = (0.75, -0.25, 0).
        near(posed[2].position, Vec3::new(0.75, -0.25, 0.0));
        // Normal: half +X (root), half +Y (arm turned) -> normalized diagonal.
        near(posed[2].normal, Vec3::new(1.0, 1.0, 0.0).normalize());
    }

    #[test]
    fn moving_the_root_moves_everything_rigidly() {
        let mesh = mesh();
        let root = Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0));
        let matrices = joint_matrices(&mesh, &pose(root, Mat4::IDENTITY));
        let posed = skin_lod(&mesh.lods[0], &matrices);
        near(posed[0].position, Vec3::new(0.0, 1.0, 5.0));
        near(posed[1].position, Vec3::new(1.0, 0.0, 5.0));
        near(posed[2].position, Vec3::new(0.5, 0.0, 5.0));
    }

    #[test]
    fn a_joint_whose_bone_is_not_in_the_pose_leaves_its_vertices_alone() {
        let mesh = mesh();
        let mut world = pose(
            Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0)),
            Mat4::IDENTITY,
        );
        world.remove("arm");
        let posed = skin_lod(&mesh.lods[0], &joint_matrices(&mesh, &world));
        near(posed[1].position, Vec3::new(1.0, 0.0, 0.0));
    }

    #[test]
    fn unusable_influences_are_skipped_and_the_rest_renormalized() {
        let mesh = mesh();
        let matrices = joint_matrices(
            &mesh,
            &pose(
                Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0)),
                Mat4::IDENTITY,
            ),
        );
        let mut v = mesh.lods[0].vertices[0];
        v.joints = [0, 7, 0, 0];
        v.weights = [0.5, 0.5, 0.0, 0.0];
        near(
            skin_vertex(&v, &matrices).position,
            Vec3::new(0.0, 1.0, 2.0),
        );
        v.weights = [0.0; 4];
        near(
            skin_vertex(&v, &matrices).position,
            Vec3::new(0.0, 1.0, 0.0),
        );
    }

    #[test]
    fn bone_influence_sums_the_joints_that_follow_a_bone() {
        let mesh = mesh();
        let lod = &mesh.lods[0];
        assert_eq!(bone_influence(&mesh, lod, "arm"), vec![0.0, 1.0, 0.5]);
        assert_eq!(bone_influence(&mesh, lod, "root"), vec![1.0, 0.0, 0.5]);
        assert_eq!(bone_influence(&mesh, lod, "nope"), vec![0.0; 3]);
        assert_eq!(joint_vertex_counts(&mesh, lod), vec![2, 2]);
    }
}
