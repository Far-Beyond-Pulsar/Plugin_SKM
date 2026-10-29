//! Animation runtime: everything that turns validated assets into poses.
//!
//! Compiled data ([`clip::CompiledClip`], compiled graphs, blend spaces, rigs,
//! pose databases) is immutable and shared between instances. Per-instance
//! mutable state lives in the types that start with `Instance`/`State` and is
//! owned by the component that plays it.

pub mod clip;
pub mod pose;
pub mod blendspace;
pub mod ik;
pub mod ikrig;
pub mod retarget;
pub mod posesearch;
pub mod montage;
pub mod graph;
pub mod memory;
pub mod anim_instance;
