//! The skeletal assets as data: no UI, so the editor plugin and the importer
//! share one definition of every format.
//!
//! - [`assets`]: the `.skeleton` and `.animseq` files
//! - [`skmesh`]: the `.skmesh` file (skinned geometry; binary with a JSON header)
//! - [`blend_space`], [`montage`], [`anim_graph`]: the `.blendspace`, `.montage`
//!   and `.animgraph` files
//! - [`ik_rig`], [`retarget`], [`ik_retarget`], [`pose_db`]: the `.ikrig`,
//!   `.animretarget`, `.ikretarget` and `.posedb` files
//! - [`dependencies`]: typed edges between all of the above, and the checks
//!   (broken refs, skeleton compatibility, cycles) that run over them
//! - [`ids`]: the stable ids assets are linked by
//! - [`math`], [`skeleton`], [`animation`]: the data model and pose evaluation
//! - [`skinning`]: CPU skinning of a skeletal mesh by a pose
//! - [`codec`], [`diagnostics`], [`issue`]: shared codec helpers and what checks report

pub mod animation;
pub mod assets;
pub mod codec;
pub mod diagnostics;
pub mod ids;
pub mod issue;
pub mod math;
pub mod skeleton;
pub mod skinning;
pub mod skmesh;

pub mod sockets;

pub mod curves;
pub mod dependencies;
pub mod notifies;
pub mod notify_schema;
pub mod root_motion;
pub mod sequence_features;

pub mod anim_graph;
pub mod blend_space;
pub mod bone_mask;
pub mod ik_retarget;
pub mod ik_rig;
pub mod montage;
pub mod picker;
pub mod pose_db;
pub mod report;
pub mod retarget;
