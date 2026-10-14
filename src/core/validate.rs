//! Problems with an animation sequence's link to its skeleton.

use super::animation::AnimationClip;
use super::library::SkeletonLink;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub message: String,
}

impl Issue {
    fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
        }
    }
}

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
