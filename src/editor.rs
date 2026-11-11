//! What the animation plugin registers for the editor (the `editor`
//! feature): its asset editors.

use skeletal_animation_editor::builtin_provider::SkeletalAnimationBuiltinProvider;

/// Everything the animation plugin registers for the editor.
pub(crate) fn build(p: &mut pulsar_plugins::PluginBuilder) {
    // ── Asset editors ───────────────────────────────────────────────────────

    // Skeletons, sequences, skeletal meshes and every higher-level animation
    // asset (blend spaces, montages, graphs, retargets, IK rigs, pose
    // databases) open in their editors.
    p.add(plugin_manager::PluginEditorProvider {
        origin: p.origin(),
        id: SkeletalAnimationBuiltinProvider::ID,
        create: SkeletalAnimationBuiltinProvider::create,
    });
}
