//! Problems with the links between the skeletal assets, and with a mesh.

use super::animation::AnimationClip;
use super::ids::SkeletonId;
use super::library::SkeletonLink;
use super::mesh_library::MeshLink;
use super::skeleton::Skeleton;
use super::skmesh::{validate_mesh, SkeletalMeshAsset};

pub use skeletal_assets::issue::{Issue, Severity};

/// Everything wrong with `clip` given what its skeleton reference resolved to.
///
/// Tracks name bones by bone id; once a skeleton is attached, a track whose
/// bone is not in it is reported (and has no effect on the pose) but is never
/// removed from the clip.
pub fn validate_sequence(clip: &AnimationClip, link: &SkeletonLink) -> Vec<Issue> {
    match link {
        SkeletonLink::Unset => vec![Issue::new(
            Severity::Warning,
            "No skeleton is assigned. Pick one to see and edit the pose.",
        )],
        SkeletonLink::Resolving(_) => vec![Issue::new(Severity::Info, "Looking for the skeleton in the project...")],
        SkeletonLink::Missing(id) => vec![Issue::new(
            Severity::Error,
            format!(
                "The skeleton this sequence uses ({}) is not in the project. Showing keys only, read-only.",
                id.short()
            ),
        )],
        SkeletonLink::Ambiguous { id, paths } => vec![Issue::new(
            Severity::Error,
            format!(
                "More than one skeleton file has id {} ({} files). Showing keys only, read-only, until one gets a new id.",
                id.short(),
                paths.len()
            ),
        )],
        SkeletonLink::Resolved(entry) => {
            let orphans = clip.orphan_tracks(&entry.skeleton);
            if orphans.is_empty() {
                return Vec::new();
            }
            let mut names: Vec<&str> = orphans.iter().take(3).map(|t| t.bone_id.as_str()).collect();
            if orphans.len() > names.len() {
                names.push("...");
            }
            vec![Issue::new(
                Severity::Warning,
                format!(
                    "{} track(s) animate bones that are not in \"{}\" and are ignored: {}.",
                    orphans.len(),
                    entry.name,
                    names.join(", ")
                ),
            )]
        }
    }
}

/// Everything wrong with a skeletal mesh being edited, given what its
/// skeleton reference resolved to: the link itself, then the mesh's own data
/// (weights, joint indices, joints whose bone is missing from the skeleton).
///
/// Only reports; nothing in the mesh is ever changed or removed.
pub fn validate_skeletal_mesh(mesh: &SkeletalMeshAsset, link: &SkeletonLink) -> Vec<Issue> {
    let mut issues = match link {
        SkeletonLink::Unset => vec![Issue::new(
            Severity::Warning,
            "No skeleton is assigned. Pick the skeleton this mesh is skinned to.",
        )],
        SkeletonLink::Resolving(_) => vec![Issue::new(Severity::Info, "Looking for the skeleton in the project...")],
        SkeletonLink::Missing(id) => vec![Issue::new(
            Severity::Error,
            format!(
                "The skeleton this mesh is skinned to ({}) is not in the project. The joint-to-bone checks are skipped.",
                id.short()
            ),
        )],
        SkeletonLink::Ambiguous { id, paths } => vec![Issue::new(
            Severity::Error,
            format!(
                "More than one skeleton file has id {} ({} files). The joint-to-bone checks are skipped until one gets a new id.",
                id.short(),
                paths.len()
            ),
        )],
        SkeletonLink::Resolved(_) => Vec::new(),
    };
    let skeleton = match link {
        SkeletonLink::Resolved(entry) => Some(&*entry.skeleton),
        _ => None,
    };
    issues.extend(validate_mesh(mesh, skeleton));
    issues
}

