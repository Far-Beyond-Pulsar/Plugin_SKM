//! What the animation plugin registers for the editor (the `editor`
//! feature): its asset editors.

use skeletal_animation_editor::builtin_provider::SkeletalAnimationBuiltinProvider;

// ── Asset editors ───────────────────────────────────────────────────────────

// Skeletons, sequences, skeletal meshes and every higher-level animation
// asset (blend spaces, montages, graphs, retargets, IK rigs, pose databases)
// open in their editors.
inventory::submit! {
    plugin_manager::PluginEditorProvider {
        origin: pulsar_plugins::origin!(),
        id: SkeletalAnimationBuiltinProvider::ID,
        create: SkeletalAnimationBuiltinProvider::create,
    }
}
