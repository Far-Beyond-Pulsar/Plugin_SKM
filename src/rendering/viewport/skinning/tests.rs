use super::super::skin_gpu::validate_sizes;
use super::*;
use crate::core::skinning::joint_matrices;
use crate::core::{
    skinning::{skin_lod, skin_vertex},
    skmesh::{Joint, MeshLod, SkinnedVertex},
};

pub(crate) fn fixture(joint_count: usize) -> Arc<PreparedSkin> {
    let mut asset = SkeletalMeshAsset::new(None);
    asset.joints = (0..joint_count)
        .map(|j| Joint {
            bone: if j == 2 {
                "missing".into()
            } else {
                format!("bone{j}")
            },
            inverse_bind: Mat4::from_translation(Vec3::new(-0.3 * j.min(4) as f32, 0.1, -0.4)).0,
        })
        .collect();
    let patterns = [
        ([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
        ([1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
        ([0, 1, 0, 0], [0.3, 0.7, 0.0, 0.0]),
        ([0, 1, 2, 3], [0.1, 0.2, 0.3, 0.4]),
        ([0, 1, 2, 3], [0.2, 0.4, 0.6, 0.8]),
        ([0, 1, 2, 3], [0.0, 0.0, 0.0, 0.0]),
        ([u16::MAX, 0, 1, 3], [0.5, 0.25, -0.2, 0.0]),
        ([u16::MAX, u16::MAX, 0, 0], [1.0, 0.0, 0.0, 0.0]),
        ([2, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
        ([0, 1, u16::MAX, 3], [0.5, 0.5, 0.0, -1.0]),
    ];
    asset.lods = vec![MeshLod {
        vertices: (0..100)
            .map(|i| {
                let (joints, weights) = patterns[i % patterns.len()];
                SkinnedVertex {
                    position: [i as f32 * 0.17 - 5.0, (i as f32).sin(), -2.0],
                    normal: match i % 4 {
                        0 => [1.0, 2.0, -1.0],
                        1 => [0.0; 3],
                        2 => [1e-10, 0.0, 0.0],
                        _ => [0.0, 0.0, 1.0],
                    },
                    uv: [0.25, 0.75],
                    joints,
                    weights,
                }
            })
            .collect(),
        indices: vec![0, 1, 2, 10, 20, 30, 99, 98, 97],
        ..MeshLod::default()
    }];
    asset.lods.push(MeshLod {
        vertices: asset.lods[0].vertices[..3].to_vec(),
        indices: vec![0, 1, 2],
        ..MeshLod::default()
    });
    PreparedSkin::new(Arc::new(asset)).unwrap()
}

pub(crate) fn world(time: f32) -> HashMap<String, Mat4> {
    // insertion order deliberately differs from the mesh's joint order.
    [3, 1, 0]
        .into_iter()
        .map(|j| {
            let transform = Mat4::from_translation(Vec3::new(100.0 * time, -4.0, 2.0 * j as f32))
                .mul(&Mat4::from_euler_xyz_deg(Vec3::new(
                    37.0 * time,
                    -21.0,
                    90.0 * j as f32,
                )))
                .mul(&Mat4::from_scale(Vec3::new(-1.5, 0.3 + time, 2.0)));
            (format!("bone{j}"), transform)
        })
        .collect()
}

#[test]
fn packed_attributes_and_palette_match_wgsl() {
    use std::mem::{offset_of, size_of};
    assert_eq!(size_of::<SkinVertex>(), 64);
    assert_eq!(offset_of!(SkinVertex, normal), 12);
    assert_eq!(offset_of!(SkinVertex, uv), 24);
    assert_eq!(offset_of!(SkinVertex, joints), 32);
    assert_eq!(offset_of!(SkinVertex, weights), 48);
    assert_eq!(size_of::<SkinJoint>(), 80);
    assert_eq!(offset_of!(SkinJoint, selected), 64);
    assert_eq!(size_of::<SkinUniforms>(), 32);
    assert_eq!(offset_of!(SkinUniforms, params), 16);
    assert_eq!(
        super::super::skin_gpu::ATTRIBUTES
            .iter()
            .map(|a| a.offset)
            .collect::<Vec<_>>(),
        vec![0, 12, 24, 32, 48]
    );
    let mesh = fixture(4);
    for (packed, source) in mesh
        .pack_vertices(0)
        .iter()
        .zip(&mesh.asset.lods[0].vertices)
    {
        assert_eq!(packed.joints, source.joints.map(u32::from));
        assert_eq!(packed.position, source.position);
        assert_eq!(packed.normal, source.normal);
        assert_eq!(packed.uv, source.uv);
        assert_eq!(packed.weights, source.weights);
    }
}

#[test]
fn bounds_contain_cpu_vertices_with_large_animated_mirrored_blends_and_fallbacks() {
    for count in [0, 1, 4, 65536] {
        let mesh = fixture(count);
        for time in [0.0, 0.1, 2.0, 37.0] {
            let world = world(time);
            for lod in 0..2 {
                let frame =
                    SkinFrame::new(mesh.clone(), lod, &world, Some("bone1"), [1.0; 4]).unwrap();
                let corners = mesh.lods[lod].posed_bounds(&frame.joints).unwrap();
                let min = corners[0];
                let max = corners[7];
                let matrices = joint_matrices(&mesh.asset, &world);
                for v in skin_lod(&mesh.asset.lods[lod], &matrices) {
                    let p = v.position;
                    assert!(
                        p.x >= min.x
                            && p.y >= min.y
                            && p.z >= min.z
                            && p.x <= max.x
                            && p.y <= max.y
                            && p.z <= max.z,
                        "{p:?} outside {min:?}..{max:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn palette_order_inverse_bind_missing_bones_and_time_follow_cpu_reference() {
    let mesh = fixture(4);
    let w = world(1.0);
    let frame = SkinFrame::new(mesh.clone(), 0, &w, Some("bone1"), [0.5; 4]).unwrap();
    assert_eq!(
        frame.joints.iter().map(|j| j.matrix).collect::<Vec<_>>(),
        joint_matrices(&mesh.asset, &w)
            .into_iter()
            .map(|m| m.0)
            .collect::<Vec<_>>()
    );
    assert_eq!(frame.joints[2].matrix, Mat4::IDENTITY.0);
    assert_eq!(
        frame
            .joints
            .iter()
            .map(|j| j.selected[0])
            .collect::<Vec<_>>(),
        vec![0.0, 1.0, 0.0, 0.0]
    );
    assert_ne!(
        frame.joints,
        SkinFrame::new(mesh, 0, &world(1.2), None, [0.5; 4])
            .unwrap()
            .joints
    );
}

#[test]
fn cache_invalidation_handles_reimport_same_id_joint_edits_and_lod_changes() {
    let original = fixture(4).asset.clone();
    let mut cache = SkinCache::default();
    let a = cache.get(Some(&original), 1).unwrap();
    assert!(Arc::ptr_eq(&a, &cache.get(Some(&original), 1).unwrap()));
    let b = cache.get(Some(&original), 2).unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    let mut replacement = original.clone();
    Arc::make_mut(&mut replacement).joints[0].inverse_bind = Mat4::IDENTITY.0;
    Arc::make_mut(&mut replacement).lods[0].vertices[0].position = [50.0; 3];
    let c = cache.get(Some(&replacement), 2).unwrap();
    assert!(!Arc::ptr_eq(&b, &c));
    assert_eq!(c.asset.lods[0].vertices[0].position, [50.0; 3]);
    let w = world(0.1);
    let f = SkinFrame::new(c.clone(), 999, &w, None, [1.0; 4]).unwrap();
    assert_eq!(f.lod, 1);
    assert!(Arc::ptr_eq(&f.mesh, &c));
    assert!(cache.get(None, 3).is_none());
    assert!(cache.source.is_none());
}

#[test]
fn empty_invalid_data_and_device_limits_are_handled_before_upload() {
    let empty = PreparedSkin::new(Arc::new(SkeletalMeshAsset::new(None))).unwrap();
    assert!(SkinFrame::new(empty, 0, &HashMap::new(), None, [1.0; 4]).is_none());
    let mut asset = (*fixture(4).asset).clone();
    asset.lods[0].indices = vec![0, 1, 200, 0, 1, 2, 99];
    let mesh = PreparedSkin::new(Arc::new(asset.clone())).unwrap();
    assert_eq!(mesh.lods[0].indices, vec![0, 1, 2]);
    asset.lods[0].vertices[0].weights[0] = f32::NAN;
    assert!(PreparedSkin::new(Arc::new(asset)).is_err());
    let mut limits = wgpu::Limits::default();
    assert!(validate_sizes(&mesh, &limits).is_ok());
    limits.max_storage_buffer_binding_size = 80;
    assert!(validate_sizes(&mesh, &limits).is_err());
    limits.max_storage_buffer_binding_size = 1 << 20;
    limits.max_buffer_size = 63;
    assert!(validate_sizes(&mesh, &limits).is_err());
}

#[test]
fn bound_boxes_cover_vertices_with_no_usable_joint_and_missing_lod_geometry() {
    let mut asset = SkeletalMeshAsset::new(None);
    asset.lods.push(MeshLod::default());
    let mesh = PreparedSkin::new(Arc::new(asset)).unwrap();
    assert!(mesh.lods[0].posed_bounds(&[]).is_none());
    let mesh = fixture(0);
    let corners = mesh.lods[0].posed_bounds(&[]).unwrap();
    for v in &mesh.asset.lods[0].vertices {
        let p = skin_vertex(v, &[]).position;
        assert!(p.x >= corners[0].x && p.x <= corners[7].x);
    }
}

#[test]
fn wgsl_storage_offsets_match_the_uploaded_bytes() {
    let module = naga::front::wgsl::parse_str(&super::super::shaders::skin()).unwrap();
    for (name, size, offsets) in [
        ("SkinJoint", 80, vec![0, 64]),
        ("SkinUniforms", 32, vec![0, 16]),
    ] {
        let ty = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some(name))
            .unwrap()
            .1;
        let naga::TypeInner::Struct { members, span } = &ty.inner else {
            panic!("{name} is not a struct");
        };
        assert_eq!(*span, size);
        assert_eq!(
            members.iter().map(|m| m.offset).collect::<Vec<_>>(),
            offsets
        );
    }
}

#[test]
fn skeleton_replacement_updates_the_pose_without_repacking_static_vertices() {
    let mesh = fixture(4);
    let a = SkinFrame::new(mesh.clone(), 0, &world(0.2), None, [1.0; 4]).unwrap();
    let b = SkinFrame::new(mesh.clone(), 0, &HashMap::new(), None, [1.0; 4]).unwrap();
    assert!(Arc::ptr_eq(&a.mesh, &b.mesh));
    assert_ne!(a.joints, b.joints);
    assert!(b.joints.iter().all(|j| j.matrix == Mat4::IDENTITY.0));
    let mut bad = HashMap::new();
    let mut matrix = Mat4::IDENTITY;
    matrix.0[0] = f32::NAN;
    bad.insert("bone0".into(), matrix);
    assert!(SkinFrame::new(mesh, 0, &bad, None, [1.0; 4]).is_none());
}
