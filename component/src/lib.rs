//! Animation (`pulsar.animation`): the component crate of the plugin that
//! owns skeletons, animation assets and the animator.
//!
//! - [`AnimationComponent`]: the animator. It plays an animation graph (or an
//!   old scene's single sequence), Montages and root motion on the skeleton
//!   of the skinned mesh it drives, raises the notifies its assets declare,
//!   and shows its pose on that mesh
//!   (`helio_component::skinning::SkinnedMeshAccess`, the meshes').
//! - What the engine runs for it, each through a generic registration: the
//!   animation phase and the notifies (`TickPhaseRegistration`,
//!   `ComponentEventsRegistration`), the pose a physics rig reads and drives
//!   (`helio_component::skinning::SkeletalPoseAccess`) and the record
//!   migration that split the animator from the skinned mesh ([`records`]).
//!
//! Everything registers through `inventory` once the crate is linked.

// The plugin's descriptor (`../pulsar-plugin.toml`): linking this crate
// installs the plugin; a project enables it or not (`pulsar_plugins`).
pulsar_plugins::installed!("../pulsar-plugin.toml");

pub mod anim_source;
mod component;
pub mod records;

pub use component::*;

use pulsar_scenedb::World;

// The animation: its phase before the game's systems (physics reads this
// frame's pose), its notifies after the scripts.
inventory::submit! {
    engine_backend::scene::game_hooks::TickPhaseRegistration {
        origin: pulsar_plugins::origin!(),
        name: "skeletal_animation",
        before_systems: Some(run_animation_phase),
        after_scripts: Some(publish_animation_notifies),
    }
}

fn publish_animation_notifies(
    world: &mut World,
    hub: &pulsar_events::EventHub,
    resolve: engine_backend::scene::game_hooks::ScriptVariableResolver,
) {
    flush_animation_notifies(world, hub, resolve);
}

// A physics rig reads and drives the animated pose
// (`helio_component::skinning`).
inventory::submit! {
    helio_component::skinning::SkeletalPoseAccess {
        origin: pulsar_plugins::origin!(),
        component: pulsar_scenedb::component_id::<AnimationComponent>,
        pose: |world, instance| {
            let animator = world.get::<AnimationComponent>(instance)?;
            Some(helio_component::skinning::SkeletalPose {
                mesh: animator.mesh.clone(),
                bone_ids: animator.bone_ids.to_vec(),
                bone_world_pose: animator.bone_world_pose.clone(),
            })
        },
        drive_bones: |world, instance, bones| {
            let driven = world
                .get_mut::<AnimationComponent>(instance)
                .is_some_and(|mut animator| animator.apply_physics_bones(bones));
            show_pose(world, instance);
            driven
        },
        release_bones: |world, instance| {
            if let Some(mut animator) = world.get_mut::<AnimationComponent>(instance) {
                animator.clear_physics_pose();
            }
            show_pose(world, instance);
        },
    }
}

// An animator's events are the notifies its animation assets declare.
inventory::submit! {
    engine_backend::scene::ComponentEventsRegistration {
        origin: pulsar_plugins::origin!(),
        class_name: ANIMATION_COMPONENT,
        events: |root, assets| {
            let catalog = component_animation_notify_events(root, assets);
            engine_backend::scene::ComponentEventCatalog {
                events: catalog.events,
                diagnostics: catalog
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| (diagnostic.path, diagnostic.error))
                    .collect(),
            }
        },
    }
}
