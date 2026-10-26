//! fail closed before destructive edits; dependent files are never rewritten here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::assets::{load_animseq_asset, read_text};
use crate::core::ids::SkeletonId;
use crate::core::skmesh::SkeletalMeshAsset;

pub(crate) fn check(
    root: &Path,
    skeleton_id: &SkeletonId,
    affected: &HashSet<String>,
    reparent: bool,
) -> Result<(), String> {
    let entries = engine_fs::virtual_fs::manifest(root)
        .map_err(|e| format!("Cannot check dependent assets: {e}"))?;
    if entries.len() > 200_000 {
        return Err("Cannot check dependent assets: the project exceeds the scan limit.".into());
    }
    let mut paths: Vec<PathBuf> = entries
        .into_iter()
        .filter(|entry| !entry.is_dir)
        .filter(|entry| {
            !Path::new(&entry.path).components().any(|part| {
                matches!(
                    part.as_os_str().to_str(),
                    Some(
                        ".pulsar"
                            | ".git"
                            | ".svn"
                            | ".hg"
                            | "target"
                            | "node_modules"
                            | ".cache"
                            | ".gradle"
                    )
                )
            })
        })
        .map(|entry| root.join(entry.path))
        .collect();
    paths.sort();
    let mut mesh_ids = HashSet::new();
    for path in &paths {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("animseq") {
            let asset = load_animseq_asset(path)
                .map_err(|e| format!("Cannot check {}: {e}", path.display()))?;
            if asset.skeleton.as_ref() == Some(skeleton_id)
                && asset.clip.tracks.iter().any(|track| {
                    if reparent {
                        !track.keyframes.is_empty()
                    } else {
                        affected.contains(&track.bone_id)
                    }
                })
            {
                return Err(format!(
                    "{} references this skeleton's bones. {}",
                    path.display(),
                    if reparent {
                        "Reparenting requires migrating local animation tracks first."
                    } else {
                        "Remove or migrate those tracks before deleting bones."
                    }
                ));
            }
        } else if extension.eq_ignore_ascii_case("skmesh") {
            let (mesh_id, skeleton, joints) = SkeletalMeshAsset::peek_joints_at(path)
                .map_err(|e| format!("Cannot check {}: {e}", path.display()))?;
            if skeleton.as_ref() == Some(skeleton_id) {
                if !reparent && joints.iter().any(|joint| affected.contains(&joint.bone)) {
                    return Err(format!("{} has joints bound to these bones. Migrate the mesh before deleting them.", path.display()));
                }
                mesh_ids.insert(mesh_id);
            }
        }
    }
    if !reparent {
        for path in &paths {
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("physasset"))
            {
                let asset: pulsar_physics::skeletal::SkeletalPhysicsAsset =
                    serde_json::from_str(&read_text(path).map_err(|e| e.to_string())?)
                        .map_err(|e| format!("Cannot check {}: {e}", path.display()))?;
                if mesh_ids.contains(&asset.skeletal_mesh)
                    && (asset.bodies.iter().any(|b| affected.contains(&b.bone_id))
                        || asset.constraints.iter().any(|c| {
                            affected.contains(&c.bone_id) || affected.contains(&c.connected_bone_id)
                        }))
                {
                    return Err(format!("{} has physics bodies or constraints on these bones. Migrate the physics asset before deleting them.", path.display()));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{assets, sample, skmesh, AnimationClip};

    #[test]
    fn deletion_rejects_tracks_and_joints_without_rewriting_dependents() {
        let dir = tempfile::tempdir().unwrap();
        let (skeleton, mut sequence) = sample::sample_assets();
        let path = dir.path().join("walk.animseq");
        assets::save_animseq_asset(&path, &sequence).unwrap();
        let before = std::fs::read(&path).unwrap();
        let affected = sequence
            .clip
            .tracks
            .iter()
            .map(|t| t.bone_id.clone())
            .collect();
        assert!(check(dir.path(), &skeleton.id, &affected, false)
            .unwrap_err()
            .contains("tracks"));
        assert!(check(dir.path(), &skeleton.id, &HashSet::new(), true)
            .unwrap_err()
            .contains("migrating"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        sequence.skeleton = Some(SkeletonId::new());
        assets::save_animseq_asset(&path, &sequence).unwrap();
        assert!(check(dir.path(), &skeleton.id, &affected, false).is_ok());
        let mesh = sample::sample_mesh(&skeleton);
        let mesh_path = dir.path().join("body.skmesh");
        skmesh::save_skmesh_asset(&mesh_path, &mesh).unwrap();
        let before = std::fs::read(&mesh_path).unwrap();
        let affected = HashSet::from([mesh.joints[0].bone.clone()]);
        assert!(check(dir.path(), &skeleton.id, &affected, false)
            .unwrap_err()
            .contains("joints"));
        assert!(check(dir.path(), &skeleton.id, &affected, true).is_ok());
        assert_eq!(std::fs::read(&mesh_path).unwrap(), before);
    }

    #[test]
    fn incomplete_scans_and_unreadable_assets_do_not_authorize_deletion() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check(
            &dir.path().join("missing"),
            &SkeletonId::new(),
            &HashSet::new(),
            false
        )
        .is_err());
        std::fs::write(dir.path().join("broken.animseq"), "not json").unwrap();
        assert!(check(dir.path(), &SkeletonId::new(), &HashSet::new(), false).is_err());
    }

    #[test]
    fn empty_tracks_block_deletion_but_do_not_require_reparent_migration() {
        let dir = tempfile::tempdir().unwrap();
        let (skeleton, mut sequence) = sample::sample_assets();
        let affected = sequence
            .clip
            .tracks
            .iter()
            .map(|t| t.bone_id.clone())
            .collect();
        for track in &mut sequence.clip.tracks {
            track.keyframes.clear();
        }
        assets::save_animseq_asset(&dir.path().join("empty.animseq"), &sequence).unwrap();
        assert!(check(dir.path(), &skeleton.id, &affected, false).is_err());
        assert!(check(dir.path(), &skeleton.id, &affected, true).is_ok());
    }

    #[test]
    fn world_preserving_reparent_keeps_joint_matrices_and_skinned_vertices() {
        let (asset, _) = sample::sample_assets();
        let mesh = sample::sample_mesh(&asset);
        let mut skeleton = asset.skeleton();
        let clip = AnimationClip::bind_pose();
        let before = crate::core::skinning::joint_matrices(
            &mesh,
            &crate::core::evaluate_world_transforms(&skeleton, &clip, 0.0),
        );
        skeleton
            .reparent_bone("upper_arm_l", Some("pelvis"))
            .unwrap();
        let after = crate::core::skinning::joint_matrices(
            &mesh,
            &crate::core::evaluate_world_transforms(&skeleton, &clip, 0.0),
        );
        assert_eq!(before.len(), after.len());
        for (before, after) in before.iter().zip(&after) {
            assert!(before.approx_eq(after, 1e-5));
        }
        for vertex in &mesh.lods[0].vertices {
            let before = crate::core::skinning::skin_vertex(vertex, &before);
            let after = crate::core::skinning::skin_vertex(vertex, &after);
            assert!(before.position.sub(after.position).length() < 1e-5);
        }
    }

    #[test]
    fn physics_references_on_a_bone_without_mesh_joints_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (skeleton, _) = sample::sample_assets();
        let mut mesh = sample::sample_mesh(&skeleton);
        mesh.joints.clear();
        skmesh::save_skmesh_asset(&dir.path().join("body.skmesh"), &mesh).unwrap();
        let physics = pulsar_physics::skeletal::SkeletalPhysicsAsset::from_skeleton(
            mesh.id,
            &skeleton.skeleton(),
        );
        let path = dir.path().join("body.physasset");
        std::fs::write(&path, serde_json::to_string(&physics).unwrap()).unwrap();
        assert!(check(
            dir.path(),
            &skeleton.id,
            &HashSet::from(["root".into()]),
            false
        )
        .unwrap_err()
        .contains("physics"));
    }
}

#[cfg(test)]
mod hidden_asset_tests {
    use super::*;
    use crate::core::{assets, sample, skmesh};

    #[test]
    fn hidden_files_and_directories_protect_animation_mesh_and_physics_references() {
        for folder in ["", ".authored"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let assets_dir = root.join(folder);
            std::fs::create_dir_all(&assets_dir).unwrap();
            let (skeleton, sequence) = sample::sample_assets();
            let affected = skeleton.bones.iter().map(|b| b.id.clone()).collect();
            let sequence_path = assets_dir.join(".hidden.animseq");
            assets::save_animseq_asset(&sequence_path, &sequence).unwrap();
            assert!(check(root, &skeleton.id, &affected, false)
                .unwrap_err()
                .contains("tracks"));
            assert!(check(root, &skeleton.id, &affected, true).is_err());
            std::fs::remove_file(sequence_path).unwrap();
            let mut mesh = sample::sample_mesh(&skeleton);
            let mesh_path = assets_dir.join(".hidden.skmesh");
            skmesh::save_skmesh_asset(&mesh_path, &mesh).unwrap();
            assert!(check(root, &skeleton.id, &affected, false)
                .unwrap_err()
                .contains("joints"));
            mesh.joints.clear();
            skmesh::save_skmesh_asset(&mesh_path, &mesh).unwrap();
            let physics = pulsar_physics::skeletal::SkeletalPhysicsAsset::from_skeleton(
                mesh.id,
                &skeleton.skeleton(),
            );
            let physics_path = assets_dir.join(".hidden.physasset");
            std::fs::write(&physics_path, serde_json::to_string(&physics).unwrap()).unwrap();
            assert!(check(root, &skeleton.id, &affected, false)
                .unwrap_err()
                .contains("physics"));
            assert!(check(root, &skeleton.id, &affected, true).is_ok());
        }
    }

    #[test]
    fn only_established_metadata_and_build_directories_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        for folder in [
            ".pulsar/trash",
            ".git",
            "target",
            "node_modules",
            ".cache",
            ".gradle",
            ".svn",
            ".hg",
        ] {
            std::fs::create_dir_all(dir.path().join(folder)).unwrap();
            std::fs::write(dir.path().join(folder).join("broken.animseq"), "broken").unwrap();
        }
        assert!(check(dir.path(), &SkeletonId::new(), &HashSet::new(), false).is_ok());
    }

    #[test]
    fn joint_reference_checks_do_not_require_the_geometry_payload() {
        let dir = tempfile::tempdir().unwrap();
        let (skeleton, _) = sample::sample_assets();
        let mesh = sample::sample_mesh(&skeleton);
        let mut bytes = mesh.encode();
        let header_len = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
        bytes.truncate(16 + header_len);
        let path = dir.path().join("header.skmesh");
        std::fs::write(&path, bytes).unwrap();
        assert!(skmesh::load_skmesh_asset(&path).is_err());
        assert!(check(
            dir.path(),
            &skeleton.id,
            &HashSet::from([mesh.joints[0].bone.clone()]),
            false
        )
        .unwrap_err()
        .contains("joints"));
        assert!(check(dir.path(), &skeleton.id, &HashSet::new(), true).is_ok());
    }
}
