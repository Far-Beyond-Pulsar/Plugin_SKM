//! Animation runtime: everything that turns validated assets into poses.
//!
//! Compiled data ([`clip::CompiledClip`], compiled graphs, blend spaces, rigs,
//! pose databases) is immutable and shared between instances. Per-instance
//! mutable state lives in the types that start with `Instance`/`State` and is
//! owned by the component that plays it.

pub mod anim_instance;
pub mod blendspace;
pub mod clip;
pub mod graph;
pub mod ik;
pub mod ikrig;
pub mod memory;
pub mod montage;
pub mod playback;
pub mod pose;
pub mod posesearch;
pub mod retarget;
pub mod rootmotion;
