//! Animation (`pulsar.animation`): the plugin's root crate, the one place
//! the plugin registers everything it contributes.
//!
//! The plugin's other crates hold the code: `skeletal_assets` (the skeletal
//! asset formats, shared with the importer), `animation_component` (the
//! animator, its animation phase, notifies and record migration) and
//! `skeletal_animation_editor` (the animation asset editors). None of them
//! registers a plugin contribution; this crate imports them and registers
//! their registrations, so this file and the two modules below are the whole
//! list of what the plugin brings:
//!
//! - [`runtime`]: what a game links. The animator class, its animation
//!   phase and notifies, its notify catalog for scripts, the skeletal pose
//!   a physics rig reads and drives, and the record migration that split
//!   the animator from the skinned mesh.
//! - `editor` (with the `editor` feature): what the editor adds. The
//!   skeleton, sequence, skeletal mesh and animation asset editors.
//!
//! Every registration names this crate as its origin
//! (`pulsar_plugins::origin!()`), so the plugin gate skips all of them while
//! a project disables the plugin, and the plugin catalog lists them under
//! `pulsar.animation`. Hosts reach this crate through the installed set or a
//! generated game, a game without the `editor` feature.

// The plugin's descriptor (`pulsar-plugin.toml`): linking this crate installs
// the plugin; a project enables it or not (`pulsar_plugins`).
pulsar_plugins::installed!("pulsar-plugin.toml", build);

pub use animation_component;
#[cfg(feature = "editor")]
pub use skeletal_animation_editor;

mod examples;
pub mod runtime;

#[cfg(feature = "editor")]
pub mod editor;

/// Everything the plugin registers (runtime always, editor with the `editor`
/// feature), through the builder the kernel hands the plugin's root.
pub fn build(p: &mut pulsar_plugins::PluginBuilder) {
    // The plugin's `examples/` folder, read-only (`examples`).
    p.add(examples::provider(p.origin()));
    runtime::build(p);
    #[cfg(feature = "editor")]
    editor::build(p);
}
