//! The plugin's editor provider: its file types and editors, offered to
//! the editor while the plugin is enabled (`plugin_manager::PluginEditorProvider`).

use gpui::{App, Window};
use plugin_editor_api::*;
use plugin_manager::{BuiltinEditorProvider, EditorContext};
use std::path::PathBuf;
use std::sync::Arc;
use ui::dock::PanelView;

/// Wraps the skeletal_animation_plugin crate as a built-in editor provider.
/// Opens `.skeleton`, `.skmesh` and `.animseq` files (and old `.skelanim`
/// folders, which are converted) in the Skeleton, Skeletal Mesh and Animation
/// Sequence editors, and each higher-level animation asset (`.blendspace`,
/// `.montage`, `.animgraph`, `.animretarget`, `.ikrig`, `.ikretarget`,
/// `.posedb`) in its own editor.
pub struct SkeletalAnimationBuiltinProvider;

impl BuiltinEditorProvider for SkeletalAnimationBuiltinProvider {
    fn provider_id(&self) -> &str {
        "com.pulsar.skeletal-animation-editor"
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        crate::SkeletalAnimationPlugin::default().file_types()
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        crate::SkeletalAnimationPlugin::default().editors()
    }

    fn can_handle(&self, editor_id: &EditorId) -> bool {
        crate::provides_editor(editor_id.as_str())
    }

    fn create_editor(
        &self,
        file_path: PathBuf,
        editor_context: &EditorContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        let undo = editor_context.services.undo.clone();
        crate::SkeletalAnimationPlugin::create_editor_with_undo(
            file_path,
            Some(&undo),
            window,
            cx,
        )
    }

    fn create_editor_for(
        &self,
        editor_id: &EditorId,
        file_path: PathBuf,
        editor_context: &EditorContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        let undo = editor_context.services.undo.clone();
        crate::SkeletalAnimationPlugin::create_editor_for(
            editor_id.as_str(),
            file_path,
            Some(&undo),
            window,
            cx,
        )
    }
}

plugin_manager::inventory::submit! {
    plugin_manager::PluginEditorProvider {
        origin: pulsar_plugins::origin!(),
        create: || Arc::new(SkeletalAnimationBuiltinProvider),
    }
}
