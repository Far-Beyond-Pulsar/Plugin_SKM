//! The skeletal assets as data: no UI, so the editor plugin and the importer
//! share one definition of every format.
//!
//! - [`assets`]: the `.skeleton` and `.animseq` files
//! - [`skmesh`]: the `.skmesh` file (skinned geometry; binary with a JSON header)
//! - [`ids`]: the stable ids assets are linked by
//! - [`math`], [`skeleton`], [`animation`]: the data model and pose evaluation
//! - [`skinning`]: CPU skinning of a skeletal mesh by a pose
//! - [`issue`]: what asset checks report

pub mod animation;
pub mod assets;
pub mod ids;
pub mod issue;
pub mod math;
pub mod skeleton;
pub mod skinning;
pub mod skmesh;
