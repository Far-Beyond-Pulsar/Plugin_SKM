//! Animation (`pulsar.animation`): the component crate of the plugin that
//! owns skeletons, animation assets and the animator.
//!
//! - [`AnimationComponent`]: the animator. It plays an animation graph (or an
//!   old scene's single sequence), Montages and root motion on the skeleton
//!   of the skinned mesh it drives, raises the notifies its assets declare,
//!   and shows its pose on that mesh
//!   (`helio_component::skinning::SkinnedMeshAccess`, the meshes').
//! - What the engine runs for it: the animation phase and the notifies
//!   ([`run_animation_phase`], [`flush_animation_notifies`],
//!   [`component_animation_notify_events`]), the pose a physics rig reads and
//!   drives ([`AnimationComponent::apply_physics_bones`], [`show_pose`]) and
//!   the record migration that split the animator from the skinned mesh
//!   ([`records`]).
//!
//! This crate registers no plugin contribution: the plugin's root crate
//! (`skeletal_animation_plugin`) submits the component class, the tick phase,
//! the notify catalog, the asset-change hook, the skeletal pose access and
//! the record migration.
//! Type attachments stay with the type (its handle ledger entry, reflected
//! methods, scene-props applier and component events).

pub mod anim_source;
mod component;
pub mod records;

pub use component::*;

// The unit tests attach animators to worlds, which takes a registered class.
// A test build of this crate does not link the root crate, so it registers
// the class itself; every other build gets it from the root only.
#[cfg(test)]
pulsar_world_registry::submit_world_component!(AnimationComponent);
