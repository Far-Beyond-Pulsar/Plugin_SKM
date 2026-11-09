//! What a reference field shows for each state its link can be in: the button
//! label, its tooltip, and the status tag. Pure functions of the link, so the
//! states can be tested without any UI; [`super::skeleton_ref`] and
//! [`super::mesh_ref`] turn the result into widgets.

use std::path::Path;

use crate::core::{MeshLink, SkeletonLink};

/// Which kind of tag a status is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Warning,
    Secondary,
    Success,
    Danger,
}

/// What one reference field shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldState {
    pub label: String,
    pub tooltip: String,
    pub status: &'static str,
    pub tone: Tone,
}

/// `path` relative to the project, with `/` separators.
pub fn display_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn join_paths(paths: &[std::path::PathBuf], root: &Path) -> String {
    paths
        .iter()
        .map(|p| display_path(p, root))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The skeleton field. `unset_tooltip` says what the skeleton is for (a
/// sequence animates it, a mesh is skinned to it).
pub fn skeleton_field(link: &SkeletonLink, root: &Path, unset_tooltip: &str) -> FieldState {
    match link {
        SkeletonLink::Unset => FieldState {
            label: "No skeleton".into(),
            tooltip: unset_tooltip.into(),
            status: "Unset",
            tone: Tone::Warning,
        },
        SkeletonLink::Resolving(id) => FieldState {
            label: format!("Skeleton {}", id.short()),
            tooltip: "Looking for the skeleton in the project".into(),
            status: "Resolving",
            tone: Tone::Secondary,
        },
        SkeletonLink::Resolved(entry) => FieldState {
            label: entry.name.clone(),
            tooltip: if entry.path.as_os_str().is_empty() {
                "Sample skeleton (not saved)".into()
            } else {
                display_path(&entry.path, root)
            },
            status: "Linked",
            tone: Tone::Success,
        },
        SkeletonLink::Missing(id) => FieldState {
            label: format!("Missing {}", id.short()),
            tooltip: format!("No skeleton with id {id} is in the project"),
            status: "Missing",
            tone: Tone::Danger,
        },
        SkeletonLink::Ambiguous { id, paths } => FieldState {
            label: format!("Ambiguous {}", id.short()),
            tooltip: format!("Claimed by: {}", join_paths(paths, root)),
            status: "Ambiguous",
            tone: Tone::Danger,
        },
    }
}

/// The preview mesh field.
pub fn mesh_field(link: &MeshLink, root: &Path) -> FieldState {
    match link {
        MeshLink::Unset => FieldState {
            label: "No preview mesh".into(),
            tooltip: "Pick a skeletal mesh to draw with the skeleton".into(),
            status: "None",
            tone: Tone::Secondary,
        },
        MeshLink::Resolving(id) => FieldState {
            label: format!("Mesh {}", id.short()),
            tooltip: "Looking for the mesh in the project".into(),
            status: "Resolving",
            tone: Tone::Secondary,
        },
        MeshLink::Resolved(entry) => FieldState {
            label: entry.name.clone(),
            tooltip: display_path(&entry.path, root),
            status: "Linked",
            tone: Tone::Success,
        },
        MeshLink::Missing(id) => FieldState {
            label: format!("Missing {}", id.short()),
            tooltip: format!("No skeletal mesh with id {id} is in the project"),
            status: "Missing",
            tone: Tone::Danger,
        },
        MeshLink::Ambiguous { id, paths } => FieldState {
            label: format!("Ambiguous {}", id.short()),
            tooltip: format!("Claimed by: {}", join_paths(paths, root)),
            status: "Ambiguous",
            tone: Tone::Danger,
        },
    }
}

/// Where the linked skeleton is, relative to the project, for the picker to
/// highlight; empty when the link does not resolve.
pub fn skeleton_picked_path(link: &SkeletonLink, root: &Path) -> String {
    match link {
        SkeletonLink::Resolved(entry) => display_path(&entry.path, root),
        _ => String::new(),
    }
}

/// Where the linked mesh is, relative to the project.
pub fn mesh_picked_path(link: &MeshLink, root: &Path) -> String {
    match link {
        MeshLink::Resolved(entry) => display_path(&entry.path, root),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::{SkeletalMeshId, SkeletonId};
    use crate::core::library::Entry;
    use crate::core::mesh_library::MeshEntry;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn root() -> PathBuf {
        PathBuf::from("/project")
    }

    fn skeleton_entry(path: &str) -> Entry {
        Entry {
            sockets: Default::default(),
            id: SkeletonId::new(),
            path: PathBuf::from(path),
            name: "Humanoid".into(),
            skeleton: Arc::new(Default::default()),
        }
    }

    #[test]
    fn every_skeleton_link_state_has_its_own_label_and_tag() {
        let root = root();
        let id = SkeletonId::new();
        let hint = "Pick the skeleton this mesh is skinned to";

        let unset = skeleton_field(&SkeletonLink::Unset, &root, hint);
        assert_eq!(
            (unset.label.as_str(), unset.status, unset.tone),
            ("No skeleton", "Unset", Tone::Warning)
        );
        assert_eq!(unset.tooltip, hint);

        let resolving = skeleton_field(&SkeletonLink::Resolving(id.clone()), &root, hint);
        assert_eq!(
            (resolving.status, resolving.tone),
            ("Resolving", Tone::Secondary)
        );
        assert!(resolving.label.contains(id.short()));

        let linked = skeleton_field(
            &SkeletonLink::Resolved(skeleton_entry("/project/rigs/Human.skeleton")),
            &root,
            hint,
        );
        assert_eq!(
            (linked.label.as_str(), linked.status, linked.tone),
            ("Humanoid", "Linked", Tone::Success)
        );
        assert_eq!(
            linked.tooltip, "rigs/Human.skeleton",
            "the path is shown relative to the project"
        );
        let sample = skeleton_field(&SkeletonLink::Resolved(skeleton_entry("")), &root, hint);
        assert!(sample.tooltip.contains("not saved"));

        let missing = skeleton_field(&SkeletonLink::Missing(id.clone()), &root, hint);
        assert_eq!((missing.status, missing.tone), ("Missing", Tone::Danger));
        assert!(missing.tooltip.contains(id.as_str()));

        let ambiguous = skeleton_field(
            &SkeletonLink::Ambiguous {
                id,
                paths: vec!["/project/a.skeleton".into(), "/project/b/c.skeleton".into()],
            },
            &root,
            hint,
        );
        assert_eq!(
            (ambiguous.status, ambiguous.tone),
            ("Ambiguous", Tone::Danger)
        );
        assert_eq!(ambiguous.tooltip, "Claimed by: a.skeleton, b/c.skeleton");
    }

    #[test]
    fn every_mesh_link_state_has_its_own_label_and_tag() {
        let root = root();
        let id = SkeletalMeshId::new();

        let unset = mesh_field(&MeshLink::Unset, &root);
        assert_eq!(
            (unset.label.as_str(), unset.status, unset.tone),
            ("No preview mesh", "None", Tone::Secondary)
        );

        let resolving = mesh_field(&MeshLink::Resolving(id.clone()), &root);
        assert_eq!(
            (resolving.status, resolving.tone),
            ("Resolving", Tone::Secondary)
        );

        let entry = MeshEntry {
            id: id.clone(),
            path: "/project/chars/Hero.skmesh".into(),
            name: "Hero".into(),
            skeleton: None,
        };
        let linked = mesh_field(&MeshLink::Resolved(entry.clone()), &root);
        assert_eq!(
            (linked.label.as_str(), linked.status, linked.tone),
            ("Hero", "Linked", Tone::Success)
        );
        assert_eq!(linked.tooltip, "chars/Hero.skmesh");

        let missing = mesh_field(&MeshLink::Missing(id.clone()), &root);
        assert_eq!((missing.status, missing.tone), ("Missing", Tone::Danger));

        let ambiguous = mesh_field(
            &MeshLink::Ambiguous {
                id,
                paths: vec!["/project/a.skmesh".into(), "/project/b.skmesh".into()],
            },
            &root,
        );
        assert_eq!(
            (ambiguous.status, ambiguous.tone),
            ("Ambiguous", Tone::Danger)
        );
        assert!(ambiguous.tooltip.contains("a.skmesh") && ambiguous.tooltip.contains("b.skmesh"));
    }

    #[test]
    fn the_picker_highlights_only_a_resolved_link() {
        let root = root();
        let entry = skeleton_entry("/project/rigs/Human.skeleton");
        assert_eq!(
            skeleton_picked_path(&SkeletonLink::Resolved(entry), &root),
            "rigs/Human.skeleton"
        );
        assert_eq!(skeleton_picked_path(&SkeletonLink::Unset, &root), "");
        assert_eq!(
            skeleton_picked_path(&SkeletonLink::Missing(SkeletonId::new()), &root),
            ""
        );

        let mesh = MeshEntry {
            id: SkeletalMeshId::new(),
            path: "/project/Hero.skmesh".into(),
            name: "Hero".into(),
            skeleton: None,
        };
        assert_eq!(
            mesh_picked_path(&MeshLink::Resolved(mesh), &root),
            "Hero.skmesh"
        );
        assert_eq!(mesh_picked_path(&MeshLink::Unset, &root), "");
        // A path outside the project is shown whole rather than mangled.
        assert_eq!(
            display_path(Path::new("/elsewhere/x.skeleton"), &root),
            "/elsewhere/x.skeleton"
        );
    }
}