/// Problems with the optional preview mesh of a skeleton or sequence, given
/// what its reference resolved to, the skeleton on screen (`skeleton_id` is its
/// id, if known) and the loaded mesh, if it has loaded. A preview is only a
/// convenience, so none of these is more than a warning.
pub fn validate_preview(
    link: &MeshLink,
    skeleton_id: Option<&SkeletonId>,
    skeleton: &Skeleton,
    mesh: Option<&SkeletalMeshAsset>,
) -> Vec<Issue> {
    match link {
        MeshLink::Unset | MeshLink::Resolving(_) => Vec::new(),
        MeshLink::Missing(id) => vec![Issue::new(
            Severity::Warning,
            format!("The preview mesh ({}) is not in the project, so no mesh is drawn.", id.short()),
        )],
        MeshLink::Ambiguous { id, paths } => vec![Issue::new(
            Severity::Warning,
            format!(
                "More than one mesh file has id {} ({} files), so no mesh is drawn until one gets a new id.",
                id.short(),
                paths.len()
            ),
        )],
        MeshLink::Resolved(entry) => {
            let mut issues = Vec::new();
            if skeleton_id.is_some() && entry.skeleton.as_ref() != skeleton_id {
                issues.push(Issue::new(
                    Severity::Warning,
                    format!(
                        "The preview mesh \"{}\" is skinned to a different skeleton and may not follow this one.",
                        entry.name
                    ),
                ));
            }
            if let (Some(mesh), false) = (mesh, skeleton.bones.is_empty()) {
                let missing: Vec<&str> = mesh
                    .joints
                    .iter()
                    .map(|j| j.bone.as_str())
                    .filter(|bone| skeleton.bone(bone).is_none())
                    .collect();
                if !missing.is_empty() {
                    let mut names: Vec<&str> = missing.iter().take(3).copied().collect();
                    if missing.len() > names.len() {
                        names.push("...");
                    }
                    issues.push(Issue::new(
                        Severity::Warning,
                        format!(
                            "{} joint(s) of the preview mesh follow bones that are not in this skeleton and stay still: {}.",
                            missing.len(),
                            names.join(", ")
                        ),
                    ));
                }
            }
            issues
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::animation::BoneTrack;
    use crate::core::ids::SkeletonId;
    use crate::core::library::Entry;
    use crate::core::sample;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn resolved() -> SkeletonLink {
        SkeletonLink::Resolved(Entry {
            id: SkeletonId::new(),
            path: PathBuf::new(),
            name: "Humanoid".into(),
            skeleton: Arc::new(sample::humanoid_skeleton()),
        })
    }

    #[test]
    fn a_clean_sequence_has_no_issues() {
        let clip = sample::run_animation(&sample::humanoid_skeleton());
        assert!(validate_sequence(&clip, &resolved()).is_empty());
    }

    #[test]
    fn tracks_for_unknown_bones_are_reported_but_kept() {
        let mut clip = sample::run_animation(&sample::humanoid_skeleton());
        let before = clip.tracks.len();
        clip.tracks.push(BoneTrack {
            bone_id: "tail_03".into(),
            keyframes: Vec::new(),
        });
        let issues = validate_sequence(&clip, &resolved());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains("tail_03"));
        assert_eq!(clip.tracks.len(), before + 1, "nothing is deleted");
    }


    fn resolved_for(asset: &crate::core::assets::SkeletonAsset) -> SkeletonLink {
        SkeletonLink::Resolved(Entry {
            id: asset.id.clone(),
            path: PathBuf::new(),
            name: "Humanoid".into(),
            skeleton: Arc::new(asset.skeleton()),
        })
    }

    #[test]
    fn a_skeletal_mesh_is_checked_against_its_skeleton_link() {
        let (skeleton, _) = sample::sample_assets();
        let mesh = sample::sample_mesh(&skeleton);
        assert!(validate_skeletal_mesh(&mesh, &resolved_for(&skeleton)).is_empty());

        // Unresolved links are reported, and the bone checks are skipped
        // rather than flagging every joint.
        for (link, severity) in [
            (SkeletonLink::Unset, Severity::Warning),
            (SkeletonLink::Missing(SkeletonId::new()), Severity::Error),
        ] {
            let issues = validate_skeletal_mesh(&mesh, &link);
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert_eq!(issues[0].severity, severity);
        }

        // A joint whose bone the skeleton lacks is an error, and stays in the mesh.
        let mut broken = mesh.clone();
        broken.joints[3].bone = "tail".into();
        let issues = validate_skeletal_mesh(&broken, &resolved_for(&skeleton));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("tail"));
        assert_eq!(broken.joints.len(), mesh.joints.len());
    }

    #[test]
    fn a_preview_mesh_link_only_ever_warns() {
        use crate::core::mesh_library::MeshEntry;
        use crate::core::ids::SkeletalMeshId;

        let (skeleton, _) = sample::sample_assets();
        let mesh = sample::sample_mesh(&skeleton);
        let entry = |skeleton: Option<SkeletonId>| MeshEntry {
            id: mesh.id.clone(),
            path: PathBuf::from("Body.skmesh"),
            name: "Body".into(),
            skeleton,
        };
        let data = skeleton.skeleton();
        let check = |link: &MeshLink, mesh: Option<&SkeletalMeshAsset>| {
            validate_preview(link, Some(&skeleton.id), &data, mesh)
        };

        assert!(check(&MeshLink::Unset, None).is_empty());
        assert!(check(&MeshLink::Resolving(SkeletalMeshId::new()), None).is_empty());
        assert!(check(&MeshLink::Resolved(entry(Some(skeleton.id.clone()))), Some(&mesh)).is_empty());

        let missing = check(&MeshLink::Missing(SkeletalMeshId::new()), None);
        let ambiguous = check(
            &MeshLink::Ambiguous { id: SkeletalMeshId::new(), paths: vec![PathBuf::from("a"), PathBuf::from("b")] },
            None,
        );
        assert!(missing.iter().chain(&ambiguous).all(|i| i.severity == Severity::Warning));
        assert!(ambiguous[0].message.contains("2 files"));

        // Skinned to some other skeleton.
        let other = check(&MeshLink::Resolved(entry(Some(SkeletonId::new()))), Some(&mesh));
        assert_eq!(other.len(), 1);
        assert!(other[0].message.contains("different skeleton"));

        // Joints that follow bones this skeleton lacks.
        let mut odd = mesh.clone();
        odd.joints[2].bone = "wing".into();
        let issues = check(&MeshLink::Resolved(entry(Some(skeleton.id.clone()))), Some(&odd));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("wing"));
    }
    #[test]
    fn unresolved_links_are_reported() {
        let clip = sample::run_animation(&sample::humanoid_skeleton());
        let id = SkeletonId::new();
        assert_eq!(validate_sequence(&clip, &SkeletonLink::Unset)[0].severity, Severity::Warning);
        assert_eq!(validate_sequence(&clip, &SkeletonLink::Missing(id.clone()))[0].severity, Severity::Error);
        let ambiguous = SkeletonLink::Ambiguous {
            id,
            paths: vec![PathBuf::from("a"), PathBuf::from("b")],
        };
        assert!(validate_sequence(&clip, &ambiguous)[0].message.contains("2 files"));
    }
}
