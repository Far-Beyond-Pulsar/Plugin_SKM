//! The skeletal assets as data: no UI, so the editor plugin and the importer
//! share one definition of every format.
//!
//! - [`assets`]: the `.skeleton` and `.animseq` files
//! - [`ids`]: the stable ids assets are linked by
//! - [`math`], [`skeleton`], [`animation`]: the data model and pose evaluation

pub mod animation;
pub mod assets;
pub mod ids;
pub mod math;
pub mod skeleton;
