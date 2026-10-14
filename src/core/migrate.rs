//! Conversion of the old monolithic `.skelanim` folder (`skeleton.json` +
//! `animation.json`) into a `.skeleton` + `.animseq` pair.
//!
//! The conversion never touches the old folder; the new files are written next
//! to it, and the sequence refers to the new skeleton by id. Bone ids are kept
//! as they were, so every track still lands on its bone.

use std::path::{Path, PathBuf};

use super::animation::AnimationClip;
use super::assets::{
    load_animseq_asset, load_skeleton_asset, save_animseq_asset, save_skeleton_asset, AnimSeqAsset,
    SkeletonAsset, ANIMSEQ_EXTENSION, SKELETON_EXTENSION,
};
use super::serialization::{self, SKELETON_FILE};
use super::skeleton::Skeleton;

/// The result of converting one `.skelanim` folder.
#[derive(Clone, Debug)]
pub struct Migrated {
    pub skeleton_path: PathBuf,
    pub sequence_path: PathBuf,
    /// The folder had been converted before; the earlier pair was reused
    /// instead of writing another.
    pub reused: bool,
}

/// Whether `path` is an old-format asset folder.
pub fn is_legacy_folder(path: &Path) -> bool {
    path.is_dir() && path.join(SKELETON_FILE).is_file()
}

/// Split an old asset's data into the two new assets.
pub fn convert(skeleton: Skeleton, clip: AnimationClip) -> (SkeletonAsset, AnimSeqAsset) {
    let skeleton = SkeletonAsset::new(skeleton);
    let sequence = AnimSeqAsset::new(Some(skeleton.id.clone()), clip);
    (skeleton, sequence)
}

/// Convert the `.skelanim` folder `dir` into `<name>.skeleton` and
/// `<name>.animseq` in the folder's parent directory.
///
/// Converting the same folder again reuses the earlier pair. If other files
/// already have those names, the new pair is named `<name> 2`, `<name> 3`, ...
pub fn migrate_folder(dir: &Path) -> anyhow::Result<Migrated> {
    let parent = dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", dir.display()))?;
    let stem = dir
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Animation".to_string());

    let paths = |name: &str| {
        (
            parent.join(format!("{name}.{SKELETON_EXTENSION}")),
            parent.join(format!("{name}.{ANIMSEQ_EXTENSION}")),
        )
    };

    let (skeleton_path, sequence_path) = paths(&stem);
    if skeleton_path.is_file() && sequence_path.is_file() {
        if let (Ok(skeleton), Ok(sequence)) = (
            load_skeleton_asset(&skeleton_path),
            load_animseq_asset(&sequence_path),
        ) {
            if sequence.skeleton.as_ref() == Some(&skeleton.id) {
                return Ok(Migrated {
                    skeleton_path,
                    sequence_path,
                    reused: true,
                });
            }
        }
    }

    let skeleton = serialization::load_skeleton(dir)?;
    let clip = serialization::load_animation(dir)?;
    let (skeleton_asset, sequence_asset) = convert(skeleton, clip);

    let (skeleton_path, sequence_path) = (1..)
        .map(|n| {
            if n == 1 {
                stem.clone()
            } else {
                format!("{stem} {n}")
            }
        })
        .map(|name| paths(&name))
        .find(|(s, a)| !s.exists() && !a.exists())
        .expect("an unbounded range always yields a free name");

    // The skeleton goes first so that a failure part-way leaves no sequence
    // pointing at nothing.
    save_skeleton_asset(&skeleton_path, &skeleton_asset)?;
    save_animseq_asset(&sequence_path, &sequence_asset)?;
    Ok(Migrated {
        skeleton_path,
        sequence_path,
        reused: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::library::{Library, SkeletonLink};
    use crate::core::sample;

    fn legacy(dir: &Path, name: &str) -> PathBuf {
        let folder = dir.join(name);
        let skeleton = sample::humanoid_skeleton();
        serialization::save_skeleton(&folder, &skeleton).unwrap();
        serialization::save_animation(&folder, &sample::run_animation(&skeleton)).unwrap();
        folder
    }

    fn bytes(folder: &Path) -> (Vec<u8>, Vec<u8>) {
        (
            std::fs::read(folder.join("skeleton.json")).unwrap(),
            std::fs::read(folder.join("animation.json")).unwrap(),
        )
    }

    #[test]
    fn an_old_folder_becomes_a_skeleton_and_a_sequence_that_references_it() {
        let dir = tempfile::tempdir().unwrap();
        let folder = legacy(dir.path(), "Run.skelanim");
        assert!(is_legacy_folder(&folder));
        let before = bytes(&folder);

        let migrated = migrate_folder(&folder).unwrap();
        assert!(!migrated.reused);
        assert_eq!(migrated.skeleton_path, dir.path().join("Run.skeleton"));
        assert_eq!(migrated.sequence_path, dir.path().join("Run.animseq"));

        let old = sample::humanoid_skeleton();
        let skeleton = load_skeleton_asset(&migrated.skeleton_path).unwrap();
        assert!(!skeleton.id.as_str().is_empty());
        assert_eq!(skeleton.bones.len(), old.bones.len());
        let sequence = load_animseq_asset(&migrated.sequence_path).unwrap();
        assert_eq!(sequence.skeleton, Some(skeleton.id.clone()));
        let clip = sample::run_animation(&old);
        assert_eq!(sequence.clip.tracks.len(), clip.tracks.len());
        assert_eq!(sequence.clip.key_count(), clip.key_count());
        // Every track still names a bone that exists.
        assert!(sequence.clip.orphan_tracks(&skeleton.skeleton()).is_empty());

        // The project index resolves the new reference.
        let lib = Library::scan(dir.path());
        assert!(matches!(
            lib.link(sequence.skeleton.as_ref()),
            SkeletonLink::Resolved(_)
        ));

        // The old folder is exactly as it was.
        assert!(is_legacy_folder(&folder));
        assert_eq!(bytes(&folder), before);
    }

    #[test]
    fn converting_twice_reuses_the_first_pair() {
        let dir = tempfile::tempdir().unwrap();
        let folder = legacy(dir.path(), "Run.skelanim");
        let first = migrate_folder(&folder).unwrap();
        let second = migrate_folder(&folder).unwrap();
        assert!(second.reused);

        assert_eq!(second.sequence_path, first.sequence_path);
        let created = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(created, 3, "the folder plus one pair");
    }

    #[test]
    fn existing_unrelated_files_are_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let folder = legacy(dir.path(), "Run.skelanim");
        let theirs = SkeletonAsset::new(sample::humanoid_skeleton());
        save_skeleton_asset(&dir.path().join("Run.skeleton"), &theirs).unwrap();

        let migrated = migrate_folder(&folder).unwrap();
        assert_eq!(migrated.skeleton_path, dir.path().join("Run 2.skeleton"));
        assert_eq!(migrated.sequence_path, dir.path().join("Run 2.animseq"));
        assert_eq!(
            load_skeleton_asset(&dir.path().join("Run.skeleton"))
                .unwrap()
                .id,
            theirs.id
        );
    }

    #[test]
    fn a_folder_without_its_files_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Broken.skelanim");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(!is_legacy_folder(&folder));
        assert!(migrate_folder(&folder).is_err());
        assert!(!dir.path().join("Broken.skeleton").exists());
    }
}
