//! Disk formats of the two animation assets.
//!
//! Each asset is one standalone JSON file with a `version` and, for a
//! skeleton, an `id` header:
//!
//! - `.skeleton` — [`SkeletonAsset`]: the bone hierarchy and bind pose, plus
//!   the skeleton's stable [`SkeletonId`].
//! - `.animseq` — [`AnimSeqAsset`]: one animation sequence (fps, duration,
//!   per-bone tracks). It holds the *id* of the skeleton it animates, never
//!   the skeleton itself, and its tracks name bones by bone id.
//!
//! Reads and writes go through `engine_fs::virtual_fs` so they work on remote
//! projects too.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::animation::AnimationClip;
use super::ids::{AnimSequenceId, SkeletalMeshId, SkeletonId};
use super::skeleton::{Bone, Skeleton};

/// File extension of skeleton assets.
pub const SKELETON_EXTENSION: &str = "skeleton";
/// File extension of animation sequence assets.
pub const ANIMSEQ_EXTENSION: &str = "animseq";

/// Newest format version this build reads and writes. Older files load as is;
/// newer ones are refused rather than half-read.
pub const FORMAT_VERSION: u32 = 1;

fn current_version() -> u32 {
    FORMAT_VERSION
}

/// A skeleton file: id header plus bones.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkeletonAsset {
    #[serde(default = "current_version")]
    pub version: u32,
    /// Generated once and never changed. A file without one gets a fresh id on
    /// parse; [`SkeletonAsset::peek_id`] tells whether the text had its own.
    #[serde(default = "SkeletonId::new")]
    pub id: SkeletonId,
    #[serde(default)]
    pub bones: Vec<Bone>,
    /// A skeletal mesh to draw in the skeleton editor, for orientation only.
    /// Optional, and absent from files written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_mesh: Option<SkeletalMeshId>,
}

impl SkeletonAsset {
    /// A new asset around `skeleton`, with a fresh id.
    pub fn new(skeleton: Skeleton) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: SkeletonId::new(),
            bones: skeleton.bones,
            preview_mesh: None,
        }
    }

    pub fn skeleton(&self) -> Skeleton {
        Skeleton {
            bones: self.bones.clone(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        parse_versioned(text)
    }

    pub fn to_text(&self) -> String {
        // Serializing plain data cannot fail.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// The id written in `text`, if it has a valid one.
    pub fn peek_id(text: &str) -> Option<SkeletonId> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        SkeletonId::parse(value.get("id")?.as_str()?)
    }
}

/// An animation sequence file: the skeleton it animates, by id, plus a clip.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimSeqAsset {
    #[serde(default = "current_version")]
    pub version: u32,
    /// Stable identity retained when the file is moved or reimported.
    #[serde(default = "AnimSequenceId::new")]
    pub id: AnimSequenceId,
    /// The skeleton this sequence animates. `None` until one is picked. Kept
    /// as is even when no skeleton with this id exists in the project.
    #[serde(default)]
    pub skeleton: Option<SkeletonId>,
    /// A skeletal mesh to draw posed by this clip. Optional, and absent from
    /// files written before it existed; it only changes the preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_mesh: Option<SkeletalMeshId>,
    #[serde(flatten)]
    pub clip: AnimationClip,
}

