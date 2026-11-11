//! What the animation plugin registers for a game (and the editor): every
//! runtime contribution, grouped by the engine seam it plugs into.

use animation_component::{
    animation_asset_changed, component_animation_notify_events, flush_animation_notifies, records,
    run_animation_phase, show_pose, AnimationComponent, ANIMATION_COMPONENT,
};
use engine_backend::scene::game_hooks::{
    AssetChangedRegistration, ScriptVariableResolver, TickPhaseRegistration,
};
use engine_backend::scene::{ComponentEventCatalog, ComponentEventsRegistration};
use helio_component::skinning::{SkeletalPose, SkeletalPoseAccess};
use pulsar_class::records::RecordMigrationRegistration;
use pulsar_scenedb::World;

// ── Component classes ───────────────────────────────────────────────────────

pulsar_world_registry::submit_world_component!(AnimationComponent);

// ── Game loop ───────────────────────────────────────────────────────────────

// The animation: its phase before the game's systems (physics reads this
// frame's pose), its notifies after the scripts.
inventory::submit! {
    TickPhaseRegistration {
        origin: pulsar_plugins::origin!(),
        name: "skeletal_animation",
        before_systems: Some(run_animation_phase),
        after_scripts: Some(publish_animation_notifies),
    }
}

fn publish_animation_notifies(
    world: &mut World,
    hub: &pulsar_events::EventHub,
    resolve: ScriptVariableResolver,
) {
    flush_animation_notifies(world, hub, resolve);
}

// An animator's events, for scripts, are the notifies its animation assets
// declare.
inventory::submit! {
    ComponentEventsRegistration {
        origin: pulsar_plugins::origin!(),
        class_name: ANIMATION_COMPONENT,
        events: |root, assets| {
            let catalog = component_animation_notify_events(root, assets);
            ComponentEventCatalog {
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

// A changed animation asset file recompiles the running graphs that depend
// on it (a sequence, Blend Space, rig or Montage the graph names).
inventory::submit! {
    AssetChangedRegistration {
        origin: pulsar_plugins::origin!(),
        name: "animation.graph_dependencies",
        on_changed: animation_asset_changed,
    }
}

// ── Skinning ────────────────────────────────────────────────────────────────

// A physics rig reads and drives the animated pose
// (`helio_component::skinning`).
inventory::submit! {
    SkeletalPoseAccess {
        origin: pulsar_plugins::origin!(),
        name: "animation.animator_pose",
        component: pulsar_scenedb::component_id::<AnimationComponent>,
        pose: |world, instance| {
            let animator = world.get::<AnimationComponent>(instance)?;
            Some(SkeletalPose {
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

// ── Level records ───────────────────────────────────────────────────────────

// A skinned mesh record that held its animation gets an animator record.
inventory::submit! {
    RecordMigrationRegistration {
        origin: pulsar_plugins::origin!(),
        name: "animation.split_animator",
        description: records::SPLIT,
        entries: Some(records::split_animator),
        from_props: None,
        stale_props: &[],
        retired_fields: &[],
    }
}

// ── Asset updates ───────────────────────────────────────────────────────────

/// The custom asset kind animation files publish their updates under.
pub const ANIMATION_ASSET_KIND: &str = "animation";

/// Files of the animation family (skeleton, sequence, blend space, Montage,
/// graph, IK rig and retarget assets, pose database).
pub fn is_animation_extension(extension: &str) -> bool {
    [
        "skeleton",
        "animseq",
        "blendspace",
        "montage",
        "animgraph",
        "ikrig",
        "ikretarget",
        "animretarget",
        "posedb",
    ]
    .iter()
    .any(|known| extension.eq_ignore_ascii_case(known))
}

/// An animation file changed on disk is an animation update: the project
/// watcher publishes it and the asset-changed hooks
/// (`reload_animation_dependencies`) reload what uses it. The editor's
/// animation saves publish nothing themselves, so nothing reloads twice.
pub fn animation_change(path: &std::path::Path) -> Option<ui_types_common::AssetKind> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| is_animation_extension(ext))
        .map(|_| ui_types_common::AssetKind::Custom(ANIMATION_ASSET_KIND.to_string()))
}

inventory::submit! {
    engine_fs::AssetChangeKind {
        origin: pulsar_plugins::origin!(),
        id: "animation.files",
        kind_of: animation_change,
    }
}

#[cfg(test)]
mod tests {
    /// Every registration of the plugin names this root, so it resolves to
    /// the plugin's id.
    #[test]
    fn every_registration_resolves_to_the_plugin() {
        let ours: Vec<_> = pulsar_plugins::contributions()
            .into_iter()
            .filter(|summary| summary.origin == pulsar_plugins::origin!())
            .collect();
        assert!(ours
            .iter()
            .any(|summary| summary.category == "component.class"));
        for summary in &ours {
            assert_eq!(
                pulsar_plugins::plugin_of(summary.origin).map(|plugin| plugin.id.as_str()),
                Some("pulsar.animation"),
                "{summary:?}"
            );
        }
    }

    #[test]
    fn the_animation_family_is_recognised_by_extension_only() {
        use super::{animation_change, is_animation_extension, ANIMATION_ASSET_KIND};
        for extension in [
            "skeleton",
            "animseq",
            "blendspace",
            "montage",
            "animgraph",
            "ikrig",
            "ikretarget",
            "animretarget",
            "posedb",
            "ANIMSEQ",
        ] {
            assert!(is_animation_extension(extension), "{extension}");
        }
        for extension in ["png", "mat", "json", "animmontage", "anim_graph", ""] {
            assert!(!is_animation_extension(extension), "{extension}");
        }
        let animation = ui_types_common::AssetKind::Custom(ANIMATION_ASSET_KIND.to_string());
        assert_eq!(
            animation_change(std::path::Path::new("a/walk.animseq")),
            Some(animation.clone())
        );
        assert_eq!(
            animation_change(std::path::Path::new("a/body.SKELETON")),
            Some(animation)
        );
        assert_eq!(animation_change(std::path::Path::new("a/rock.png")), None);
    }

    #[test]
    fn the_animator_class_is_registered() {
        assert!(pulsar_world_registry::component_id_for_class(
            animation_component::ANIMATION_COMPONENT
        )
        .is_some());
    }
}
