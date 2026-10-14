//! The plugin's core: the project library, validation and migration. The asset
//! formats, ids and math live in the `skeletal_assets` crate (shared with the
//! importer) and are re-exported here under their old paths.

pub mod library;
pub mod migrate;
pub mod sample;
pub mod serialization;
pub mod validate;

pub use skeletal_assets::{animation, assets, ids, math, skeleton};

pub use animation::{bone_world_transform, evaluate_world_transforms, AnimationClip, BoneTrack, Keyframe};
pub use ids::SkeletonId;
pub use library::{Library, SkeletonLink};
pub use math::{Mat4, Transform, Vec3};
pub use skeleton::Skeleton;