impl AnimSeqAsset {
    pub fn new(skeleton: Option<SkeletonId>, clip: AnimationClip) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: AnimSequenceId::new(),
            skeleton,
            preview_mesh: None,
            clip,
        }
    }

    pub fn with_preview_mesh(mut self, preview_mesh: Option<SkeletalMeshId>) -> Self {
        self.preview_mesh = preview_mesh;
        self
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        parse_versioned(text)
    }

    /// The id stored in `text`, if it has a valid one.
    pub fn peek_id(text: &str) -> Option<AnimSequenceId> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        AnimSequenceId::parse(value.get("id")?.as_str()?)
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Parse `text`, refusing a `version` newer than [`FORMAT_VERSION`].
fn parse_versioned<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(version) = value.get("version").and_then(|v| v.as_u64()) {
        if version > FORMAT_VERSION as u64 {
            return Err(format!(
                "file format version {version} is newer than this editor supports ({FORMAT_VERSION})"
            ));
        }
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

pub fn read_text(path: &Path) -> anyhow::Result<String> {
    Ok(String::from_utf8(engine_fs::virtual_fs::read_file(path)?)?)
}

fn write_text(path: &Path, text: &str) -> anyhow::Result<()> {
    engine_fs::virtual_fs::write_file_atomically(path, text.as_bytes())
}

pub fn load_skeleton_asset(path: &Path) -> anyhow::Result<SkeletonAsset> {
    SkeletonAsset::parse(&read_text(path)?).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

pub fn save_skeleton_asset(path: &Path, asset: &SkeletonAsset) -> anyhow::Result<()> {
    write_text(path, &asset.to_text())
}

pub fn load_animseq_asset(path: &Path) -> anyhow::Result<AnimSeqAsset> {
    AnimSeqAsset::parse(&read_text(path)?).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

pub fn save_animseq_asset(path: &Path, asset: &AnimSeqAsset) -> anyhow::Result<()> {
    write_text(path, &asset.to_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{BoneTrack, Keyframe};
    use crate::math::Transform;

    /// A two-bone skeleton and a clip that moves the second bone.
    mod sample {
        use super::*;

        pub fn humanoid_skeleton() -> Skeleton {
            let bone = |id: &str, parent: Option<&str>| Bone {
                id: id.into(),
                name: id.into(),
                parent: parent.map(str::to_string),
                bind_transform: Transform::IDENTITY,
            };
            Skeleton {
                bones: vec![bone("root", None), bone("arm", Some("root"))],
            }
        }

        pub fn run_animation(_skeleton: &Skeleton) -> AnimationClip {
            let key = |time: f32| Keyframe::linear(time, Transform::IDENTITY);
            AnimationClip {
                name: "Run".into(),
                duration: 1.0,
                fps: 30.0,
                tracks: vec![BoneTrack {
                    bone_id: "arm".into(),
                    keyframes: vec![key(0.0), key(1.0)],
                }],
            }
        }
    }

    #[test]
    fn a_sequence_round_trips_and_keeps_its_skeleton_id() {
        let skeleton = SkeletonAsset::new(sample::humanoid_skeleton());
        let clip = sample::run_animation(&skeleton.skeleton());
        let seq = AnimSeqAsset::new(Some(skeleton.id.clone()), clip.clone());

        let back = AnimSeqAsset::parse(&seq.to_text()).unwrap();
        assert_eq!(back.skeleton, Some(skeleton.id));
        assert_eq!(back.clip.name, clip.name);
        assert_eq!(back.clip.fps, clip.fps);
        assert_eq!(back.clip.duration, clip.duration);
        assert_eq!(back.clip.tracks.len(), clip.tracks.len());
        assert_eq!(back.clip.key_count(), clip.key_count());
        // The sequence file holds a reference, not the skeleton's bones.
        let value: serde_json::Value = serde_json::from_str(&seq.to_text()).unwrap();
        assert!(value.get("bones").is_none());
        assert!(value["skeleton"].is_string());
    }

    #[test]
    fn an_unset_skeleton_round_trips_as_none() {
        let clip = sample::run_animation(&sample::humanoid_skeleton());
        let seq = AnimSeqAsset::new(None, clip);
        assert!(AnimSeqAsset::parse(&seq.to_text())
            .unwrap()
            .skeleton
            .is_none());
    }

    #[test]
    fn a_skeleton_round_trips_with_its_id() {
        let asset = SkeletonAsset::new(sample::humanoid_skeleton());
        let back = SkeletonAsset::parse(&asset.to_text()).unwrap();
        assert_eq!(back.id, asset.id);
        assert_eq!(back.bones.len(), asset.bones.len());
        assert_eq!(SkeletonAsset::peek_id(&asset.to_text()), Some(asset.id));
    }

    #[test]
    fn a_file_without_an_id_parses_but_has_nothing_to_peek() {
        let text = r#"{ "version": 1, "bones": [] }"#;
        assert_eq!(SkeletonAsset::peek_id(text), None);
        assert!(SkeletonAsset::parse(text).is_ok());
        assert_eq!(SkeletonAsset::peek_id(r#"{ "id": "nope" }"#), None);
    }

    #[test]
    fn the_preview_mesh_is_optional_and_files_without_it_still_load() {
        // A version 1 sequence written before preview meshes existed.
        let old = r#"{ "version": 1, "skeleton": null, "name": "Idle", "duration": 1.0, "fps": 30.0, "tracks": [] }"#;
        let seq = AnimSeqAsset::parse(old).unwrap();
        assert_eq!(seq.preview_mesh, None);
        assert!(
            !seq.to_text().contains("preview_mesh"),
            "an unset link adds nothing to the file"
        );

        let mesh = SkeletalMeshId::new();
        let with = AnimSeqAsset::new(None, seq.clip.clone()).with_preview_mesh(Some(mesh.clone()));
        assert_eq!(
            AnimSeqAsset::parse(&with.to_text()).unwrap().preview_mesh,
            Some(mesh.clone())
        );
        // The clip fields still flatten next to it.
        assert_eq!(
            AnimSeqAsset::parse(&with.to_text()).unwrap().clip.name,
            "Idle"
        );

        let old_skeleton =
            r#"{ "version": 1, "id": "5b7a4a3e-0000-4000-8000-000000000001", "bones": [] }"#;
        assert_eq!(
            SkeletonAsset::parse(old_skeleton).unwrap().preview_mesh,
            None
        );
        let mut skeleton = SkeletonAsset::new(sample::humanoid_skeleton());
        skeleton.preview_mesh = Some(mesh.clone());
        assert_eq!(
            SkeletonAsset::parse(&skeleton.to_text())
                .unwrap()
                .preview_mesh,
            Some(mesh)
        );
    }

    #[test]
    fn a_newer_format_is_refused() {
        let text = format!(r#"{{ "version": {}, "bones": [] }}"#, FORMAT_VERSION + 1);
        assert!(SkeletonAsset::parse(&text).unwrap_err().contains("newer"));
    }

    #[test]
    fn files_round_trip_through_the_virtual_fs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Humanoid.skeleton");
        let asset = SkeletonAsset::new(sample::humanoid_skeleton());
        save_skeleton_asset(&path, &asset).unwrap();
        assert_eq!(load_skeleton_asset(&path).unwrap().id, asset.id);

        let seq_path = dir.path().join("Run.animseq");
        let seq = AnimSeqAsset::new(
            Some(asset.id.clone()),
            sample::run_animation(&asset.skeleton()),
        );
        save_animseq_asset(&seq_path, &seq).unwrap();
        assert_eq!(
            load_animseq_asset(&seq_path).unwrap().skeleton,
            Some(asset.id)
        );
    }
}
