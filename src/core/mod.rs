//! The plugin's core: the project libraries, validation and migration. The
//! asset formats, ids, math and skinning live in the `skeletal_assets` crate
//! (shared with the importer) and are re-exported here under their old paths.

pub mod library;
pub mod material_ids;
pub mod mesh_library;
pub mod migrate;
pub mod sample;
pub mod serialization;
pub mod validate;

pub use skeletal_assets::{animation, assets, ids, issue, math, skeleton, skinning, skmesh};

pub use animation::{
    bone_world_transform, evaluate_world_transforms, AnimationClip, BoneTrack, KeyInterpolation,
    Keyframe, KeyframeTangent,
};
pub use ids::{AnimSequenceId, SkeletalMeshId, SkeletonId};
pub use library::{Library, SkeletonLink};
pub use math::{Mat4, Transform, Vec3};
pub use mesh_library::{MeshLibrary, MeshLink};
pub use skeleton::Skeleton;
